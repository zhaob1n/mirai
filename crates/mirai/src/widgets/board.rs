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

use mirai_core::{
    Board, COLUMNS, Color, DeadSet, GameTree, MarkKind, Marks, NodeId, Point, Position, Rules, Size,
};
use mirai_proto::types::dq_policy;

use crate::app::{AppState, EditorTool, NodeRef};
use crate::widgets::paint::{fill_disc, hline, over, rgba8, stroke_disc, stroke_rect, vline};

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

/// Opacity floor and ceiling for a candidate blob.
///
/// Hue is how much the move loses ([`crate::palette`]), or grey when there is not enough search
/// for that loss to mean anything. Opacity is how much search stands behind the reading, and it
/// reaches the ceiling at [`crate::palette::TRUSTED_VISITS`] — the same visit where the blob
/// stops being grey and starts carrying figures. So the fade only ever applies to a move that
/// has no colour yet: once a candidate is worth a hue it is drawn at full strength, and how
/// much search it actually got is then a number in the label and in the list's Visits column,
/// not a shade the eye has to estimate.
///
/// It is deliberately *not* the move's share of the search, which LizzieYzy uses and this widget
/// used to: a search left running puts nine tenths of its visits on the move it likes, and a
/// share-scaled blob then draws the very move the reviewer opened the file for — a 132-visit
/// blunder beside a 9 300-visit pick — at the floor, in a red nobody can see.
///
/// The floor stays low on purpose. With 20 suggestions most of the board is the tail of the
/// list, and the tail's job is to be *there* without competing with the moves that carry numbers.
const BLOB_ALPHA_MIN: f32 = 0.18;
const BLOB_ALPHA_MAX: f32 = 0.85;

/// Below this many visits the numbers are noise, and unreadable through a faint blob. The
/// engine's own choice and the record's next move keep theirs whatever their search. Same
/// floor as [`crate::palette::TRUSTED_VISITS`]: an estimate not worth printing is not a
/// colour either, and not worth drawing at full strength.
const LABEL_MIN_VISITS: u32 = crate::palette::TRUSTED_VISITS;

/// How many times `Layout::cell` must repeat before the board draws text again.
///
/// Two, not one. A spring can sit on the same integer width for a single frame near its end,
/// so one repeat is no evidence that it has stopped — and accepting one was measured doing
/// real damage: on a 31-frame fold the board took a mid-animation plateau for a stop, paid
/// 19 ms rasterising glyphs at a size it then threw away, blew that frame at 30 ms, and
/// dropped the text again on the next. A plateau long enough to fool two repeats is one the
/// eye cannot tell from a stop anyway.
const CELL_SETTLED: u8 = 2;

/// How opaque the blob for a candidate with this much search behind it is drawn.
fn blob_alpha(visits: u32) -> f32 {
    let t = (visits as f32 / crate::palette::TRUSTED_VISITS as f32).min(1.0);
    BLOB_ALPHA_MIN + (BLOB_ALPHA_MAX - BLOB_ALPHA_MIN) * t
}

/// Black or white text, whichever reads better on `bg` — the blob already composited over
/// the board, because a faint blob is mostly wood.
fn text_on(bg: gdk::RGBA) -> gdk::RGBA {
    if crate::palette::is_light(bg.red(), bg.green(), bg.blue()) {
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

/// The ring that marks the move the game record plays next, and its width in pixels.
///
/// This outline used to mark the engine's own choice, and handing it over is deliberate: the
/// visit ramp already puts the busiest move — which is the engine's pick in all but the closest
/// positions — at the top of the ramp in red, and the candidate list's first row *is* the pick,
/// in the engine's own order. One ring is worth more on the move a reviewer is standing next to
/// than on the one the colour already names. Everything else on the board is taken: the ramp
/// spans indigo to red, the last move is red, and an inner ring cuts through the three lines of
/// text a labelled blob carries.
const RECORD_RING: gdk::RGBA = gdk::RGBA::new(1.0, 1.0, 1.0, 0.95);
const RECORD_RING_W: f32 = 2.0;

/// The disc under a record move the search never reached, so that [`RECORD_RING`] has
/// something darker than wood to read against.
const EMPTY_BLOB: gdk::RGBA = gdk::RGBA::new(0.0, 0.0, 0.0, 0.18);

/// Opacity of the stone previewed under the pointer before a click places it. Lighter than a
/// played stone, heavier than a dead one (0.35), so it reads as "not yet" rather than "gone".
const HOVER_STONE_ALPHA: f32 = 0.5;

fn is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

/// A hit-tested intersection click. Window installs the only hook.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoardClick {
    pub point: Point,
    pub button: u32,
    pub modifiers: gdk::ModifierType,
}

/// `Rc`, not `Box`: the hook re-enters this widget — the play controller redraws the
/// score overlay from inside it — so a click clones the handle and releases the cell
/// before calling, rather than holding a borrow across it.
type ClickHook = Rc<dyn Fn(BoardClick) + 'static>;

/// The geometry and position a single `snapshot` pass draws against.
///
/// Every `draw_*` layer needs the same values, so they travel together rather than as
/// repeated parameters.
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
    /// Whether this pass may emit text. False while `Layout::cell` is still moving, which
    /// is the one thing that makes every glyph a fresh rasterisation; see `snapshot`.
    pub labels: bool,
}
struct BoardProjection {
    size: Size,
    rules: Rules,
    position: Position,
    marks: Marks,
    /// Points that carry a shape or label; built once per projection, indexed in snapshot.
    marked: Box<[bool]>,
    last: Option<(Color, Point)>,
    /// The move the record plays on from the cursor, when it is a real point on an empty
    /// intersection; see [`record_next`].
    next: Option<Point>,
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
        /// Where the pointer is, in widget coordinates, whatever the tool. Coordinates, not a
        /// `Point`: an index would land on another intersection after a size or layout
        /// change, and neither emits a motion event. The ghost stone is hit-tested from this
        /// at draw time so it follows the position, tool and geometry without bookkeeping.
        pub pointer: Cell<Option<(f64, f64)>>,
        /// Set while a game is running and a click would not place a stone for the person:
        /// the engine's turn, scoring, a finished game.
        pub play_locked: Cell<bool>,
        pub static_layer: RefCell<Option<(StaticKey, gsk::RenderNode)>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        pub menu_point: Cell<Option<Point>>,
        pub menu_cursor: Cell<Option<NodeRef>>,
        pub play_here: OnceCell<gio::SimpleAction>,
        pub main_line: OnceCell<gio::SimpleAction>,
        pub delete_branch: OnceCell<gio::SimpleAction>,
        pub(super) click_hook: RefCell<Option<ClickHook>>,
        pub dead: RefCell<Option<DeadSet>>,
        pub territory: RefCell<Option<Box<[Option<Color>]>>>,
        pub(super) projection: RefCell<Option<BoardProjection>>,
        /// `Layout::cell` of the previous snapshot, and `None` before the first one.
        ///
        /// Labels are pango glyphs sized to `cell`, and GSK keys its glyph cache on the
        /// `PangoFont` and the glyph — never on where the glyph lands. So a board that only
        /// moves keeps its text for free, while a board whose `cell` changed re-rasterises
        /// every digit. This is the only thing worth deferring on.
        pub drawn_cell: Cell<Option<f32>>,
        /// Consecutive snapshots that have shared `drawn_cell`; see [`CELL_SETTLED`].
        pub cell_repeats: Cell<u8>,
        /// Set while the one-shot tick callback that repaints at a settled `cell` is queued.
        pub label_tick: Cell<bool>,
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

        /// Queues exactly one redraw on the next frame.
        ///
        /// It cannot be a plain `queue_draw`: GTK clears `draw_needed` *after* the
        /// `snapshot` vfunc returns (`gtk_widget_do_snapshot`), so a request made from
        /// inside the vfunc is swallowed. A tick callback runs in the next frame's update
        /// phase, ahead of layout and paint, so the redraw it asks for is served by that
        /// same frame. A GLib idle would instead run between two frames at a priority the
        /// frame clock outranks, which is how the previous version could land its repaint
        /// in the middle of an animation.
        fn redraw_next_frame(&self) {
            if self.label_tick.replace(true) {
                return;
            }
            self.obj().add_tick_callback(|obj, _| {
                obj.imp().label_tick.set(false);
                obj.queue_draw();
                glib::ControlFlow::Break
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
            let _t = crate::render_probe::Timer::new("board-snapshot");
            let projection = self.projection.borrow();
            let Some(projection) = projection.as_ref() else {
                return;
            };
            let l = self.layout.get();
            if l.cell < 3.0 {
                return;
            }
            crate::render_probe::trace("board-cell", l.cell);
            // Text is deferred while `cell` is moving. GSK caches a rasterised glyph under its
            // `PangoFont`, so a board that only moves or recentres keeps every digit for free
            // and a board whose `cell` changed pays for all of them again — measured at
            // 0.5-1.9 ms against 11-19 ms (`RENDERING.md` §6). Deferring on the allocation
            // instead was both too eager, suppressing frames that were already cheap, and
            // unsound: GTK skips `size_allocate` when the pixel size is unchanged, so that
            // signal goes quiet exactly on a spring's plateau frames.
            let repeats = if self.drawn_cell.replace(Some(l.cell)) == Some(l.cell) {
                self.cell_repeats.get().saturating_add(1)
            } else {
                0
            };
            self.cell_repeats.set(repeats);
            let labels = repeats >= CELL_SETTLED || !crate::render_probe::label_defer();
            if !labels {
                self.redraw_next_frame();
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
                    labels,
                };
                self.draw_stones(snapshot, scene, None);
                self.draw_numbers(snapshot, scene, &seq, None, None);
                return;
            }

            let scene = Scene {
                l,
                size,
                board: &projection.position.board,
                dark,
                labels,
            };
            let dead = self.dead.borrow();
            self.draw_stones(snapshot, scene, dead.as_ref());
            if let Some(territory) = self.territory.borrow().as_ref() {
                self.draw_territory(snapshot, scene, territory);
            }
            drop(dead);

            let marked = projection.marked.as_ref();
            if let Some(nums) = projection.move_numbers.as_ref() {
                self.draw_numbers(
                    snapshot,
                    scene,
                    nums,
                    projection.last.map(|(_, p)| p),
                    Some(marked),
                );
            } else if let Some((color, p)) = projection.last
                && !marked[p.index()]
            {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                fill_disc(snapshot, cx, cy, l.stone_r * 0.34, &last_move_tint(color));
            }

            if let Some(report) = projection.report.as_ref() {
                self.draw_candidates(snapshot, scene, report, projection);
            }
            if let Some((p, color)) = self.obj().hover_stone(projection) {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                snapshot.push_opacity(HOVER_STONE_ALPHA as f64);
                draw_stone(snapshot, cx, cy, l.stone_r, color, false);
                snapshot.pop();
            }
            self.draw_marks(snapshot, scene, &projection.marks);
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
            marked: Option<&[bool]>,
        ) {
            if !scene.labels {
                return;
            }
            let _t = crate::render_probe::Timer::new("board-numbers");
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
                    if marked.is_some_and(|m| m[p.index()]) {
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
                ..
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
                    match kind {
                        // An axis-aligned outline is a border node; only the diagonal and
                        // curved marks have to be paths, and each of those covers one stone,
                        // so the area GSK rasterises is a cell rather than the board.
                        MarkKind::Circle => {
                            stroke_disc(
                                snapshot,
                                cx,
                                cy,
                                r * 0.62,
                                stroke.line_width(),
                                &color_at(p),
                            );
                        }
                        MarkKind::Square => {
                            let h = r * 0.62;
                            let rect = graphene::Rect::new(cx - h, cy - h, h * 2.0, h * 2.0);
                            stroke_rect(snapshot, &rect, stroke.line_width(), &color_at(p));
                        }
                        MarkKind::Triangle => {
                            let pb = gsk::PathBuilder::new();
                            pb.move_to(cx, cy - r * 0.82);
                            pb.line_to(cx + r * 0.74, cy + r * 0.52);
                            pb.line_to(cx - r * 0.74, cy + r * 0.52);
                            pb.close();
                            snapshot.append_stroke(&pb.to_path(), &stroke, &color_at(p));
                        }
                        MarkKind::Cross => {
                            let h = r * 0.6;
                            let pb = gsk::PathBuilder::new();
                            pb.move_to(cx - h, cy - h);
                            pb.line_to(cx + h, cy + h);
                            pb.move_to(cx + h, cy - h);
                            pb.line_to(cx - h, cy + h);
                            snapshot.append_stroke(&pb.to_path(), &stroke, &color_at(p));
                        }
                    }
                }
            }

            if scene.labels {
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
            report: &mirai_engine::Report,
            projection: &BoardProjection,
        ) {
            let _t = crate::render_probe::Timer::new("board-candidates");
            let to_play = projection.position.to_play;
            let max = projection.suggestion_limit;
            let next = projection.next;
            let marked = &projection.marked;
            let Scene {
                l,
                size,
                board,
                dark,
                labels,
            } = scene;
            // Losses are pick.utility − candidate.utility, so the pick is always the coolest
            // blob and a candidate that reads better than it clamps there too.
            let pick_utility = report.moves.first().map(|m| m.utility_for(to_play));
            let obj = self.obj();
            let mut fd = obj.pango_context().font_description().unwrap_or_default();
            let layout = obj.create_pango_layout(None);
            let mut ringed = false;

            for (rank, info) in report.moves.iter().take(max).enumerate() {
                if info.mv.is_pass() || !size.contains(info.mv) || board.at(info.mv).is_some() {
                    continue;
                }
                let (x, y) = size.xy(info.mv);
                let (cx, cy) = l.xy(x, y);
                let rgb = match pick_utility {
                    Some(p) => crate::palette::colour(
                        crate::palette::grade(p - info.utility_for(to_play)),
                        rank,
                        info.visits,
                    ),
                    None => crate::palette::GRADE_RAMP[0],
                };
                let blob = rgba8(rgb, blob_alpha(info.visits));
                fill_disc(snapshot, cx, cy, l.stone_r, &blob);
                let skip_overlay = marked[info.mv.index()];
                let played = next == Some(info.mv);
                if played && !skip_overlay {
                    stroke_disc(snapshot, cx, cy, l.stone_r, RECORD_RING_W, &RECORD_RING);
                    ringed = true;
                }

                // The engine's pick and the record's move keep their numbers whatever their
                // search: a played move the engine dismissed is exactly the one to read.
                if skip_overlay
                    || (info.visits < LABEL_MIN_VISITS && rank != 0 && !played)
                    || !labels
                {
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

            // The record can continue with a move that got no blob — one the engine never
            // searched, or one past the suggestion limit — and that is precisely the case a
            // reviewer is looking for. A white ring on bare wood is nearly invisible, so it
            // gets a dim disc to sit on, which also reads as what it is: a move with no search
            // behind it.
            if let Some(p) = next
                && !ringed
                && !marked[p.index()]
            {
                let (x, y) = size.xy(p);
                let (cx, cy) = l.xy(x, y);
                fill_disc(snapshot, cx, cy, l.stone_r, &EMPTY_BLOB);
                stroke_disc(snapshot, cx, cy, l.stone_r, RECORD_RING_W, &RECORD_RING);
            }
        }
    }
}

/// The move the game record plays on from `cursor`, when there is one to draw.
///
/// `children[0]` is the main line, which is exactly where [`AppState::go_next`] goes, so the
/// ring on the board and the forward key can never point at different moves. The filters are
/// ordered: [`Point::PASS`] is outside every board, and `Board::at` answers `None` for a point
/// it does not contain, so a pass has to be rejected before either is asked about it.
fn record_next(tree: &GameTree, cursor: NodeId, position: &Position) -> Option<Point> {
    let &child = tree.children(cursor).first()?;
    let (_, p) = tree.node(child).mv?;
    let drawable = !p.is_pass() && tree.info.size.contains(p) && position.board.at(p).is_none();
    drawable.then_some(p)
}

/// One slot per intersection; out-of-board marks are ignored. Built outside snapshot.
fn mark_mask(size: Size, marks: &Marks) -> Box<[bool]> {
    let mut marked = vec![false; size.points()];
    let mut paint = |p: Point| {
        if size.contains(p) {
            marked[p.index()] = true;
        }
    };
    for kind in [
        MarkKind::Triangle,
        MarkKind::Square,
        MarkKind::Circle,
        MarkKind::Cross,
    ] {
        for &p in marks.of(kind) {
            paint(p);
        }
    }
    for &(p, _) in &marks.labels {
        paint(p);
    }
    marked.into_boxed_slice()
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

    /// Window installs the sole click hook. The board does not play by default.
    pub(crate) fn set_click_hook(&self, hook: impl Fn(BoardClick) + 'static) {
        *self.imp().click_hook.borrow_mut() = Some(Rc::new(hook));
    }

    /// Hit-tests `(x, y)` and delivers the click to the window hook. No default play path.
    pub(crate) fn click_at(&self, button: u32, modifiers: gdk::ModifierType, x: f64, y: f64) {
        let Some(point) = self.point_at(x, y) else {
            return;
        };
        self.clear_preview();
        let hook = self.imp().click_hook.borrow().clone();
        if let Some(hook) = hook {
            hook(BoardClick {
                point,
                button,
                modifiers,
            });
        }
    }

    /// Centre of `p` in widget coordinates from the live layout, if both are valid.
    pub(crate) fn point_center(&self, p: Point) -> Option<(f64, f64)> {
        let layout = self.imp().layout.get();
        if layout.cell <= 0.0 || layout.width <= 0 || layout.height <= 0 {
            return None;
        }
        let size = self.imp().board_size();
        if !size.contains(p) {
            return None;
        }
        let (x, y) = size.xy(p);
        let (cx, cy) = layout.xy(x, y);
        Some((cx as f64, cy as f64))
    }

    /// Toolbar callers supply their bounds in board coordinates; board clicks use `point`.
    pub(crate) fn show_menu(
        &self,
        point: Option<Point>,
        toolbar_anchor: Option<gdk::Rectangle>,
        editable: bool,
    ) {
        let imp = self.imp();
        imp.menu_point.set(point);
        imp.menu_cursor.set(Some(self.state().cursor_ref()));
        if let Some(play) = imp.play_here.get() {
            play.set_enabled(editable && point.is_some());
        }
        if let Some(main_line) = imp.main_line.get() {
            main_line.set_enabled(editable);
        }
        if let Some(delete) = imp.delete_branch.get() {
            delete.set_enabled(editable);
        }
        let Some(popover) = imp.popover.borrow().clone() else {
            return;
        };
        popover.set_halign(if toolbar_anchor.is_some() {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let Some(rect) = toolbar_anchor.or_else(|| {
            point
                .and_then(|p| self.point_center(p))
                .map(|(x, y)| gdk::Rectangle::new(x.round() as i32, y.round() as i32, 1, 1))
        }) else {
            return;
        };
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    }

    pub(crate) fn close_menu(&self) {
        self.imp().menu_point.set(None);
        self.imp().menu_cursor.set(None);
        if let Some(popover) = self.imp().popover.borrow().as_ref() {
            popover.popdown();
        }
    }

    pub(crate) fn clear_preview(&self) {
        let hover = self.imp().hover.replace(None);
        let pinned = self.imp().pinned.replace(None);
        if hover.is_some() || pinned.is_some() {
            self.queue_draw();
        }
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
    /// hover over a blob still wins while it lasts. Non-Play tools ignore the overlay.
    pub fn set_pv_preview(&self, index: Option<usize>) {
        if self.imp().pinned.get() == index {
            return;
        }
        self.imp().pinned.set(index);
        self.queue_draw();
    }

    /// Suppresses the ghost stone while a game is running and a click would place nothing.
    pub(crate) fn set_play_locked(&self, locked: bool) {
        if self.imp().play_locked.replace(locked) != locked {
            self.queue_draw();
        }
    }

    /// The stone a primary click at the pointer would place, drawn translucent before it
    /// lands: the side to play where that move is legal (superko aside, which needs the
    /// history), or the setup colour on an empty point. Never on a candidate blob: that is
    /// the PV preview's, and the hover index which drives it is only refreshed by motion.
    fn hover_stone(&self, projection: &BoardProjection) -> Option<(Point, Color)> {
        let imp = self.imp();
        if imp.play_locked.get() {
            return None;
        }
        let (x, y) = imp.pointer.get()?;
        let p = imp.layout.get().hit(projection.size, x, y)?;
        let on_candidate = projection.report.as_ref().is_some_and(|report| {
            report
                .moves
                .iter()
                .take(projection.suggestion_limit)
                .any(|info| info.mv == p)
        });
        if on_candidate {
            return None;
        }
        let board = &projection.position.board;
        match self.state().editor_tool() {
            EditorTool::Play => {
                let color = projection.position.to_play;
                board
                    .is_legal(color, p, &projection.rules)
                    .then_some((p, color))
            }
            EditorTool::Setup(color) => board.at(p).is_none().then_some((p, color)),
            _ => None,
        }
    }

    /// The candidate whose PV is currently previewed: pointer hover first, then the pin.
    fn active_preview(&self) -> Option<usize> {
        if self.state().editor_tool() != EditorTool::Play {
            return None;
        }
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
        let (size, rules, marks, last, next, move_numbers) = {
            let tree = state.tree();
            let size = tree.info.size;
            let rules = tree.info.rules.rules();
            let node = tree.node(cursor);
            let marks = node.marks.clone();
            let last = node
                .mv
                .filter(|&(color, p)| !p.is_pass() && position.board.at(p) == Some(color));
            let next = record_next(&tree, cursor, &position);
            let move_numbers = state.show_move_numbers().then(|| tree.move_numbers(cursor));
            (size, rules, marks, last, next, move_numbers)
        };
        let marked = mark_mask(size, &marks);
        self.imp().projection.replace(Some(BoardProjection {
            size,
            rules,
            position,
            marks,
            marked,
            last,
            next,
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
        for button in [gdk::BUTTON_PRIMARY, gdk::BUTTON_SECONDARY] {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(button);
            gesture.connect_released(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |gesture, _, x, y| {
                    view.click_at(
                        gesture.current_button(),
                        gesture.current_event_state(),
                        x,
                        y,
                    );
                }
            ));
            self.add_controller(gesture);
        }

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
                let Some(p) = view.imp().menu_point.get() else {
                    return;
                };
                if view.resolved_menu_cursor().is_none() {
                    return;
                }
                view.activate_win_u32("win.play-at", u32::from(p.0));
            }
        ));
        group.add_action(&play);
        let _ = self.imp().play_here.set(play);

        let main_line = gio::SimpleAction::new("main-line", None);
        main_line.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                let Some(id) = view.menu_node_target() else {
                    return;
                };
                view.activate_win_u32("win.promote-line-at", id.0);
            }
        ));
        group.add_action(&main_line);
        let _ = self.imp().main_line.set(main_line);

        let delete = gio::SimpleAction::new("delete-branch", None);
        delete.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                let Some(id) = view.menu_node_target() else {
                    return;
                };
                view.activate_win_u32("win.delete-branch-at", id.0);
            }
        ));
        group.add_action(&delete);
        let _ = self.imp().delete_branch.set(delete);

        let copy = gio::SimpleAction::new("copy-sgf", None);
        copy.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _| {
                let _ = view.activate_action("win.copy-sgf", None);
            }
        ));
        group.add_action(&copy);

        self.insert_action_group("board", Some(&group));

        let menu = gio::Menu::new();
        menu.append(Some("Play Here"), Some("board.play-here"));
        menu.append(Some("Set as Main Line"), Some("board.main-line"));
        menu.append(Some("Delete Branch"), Some("board.delete-branch"));
        menu.append(Some("Copy SGF"), Some("board.copy-sgf"));
        let to_play = gio::Menu::new();
        to_play.append(Some("Black to Play"), Some("win.to-play::black"));
        to_play.append(Some("White to Play"), Some("win.to-play::white"));
        menu.append_section(None, &to_play);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(self);
        popover.set_has_arrow(false);
        *self.imp().popover.borrow_mut() = Some(popover);
    }

    fn activate_win_u32(&self, name: &str, value: u32) {
        let _ = self.activate_action(name, Some(&value.to_variant()));
    }

    fn resolved_menu_cursor(&self) -> Option<NodeId> {
        self.state().resolve_node(self.imp().menu_cursor.get()?)
    }

    /// The child of the saved menu cursor whose move sits on the menu point, if any.
    fn node_for_menu_point(&self) -> Option<NodeId> {
        let p = self.imp().menu_point.get()?;
        let cursor = self.resolved_menu_cursor()?;
        let tree = self.state().tree();
        tree.children(cursor)
            .iter()
            .copied()
            .find(|&c| tree.node(c).mv.is_some_and(|(_, mv)| mv == p))
    }

    fn menu_node_target(&self) -> Option<NodeId> {
        let cursor = self.resolved_menu_cursor()?;
        Some(self.node_for_menu_point().unwrap_or(cursor))
    }

    pub(crate) fn update_hover(&self, pointer: Option<(f64, f64)>) {
        let old = self
            .imp()
            .pointer
            .replace(pointer)
            .and_then(|(x, y)| self.point_at(x, y));
        let new = pointer.and_then(|(x, y)| self.point_at(x, y));
        let tool = self.state().editor_tool();
        // Only these tools draw a ghost stone; the others redraw for nothing.
        let ghost = matches!(tool, EditorTool::Play | EditorTool::Setup(_));
        let idx = if tool == EditorTool::Play {
            pointer.and_then(|(x, y)| self.candidate_at(x, y))
        } else {
            None
        };
        let hovered = self.imp().hover.replace(idx) != idx;
        if (ghost && old != new) || hovered {
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
    use mirai_core::{GameInfo, RuleSet};

    /// Opacity says how much search stands behind a blob's colour, so it rises with the visits
    /// themselves — not with their share of a search that may have run for ten seconds — and it
    /// tops out at [`crate::palette::TRUSTED_VISITS`], the same visit at which the blob stops
    /// being grey and starts carrying figures. One threshold: a faded blob is always a grey
    /// one, and a coloured blob is always full strength.
    #[test]
    fn opacity_tracks_how_much_a_move_was_searched() {
        assert_eq!(blob_alpha(0), BLOB_ALPHA_MIN);
        assert!(blob_alpha(5) > blob_alpha(1));
        assert!(blob_alpha(9) > blob_alpha(5));
        let full = crate::palette::TRUSTED_VISITS;
        assert!((blob_alpha(full) - BLOB_ALPHA_MAX).abs() < 1e-6);
        assert_eq!(blob_alpha(50_000), blob_alpha(full));
        assert_eq!(LABEL_MIN_VISITS, full);
        assert!(crate::palette::is_known(1, full) && !crate::palette::is_known(1, full - 1));
        assert!(blob_alpha(full - 1) < BLOB_ALPHA_MAX);
    }

    /// The ring answers "where does the record go next?", so it has to read `children[0]` —
    /// the main line the forward key walks — and it has to refuse everything it cannot draw a
    /// ring on. A pass is the sharp case: [`Point::PASS`] is outside every board, so asking
    /// the board about it first would index past the end.
    #[test]
    fn the_record_ring_follows_the_main_line_and_only_drawable_points() {
        let size = Size::square(9);
        let mut tree = GameTree::new(GameInfo::new(size, RuleSet::default()));
        let root = tree.root();
        let position = tree.position(root).clone();
        assert_eq!(
            record_next(&tree, root, &position),
            None,
            "a leaf has no next"
        );

        let d4 = size.point(3, 5);
        let played = tree.play(root, Color::Black, d4).expect("legal");
        // A variation added afterwards is `children[1]`, so the main line still wins.
        tree.add_variation(root, Color::Black, size.point(5, 3))
            .expect("legal");
        assert_eq!(record_next(&tree, root, &position), Some(d4));

        let after = tree.position(played).clone();
        let passed = tree
            .play(played, Color::White, Point::PASS)
            .expect("a pass is always legal");
        assert_eq!(
            record_next(&tree, played, &after),
            None,
            "a pass is not a point"
        );

        // Only a loaded record can put a move on an occupied point; nothing may ring it.
        let bogus = tree.add_child(passed);
        tree.node_mut(bogus).mv = Some((Color::Black, d4));
        let after_pass = tree.position(passed).clone();
        assert_eq!(record_next(&tree, passed, &after_pass), None);
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
