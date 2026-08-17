// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `WinrateGraph` — win-rate curve, score-lead curve and blunder strip. Step 10.
//!
//! Everything is drawn from the cached [`mirai_core::NodeAnalysis`] on the nodes of the
//! current main line, so the graph is a pure function of the tree and costs nothing when
//! no analysis has been stored yet.

use std::cell::{Cell, RefCell};

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use mirai_client::batch::is_blunder_drop;
use mirai_core::{Color, NodeId};

use crate::app::AppState;
use crate::widgets::paint::{fill_disc, hline};

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
    pub(crate) fn color(self) -> Option<gdk::RGBA> {
        match self {
            Severity::None => None,
            Severity::Minor => Some(gdk::RGBA::new(0.910, 0.784, 0.220, 0.85)),
            Severity::Medium => Some(gdk::RGBA::new(0.910, 0.502, 0.173, 0.85)),
            Severity::Major => Some(gdk::RGBA::new(0.910, 0.314, 0.220, 0.9)),
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

/// The win rate seen by `color`, given the Black-perspective value mirai stores.
#[inline]
pub(crate) fn winrate_for(color: Color, black_winrate: f32) -> f32 {
    match color {
        Color::Black => black_winrate,
        Color::White => 1.0 - black_winrate,
    }
}

/// How much the player who played the move lost by playing it.
///
/// `before` is the Black-perspective win rate of the parent position, `after` that of the
/// position the move created. A White blunder shows up as a rise in Black's win rate, so
/// both are flipped into the mover's own perspective first.
pub(crate) fn blunder_severity(mover: Color, before: f32, after: f32) -> Severity {
    severity_of_drop(winrate_for(mover, before) - winrate_for(mover, after))
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

const PAD_L: f32 = 30.0;
const PAD_R: f32 = 34.0;
const PAD_T: f32 = 7.0;
const PAD_B: f32 = 3.0;
const STRIP_H: f32 = 10.0;
const STRIP_GAP: f32 = 2.0;
const NAT_HEIGHT: i32 = 140;

impl Geom {
    fn new(width: f32, height: f32, samples: usize) -> Geom {
        let strip_bottom = (height - PAD_B).max(PAD_T + 1.0);
        let strip_top = (strip_bottom - STRIP_H).max(PAD_T + 1.0);
        let bottom = (strip_top - STRIP_GAP).max(PAD_T + 1.0);
        let right = (width - PAD_R).max(PAD_L + 1.0);
        let span = right - PAD_L;
        let step = if samples > 1 {
            span / (samples - 1) as f32
        } else {
            span
        };
        Geom {
            left: PAD_L,
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
fn node_at(projection: &GraphProjection, width: f32, height: f32, x: f32) -> Option<NodeId> {
    if projection.line.is_empty() {
        return None;
    }
    let geom = Geom::new(width, height, projection.line.len());
    Some(projection.line[geom.index_at(x, projection.line.len())])
}

fn with_alpha(c: gdk::RGBA, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), a)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct WinrateGraph {
        pub window: glib::WeakRef<crate::window_shell::MiraiWindow>,
        pub pressed: Cell<bool>,
        pub(super) projection: RefCell<GraphProjection>,
        pub(super) render_cache: RefCell<Option<(RenderKey, gsk::RenderNode)>>,
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
                _ => (120, 240, -1, -1),
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
    pub fn new(window: &crate::window_shell::MiraiWindow) -> WinrateGraph {
        let this: WinrateGraph = glib::Object::new();
        this.imp().window.set(Some(window));
        this.add_css_class("mirai-winrate");
        this.set_hexpand(true);
        this.set_tooltip_text(Some(
            "Win rate (solid) and score lead (dashed) over the main line",
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

    pub fn state(&self) -> AppState {
        self.imp()
            .window
            .upgrade()
            .and_then(|window| window.with_ui(|ui| ui.state.clone()))
            .expect("WinrateGraph has no live window state")
    }
    pub(crate) fn refresh(&self) {
        let state = self.state();
        *self.imp().projection.borrow_mut() = self.samples(&state);
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
        let Some(id) = node_at(
            &self.imp().projection.borrow(),
            self.width() as f32,
            self.height() as f32,
            x,
        ) else {
            return;
        };
        // INV-10: `set_cursor` emits Cursor and Report, both of which rebuild
        // this projection. `id` is a copy — the RefCell is not borrowed.
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

        let mut samples = Vec::with_capacity(line.len());
        let mut prev_winrate: Option<f32> = None;
        for &id in &line {
            let node = tree.node(id);
            let analysis = node.analysis.as_ref();
            let winrate = analysis.map(|a| a.winrate);
            let lead = analysis.map(|a| a.score_lead);
            let blunder = match (node.mv, prev_winrate, winrate) {
                (Some((mover, _)), Some(before), Some(after)) => {
                    blunder_severity(mover, before, after)
                }
                _ => Severity::None,
            };
            samples.push(Sample {
                winrate,
                lead,
                blunder,
            });
            if winrate.is_some() {
                prev_winrate = winrate;
            }
        }
        GraphProjection {
            line,
            samples,
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
        let projection = self.imp().projection.borrow();
        let samples = &projection.samples;
        let geom = Geom::new(width, height, samples.len().max(1));

        let fg = self.color();
        let axis = with_alpha(fg, 0.16);
        let faint = with_alpha(fg, 0.35);
        let curve = with_alpha(fg, 0.92);
        let accent = adw::StyleManager::default()
            .accent_color()
            .to_standalone_rgba(adw::StyleManager::default().is_dark());

        // Plot background, so the graph reads as a panel rather than floating ink.
        snapshot.append_color(
            &with_alpha(fg, 0.05),
            &graphene::Rect::new(
                geom.left,
                geom.top,
                geom.right - geom.left,
                geom.bottom - geom.top,
            ),
        );

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
        snapshot.append_stroke(&half.to_path(), &half_stroke, &with_alpha(fg, 0.45));

        // The score axis range: symmetric, never tighter than ±5 points.
        let max_lead = samples
            .iter()
            .filter_map(|s| s.lead)
            .fold(0.0f32, |m, v| m.max(v.abs()));
        let range = max_lead.ceil().max(5.0);

        // Axis labels.
        let font = pango::FontDescription::from_string("Sans 7");
        self.label(snapshot, &font, "100", 2.0, geom.top - 1.0, &faint);
        self.label(snapshot, &font, "50", 2.0, y50 - 6.0, &faint);
        self.label(snapshot, &font, "0", 2.0, geom.bottom - 11.0, &faint);
        let hi = format!("+{}", range as i32);
        let lo = format!("-{}", range as i32);
        self.label(
            snapshot,
            &font,
            &hi,
            geom.right + 4.0,
            geom.top - 1.0,
            &with_alpha(accent, 0.75),
        );
        self.label(
            snapshot,
            &font,
            &lo,
            geom.right + 4.0,
            geom.bottom - 11.0,
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
        let geom = Geom::new(width, height, samples.len());
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
            let font = pango::FontDescription::from_string("Sans 7");
            let text = format!("{:.1}%", winrate * 100.0);
            let tx = (x + 5.0).min(geom.right - 28.0);
            self.label(snapshot, &font, &text, tx, geom.top + 1.0, &curve);
        }
    }

    fn label(
        &self,
        snapshot: &gtk::Snapshot,
        font: &pango::FontDescription,
        text: &str,
        x: f32,
        y: f32,
        color: &gdk::RGBA,
    ) {
        let layout = self.create_pango_layout(Some(text));
        layout.set_font_description(Some(font));
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x, y));
        snapshot.append_layout(&layout, color);
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
    fn geometry_maps_axes_and_clicks() {
        let g = Geom::new(300.0, 140.0, 11);
        assert!(g.y_winrate(1.0) < g.y_winrate(0.0));
        assert!((g.y_winrate(0.5) - (g.top + g.bottom) * 0.5).abs() < 0.01);
        // The score axis is symmetric about the middle of the plot.
        assert!((g.y_lead(3.0, 10.0) + g.y_lead(-3.0, 10.0) - (g.top + g.bottom)).abs() < 0.01);
        assert_eq!(g.index_at(g.x(0) - 50.0, 11), 0);
        assert_eq!(g.index_at(g.x(7) + 1.0, 11), 7);
        assert_eq!(g.index_at(g.x(10) + 50.0, 11), 10);
        // A single sample must not divide by zero.
        let g1 = Geom::new(300.0, 140.0, 1);
        assert_eq!(g1.index_at(123.0, 1), 0);
    }

    #[test]
    fn a_click_takes_an_owned_node_so_the_projection_can_be_rebuilt() {
        let projection = std::cell::RefCell::new(GraphProjection {
            line: vec![NodeId(1), NodeId(2), NodeId(3)],
            samples: vec![],
            cursor_index: 0,
        });
        let id = node_at(&projection.borrow(), 300.0, 140.0, 150.0).expect("a node");
        *projection.borrow_mut() = GraphProjection::default();
        assert!(matches!(id, NodeId(1) | NodeId(2) | NodeId(3)));
        assert!(node_at(&projection.borrow(), 300.0, 140.0, 150.0).is_none());
    }
}
