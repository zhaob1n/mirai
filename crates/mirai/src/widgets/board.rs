// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `BoardView` — the goban. Step 9.
//!
//! Everything is drawn in `WidgetImpl::snapshot` with `gsk`. The static part of the board
//! (wood, grid, star points, coordinates) never changes between allocations, so it is built
//! once into a throwaway `gtk::Snapshot`, turned into a `gsk::RenderNode` and replayed with a
//! single `append_node` every frame.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use mirai_core::{Board, COLUMNS, Color, DeadSet, MarkKind, Marks, Point, Position, Rules, Size};
use mirai_proto::types::dq_policy;

use crate::app::AppState;
use crate::widgets::paint::{fill_disc, hline, over, stroke_disc, stroke_rect, vline};

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

/// The visit ramp: indigo (barely searched) through blue, teal, green and amber to red (the
/// engine's choice). Seven stops rather than five because the low end is where the
/// candidates crowd — KataGo spends most of its visits on one or two moves.
const VISIT_RAMP: [(f32, [u8; 3]); 7] = [
    (0.00, [0x3B, 0x2E, 0x8F]),
    (0.18, [0x2B, 0x5F, 0xD9]),
    (0.36, [0x18, 0x9E, 0xC0]),
    (0.54, [0x2E, 0xB8, 0x62]),
    (0.72, [0xC8, 0xCF, 0x2C]),
    (0.88, [0xF0, 0xA4, 0x22]),
    (1.00, [0xE2, 0x3D, 0x2E]),
];

/// Opacity floor and ceiling for a candidate blob, and the natural-log span between them.
///
/// Depth is the second, blunter channel for the same quantity the hue carries: how much
/// search a move actually got. A share five log units below the best move (≈0.7% of its
/// visits) is drawn at the floor and no fainter. LizzieYzy computes the same thing as
/// `minAlpha + (maxAlpha - minAlpha) * max(0, log(share) / 5 + 1)`.
const BLOB_ALPHA_MIN: f32 = 0.18;
const BLOB_ALPHA_MAX: f32 = 0.85;
const BLOB_ALPHA_DECAY: f32 = 5.0;

/// Below this share the numbers are noise on a crowded board, and unreadable through a faint
/// blob. The engine's own choice keeps its numbers whatever its share.
const LABEL_MIN_SHARE: f32 = 0.02;

#[inline]
fn lerp8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Colour for a candidate at ramp position `f` in `0..=1`.
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

/// Where a visit share sits on the ramp.
///
/// The raw share leaves everything but the best move on the bottom stop, so it is
/// square-rooted first — the same spread LizzieYzy applies with its default
/// `suggestion-color-ratio = 2`.
#[inline]
fn ramp_position(share: f32) -> f32 {
    share.max(0.0).sqrt()
}

/// How opaque the blob for a candidate with this visit share is drawn.
fn blob_alpha(share: f32) -> f32 {
    let t = if share > 0.0 {
        (share.ln() / BLOB_ALPHA_DECAY + 1.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    BLOB_ALPHA_MIN + (BLOB_ALPHA_MAX - BLOB_ALPHA_MIN) * t
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

/// Black or white text, whichever reads better on `bg` — the blob already composited over
/// the board, because a faint blob is mostly wood.
fn text_on(bg: gdk::RGBA) -> gdk::RGBA {
    let lum = 0.299 * bg.red() + 0.587 * bg.green() + 0.114 * bg.blue();
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
///
/// `Rc`, not `Box`: the hook re-enters this widget — the play controller redraws the score
/// overlay from inside it — so a click clones the handle and releases the cell before
/// calling, rather than holding a borrow across it.
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
struct BoardProjection {
    size: Size,
    rules: Rules,
    position: Position,
    marks: Marks,
    last: Option<(Color, Point)>,
    move_numbers: Option<Box<[u16]>>,
    report: Option<Arc<mirai_engine::Report>>,
    /// Resolved from `AnalysisSettings::suggestion_limit`, so `usize::MAX` means "all".
    suggestion_limit: usize,
    show_coordinates: bool,
    ownership_overlay: bool,
    policy_overlay: bool,
    ownership_texture: Option<gdk::Texture>,
    policy_texture: Option<gdk::Texture>,
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
        pub state: OnceCell<AppState>,
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
        pub(super) projection: RefCell<Option<BoardProjection>>,
        /// Last allocation we snapshotted. Labels paint only when this matches the current
        /// layout. A sidebar fold changes the board's width every frame; `cell` tracks that
        /// width only while it is the tighter constraint, otherwise only `origin_x` moves.
        /// Either way the snapshot is new, and pango at a new size is a fresh GSK text node.
        pub labeled: Cell<Option<Layout>>,
        /// Coalesces the idle redraw that follows a skipped-label snapshot.
        pub idle_draw: Cell<bool>,
        pub defer_labels: Cell<bool>,
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
            self.projection
                .borrow()
                .as_ref()
                .map_or(Size::square(19), |p| p.size)
        }

        fn show_coords(&self) -> bool {
            self.projection
                .borrow()
                .as_ref()
                .is_some_and(|projection| projection.show_coordinates)
        }

        fn queue_stable_redraw(&self) {
            if self.idle_draw.replace(true) {
                return;
            }
            let weak = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.imp().idle_draw.set(false);
                    this.queue_draw();
                }
            });
        }

        /// Builds the wood, grid, star points and coordinate labels once.
        fn build_static(
            &self,
            l: Layout,
            size: Size,
            coords: bool,
            dark: bool,
        ) -> Option<gsk::RenderNode> {
            let obj = self.obj();
            let s = gtk::Snapshot::new();
            let cell = l.cell;
            let pad = cell * EDGE_PAD;
            let grid_w = (size.w as f32 - 1.0) * cell;
            let grid_h = (size.h as f32 - 1.0) * cell;

            // Wood: a colour node inside a rounded clip, not a rounded-rect fill — see the
            // note on `paint::fill_disc`.
            let rect = graphene::Rect::new(
                l.origin_x - pad,
                l.origin_y - pad,
                grid_w + 2.0 * pad,
                grid_h + 2.0 * pad,
            );
            let wood = wood_color(dark);
            s.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, (cell * 0.2).min(8.0)));
            s.append_color(&wood, &rect);
            s.pop();

            // Grid. Axis-aligned lines are rectangles; as one stroked path, GSK re-evaluated
            // all 38 segments across the whole board every time the board was resized.
            let ink = over(LINE_COLOR, wood);
            let lw = (cell * 0.035).max(1.0);
            for y in 0..size.h {
                let (_, cy) = l.xy(0, y);
                hline(&s, l.origin_x, l.origin_x + grid_w, cy, lw, &ink);
            }
            for x in 0..size.w {
                let (cx, _) = l.xy(x, 0);
                vline(&s, cx, l.origin_y, l.origin_y + grid_h, lw, &ink);
            }

            // A heavier border around the playing area.
            let border = graphene::Rect::new(l.origin_x, l.origin_y, grid_w, grid_h);
            stroke_rect(&s, &border, (cell * 0.06).max(1.4), &ink);

            // Star points.
            let star_r = (cell * 0.10).max(1.5);
            for p in size.star_points() {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                fill_disc(&s, cx, cy, star_r, &ink);
            }

            // Coordinates.
            if coords {
                let mut fd = obj.pango_context().font_description().unwrap_or_default();
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

        fn static_node(
            &self,
            l: Layout,
            size: Size,
            coords: bool,
            dark: bool,
        ) -> Option<gsk::RenderNode> {
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
            let projection = self.projection.borrow();
            let Some(projection) = projection.as_ref() else {
                return;
            };
            let l = self.layout.get();
            if l.cell < 3.0 {
                return;
            }
            // First paint, or a repeat at the same allocation: draw numbers. A changed
            // allocation is the sidebar animation (or a live resize) — blobs only, then
            // one idle snapshot at this size brings the text back.
            let paint_labels = match self.labeled.replace(Some(l)) {
                None => true,
                Some(prev) => prev == l,
            };
            self.defer_labels.set(!paint_labels);
            if !paint_labels {
                self.queue_stable_redraw();
            }
            let size = projection.size;
            let dark = is_dark();

            if let Some(node) = self.static_node(l, size, projection.show_coordinates, dark) {
                snapshot.append_node(&node);
            }

            // Heat maps cover the intersections, half a cell beyond the outer lines.
            let heat = graphene::Rect::new(
                l.origin_x - l.cell * 0.5,
                l.origin_y - l.cell * 0.5,
                size.w as f32 * l.cell,
                size.h as f32 * l.cell,
            );
            if projection.ownership_overlay
                && let Some(texture) = projection.ownership_texture.as_ref()
            {
                snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Nearest, &heat);
            }

            if projection.policy_overlay
                && let Some(texture) = projection.policy_texture.as_ref()
            {
                snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Nearest, &heat);
            }

            let preview = self.obj().active_preview();
            if let Some(idx) = preview
                && let Some(report) = projection.report.as_ref()
                && let Some(info) = report.moves.get(idx)
            {
                let mut board = projection.position.board.clone();
                let mut color = projection.position.to_play;
                let mut seq = vec![0u16; size.points()];
                let mut n = 0u16;
                for &p in &info.pv {
                    n += 1;
                    if p.is_pass() {
                        color = color.other();
                        continue;
                    }
                    if !size.contains(p) || board.play(color, p, &projection.rules).is_err() {
                        break;
                    }
                    seq[p.index()] = n;
                    color = color.other();
                }
                let scene = Scene {
                    l,
                    size,
                    board: &board,
                    dark,
                };
                self.draw_stones(snapshot, scene, None);
                self.draw_numbers(snapshot, scene, &seq, None);
                return;
            }

            let scene = Scene {
                l,
                size,
                board: &projection.position.board,
                dark,
            };
            let dead = self.dead.borrow();
            self.draw_stones(snapshot, scene, dead.as_ref());
            if let Some(territory) = self.territory.borrow().as_ref() {
                self.draw_territory(snapshot, scene, territory);
            }
            drop(dead);

            if let Some(nums) = projection.move_numbers.as_ref() {
                self.draw_numbers(snapshot, scene, nums, projection.last.map(|(_, p)| p));
            } else if let Some((color, p)) = projection.last {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                fill_disc(snapshot, cx, cy, l.stone_r * 0.34, &last_move_tint(color));
            }

            self.draw_marks(snapshot, scene, &projection.marks);
            if let Some(report) = projection.report.as_ref() {
                self.draw_candidates(
                    snapshot,
                    scene,
                    projection.position.to_play,
                    report,
                    projection.suggestion_limit,
                );
            }
        }
    }

    impl BoardView {
        fn draw_stones(&self, snapshot: &gtk::Snapshot, scene: Scene, dead: Option<&DeadSet>) {
            let Scene { l, size, board, .. } = scene;
            let r = l.stone_r;
            let off = (l.cell * 0.05).max(1.0);

            // The shadows are a pass of their own so a stone is never drawn over its
            // neighbour's shadow. Stones are 0.96 cells across, so the discs never overlap
            // and drawing them one by one blends exactly like the single path this replaced.
            let shadow = gdk::RGBA::new(0.0, 0.0, 0.0, 0.25);
            for y in 0..size.h {
                for x in 0..size.w {
                    if board.at(size.point(x, y)).is_some() {
                        let (cx, cy) = l.xy(x, y);
                        fill_disc(snapshot, cx + off, cy + off, r, &shadow);
                    }
                }
            }

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
                    let alive =
                        board.at(p).is_some() && !dead.as_ref().is_some_and(|d| d.is_dead(p));
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
                    stroke_rect(snapshot, &rect, 1.0, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.45));
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
            if self.defer_labels.get() {
                return;
            }
            let Scene { l, size, board, .. } = scene;
            let obj = self.obj();
            let mut fd = obj.pango_context().font_description().unwrap_or_default();
            fd.set_weight(pango::Weight::Bold);
            // One layout for the whole pass: creating a PangoLayout is a GObject construction
            // plus a font-map lookup, and a numbered 200-move game would otherwise pay that
            // once per stone per frame.
            let layout = obj.create_pango_layout(None);
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
                    layout.set_text(&text);
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

        fn draw_marks(&self, snapshot: &gtk::Snapshot, scene: Scene, marks: &Marks) {
            if marks.is_empty() {
                return;
            }
            let Scene {
                l,
                size,
                board,
                dark,
            } = scene;
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
                    if kind == MarkKind::Circle {
                        stroke_disc(
                            snapshot,
                            cx,
                            cy,
                            r * 0.62,
                            stroke.line_width(),
                            &color_at(p),
                        );
                        continue;
                    }
                    // The other three are genuinely paths, and each covers one stone, so the
                    // area GSK has to walk is a cell rather than the board.
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
                        MarkKind::Cross => {
                            let h = r * 0.6;
                            pb.move_to(cx - h, cy - h);
                            pb.line_to(cx + h, cy + h);
                            pb.move_to(cx + h, cy - h);
                            pb.line_to(cx - h, cy + h);
                        }
                        MarkKind::Circle => unreachable!("handled above"),
                    }
                    snapshot.append_stroke(&pb.to_path(), &stroke, &color_at(p));
                }
            }

            if !self.defer_labels.get() {
                let mut fd = obj.pango_context().font_description().unwrap_or_default();
                fd.set_weight(pango::Weight::Bold);
                let layout = obj.create_pango_layout(None);
                for (p, text) in &marks.labels {
                    if !size.contains(*p) || text.is_empty() {
                        continue;
                    }
                    let (x, y) = size.xy(*p);
                    let (cx, cy) = l.xy(x, y);
                    if board.at(*p).is_none() {
                        // A wood disc keeps the label legible over the grid lines.
                        fill_disc(snapshot, cx, cy, r * 0.85, &wood_color(dark));
                    }
                    let scale = match text.chars().count() {
                        1 => 0.52,
                        2 => 0.40,
                        _ => 0.30,
                    };
                    fd.set_absolute_size((l.cell * scale) as f64 * pango::SCALE as f64);
                    layout.set_text(text);
                    layout.set_font_description(Some(&fd));
                    draw_text(snapshot, &layout, cx, cy, &color_at(*p));
                }
            }
        }

        fn draw_candidates(
            &self,
            snapshot: &gtk::Snapshot,
            scene: Scene,
            to_play: Color,
            report: &mirai_engine::Report,
            max: usize,
        ) {
            let Scene {
                l,
                size,
                board,
                dark,
            } = scene;
            let best = report.best_visits().max(1) as f32;
            let obj = self.obj();
            let mut fd = obj.pango_context().font_description().unwrap_or_default();
            let layout = obj.create_pango_layout(None);

            for info in report.moves.iter().take(max) {
                if info.mv.is_pass() || !size.contains(info.mv) || board.at(info.mv).is_some() {
                    continue;
                }
                let (x, y) = size.xy(info.mv);
                let (cx, cy) = l.xy(x, y);
                let share = info.visits as f32 / best;
                let rgb = ramp_rgb(ramp_position(share));
                let blob = rgba8(rgb, blob_alpha(share));
                fill_disc(snapshot, cx, cy, l.stone_r, &blob);
                if info.order == 0 {
                    stroke_disc(
                        snapshot,
                        cx,
                        cy,
                        l.stone_r,
                        2.0,
                        &gdk::RGBA::new(1.0, 1.0, 1.0, 0.95),
                    );
                }

                if share < LABEL_MIN_SHARE && info.order != 0 {
                    continue;
                }
                if self.defer_labels.get() {
                    continue;
                }

                let fg = text_on(over(blob, wood_color(dark)));
                let mut lines: Vec<(String, f32)> = Vec::with_capacity(3);
                lines.push((crate::util::pct1(info.winrate_for(to_play)), 0.22));
                if l.cell >= 26.0 {
                    lines.push((crate::util::signed1(info.score_lead_for(to_play)), 0.19));
                }
                if l.cell >= 34.0 {
                    lines.push((crate::util::si_visits(info.visits), 0.17));
                }

                // Measure, then draw, through one layout: the three lines have to be
                // centred as a block, so the heights are needed before the first glyph.
                let mut heights = [0.0f32; 3];
                let mut total = 0.0f32;
                for (i, (text, scale)) in lines.iter().enumerate() {
                    fd.set_absolute_size((l.cell * scale) as f64 * pango::SCALE as f64);
                    layout.set_text(text);
                    layout.set_font_description(Some(&fd));
                    let h = layout.pixel_size().1 as f32;
                    heights[i] = h;
                    total += h;
                }
                let mut ty = cy - total * 0.5;
                for (i, (text, scale)) in lines.iter().enumerate() {
                    fd.set_absolute_size((l.cell * scale) as f64 * pango::SCALE as f64);
                    layout.set_text(text);
                    layout.set_font_description(Some(&fd));
                    draw_text(snapshot, &layout, cx, ty + heights[i] * 0.5, &fg);
                    ty += heights[i];
                }
            }
        }
    }
}

/// Uploads an RGBA8-premultiplied `w * h` buffer and stretches it over `rect`.
fn texture(buf: Vec<u8>, size: Size) -> gdk::Texture {
    let bytes = glib::Bytes::from_owned(buf);
    gdk::MemoryTextureBuilder::new()
        .set_bytes(Some(&bytes))
        .set_width(size.w as i32)
        .set_height(size.h as i32)
        .set_stride(size.w as usize * 4)
        .set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied)
        .build()
        .upcast()
}

fn ownership_texture(report: &mirai_engine::Report, size: Size) -> Option<gdk::Texture> {
    let ownership = report.ownership.as_ref()?;
    if ownership.len() != size.points() {
        return None;
    }
    let mut buf = vec![0u8; size.points() * 4];
    for (i, &value) in ownership.iter().enumerate() {
        let alpha = (value.unsigned_abs() as f32 / 127.0) * 0.45;
        let channel = if value >= 0 { 0.0 } else { 255.0 * alpha };
        let pixel = &mut buf[i * 4..i * 4 + 4];
        pixel[0] = channel as u8;
        pixel[1] = channel as u8;
        pixel[2] = channel as u8;
        pixel[3] = (alpha * 255.0) as u8;
    }
    Some(texture(buf, size))
}

fn policy_texture(report: &mirai_engine::Report, size: Size) -> Option<gdk::Texture> {
    let policy = report.policy.as_ref()?;
    if policy.len() != size.points() + 1 {
        return None;
    }
    let accent = adw::StyleManager::default().accent_color_rgba();
    let mut buf = vec![0u8; size.points() * 4];
    for i in 0..size.points() {
        let Some(probability) = dq_policy(policy[i]) else {
            continue;
        };
        let alpha = probability.max(0.0).sqrt() * 0.6;
        let pixel = &mut buf[i * 4..i * 4 + 4];
        pixel[0] = (accent.red() * alpha * 255.0) as u8;
        pixel[1] = (accent.green() * alpha * 255.0) as u8;
        pixel[2] = (accent.blue() * alpha * 255.0) as u8;
        pixel[3] = (alpha * 255.0) as u8;
    }
    Some(texture(buf, size))
}

fn draw_text(
    snapshot: &gtk::Snapshot,
    layout: &pango::Layout,
    cx: f32,
    cy: f32,
    color: &gdk::RGBA,
) {
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
    let base = match color {
        Color::Black => gdk::RGBA::new(0.09, 0.09, 0.10, 1.0),
        Color::White => gdk::RGBA::new(0.93, 0.92, 0.89, 1.0),
    };
    fill_disc(snapshot, cx, cy, r, &base);

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
    stroke_disc(snapshot, cx, cy, r, (r * 0.07).max(0.8), &rim);

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
        this.imp()
            .state
            .set(state.clone())
            .expect("a fresh BoardView cannot already hold a state");
        this.build_menu();
        this.install_controllers();
        this.observe(state);
        this.rebuild_projection(state);
        this
    }

    /// The window state this view draws, held directly.
    ///
    /// A view cannot be built without one, so there is no live-window question to ask: the
    /// old route back through `MiraiWindow` cost a weak upgrade and a `RefCell` borrow on
    /// every refresh, and could only answer it by panicking.
    pub fn state(&self) -> &AppState {
        self.imp()
            .state
            .get()
            .expect("BoardView was built without a state")
    }

    /// The intersection under widget coordinates `(x, y)`, if any.
    pub fn point_at(&self, x: f64, y: f64) -> Option<Point> {
        let size = self.imp().board_size();
        self.imp().layout.get().hit(size, x, y)
    }

    /// When set, a primary click calls this instead of [`AppState::play_move`].
    /// Returning `true` means the click was consumed.
    ///
    /// Taken by value so the closure is boxed exactly once, into the `Rc` it is stored in.
    pub fn set_click_hook(&self, hook: impl Fn(Point) -> bool + 'static) {
        let hook: ClickHook = Rc::new(hook);
        *self.imp().click_hook.borrow_mut() = Some(hook);
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
                        let state = view.state();
                        view.rebuild_projection(state);
                        view.queue_resize();
                        view.queue_draw();
                    }
                ),
            );
        }
    }

    pub(crate) fn refresh_tree(&self) {
        let state = self.state();
        self.rebuild_projection(state);
        self.queue_resize();
        self.queue_draw();
    }

    pub(crate) fn refresh_cursor(&self) {
        self.imp().hover.set(None);
        self.imp().pinned.set(None);
        let state = self.state();
        self.rebuild_projection(state);
        self.queue_draw();
    }

    pub(crate) fn refresh_report(&self) {
        let state = self.state();
        if let Some(projection) = self.imp().projection.borrow_mut().as_mut() {
            projection.report = state.last_report();
            projection.suggestion_limit = state.config().analysis.suggestion_limit();
        } else {
            self.rebuild_projection(state);
            self.queue_draw();
            return;
        }
        self.rebuild_overlay_textures();
        self.queue_draw();
    }

    fn rebuild_projection(&self, state: &AppState) {
        let cursor = state.cursor();
        let position = state.position();
        let (size, rules, marks, last, move_numbers) = {
            let tree = state.tree();
            let size = tree.info.size;
            let rules = tree.info.rules.rules();
            let node = tree.node(cursor);
            let marks = node.marks.clone();
            let last = node
                .mv
                .filter(|&(color, p)| !p.is_pass() && position.board.at(p) == Some(color));
            let move_numbers = state.show_move_numbers().then(|| tree.move_numbers(cursor));
            (size, rules, marks, last, move_numbers)
        };
        self.imp().projection.replace(Some(BoardProjection {
            size,
            rules,
            position,
            marks,
            last,
            move_numbers,
            report: state.last_report(),
            suggestion_limit: state.config().analysis.suggestion_limit(),
            show_coordinates: state.show_coordinates(),
            ownership_overlay: state.ownership_overlay(),
            policy_overlay: state.policy_overlay(),
            ownership_texture: None,
            policy_texture: None,
        }));
        self.rebuild_overlay_textures();
    }

    fn rebuild_overlay_textures(&self) {
        let mut projection = self.imp().projection.borrow_mut();
        let Some(projection) = projection.as_mut() else {
            return;
        };
        projection.ownership_texture = projection
            .report
            .as_deref()
            .and_then(|report| ownership_texture(report, projection.size));
        projection.policy_texture = projection
            .report
            .as_deref()
            .and_then(|report| policy_texture(report, projection.size));
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
        menu.append(Some("Play Here"), Some("board.play-here"));
        menu.append(Some("Set as Main Line"), Some("board.main-line"));
        menu.append(Some("Delete Branch"), Some("board.delete-branch"));
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
        if let Err(error) = state.play_move(color, p) {
            state.toast_illegal_move(error);
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
        let projection = self.imp().projection.borrow();
        let projection = projection.as_ref()?;
        let report = projection.report.as_ref()?;
        let l = self.imp().layout.get();
        if l.cell <= 0.0 {
            return None;
        }
        for (i, info) in report
            .moves
            .iter()
            .enumerate()
            .take(projection.suggestion_limit)
        {
            if info.mv.is_pass()
                || !projection.size.contains(info.mv)
                || projection.position.board.at(info.mv).is_some()
            {
                continue;
            }
            let (bx, by) = projection.size.xy(info.mv);
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
        for (position, colour) in VISIT_RAMP {
            assert_eq!(ramp_rgb(position), colour, "at {position}");
        }
    }

    #[test]
    fn visit_ramp_interpolates_and_clamps() {
        // Inside the first segment, #3B2E8F -> #2B5FD9: red falls, green and blue rise.
        let mid = ramp_rgb(0.09);
        assert!((0x2B..0x3B).contains(&mid[0]), "{mid:?}");
        assert!((0x2E..0x5F).contains(&mid[1]), "{mid:?}");
        assert!((0x8F..0xD9).contains(&mid[2]), "{mid:?}");
        assert_eq!(ramp_rgb(-3.0), ramp_rgb(0.0));
        assert_eq!(ramp_rgb(9.0), ramp_rgb(1.0));
        assert_eq!(ramp_rgb(f32::NAN), ramp_rgb(0.0));
    }

    /// Depth is the channel that says how much search a move actually got, so it has to fall
    /// with the visit share and stop at a floor rather than vanishing.
    #[test]
    fn opacity_tracks_how_much_a_move_was_searched() {
        assert!((blob_alpha(1.0) - BLOB_ALPHA_MAX).abs() < 1e-6);
        assert_eq!(blob_alpha(0.0), BLOB_ALPHA_MIN);
        assert_eq!(blob_alpha(0.001), BLOB_ALPHA_MIN);
        assert_eq!(blob_alpha(f32::NAN), BLOB_ALPHA_MIN);
        assert!(blob_alpha(0.5) > blob_alpha(0.05));
        assert!(blob_alpha(0.05) > blob_alpha(0.005));
    }

    /// The square root is the whole point: without it a 4%-visit move would sit on the
    /// bottom stop with a 0.04% one.
    #[test]
    fn ramp_position_spreads_the_crowded_low_end() {
        assert_eq!(ramp_position(1.0), 1.0);
        assert_eq!(ramp_position(0.25), 0.5);
        assert_eq!(ramp_position(-1.0), 0.0);
        assert!(ramp_position(0.04) > 0.04);
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
        assert_eq!(
            l.hit(size, 50.0 + 90.0 + 4.0, 40.0 + 60.0 - 4.0),
            Some(size.point(3, 2))
        );
        // The bottom-right corner.
        assert_eq!(
            l.hit(size, 50.0 + 18.0 * 30.0, 40.0 + 18.0 * 30.0),
            Some(size.point(18, 18))
        );
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
        assert!(
            with_coords.cell < bare.cell,
            "coordinates must shrink the grid"
        );
        assert_eq!(bare.stone_r, bare.cell * 0.48);
        // The grid is centred: equal margins on both sides.
        let right = 600.0 - (bare.origin_x + 18.0 * bare.cell);
        assert!((bare.origin_x - right).abs() < 0.01);
        // The whole board, plus its padding, fits.
        assert!(with_coords.origin_x - with_coords.cell * (EDGE_PAD + COORD_PAD) >= -0.01);
    }
}
