// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `WinrateGraph` — win-rate curve, score-lead curve and blunder strip. Step 10.
//!
//! Everything is drawn from the cached [`mirai_core::NodeAnalysis`] on the nodes of the
//! current main line, so the graph is a pure function of the tree and costs nothing when
//! no analysis has been stored yet.

use std::cell::{Cell, OnceCell, RefCell};

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use mirai_client::batch::is_blunder_drop;
use mirai_core::{Color, GameTree, NodeId};

use crate::app::AppState;
use crate::palette;
use crate::widgets::paint::{fill_disc, hline, rgba8, with_alpha};

/// How badly a move hurt the player who played it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Severity {
    /// Below the noise floor; the strip leaves it blank.
    None,
    /// 2–5 % lost.
    Minor,
    /// 5–10 % lost.
    Medium,
    /// More than 10 % lost.
    Major,
}

impl Severity {
    /// `None` means "draw nothing", which is not the same as "draw transparent".
    ///
    /// The three warm stops of [`palette::GRADE_RAMP`] are these ticks: a candidate that
    /// shows orange on the board is an orange tick here once it is played, and the ramp is
    /// the only place those hexes live.
    pub(crate) fn color(self) -> Option<gdk::RGBA> {
        match self {
            Severity::None => None,
            Severity::Minor => Some(rgba8(palette::GRADE_RAMP[3], 0.85)),
            Severity::Medium => Some(rgba8(palette::GRADE_RAMP[4], 0.85)),
            Severity::Major => Some(rgba8(palette::GRADE_RAMP[5], 0.9)),
        }
    }
}

/// Classifies a win-rate drop expressed as a fraction of 1.
pub(crate) fn severity_of_drop(drop: f32) -> Severity {
    // Same floor as `mirai_client::batch::blunders`: NaN and ≤ 2 % are noise.
    if !is_blunder_drop(drop) {
        Severity::None
    } else if drop < 0.05 {
        Severity::Minor
    } else if drop < 0.10 {
        Severity::Medium
    } else {
        Severity::Major
    }
}

/// How much the player who played the move lost by playing it.
///
/// `before` is the Black-perspective win rate of the parent position, `after` that of the
/// position the move created. A White blunder shows up as a rise in Black's win rate, so
/// both are flipped into the mover's own perspective first.
pub(crate) fn blunder_severity(mover: Color, before: f32, after: f32) -> Severity {
    severity_of_drop(mover.winrate_for(before) - mover.winrate_for(after))
}

/// One main-line sample: everything the graph needs about a node.
#[derive(Clone, Copy, Debug)]
struct Sample {
    /// Black-perspective win rate, if this node has been analysed.
    winrate: Option<f32>,
    /// Black-perspective score lead, if this node has been analysed.
    lead: Option<f32>,
    /// Blunder severity of the move *leading into* this node.
    blunder: Severity,
}

/// One sample per main-line node. Blunders use the immediate parent only: an unanalysed
/// gap is not a mistake, matching [`mirai_client::batch::blunders`].
fn collect_samples(tree: &GameTree, line: &[NodeId]) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(line.len());
    let mut prev_winrate: Option<f32> = None;
    for &id in line {
        let node = tree.node(id);
        let analysis = node.analysis.as_ref();
        let winrate = analysis.map(|a| a.winrate);
        let lead = analysis.map(|a| a.score_lead);
        let blunder = match (node.mv, prev_winrate, winrate) {
            (Some((mover, _)), Some(before), Some(after)) => blunder_severity(mover, before, after),
            _ => Severity::None,
        };
        samples.push(Sample {
            winrate,
            lead,
            blunder,
        });
        prev_winrate = winrate;
    }
    samples
}

#[derive(Default)]
struct GraphProjection {
    line: Vec<NodeId>,
    samples: Vec<Sample>,
    cursor_index: usize,
}
#[derive(Clone, Copy, PartialEq)]
struct RenderKey {
    width: i32,
    height: i32,
    foreground: gdk::RGBA,
    accent: gdk::RGBA,
    text_serial: u32,
}

#[derive(Clone, Copy)]
struct AxisMetrics {
    text_serial: u32,
    range: f32,
    left: f32,
    right: f32,
}
impl AxisMetrics {
    fn minimum_width(self) -> i32 {
        ((self.left + self.right).ceil() as i32 + 32).max(120)
    }
}

/// Geometry, in widget coordinates.
struct Geom {
    left: f32,
    right: f32,
    top: f32,
    /// Bottom of the curve area (the blunder strip lives below it).
    bottom: f32,
    strip_top: f32,
    strip_bottom: f32,
    step: f32,
}

const PAD_T: f32 = 7.0;
const PAD_B: f32 = 3.0;
const STRIP_H: f32 = 10.0;
const STRIP_GAP: f32 = 2.0;
const NAT_HEIGHT: i32 = 140;

impl Geom {
    fn new(width: f32, height: f32, samples: usize, left: f32, right_pad: f32) -> Geom {
        let strip_bottom = (height - PAD_B).max(PAD_T + 1.0);
        let strip_top = (strip_bottom - STRIP_H).max(PAD_T + 1.0);
        let bottom = (strip_top - STRIP_GAP).max(PAD_T + 1.0);
        let right = (width - right_pad).max(left + 1.0);
        let span = right - left;
        let step = if samples > 1 {
            span / (samples - 1) as f32
        } else {
            span
        };
        Geom {
            left,
            right,
            top: PAD_T,
            bottom,
            strip_top,
            strip_bottom,
            step,
        }
    }

    #[inline]
    fn x(&self, i: usize) -> f32 {
        self.left + i as f32 * self.step
    }

    /// Maps a win rate in `0..=1` onto the left axis (1.0 at the top).
    #[inline]
    fn y_winrate(&self, w: f32) -> f32 {
        self.bottom - w.clamp(0.0, 1.0) * (self.bottom - self.top)
    }

    /// Maps a score lead onto the right axis, which is symmetric about zero.
    #[inline]
    fn y_lead(&self, lead: f32, range: f32) -> f32 {
        let mid = (self.top + self.bottom) * 0.5;
        let half = (self.bottom - self.top) * 0.5;
        mid - (lead / range).clamp(-1.0, 1.0) * half
    }

    /// The sample index nearest to a widget x coordinate.
    fn index_at(&self, x: f32, samples: usize) -> usize {
        if samples <= 1 || self.step <= 0.0 {
            return 0;
        }
        let i = ((x - self.left) / self.step).round();
        (i.max(0.0) as usize).min(samples - 1)
    }
}

/// The main-line node under widget x, copied out so the projection borrow can end.
fn node_at(projection: &GraphProjection, geom: &Geom, x: f32) -> Option<NodeId> {
    if projection.line.is_empty() {
        return None;
    }
    Some(projection.line[geom.index_at(x, projection.line.len())])
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct WinrateGraph {
        pub state: OnceCell<AppState>,
        pub pressed: Cell<bool>,
        pub(super) projection: RefCell<GraphProjection>,
        pub(super) render_cache: RefCell<Option<(RenderKey, gsk::RenderNode)>>,
        pub(super) axis_metrics: Cell<Option<AxisMetrics>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for WinrateGraph {
        const NAME: &'static str = "MiraiWinrateGraph";
        type Type = super::WinrateGraph;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for WinrateGraph {}

    impl WidgetImpl for WinrateGraph {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Vertical => (NAT_HEIGHT, NAT_HEIGHT, -1, -1),
                _ => {
                    let minimum = self.obj().axis_metrics().minimum_width();
                    (minimum, minimum.max(240), -1, -1)
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct WinrateGraph(ObjectSubclass<imp::WinrateGraph>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl WinrateGraph {
    pub fn new(state: &AppState) -> WinrateGraph {
        let this: WinrateGraph = glib::Object::new();
        this.imp()
            .state
            .set(state.clone())
            .expect("a fresh WinrateGraph cannot already hold a state");
        this.add_css_class("mirai-winrate");
        this.set_hexpand(true);
        this.set_tooltip_text(Some(
            "Black win rate (solid) and Black score lead (dashed) over the main line",
        ));

        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        {
            let weak = this.downgrade();
            click.connect_pressed(move |_, _, x, _| {
                if let Some(this) = weak.upgrade() {
                    this.imp().pressed.set(true);
                    this.jump_to(x as f32);
                }
            });
        }
        {
            let weak = this.downgrade();
            click.connect_released(move |_, _, _, _| {
                if let Some(this) = weak.upgrade() {
                    this.imp().pressed.set(false);
                }
            });
        }
        {
            let weak = this.downgrade();
            click.connect_cancel(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.imp().pressed.set(false);
                }
            });
        }
        this.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        {
            let weak = this.downgrade();
            motion.connect_motion(move |_, x, _| {
                if let Some(this) = weak.upgrade()
                    && this.imp().pressed.get()
                {
                    this.jump_to(x as f32);
                }
            });
        }
        {
            let weak = this.downgrade();
            motion.connect_leave(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.imp().pressed.set(false);
                }
            });
        }
        this.add_controller(motion);

        this
    }

    /// The window state this graph plots, held directly rather than fetched back through the
    /// window on every refresh.
    pub fn state(&self) -> &AppState {
        self.imp()
            .state
            .get()
            .expect("WinrateGraph was built without a state")
    }
    fn axis_metrics(&self) -> AxisMetrics {
        let context = self.pango_context();
        let serial = context.serial();
        if let Some(axis) = self.imp().axis_metrics.get()
            && axis.text_serial == serial
        {
            return axis;
        }
        let max_lead = self
            .imp()
            .projection
            .borrow()
            .samples
            .iter()
            .filter_map(|sample| sample.lead)
            .fold(0.0f32, |max, lead| max.max(lead.abs()));
        let range = max_lead.ceil().max(5.0);
        let layout = self.create_pango_layout(Some("100"));
        let left = (layout.pixel_size().0 as f32 + 8.0).max(30.0);
        layout.set_text(&format!("+{}", range as i32));
        let positive = layout.pixel_size().0;
        layout.set_text(&format!("−{}", range as i32));
        let right = (positive.max(layout.pixel_size().0) as f32 + 8.0).max(34.0);
        let axis = AxisMetrics {
            text_serial: serial,
            range,
            left,
            right,
        };
        self.imp().axis_metrics.set(Some(axis));
        axis
    }

    pub(crate) fn refresh(&self) {
        let before = self.axis_metrics().minimum_width();
        *self.imp().projection.borrow_mut() = self.samples(self.state());
        self.imp().axis_metrics.set(None);
        if self.axis_metrics().minimum_width() != before {
            self.queue_resize();
        }
        self.imp().render_cache.borrow_mut().take();
        self.queue_draw();
    }
    pub(crate) fn refresh_cursor(&self) {
        let state = self.state();
        let tree = state.tree();
        let path = tree.path_to(state.cursor());
        let cursor_index = {
            let projection = self.imp().projection.borrow();
            projection
                .line
                .iter()
                .zip(path.iter())
                .take_while(|(a, b)| a == b)
                .count()
                .saturating_sub(1)
        };
        self.imp().projection.borrow_mut().cursor_index = cursor_index;
        self.queue_draw();
    }

    /// Moves the cursor to the main-line node nearest to a widget x coordinate.
    fn jump_to(&self, x: f32) {
        let axis = self.axis_metrics();
        let id = {
            let projection = self.imp().projection.borrow();
            let geom = Geom::new(
                self.width() as f32,
                self.height() as f32,
                projection.line.len(),
                axis.left,
                axis.right,
            );
            node_at(&projection, &geom, x)
        };
        let Some(id) = id else { return };
        // INV-10: `set_cursor` rebuilds the projection; own the ID first.
        self.state().set_cursor(id);
    }

    /// Collects the main line into drawable samples.
    fn samples(&self, state: &AppState) -> GraphProjection {
        let tree = state.tree();
        let line = tree.main_line();
        let cursor = state.cursor();
        let path = tree.path_to(cursor);
        let cursor_index = line
            .iter()
            .zip(path.iter())
            .take_while(|(a, b)| a == b)
            .count()
            .saturating_sub(1);

        GraphProjection {
            samples: collect_samples(&tree, &line),
            line,
            cursor_index,
        }
    }
    fn render_base(&self, width: i32, height: i32) -> Option<gsk::RenderNode> {
        let style = adw::StyleManager::default();
        let key = RenderKey {
            width,
            height,
            foreground: self.color(),
            accent: style.accent_color().to_standalone_rgba(style.is_dark()),
            text_serial: self.pango_context().serial(),
        };
        if let Some((cached_key, node)) = self.imp().render_cache.borrow().as_ref()
            && *cached_key == key
        {
            return Some(node.clone());
        }
        let offscreen = gtk::Snapshot::new();
        self.draw_base(&offscreen, width as f32, height as f32);
        let node = offscreen.to_node()?;
        self.imp().render_cache.replace(Some((key, node.clone())));
        Some(node)
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let (width, height) = (self.width(), self.height());
        if width < 8 || height < 8 {
            return;
        }
        if let Some(node) = self.render_base(width, height) {
            snapshot.append_node(&node);
        }
        self.draw_cursor(snapshot, width as f32, height as f32);
    }

    fn draw_base(&self, snapshot: &gtk::Snapshot, width: f32, height: f32) {
        let axis_metrics = self.axis_metrics();
        let projection = self.imp().projection.borrow();
        let samples = &projection.samples;
        let geom = Geom::new(
            width,
            height,
            samples.len().max(1),
            axis_metrics.left,
            axis_metrics.right,
        );

        let fg = self.color();
        let axis = with_alpha(fg, 0.12);
        let faint = with_alpha(fg, 0.6);
        let curve = with_alpha(fg, 0.92);
        let accent = adw::StyleManager::default()
            .accent_color()
            .to_standalone_rgba(adw::StyleManager::default().is_dark());

        // Horizontal guides at 0 %, 25 %, 75 % and 100 %. One stroked path spanning the plot
        // would make GSK walk every segment across the whole graph on each frame; horizontal
        // lines are rectangles.
        for f in [0.0f32, 0.25, 0.75, 1.0] {
            hline(
                snapshot,
                geom.left,
                geom.right,
                geom.y_winrate(f),
                1.0,
                &axis,
            );
        }

        let half = gsk::PathBuilder::new();
        let y50 = geom.y_winrate(0.5);
        half.move_to(geom.left, y50);
        half.line_to(geom.right, y50);
        let half_stroke = gsk::Stroke::new(1.0);
        half_stroke.set_dash(&[3.0, 3.0]);
        snapshot.append_stroke(&half.to_path(), &half_stroke, &with_alpha(fg, 0.22));

        let range = axis_metrics.range;

        let context = self.pango_context();
        let font = context.font_description().unwrap_or_default();
        let label_height = context.metrics(Some(&font), None).height() as f32 / pango::SCALE as f32;
        let layout = self.create_pango_layout(None);
        layout.set_text("100");
        let left_x = (geom.left - 8.0 - layout.pixel_size().0 as f32).max(2.0);
        Self::label(
            snapshot,
            &layout,
            &font,
            "100",
            left_x,
            geom.top - 1.0,
            &faint,
        );
        layout.set_text("50");
        let left_x = (geom.left - 8.0 - layout.pixel_size().0 as f32).max(2.0);
        Self::label(
            snapshot,
            &layout,
            &font,
            "50",
            left_x,
            y50 - label_height / 2.0,
            &faint,
        );
        layout.set_text("0");
        let left_x = (geom.left - 8.0 - layout.pixel_size().0 as f32).max(2.0);
        Self::label(
            snapshot,
            &layout,
            &font,
            "0",
            left_x,
            geom.bottom - label_height,
            &faint,
        );
        let hi = format!("+{}", range as i32);
        let lo = format!("−{}", range as i32);
        layout.set_text(&hi);
        let hi_x = width - layout.pixel_size().0 as f32 - 4.0;
        Self::label(
            snapshot,
            &layout,
            &font,
            &hi,
            hi_x,
            geom.top - 1.0,
            &with_alpha(accent, 0.75),
        );
        layout.set_text(&lo);
        let lo_x = width - layout.pixel_size().0 as f32 - 4.0;
        Self::label(
            snapshot,
            &layout,
            &font,
            &lo,
            lo_x,
            geom.bottom - label_height,
            &with_alpha(accent, 0.75),
        );

        // Score-lead curve, dashed, on the right axis.
        let lead_path = gsk::PathBuilder::new();
        let mut open = false;
        for (i, s) in samples.iter().enumerate() {
            match s.lead {
                Some(v) => {
                    let (x, y) = (geom.x(i), geom.y_lead(v, range));
                    if open {
                        lead_path.line_to(x, y);
                    } else {
                        lead_path.move_to(x, y);
                        open = true;
                    }
                }
                None => open = false,
            }
        }
        let lead_stroke = gsk::Stroke::new(1.5);
        lead_stroke.set_dash(&[4.0, 3.0]);
        lead_stroke.set_line_cap(gsk::LineCap::Butt);
        snapshot.append_stroke(&lead_path.to_path(), &lead_stroke, &accent);

        // Black win-rate curve on the left axis.
        let wr_path = gsk::PathBuilder::new();
        let mut open = false;
        for (i, s) in samples.iter().enumerate() {
            match s.winrate {
                Some(v) => {
                    let (x, y) = (geom.x(i), geom.y_winrate(v));
                    if open {
                        wr_path.line_to(x, y);
                    } else {
                        wr_path.move_to(x, y);
                        open = true;
                    }
                }
                None => open = false,
            }
        }
        let wr_stroke = gsk::Stroke::new(2.0);
        wr_stroke.set_line_join(gsk::LineJoin::Round);
        wr_stroke.set_line_cap(gsk::LineCap::Round);
        snapshot.append_stroke(&wr_path.to_path(), &wr_stroke, &curve);

        // Blunder strip: one bar per move, over the interval the move occupies.
        let bar_w = (geom.step * 0.9).clamp(1.0, 12.0);
        for (i, s) in samples.iter().enumerate() {
            let Some(color) = s.blunder.color() else {
                continue;
            };
            let x = (geom.x(i) - bar_w * 0.5).max(geom.left - bar_w * 0.5);
            snapshot.append_color(
                &color,
                &graphene::Rect::new(x, geom.strip_top, bar_w, geom.strip_bottom - geom.strip_top),
            );
        }
        // The strip's baseline keeps it legible when every move was sound.
        snapshot.append_color(
            &axis,
            &graphene::Rect::new(geom.left, geom.strip_bottom, geom.right - geom.left, 1.0),
        );
    }

    fn draw_cursor(&self, snapshot: &gtk::Snapshot, width: f32, height: f32) {
        let projection = self.imp().projection.borrow();
        let samples = &projection.samples;
        if samples.is_empty() {
            return;
        }
        let axis = self.axis_metrics();
        let geom = Geom::new(width, height, samples.len(), axis.left, axis.right);
        let cursor_index = projection.cursor_index.min(samples.len() - 1);
        let fg = self.color();
        let curve = with_alpha(fg, 0.92);
        let accent = adw::StyleManager::default()
            .accent_color()
            .to_standalone_rgba(adw::StyleManager::default().is_dark());
        let x = geom.x(cursor_index);
        snapshot.append_color(
            &with_alpha(accent, 0.85),
            &graphene::Rect::new(x - 0.5, geom.top, 1.0, geom.strip_bottom - geom.top),
        );
        if let Some(winrate) = samples[cursor_index].winrate {
            fill_disc(snapshot, x, geom.y_winrate(winrate), 3.0, &accent);
            let text = format!("Black {:.1}%", winrate * 100.0);
            let layout = self.create_pango_layout(Some(&text));
            let tx = (x + 5.0)
                .min(geom.right - layout.pixel_size().0 as f32)
                .max(geom.left);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(tx, geom.top + 1.0));
            snapshot.append_layout(&layout, &curve);
            snapshot.restore();
        }
    }

    fn label(
        snapshot: &gtk::Snapshot,
        layout: &pango::Layout,
        font: &pango::FontDescription,
        text: &str,
        x: f32,
        y: f32,
        color: &gdk::RGBA,
    ) {
        layout.set_text(text);
        layout.set_font_description(Some(font));
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x, y));
        snapshot.append_layout(layout, color);
        snapshot.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_thresholds() {
        assert_eq!(severity_of_drop(0.005), Severity::None);
        assert_eq!(severity_of_drop(0.03), Severity::Minor);
        assert_eq!(severity_of_drop(0.07), Severity::Medium);
        assert_eq!(severity_of_drop(0.20), Severity::Major);
        // Boundaries and improvements.
        assert_eq!(severity_of_drop(0.02), Severity::None);
        assert_eq!(severity_of_drop(0.05), Severity::Medium);
        assert_eq!(severity_of_drop(0.10), Severity::Major);
        assert_eq!(severity_of_drop(-0.3), Severity::None);
        assert_eq!(severity_of_drop(f32::NAN), Severity::None);
    }

    /// Black to move at 60 %; after Black's move Black is at `60 - drop` %.
    fn black_drop(drop: f32) -> Severity {
        blunder_severity(Color::Black, 0.60, 0.60 - drop)
    }

    /// White to move while Black is at 40 %; White losing `drop` lifts Black by `drop`.
    fn white_drop(drop: f32) -> Severity {
        blunder_severity(Color::White, 0.40, 0.40 + drop)
    }

    #[test]
    fn blunders_are_seen_from_the_movers_side() {
        for (drop, want) in [
            (0.005f32, Severity::None),
            (0.03, Severity::Minor),
            (0.07, Severity::Medium),
            (0.20, Severity::Major),
        ] {
            assert_eq!(black_drop(drop), want, "black drop {drop}");
            assert_eq!(white_drop(drop), want, "white drop {drop}");
        }
    }

    #[test]
    fn a_white_blunder_is_not_a_black_blunder() {
        // Black's win rate jumps 20 points: catastrophic for White, free for Black.
        let (before, after) = (0.40f32, 0.60f32);
        assert_eq!(
            blunder_severity(Color::White, before, after),
            Severity::Major
        );
        assert_eq!(
            blunder_severity(Color::Black, before, after),
            Severity::None
        );
    }

    #[test]
    fn a_gap_is_not_a_blunder_bar() {
        use mirai_core::{GameInfo, NodeAnalysis, RuleSet, Size};

        let size = Size::square(19);
        let mut tree = GameTree::new(GameInfo::new(size, RuleSet::Chinese));
        let mut ids = vec![tree.root()];
        for (c, gtp) in [
            (Color::Black, "D4"),
            (Color::White, "Q16"),
            (Color::Black, "Q4"),
        ] {
            let at = *ids.last().unwrap();
            ids.push(tree.play(at, c, size.from_gtp(gtp).unwrap()).unwrap());
        }

        let stored = |winrate: f32| NodeAnalysis {
            visits: 1000,
            winrate,
            score_lead: 0.0,
            score_stdev: 1.0,
            candidates: vec![],
            ownership: None,
        };
        // Move 1 at 80 %, move 3 at 40 %, move 2 empty. Spanning the gap would paint
        // move 3 as a 40-point drop that then vanishes when move 2 lands.
        tree.set_analysis(ids[1], Some(stored(0.80)));
        tree.set_analysis(ids[3], Some(stored(0.40)));
        let samples = collect_samples(&tree, &tree.main_line());
        assert_eq!(samples[2].blunder, Severity::None);
        assert_eq!(samples[3].blunder, Severity::None);

        tree.set_analysis(ids[2], Some(stored(0.41)));
        let samples = collect_samples(&tree, &tree.main_line());
        assert_eq!(samples[2].blunder, Severity::None);
        assert_eq!(samples[3].blunder, Severity::None);

        tree.set_analysis(ids[2], Some(stored(0.95)));
        let samples = collect_samples(&tree, &tree.main_line());
        assert_eq!(samples[2].blunder, Severity::Major);
        assert_eq!(samples[3].blunder, Severity::Major);
    }

    #[test]
    fn geometry_maps_axes_and_clicks() {
        let g = Geom::new(300.0, 140.0, 11, 70.0, 98.0);
        assert!(g.y_winrate(1.0) < g.y_winrate(0.0));
        assert!((g.y_winrate(0.5) - (g.top + g.bottom) * 0.5).abs() < 0.01);
        // The score axis is symmetric about the middle of the plot.
        assert!((g.y_lead(3.0, 10.0) + g.y_lead(-3.0, 10.0) - (g.top + g.bottom)).abs() < 0.01);
        assert_eq!(g.index_at(g.x(0) - 50.0, 11), 0);
        assert_eq!(g.index_at(g.x(5), 11), 5);
        assert_eq!(g.index_at(g.x(7) + 1.0, 11), 7);
        assert_eq!(g.index_at(g.x(10) + 50.0, 11), 10);
        // The same asymmetric plot geometry must pick its drawn first, middle and last node.
        let projection = GraphProjection {
            line: (0..11).map(NodeId).collect(),
            samples: vec![],
            cursor_index: 0,
        };
        for index in [0, 5, 10] {
            assert_eq!(
                node_at(&projection, &g, g.x(index)),
                Some(NodeId(index as u32))
            );
        }
        let g1 = Geom::new(300.0, 140.0, 1, 70.0, 98.0);
        assert_eq!(g1.index_at(123.0, 1), 0);
    }

    #[test]
    fn a_click_takes_an_owned_node_so_the_projection_can_be_rebuilt() {
        let projection = std::cell::RefCell::new(GraphProjection {
            line: vec![NodeId(1), NodeId(2), NodeId(3)],
            samples: vec![],
            cursor_index: 0,
        });
        let geom = Geom::new(300.0, 140.0, 3, 70.0, 98.0);
        let id = node_at(&projection.borrow(), &geom, geom.x(1)).expect("a node");
        *projection.borrow_mut() = GraphProjection::default();
        assert!(matches!(id, NodeId(1) | NodeId(2) | NodeId(3)));
        assert!(node_at(&projection.borrow(), &geom, geom.x(1)).is_none());
    }
}
