// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `BoardView` — the goban. Step 9.
//!
//! Everything is drawn in `WidgetImpl::snapshot` with `gsk`. The static part of the board
//! (wood, grid, star points, coordinates) never changes between allocations, so it is built
//! once into a throwaway `gtk::Snapshot`, turned into a `gsk::RenderNode` and replayed with a
//! single `append_node` every frame.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango};

use mirai_core::{Board, COLUMNS, Color, DeadSet, MarkKind, Point, Size};
use mirai_proto::types::dq_policy;

use crate::app::{AppState, signal};

/// Wood beyond the outermost grid line, in cells.
const EDGE_PAD: f32 = 0.6;
/// Extra band reserved outside the wood for coordinate labels, in cells.
const COORD_PAD: f32 = 0.55;

/// Cached geometry, recomputed in `size_allocate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layout {
    pub cell: f32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub stone_r: f32,
    pub width: i32,
    pub height: i32,
}

impl Layout {
    /// Centre of the intersection `(x, y)` in widget coordinates.
    #[inline]
    pub fn xy(&self, x: u8, y: u8) -> (f32, f32) {
        (
            self.origin_x + x as f32 * self.cell,
            self.origin_y + y as f32 * self.cell,
        )
    }

    /// Computes the geometry for a `size` board inside a `width` x `height` allocation.
    fn compute(width: i32, height: i32, size: Size, coords: bool) -> Layout {
        let extra = 2.0 * EDGE_PAD + if coords { 2.0 * COORD_PAD } else { 0.0 };
        let units_x = (size.w as f32 - 1.0).max(1.0) + extra;
        let units_y = (size.h as f32 - 1.0).max(1.0) + extra;
        let cell = (width as f32 / units_x)
            .min(height as f32 / units_y)
            .max(0.0);
        let grid_w = (size.w as f32 - 1.0) * cell;
        let grid_h = (size.h as f32 - 1.0) * cell;
        Layout {
            cell,
            origin_x: (width as f32 - grid_w) * 0.5,
            origin_y: (height as f32 - grid_h) * 0.5,
            stone_r: cell * 0.48,
            width,
            height,
        }
    }

    /// The intersection nearest to `(x, y)`, if the pointer is within half a cell of it.
    fn hit(&self, size: Size, x: f64, y: f64) -> Option<Point> {
        if self.cell <= 0.0 {
            return None;
        }
        let fx = (x as f32 - self.origin_x) / self.cell;
        let fy = (y as f32 - self.origin_y) / self.cell;
        let ix = fx.round();
        let iy = fy.round();
        if ix < 0.0 || iy < 0.0 || ix > size.w as f32 - 1.0 || iy > size.h as f32 - 1.0 {
            return None;
        }
        let dx = (fx - ix) * self.cell;
        let dy = (fy - iy) * self.cell;
        if dx * dx + dy * dy > (self.cell * 0.5) * (self.cell * 0.5) {
            return None;
        }
        Some(size.point(ix as u8, iy as u8))
    }
}

/// The Lizzie visit ramp: blue (rarely searched) through green to red (the engine's choice).
const VISIT_RAMP: [(f32, [u8; 3]); 5] = [
    (0.00, [0x28, 0x48, 0xC8]),
    (0.25, [0x28, 0xA0, 0xA0]),
    (0.50, [0x48, 0xC8, 0x48]),
    (0.75, [0xE8, 0xC8, 0x38]),
    (1.00, [0xE8, 0x50, 0x38]),
];

#[inline]
fn lerp8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// Colour for a candidate whose relative visit share is `f` in `0..=1`.
fn ramp_rgb(f: f32) -> [u8; 3] {
    let f = if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
    let mut i = 0;
    while i + 2 < VISIT_RAMP.len() && f > VISIT_RAMP[i + 1].0 {
        i += 1;
    }
    let (f0, c0) = VISIT_RAMP[i];
    let (f1, c1) = VISIT_RAMP[i + 1];
    let t = if f1 > f0 { (f - f0) / (f1 - f0) } else { 0.0 };
    [
        lerp8(c0[0], c1[0], t),
        lerp8(c0[1], c1[1], t),
        lerp8(c0[2], c1[2], t),
    ]
}

#[inline]
fn rgba8(c: [u8; 3], alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        alpha,
    )
}

/// Black or white text, whichever reads better on `rgb`.
fn text_on(rgb: [u8; 3]) -> gdk::RGBA {
    let lum = (0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32) / 255.0;
    if lum > 0.58 {
        gdk::RGBA::new(0.04, 0.04, 0.04, 1.0)
    } else {
        gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)
    }
}

fn wood_color(dark: bool) -> gdk::RGBA {
    if dark {
        gdk::RGBA::new(0.60, 0.45, 0.27, 1.0)
    } else {
        gdk::RGBA::new(0.85, 0.69, 0.42, 1.0)
    }
}

const LINE_COLOR: gdk::RGBA = gdk::RGBA::new(0.10, 0.08, 0.06, 0.85);

#[inline]
fn circle(cx: f32, cy: f32, r: f32) -> gsk::Path {
    let pb = gsk::PathBuilder::new();
    pb.add_circle(&graphene::Point::new(cx, cy), r);
    pb.to_path()
}

/// The highlight applied to the stone just played, in the two shades that read against
/// the stone underneath it.
fn last_move_tint(color: Color) -> gdk::RGBA {
    match color {
        Color::Black => gdk::RGBA::new(1.0, 0.35, 0.25, 1.0),
        Color::White => gdk::RGBA::new(0.85, 0.15, 0.10, 1.0),
    }
}

fn is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

/// A callback that gets first refusal on a click, and reports whether it consumed it.
pub type ClickHook = Rc<dyn Fn(Point) -> bool + 'static>;

/// The geometry and position a single `snapshot` pass draws against.
///
/// Every `draw_*` layer needs the same four values, so they travel together rather than
/// as four repeated parameters.
#[derive(Clone, Copy)]
pub struct Scene<'a> {
    /// The resolved geometry: cell size and board origin.
    pub l: Layout,
    /// The board size `l` was computed for.
    pub size: Size,
    /// The position being drawn, which is not always `state.position()` — the PV
    /// preview draws a hypothetical board.
    pub board: &'a Board,
    /// Whether the dark theme is active; layers that paint their own backing pick
    /// contrasting colours from it.
    pub dark: bool,
}

mod imp {
    use super::*;

    /// Everything the static layer depends on. When this changes it is rebuilt.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct StaticKey {
        pub width: i32,
        pub height: i32,
        pub sw: u8,
        pub sh: u8,
        pub coords: bool,
        pub dark: bool,
    }

    #[derive(Default)]
    pub struct BoardView {
        pub state: RefCell<Option<AppState>>,
        pub layout: Cell<Layout>,
        /// Candidate index under the pointer.
        pub hover: Cell<Option<usize>>,
        /// Candidate index pinned by the analysis list.
        pub pinned: Cell<Option<usize>>,
        pub static_layer: RefCell<Option<(StaticKey, gsk::RenderNode)>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        pub menu_point: Cell<Option<Point>>,
        pub click_hook: RefCell<Option<ClickHook>>,
        pub dead: RefCell<Option<DeadSet>>,
        pub territory: RefCell<Option<Box<[Option<Color>]>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BoardView {
        const NAME: &'static str = "MiraiBoardView";
        type Type = super::BoardView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BoardView {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().add_css_class("board-area");
        }

        fn dispose(&self) {
            if let Some(popover) = self.popover.borrow_mut().take() {
                popover.unparent();
            }
        }
    }

    impl BoardView {
        pub(super) fn board_size(&self) -> Size {
            match self.state.borrow().as_ref() {
                Some(state) => state.tree().info.size,
                None => Size::square(19),
            }
        }

        fn show_coords(&self) -> bool {
            self.state
                .borrow()
                .as_ref()
                .is_some_and(|s| s.show_coordinates())
        }

        /// Builds the wood, grid, star points and coordinate labels once.
        fn build_static(&self, l: Layout, size: Size, coords: bool, dark: bool) -> Option<gsk::RenderNode> {
            let obj = self.obj();
            let s = gtk::Snapshot::new();
            let cell = l.cell;
            let pad = cell * EDGE_PAD;
            let grid_w = (size.w as f32 - 1.0) * cell;
            let grid_h = (size.h as f32 - 1.0) * cell;

            // Wood.
            let rect = graphene::Rect::new(
                l.origin_x - pad,
                l.origin_y - pad,
                grid_w + 2.0 * pad,
                grid_h + 2.0 * pad,
            );
            let pb = gsk::PathBuilder::new();
            pb.add_rounded_rect(&gsk::RoundedRect::from_rect(rect, (cell * 0.2).min(8.0)));
            s.append_fill(&pb.to_path(), gsk::FillRule::Winding, &wood_color(dark));

            // Grid.
            let gb = gsk::PathBuilder::new();
            for y in 0..size.h {
                let (_, cy) = l.xy(0, y);
                gb.move_to(l.origin_x, cy);
                gb.line_to(l.origin_x + grid_w, cy);
            }
            for x in 0..size.w {
                let (cx, _) = l.xy(x, 0);
                gb.move_to(cx, l.origin_y);
                gb.line_to(cx, l.origin_y + grid_h);
            }
            s.append_stroke(
                &gb.to_path(),
                &gsk::Stroke::new((cell * 0.035).max(1.0)),
                &LINE_COLOR,
            );

            // A heavier border around the playing area.
            let bb = gsk::PathBuilder::new();
            bb.move_to(l.origin_x, l.origin_y);
            bb.line_to(l.origin_x + grid_w, l.origin_y);
            bb.line_to(l.origin_x + grid_w, l.origin_y + grid_h);
            bb.line_to(l.origin_x, l.origin_y + grid_h);
            bb.close();
            s.append_stroke(
                &bb.to_path(),
                &gsk::Stroke::new((cell * 0.06).max(1.4)),
                &LINE_COLOR,
            );

            // Star points.
            let star_r = (cell * 0.10).max(1.5);
            let sb = gsk::PathBuilder::new();
            let mut any_star = false;
            for p in size.star_points() {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                sb.add_circle(&graphene::Point::new(cx, cy), star_r);
                any_star = true;
            }
            if any_star {
                s.append_fill(&sb.to_path(), gsk::FillRule::Winding, &LINE_COLOR);
            }

            // Coordinates.
            if coords {
                let mut fd = obj
                    .pango_context()
                    .font_description()
                    .unwrap_or_default();
                fd.set_absolute_size((cell * 0.34) as f64 * pango::SCALE as f64);
                let mut fg = obj.color();
                fg.set_alpha(fg.alpha() * 0.8);
                let out = cell * (EDGE_PAD + COORD_PAD * 0.5);

                for x in 0..size.w {
                    let letter = COLUMNS[(x as usize).min(COLUMNS.len() - 1)] as char;
                    let layout = obj.create_pango_layout(Some(&letter.to_string()));
                    layout.set_font_description(Some(&fd));
                    let (cx, _) = l.xy(x, 0);
                    draw_text(&s, &layout, cx, l.origin_y - out, &fg);
                    draw_text(&s, &layout, cx, l.origin_y + grid_h + out, &fg);
                }
                for y in 0..size.h {
                    let number = size.h as u32 - y as u32;
                    let layout = obj.create_pango_layout(Some(&number.to_string()));
                    layout.set_font_description(Some(&fd));
                    let (_, cy) = l.xy(0, y);
                    draw_text(&s, &layout, l.origin_x - out, cy, &fg);
                    draw_text(&s, &layout, l.origin_x + grid_w + out, cy, &fg);
                }
            }

            s.to_node()
        }

        fn static_node(&self, l: Layout, size: Size, coords: bool, dark: bool) -> Option<gsk::RenderNode> {
            let key = StaticKey {
                width: l.width,
                height: l.height,
                sw: size.w,
                sh: size.h,
                coords,
                dark,
            };
            if let Some((cached, node)) = self.static_layer.borrow().as_ref()
                && *cached == key
            {
                return Some(node.clone());
            }
            let node = self.build_static(l, size, coords, dark)?;
            *self.static_layer.borrow_mut() = Some((key, node.clone()));
            Some(node)
        }
    }

    impl WidgetImpl for BoardView {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (180, 600, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let size = self.board_size();
            let layout = Layout::compute(width, height, size, self.show_coords());
            if self.layout.replace(layout) != layout {
                self.static_layer.borrow_mut().take();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(state) = self.state.borrow().clone() else {
                return;
            };
            let l = self.layout.get();
            if l.cell < 3.0 {
                return;
            }
            let (size, rules) = {
                let tree = state.tree();
                (tree.info.size, tree.info.rules.rules())
            };
            let dark = is_dark();

            if let Some(node) = self.static_node(l, size, state.show_coordinates(), dark) {
                snapshot.append_node(&node);
            }

            let report = state.last_report();
            let position = state.position();
            let cursor = state.cursor();

            // Heat maps cover the intersections, half a cell beyond the outer lines.
            let heat = graphene::Rect::new(
                l.origin_x - l.cell * 0.5,
                l.origin_y - l.cell * 0.5,
                size.w as f32 * l.cell,
                size.h as f32 * l.cell,
            );

            if state.ownership_overlay()
                && let Some(report) = report.as_ref()
                && let Some(own) = report.ownership.as_ref()
                && own.len() == size.points()
            {
                let mut buf = vec![0u8; size.points() * 4];
                for (i, &v) in own.iter().enumerate() {
                    let a = (v.unsigned_abs() as f32 / 127.0) * 0.45;
                    // Black is positive; premultiplied, so white is `255 * a`.
                    let c = if v >= 0 { 0.0 } else { 255.0 * a };
                    let px = &mut buf[i * 4..i * 4 + 4];
                    px[0] = c as u8;
                    px[1] = c as u8;
                    px[2] = c as u8;
                    px[3] = (a * 255.0) as u8;
                }
                blit(snapshot, buf, size, &heat);
            }

            if state.policy_overlay()
                && let Some(report) = report.as_ref()
                && let Some(policy) = report.policy.as_ref()
                && policy.len() >= size.points()
            {
                let accent = adw::StyleManager::default().accent_color_rgba();
                let mut buf = vec![0u8; size.points() * 4];
                for i in 0..size.points() {
                    let Some(p) = dq_policy(policy[i]) else {
                        continue;
                    };
                    let a = p.max(0.0).sqrt() * 0.6;
                    let px = &mut buf[i * 4..i * 4 + 4];
                    px[0] = (accent.red() * a * 255.0) as u8;
                    px[1] = (accent.green() * a * 255.0) as u8;
                    px[2] = (accent.blue() * a * 255.0) as u8;
                    px[3] = (a * 255.0) as u8;
                }
                blit(snapshot, buf, size, &heat);
            }

            // Layers 4-7, or the hovered PV preview in their place.
            let preview = self.obj().active_preview();
            if let Some(idx) = preview
                && let Some(report) = report.as_ref()
                && let Some(info) = report.moves.get(idx)
            {
                let mut board = position.board.clone();
                let mut color = position.to_play;
                let mut seq = vec![0u16; size.points()];
                let mut n = 0u16;
                for &p in &info.pv {
                    n += 1;
                    if p.is_pass() {
                        color = color.other();
                        continue;
                    }
                    if !size.contains(p) || board.play(color, p, &rules).is_err() {
                        break;
                    }
                    seq[p.index()] = n;
                    color = color.other();
                }
                let scene = Scene { l, size, board: &board, dark };
                self.draw_stones(snapshot, scene, None);
                self.draw_numbers(snapshot, scene, &seq, None);
                return;
            }

            let scene = Scene { l, size, board: &position.board, dark };
            let dead = self.dead.borrow();
            self.draw_stones(snapshot, scene, dead.as_ref());
            if let Some(territory) = self.territory.borrow().as_ref() {
                self.draw_territory(snapshot, scene, territory);
            }
            drop(dead);

            // The stone just played, when it survived the capture resolution.
            let last = { state.tree().node(cursor).mv }
                .filter(|&(color, p)| !p.is_pass() && position.board.at(p) == Some(color));

            if state.show_move_numbers() {
                let mut nums = vec![0u16; size.points()];
                {
                    let tree = state.tree();
                    for id in tree.path_to(cursor) {
                        if let Some((_, p)) = tree.node(id).mv
                            && !p.is_pass()
                            && size.contains(p)
                        {
                            nums[p.index()] = tree.move_number(id);
                        }
                    }
                }
                // The number itself carries the highlight; a mark would only hide it.
                self.draw_numbers(snapshot, scene, &nums, last.map(|(_, p)| p));
            } else if let Some((color, p)) = last {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                snapshot.append_fill(
                    &circle(cx, cy, l.stone_r * 0.34),
                    gsk::FillRule::Winding,
                    &last_move_tint(color),
                );
            }

            self.draw_marks(snapshot, scene, cursor, &state);

            if let Some(report) = report.as_ref() {
                self.draw_candidates(snapshot, scene, position.to_play, report, &state);
            }
        }
    }

    impl BoardView {
        fn draw_stones(
            &self,
            snapshot: &gtk::Snapshot,
            scene: Scene,
            dead: Option<&DeadSet>,
        ) {
            let Scene { l, size, board, .. } = scene;
            let r = l.stone_r;
            let off = (l.cell * 0.05).max(1.0);

            // One combined shadow pass so stones are never drawn over a neighbour's shadow.
            let sb = gsk::PathBuilder::new();
            let mut any = false;
            for y in 0..size.h {
                for x in 0..size.w {
                    if board.at(size.point(x, y)).is_some() {
                        let (cx, cy) = l.xy(x, y);
                        sb.add_circle(&graphene::Point::new(cx + off, cy + off), r);
                        any = true;
                    }
                }
            }
            if !any {
                return;
            }
            snapshot.append_fill(
                &sb.to_path(),
                gsk::FillRule::Winding,
                &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25),
            );

            for y in 0..size.h {
                for x in 0..size.w {
                    let p = size.point(x, y);
                    let Some(color) = board.at(p) else { continue };
                    let (cx, cy) = l.xy(x, y);
                    let ghost = dead.is_some_and(|d| d.is_dead(p));
                    draw_stone(snapshot, cx, cy, r, color, ghost);
                }
            }
        }

        fn draw_territory(
            &self,
            snapshot: &gtk::Snapshot,
            scene: Scene,
            territory: &[Option<Color>],
        ) {
            let Scene { l, size, board, .. } = scene;
            if territory.len() != size.points() {
                return;
            }
            let half = l.cell * 0.14;
            let dead = self.dead.borrow();
            for y in 0..size.h {
                for x in 0..size.w {
                    let p = size.point(x, y);
                    let Some(owner) = territory[p.index()] else {
                        continue;
                    };
                    let alive = board.at(p).is_some() && !dead.as_ref().is_some_and(|d| d.is_dead(p));
                    if alive {
                        continue;
                    }
                    let (cx, cy) = l.xy(x, y);
                    let rect = graphene::Rect::new(cx - half, cy - half, half * 2.0, half * 2.0);
                    let fill = match owner {
                        Color::Black => gdk::RGBA::new(0.05, 0.05, 0.05, 0.85),
                        Color::White => gdk::RGBA::new(0.97, 0.97, 0.95, 0.90),
                    };
                    snapshot.append_color(&fill, &rect);
                    let pb = gsk::PathBuilder::new();
                    pb.add_rect(&rect);
                    snapshot.append_stroke(
                        &pb.to_path(),
                        &gsk::Stroke::new(1.0),
                        &gdk::RGBA::new(0.0, 0.0, 0.0, 0.45),
                    );
                }
            }
        }

        /// Draws `nums[i] > 0` centred on the stone at `i`. The number at `highlight`, if any,
        /// is tinted instead of drawn in the stone's contrast colour.
        fn draw_numbers(
            &self,
            snapshot: &gtk::Snapshot,
            scene: Scene,
            nums: &[u16],
            highlight: Option<Point>,
        ) {
            let Scene { l, size, board, .. } = scene;
            let obj = self.obj();
            let mut fd = obj.pango_context().font_description().unwrap_or_default();
            fd.set_weight(pango::Weight::Bold);
            for y in 0..size.h {
                for x in 0..size.w {
                    let p = size.point(x, y);
                    let n = nums[p.index()];
                    if n == 0 {
                        continue;
                    }
                    let Some(color) = board.at(p) else { continue };
                    let text = n.to_string();
                    let scale = match text.len() {
                        1 => 0.48,
                        2 => 0.42,
                        3 => 0.32,
                        _ => 0.26,
                    };
                    fd.set_absolute_size((l.cell * scale) as f64 * pango::SCALE as f64);
                    let layout = obj.create_pango_layout(Some(&text));
                    layout.set_font_description(Some(&fd));
                    let fg = if highlight == Some(p) {
                        last_move_tint(color)
                    } else {
                        match color {
                            Color::Black => gdk::RGBA::new(1.0, 1.0, 1.0, 0.95),
                            Color::White => gdk::RGBA::new(0.05, 0.05, 0.05, 0.95),
                        }
                    };
                    let (cx, cy) = l.xy(x, y);
                    draw_text(snapshot, &layout, cx, cy, &fg);
                }
            }
        }

        fn draw_marks(
            &self,
            snapshot: &gtk::Snapshot,
            scene: Scene,
            cursor: mirai_core::NodeId,
            state: &AppState,
        ) {
            let Scene { l, size, board, dark } = scene;
            let marks = {
                let tree = state.tree();
                let node = tree.node(cursor);
                if node.marks.is_empty() {
                    return;
                }
                node.marks.clone()
            };
            let obj = self.obj();
            let r = l.stone_r;
            let stroke = gsk::Stroke::new((l.cell * 0.07).max(1.4));

            let color_at = |p: Point| match board.at(p) {
                Some(Color::Black) => gdk::RGBA::new(1.0, 1.0, 1.0, 0.95),
                Some(Color::White) => gdk::RGBA::new(0.05, 0.05, 0.05, 0.95),
                None => gdk::RGBA::new(0.10, 0.10, 0.10, 0.95),
            };

            for kind in [
                MarkKind::Triangle,
                MarkKind::Square,
                MarkKind::Circle,
                MarkKind::Cross,
            ] {
                for &p in marks.of(kind) {
                    if !size.contains(p) {
                        continue;
                    }
                    let (x, y) = size.xy(p);
                    let (cx, cy) = l.xy(x, y);
                    let pb = gsk::PathBuilder::new();
                    match kind {
                        MarkKind::Triangle => {
                            pb.move_to(cx, cy - r * 0.82);
                            pb.line_to(cx + r * 0.74, cy + r * 0.52);
                            pb.line_to(cx - r * 0.74, cy + r * 0.52);
                            pb.close();
                        }
                        MarkKind::Square => {
                            let h = r * 0.62;
                            pb.add_rect(&graphene::Rect::new(cx - h, cy - h, h * 2.0, h * 2.0));
                        }
                        MarkKind::Circle => pb.add_circle(&graphene::Point::new(cx, cy), r * 0.62),
                        MarkKind::Cross => {
                            let h = r * 0.6;
                            pb.move_to(cx - h, cy - h);
                            pb.line_to(cx + h, cy + h);
                            pb.move_to(cx + h, cy - h);
                            pb.line_to(cx - h, cy + h);
                        }
                    }
                    snapshot.append_stroke(&pb.to_path(), &stroke, &color_at(p));
                }
            }

            let mut fd = obj.pango_context().font_description().unwrap_or_default();
            fd.set_weight(pango::Weight::Bold);
            for (p, text) in &marks.labels {
                if !size.contains(*p) || text.is_empty() {
                    continue;
                }
                let (x, y) = size.xy(*p);
                let (cx, cy) = l.xy(x, y);
                if board.at(*p).is_none() {
                    // A wood disc keeps the label legible over the grid lines.
                    snapshot.append_fill(
                        &circle(cx, cy, r * 0.85),
                        gsk::FillRule::Winding,
                        &wood_color(dark),
                    );
                }
                let scale = match text.chars().count() {
                    1 => 0.52,
                    2 => 0.40,
                    _ => 0.30,
                };
                fd.set_absolute_size((l.cell * scale) as f64 * pango::SCALE as f64);
                let layout = obj.create_pango_layout(Some(text));
                layout.set_font_description(Some(&fd));
                draw_text(snapshot, &layout, cx, cy, &color_at(*p));
            }
        }

        fn draw_candidates(
            &self,
            snapshot: &gtk::Snapshot,
            scene: Scene,
            to_play: Color,
            report: &mirai_engine::Report,
            state: &AppState,
        ) {
            let Scene { l, size, board, .. } = scene;
            let max = state.config().analysis.max_suggestions as usize;
            let best = report.best_visits().max(1) as f32;
            let obj = self.obj();
            let mut fd = obj.pango_context().font_description().unwrap_or_default();

            for info in report.moves.iter().take(max) {
                if info.mv.is_pass() || !size.contains(info.mv) || board.at(info.mv).is_some() {
                    continue;
                }
                let (x, y) = size.xy(info.mv);
                let (cx, cy) = l.xy(x, y);
                let rgb = ramp_rgb(info.visits as f32 / best);
                let path = circle(cx, cy, l.stone_r);
                snapshot.append_fill(&path, gsk::FillRule::Winding, &rgba8(rgb, 0.78));
                if info.order == 0 {
                    snapshot.append_stroke(
                        &path,
                        &gsk::Stroke::new(2.0),
                        &gdk::RGBA::new(1.0, 1.0, 1.0, 0.95),
                    );
                }

                let fg = text_on(rgb);
                let mut lines: Vec<(String, f32)> = Vec::with_capacity(3);
                lines.push((crate::util::pct1(info.winrate_for(to_play)), 0.22));
                if l.cell >= 26.0 {
                    lines.push((crate::util::signed1(info.score_lead_for(to_play)), 0.19));
                }
                if l.cell >= 34.0 {
                    lines.push((crate::util::si_visits(info.visits), 0.17));
                }

                let mut laid: Vec<(pango::Layout, f32)> = Vec::with_capacity(lines.len());
                let mut total = 0.0f32;
                for (text, scale) in &lines {
                    fd.set_absolute_size((l.cell * scale) as f64 * pango::SCALE as f64);
                    let layout = obj.create_pango_layout(Some(text));
                    layout.set_font_description(Some(&fd));
                    let h = layout.pixel_size().1 as f32;
                    total += h;
                    laid.push((layout, h));
                }
                let mut ty = cy - total * 0.5;
                for (layout, h) in laid {
                    draw_text(snapshot, &layout, cx, ty + h * 0.5, &fg);
                    ty += h;
                }
            }
        }
    }
}

/// Uploads an RGBA8-premultiplied `w * h` buffer and stretches it over `rect`.
fn blit(snapshot: &gtk::Snapshot, buf: Vec<u8>, size: Size, rect: &graphene::Rect) {
    let bytes = glib::Bytes::from_owned(buf);
    let texture = gdk::MemoryTextureBuilder::new()
        .set_bytes(Some(&bytes))
        .set_width(size.w as i32)
        .set_height(size.h as i32)
        .set_stride(size.w as usize * 4)
        .set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied)
        .build();
    snapshot.append_scaled_texture(&texture, gsk::ScalingFilter::Nearest, rect);
}

fn draw_text(snapshot: &gtk::Snapshot, layout: &pango::Layout, cx: f32, cy: f32, color: &gdk::RGBA) {
    let (tw, th) = layout.pixel_size();
    snapshot.save();
    snapshot.translate(&graphene::Point::new(
        cx - tw as f32 * 0.5,
        cy - th as f32 * 0.5,
    ));
    snapshot.append_layout(layout, color);
    snapshot.restore();
}

fn draw_stone(snapshot: &gtk::Snapshot, cx: f32, cy: f32, r: f32, color: Color, ghost: bool) {
    if ghost {
        snapshot.push_opacity(0.35);
    }
    let path = circle(cx, cy, r);
    let base = match color {
        Color::Black => gdk::RGBA::new(0.09, 0.09, 0.10, 1.0),
        Color::White => gdk::RGBA::new(0.93, 0.92, 0.89, 1.0),
    };
    snapshot.append_fill(&path, gsk::FillRule::Winding, &base);

    // Specular highlight, clipped to the stone.
    let bounds = graphene::Rect::new(cx - r, cy - r, r * 2.0, r * 2.0);
    snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, r));
    let peak = match color {
        Color::Black => 0.42,
        Color::White => 0.85,
    };
    let stops = [
        gsk::ColorStop::new(0.0, gdk::RGBA::new(1.0, 1.0, 1.0, peak)),
        gsk::ColorStop::new(1.0, gdk::RGBA::new(1.0, 1.0, 1.0, 0.0)),
    ];
    snapshot.append_radial_gradient(
        &bounds,
        &graphene::Point::new(cx - r * 0.34, cy - r * 0.38),
        r * 1.2,
        r * 1.2,
        0.0,
        1.0,
        &stops,
    );
    snapshot.pop();

    let rim = match color {
        Color::Black => gdk::RGBA::new(1.0, 1.0, 1.0, 0.10),
        Color::White => gdk::RGBA::new(0.0, 0.0, 0.0, 0.32),
    };
    snapshot.append_stroke(&path, &gsk::Stroke::new((r * 0.07).max(0.8)), &rim);

    if ghost {
        snapshot.pop();
    }
}

glib::wrapper! {
    pub struct BoardView(ObjectSubclass<imp::BoardView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BoardView {
    pub fn new(state: &AppState) -> BoardView {
        let this: BoardView = glib::Object::new();
        *this.imp().state.borrow_mut() = Some(state.clone());
        this.build_menu();
        this.install_controllers();
        this.observe(state);
        this
    }

    pub fn state(&self) -> AppState {
        self.imp()
            .state
            .borrow()
            .clone()
            .expect("BoardView was built without an AppState")
    }

    /// The intersection under widget coordinates `(x, y)`, if any.
    pub fn point_at(&self, x: f64, y: f64) -> Option<Point> {
        let size = self.imp().board_size();
        self.imp().layout.get().hit(size, x, y)
    }

    /// When set, a primary click calls this instead of [`AppState::play_move`].
    /// Returning `true` means the click was consumed.
    pub fn set_click_hook(&self, f: Option<Box<dyn Fn(Point) -> bool + 'static>>) {
        *self.imp().click_hook.borrow_mut() = f.map(Rc::from);
    }

    /// Extra dead-stone shading and territory squares drawn during scoring.
    pub fn set_score_overlay(
        &self,
        dead: Option<mirai_core::DeadSet>,
        territory: Option<Box<[Option<mirai_core::Color>]>>,
    ) {
        *self.imp().dead.borrow_mut() = dead;
        *self.imp().territory.borrow_mut() = territory;
        self.queue_draw();
    }

    /// Pins the PV preview to `Report::moves[index]`; `None` clears the pin. A pointer
    /// hover over a blob still wins while it lasts.
    pub fn set_pv_preview(&self, index: Option<usize>) {
        if self.imp().pinned.get() == index {
            return;
        }
        self.imp().pinned.set(index);
        self.queue_draw();
    }

    /// The candidate whose PV is currently previewed: pointer hover first, then the pin.
    fn active_preview(&self) -> Option<usize> {
        self.imp().hover.get().or_else(|| self.imp().pinned.get())
    }

    // -- wiring -----------------------------------------------------------------------

    fn observe(&self, state: &AppState) {
        for property in [
            "show-coordinates",
            "show-move-numbers",
            "ownership-overlay",
            "policy-overlay",
        ] {
            state.connect_notify_local(
                Some(property),
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_, _| {
                        view.imp().static_layer.borrow_mut().take();
                        view.queue_resize();
                        view.queue_draw();
                    }
                ),
            );
        }

        state.connect_local(
            signal::TREE_CHANGED,
            false,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or]
                None,
                move |_| {
                    // A different board size changes the geometry.
                    view.queue_resize();
                    view.queue_draw();
                    None
                }
            ),
        );

        state.connect_local(
            signal::CURSOR_CHANGED,
            false,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or]
                None,
                move |_| {
                    // Candidate indices belong to the old position.
                    view.imp().hover.set(None);
                    view.imp().pinned.set(None);
                    view.queue_draw();
                    None
                }
            ),
        );

        state.connect_local(
            signal::REPORT,
            false,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or]
                None,
                move |_| {
                    view.queue_draw();
                    None
                }
            ),
        );
    }

    fn install_controllers(&self) {
        let primary = gtk::GestureClick::new();
        primary.set_button(gdk::BUTTON_PRIMARY);
        primary.connect_released(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, x, y| view.on_primary(x, y)
        ));
        self.add_controller(primary);

        let secondary = gtk::GestureClick::new();
        secondary.set_button(gdk::BUTTON_SECONDARY);
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, x, y| view.on_secondary(x, y)
        ));
        self.add_controller(secondary);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, dy| {
                if dy > 0.0 {
                    view.state().go_next();
                } else if dy < 0.0 {
                    view.state().go_prev();
                }
                glib::Propagation::Stop
            }
        ));
        self.add_controller(scroll);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, x, y| view.update_hover(Some((x, y)))
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_hover(None)
        ));
        self.add_controller(motion);
    }

    fn build_menu(&self) {
        let group = gio::SimpleActionGroup::new();

        let play = gio::SimpleAction::new("play-here", None);
        play.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                if let Some(p) = view.imp().menu_point.get() {
                    view.play_at(p);
                }
            }
        ));
        group.add_action(&play);

        let main_line = gio::SimpleAction::new("main-line", None);
        main_line.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                let state = view.state();
                let target = view.node_for_menu_point().unwrap_or_else(|| state.cursor());
                state.with_tree_mut(|t| t.promote_to_main_line(target));
            }
        ));
        group.add_action(&main_line);

        let delete = gio::SimpleAction::new("delete-branch", None);
        delete.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                let state = view.state();
                let cursor = state.cursor();
                match view.node_for_menu_point() {
                    Some(child) => state.with_tree_mut(|t| t.delete_branch(child)),
                    None => {
                        if state.tree().parent(cursor).is_none() {
                            state.toast("The root node cannot be deleted");
                            return;
                        }
                        state.go_prev();
                        state.with_tree_mut(|t| t.delete_branch(cursor));
                    }
                }
            }
        ));
        group.add_action(&delete);

        let copy = gio::SimpleAction::new("copy-sgf", None);
        copy.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                let state = view.state();
                let include = state.config().ui.save_analysis_in_sgf;
                let sgf = mirai_core::sgf::write(&state.tree(), include);
                view.clipboard().set_text(&sgf);
                state.toast("SGF copied to the clipboard");
            }
        ));
        group.add_action(&copy);

        self.insert_action_group("board", Some(&group));

        let menu = gio::Menu::new();
        menu.append(Some("Play here"), Some("board.play-here"));
        menu.append(Some("Set as main line"), Some("board.main-line"));
        menu.append(Some("Delete branch"), Some("board.delete-branch"));
        menu.append(Some("Copy SGF"), Some("board.copy-sgf"));

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(self);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        *self.imp().popover.borrow_mut() = Some(popover);
    }

    /// The child of the cursor whose move sits on the menu point, if there is one.
    fn node_for_menu_point(&self) -> Option<mirai_core::NodeId> {
        let p = self.imp().menu_point.get()?;
        let state = self.state();
        let cursor = state.cursor();
        let tree = state.tree();
        tree.children(cursor)
            .iter()
            .copied()
            .find(|&c| tree.node(c).mv.is_some_and(|(_, mv)| mv == p))
    }

    fn play_at(&self, p: Point) {
        let hook = self.imp().click_hook.borrow().clone();
        if let Some(hook) = hook
            && hook(p)
        {
            return;
        }
        let state = self.state();
        let color = state.to_play();
        if let Err(e) = state.play_move(color, p) {
            state.toast(e.to_string());
        }
    }

    fn on_primary(&self, x: f64, y: f64) {
        let Some(p) = self.point_at(x, y) else { return };
        self.play_at(p);
    }

    fn on_secondary(&self, x: f64, y: f64) {
        let Some(p) = self.point_at(x, y) else { return };
        let state = self.state();
        let cursor = state.cursor();
        let current = { state.tree().node(cursor).mv };
        if let Some((_, mv)) = current
            && mv == p
        {
            state.go_prev();
            state.with_tree_mut(|t| t.delete_branch(cursor));
            return;
        }

        self.imp().menu_point.set(Some(p));
        if let Some(popover) = self.imp().popover.borrow().as_ref() {
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    }

    fn update_hover(&self, pointer: Option<(f64, f64)>) {
        let idx = pointer.and_then(|(x, y)| self.candidate_at(x, y));
        if self.imp().hover.get() != idx {
            self.imp().hover.set(idx);
            self.queue_draw();
        }
    }

    /// The index into `Report::moves` of the candidate blob under `(x, y)`.
    fn candidate_at(&self, x: f64, y: f64) -> Option<usize> {
        let state = self.imp().state.borrow().clone()?;
        let report = state.last_report()?;
        let l = self.imp().layout.get();
        if l.cell <= 0.0 {
            return None;
        }
        let size = self.imp().board_size();
        let max = state.config().analysis.max_suggestions as usize;
        let board = state.position().board;
        for (i, info) in report.moves.iter().enumerate().take(max) {
            if info.mv.is_pass() || !size.contains(info.mv) || board.at(info.mv).is_some() {
                continue;
            }
            let (bx, by) = size.xy(info.mv);
            let (cx, cy) = l.xy(bx, by);
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if dx * dx + dy * dy <= l.stone_r * l.stone_r {
                return Some(i);
            }
        }
        None
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visit_ramp_hits_its_control_points() {
        assert_eq!(ramp_rgb(0.0), [0x28, 0x48, 0xC8]);
        assert_eq!(ramp_rgb(0.25), [0x28, 0xA0, 0xA0]);
        assert_eq!(ramp_rgb(0.5), [0x48, 0xC8, 0x48]);
        assert_eq!(ramp_rgb(0.75), [0xE8, 0xC8, 0x38]);
        assert_eq!(ramp_rgb(1.0), [0xE8, 0x50, 0x38]);
    }

    #[test]
    fn visit_ramp_interpolates_and_clamps() {
        // Halfway between #2848C8 and #28A0A0.
        assert_eq!(ramp_rgb(0.125), [0x28, 0x74, 0xB4]);
        assert_eq!(ramp_rgb(-3.0), ramp_rgb(0.0));
        assert_eq!(ramp_rgb(9.0), ramp_rgb(1.0));
        assert_eq!(ramp_rgb(f32::NAN), ramp_rgb(0.0));
    }

    fn layout() -> Layout {
        Layout {
            cell: 30.0,
            origin_x: 50.0,
            origin_y: 40.0,
            stone_r: 14.4,
            width: 640,
            height: 640,
        }
    }

    #[test]
    fn hit_test_snaps_to_the_nearest_intersection() {
        let size = Size::square(19);
        let l = layout();
        // Dead centre of A19 (the top-left intersection).
        assert_eq!(l.hit(size, 50.0, 40.0), Some(size.point(0, 0)));
        // 4 px off the centre of (3, 2) still snaps to it.
        assert_eq!(l.hit(size, 50.0 + 90.0 + 4.0, 40.0 + 60.0 - 4.0), Some(size.point(3, 2)));
        // The bottom-right corner.
        assert_eq!(l.hit(size, 50.0 + 18.0 * 30.0, 40.0 + 18.0 * 30.0), Some(size.point(18, 18)));
    }

    #[test]
    fn hit_test_rejects_gaps_and_the_outside() {
        let size = Size::square(19);
        let l = layout();
        // Exactly between four intersections: 15 px away in x and y, outside the 15 px radius.
        assert_eq!(l.hit(size, 65.0, 55.0), None);
        // Past the last column.
        assert_eq!(l.hit(size, 50.0 + 19.0 * 30.0, 40.0), None);
        // Above the board.
        assert_eq!(l.hit(size, 50.0, 40.0 - 20.0), None);
        // A zero-size layout never hits.
        assert_eq!(Layout::default().hit(size, 0.0, 0.0), None);
    }

    #[test]
    fn layout_centres_the_grid_and_leaves_room_for_coordinates() {
        let size = Size::square(19);
        let bare = Layout::compute(600, 600, size, false);
        let with_coords = Layout::compute(600, 600, size, true);
        assert!(with_coords.cell < bare.cell, "coordinates must shrink the grid");
        assert_eq!(bare.stone_r, bare.cell * 0.48);
        // The grid is centred: equal margins on both sides.
        let right = 600.0 - (bare.origin_x + 18.0 * bare.cell);
        assert!((bare.origin_x - right).abs() < 0.01);
        // The whole board, plus its padding, fits.
        assert!(with_coords.origin_x - with_coords.cell * (EDGE_PAD + COORD_PAD) >= -0.01);
    }
}
