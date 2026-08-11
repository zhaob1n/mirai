// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The Analysis sidebar page: the root readout, the candidate list and the blunder list.
//! Step 10 / Step 12.
//!
//! The candidate list is rebound at the report rate (10 Hz by default), so the model is
//! built once and spliced in place; only the six labels of the visible rows are touched
//! per report. The selected row index is preserved across a splice, which is what pins a
//! PV preview on the board while the engine keeps thinking.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use gtk::glib;
use gtk::pango;
use gtk::subclass::prelude::*;
use gtk::{gio, glib::clone};

use mirai_core::{Color, NodeId, Point};

use crate::app::{AppState, signal};
use crate::batch::Blunder;
use crate::util::{gtp, pct1, si_visits, signed1, visits_per_second};

// -- the row model ----------------------------------------------------------------------

mod candidate_imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::CandidateObject)]
    pub struct CandidateObject {
        /// The move in GTP notation, e.g. `Q16` or `pass`.
        #[property(get, set)]
        pub mv: RefCell<String>,
        /// Win rate for the player to move, `0.0..=1.0`.
        #[property(get, set)]
        pub winrate: Cell<f64>,
        /// Score lead for the player to move, in points.
        #[property(get, set)]
        pub score: Cell<f64>,
        #[property(get, set)]
        pub visits: Cell<u32>,
        /// Raw policy prior, `0.0..=1.0`.
        #[property(get, set)]
        pub prior: Cell<f64>,
        /// The principal variation, already rendered.
        #[property(get, set)]
        pub pv: RefCell<String>,

        /// Not a GObject property: the plain point behind `mv`.
        pub point: Cell<u16>,
        /// The first move of the PV, which is what activating the row plays.
        pub pv_first: Cell<u16>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CandidateObject {
        const NAME: &'static str = "MiraiCandidate";
        type Type = super::CandidateObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for CandidateObject {}
}

glib::wrapper! {
    /// One row of the candidate list.
    pub struct CandidateObject(ObjectSubclass<candidate_imp::CandidateObject>);
}

impl Default for CandidateObject {
    fn default() -> CandidateObject {
        glib::Object::new()
    }
}

impl CandidateObject {
    /// The move this row stands for.
    pub fn point(&self) -> Point {
        Point(self.imp().point.get())
    }

    /// The first move of this row's PV — the same point as [`CandidateObject::point`]
    /// unless the engine sent a PV that starts elsewhere.
    pub fn pv_first(&self) -> Point {
        Point(self.imp().pv_first.get())
    }
}

/// The numbers one candidate contributes, already in the side-to-move's perspective.
struct Row {
    point: Point,
    pv_first: Point,
    winrate: f32,
    score: f32,
    visits: u32,
    prior: f32,
    pv: String,
}

impl Row {
    fn into_object(self, size: mirai_core::Size) -> CandidateObject {
        let obj = CandidateObject::default();
        obj.set_mv(gtp(size, self.point));
        obj.set_winrate(self.winrate as f64);
        obj.set_score(self.score as f64);
        obj.set_visits(self.visits);
        obj.set_prior(self.prior as f64);
        obj.set_pv(self.pv);
        obj.imp().point.set(self.point.0);
        obj.imp().pv_first.set(self.pv_first.0);
        obj
    }
}

// -- the panel --------------------------------------------------------------------------

/// The widgets the panel has to reach back into after construction.
///
/// `pub` because it appears in a `pub` field of the `imp` struct, which the
/// `ObjectSubclass` impl makes publicly reachable; its own fields stay private.
pub struct Inner {
    readout: gtk::Label,
    detail: gtk::Label,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    blunder_expander: gtk::Expander,
    blunder_list: gtk::ListBox,
}

/// A callback fired with the selected candidate's index, or `None` when nothing is
/// selected, so the board can follow the pinned PV.
pub(crate) type PvHook = Box<dyn Fn(Option<usize>)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AnalysisPanel {
        pub state: RefCell<Option<AppState>>,
        pub inner: OnceCell<super::Inner>,
        pub pv_hooks: RefCell<Vec<PvHook>>,
        /// The nodes the blunder rows jump to, parallel to the list box's children.
        pub blunder_nodes: RefCell<Vec<NodeId>>,
        /// The selection index last reported to the PV hooks.
        pub last_pv: Cell<u32>,
        /// Set while the model is being spliced, so the selection churn a splice causes
        /// does not reach the hooks.
        pub splicing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AnalysisPanel {
        const NAME: &'static str = "MiraiAnalysisPanel";
        type Type = super::AnalysisPanel;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for AnalysisPanel {}
    impl WidgetImpl for AnalysisPanel {}
    impl BoxImpl for AnalysisPanel {}
}

glib::wrapper! {
    pub struct AnalysisPanel(ObjectSubclass<imp::AnalysisPanel>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AnalysisPanel {
    pub fn new(state: &AppState) -> AnalysisPanel {
        let this: AnalysisPanel = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .build();
        *this.imp().state.borrow_mut() = Some(state.clone());
        this.imp().last_pv.set(gtk::INVALID_LIST_POSITION);
        this.build();
        this.connect_state();
        this.refresh();
        this
    }

    pub fn state(&self) -> AppState {
        self.imp()
            .state
            .borrow()
            .clone()
            .expect("AnalysisPanel was built without an AppState")
    }

    fn inner(&self) -> &Inner {
        self.imp().inner.get().expect("panel was not built")
    }

    /// Called by the window to follow the pinned PV: the index of the selected candidate,
    /// or `None` when nothing is selected.
    pub fn connect_pv_preview(&self, f: impl Fn(Option<usize>) + 'static) {
        self.imp().pv_hooks.borrow_mut().push(Box::new(f));
    }

    // -- construction -------------------------------------------------------------------

    fn build(&self) {
        let readout = gtk::Label::builder()
            .xalign(0.0)
            .label("No analysis")
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        readout.add_css_class("mirai-readout");
        readout.add_css_class("heading");
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .label("")
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        detail.add_css_class("mirai-status-dim");

        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        header.append(&readout);
        header.append(&detail);
        self.append(&header);

        let store = gio::ListStore::new::<CandidateObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        selection.set_selected(gtk::INVALID_LIST_POSITION);

        let columns = gtk::ColumnView::builder()
            .model(&selection)
            .show_column_separators(false)
            .show_row_separators(false)
            .single_click_activate(false)
            .vexpand(true)
            .build();
        columns.append_column(&text_column("Move", 0.0, false, None, |c| c.mv()));
        columns.append_column(&text_column("Win", 1.0, false, None, |c| {
            format!("{}%", pct1(c.winrate() as f32))
        }));
        columns.append_column(&text_column("Score", 1.0, false, None, |c| {
            signed1(c.score() as f32)
        }));
        columns.append_column(&text_column("Visits", 1.0, false, None, |c| {
            si_visits(c.visits())
        }));
        columns.append_column(&text_column("Prior", 1.0, false, None, |c| {
            format!("{}%", pct1(c.prior() as f32))
        }));
        columns.append_column(&text_column(
            "PV",
            0.0,
            true,
            Some("mirai-pv-label"),
            |c| c.pv(),
        ));

        let scroller = gtk::ScrolledWindow::builder()
            .child(&columns)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();
        self.append(&scroller);

        // The blunder list, filled by a whole-game analysis and hidden until there is one.
        let blunder_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        blunder_list.add_css_class("boxed-list");
        let blunder_scroller = gtk::ScrolledWindow::builder()
            .child(&blunder_list)
            .propagate_natural_height(true)
            .max_content_height(240)
            .build();
        let blunder_expander = gtk::Expander::builder()
            .label("Blunders")
            .expanded(true)
            .visible(false)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(12)
            .child(&blunder_scroller)
            .build();
        self.append(&blunder_expander);

        // Selecting a row pins the PV preview; activating it plays the move.
        selection.connect_selected_notify(clone!(
            #[weak(rename_to = panel)]
            self,
            move |_| {
                if !panel.imp().splicing.get() {
                    panel.sync_pv();
                }
            }
        ));
        columns.connect_activate(clone!(
            #[weak(rename_to = panel)]
            self,
            #[weak]
            store,
            move |_, position| {
                let Some(row) = store.item(position).and_downcast::<CandidateObject>() else {
                    return;
                };
                panel.play(row.pv_first());
            }
        ));
        blunder_list.connect_row_activated(clone!(
            #[weak(rename_to = panel)]
            self,
            move |_, row| {
                let nodes = panel.imp().blunder_nodes.borrow();
                if let Some(&node) = nodes.get(row.index().max(0) as usize) {
                    drop(nodes);
                    panel.state().set_cursor(node);
                }
            }
        ));

        let _ = self.imp().inner.set(Inner {
            readout,
            detail,
            store,
            selection,
            blunder_expander,
            blunder_list,
        });
    }

    fn connect_state(&self) {
        let state = self.state();
        for name in [signal::REPORT, signal::CURSOR_CHANGED, signal::TREE_CHANGED] {
            state.connect_closure(
                name,
                false,
                glib::closure_local!(
                    #[weak(rename_to = panel)]
                    self,
                    move |_: AppState| panel.refresh()
                ),
            );
        }
    }

    fn play(&self, p: Point) {
        let state = self.state();
        let color = state.to_play();
        if let Err(e) = state.play_move(color, p) {
            state.toast(e.to_string());
        }
    }

    // -- refreshing ---------------------------------------------------------------------

    fn refresh(&self) {
        let state = self.state();
        let size = state.tree().info.size;
        let to_play = state.to_play();
        let max = state.config().analysis.max_suggestions as usize;

        let (headline, rows) = match state.last_report() {
            Some(report) => {
                let mover = report.root.current_player;
                let head = Headline {
                    color: mover,
                    visits: report.root.visits,
                    winrate: report.root.winrate_for(mover),
                    score: report.root.score_lead_for(mover),
                    stdev: report.root.score_stdev_f32(),
                    // Only a live search has a speed; the cached branch below never does.
                    speed: state.analysis_speed(),
                };
                let rows = report
                    .moves
                    .iter()
                    .take(max.max(1))
                    .map(|m| Row {
                        point: m.mv,
                        pv_first: m.pv.first().copied().unwrap_or(m.mv),
                        winrate: m.winrate_for(mover),
                        score: m.score_lead_for(mover),
                        visits: m.visits,
                        prior: m.prior_f32(),
                        pv: pv_text(size, &m.pv),
                    })
                    .collect::<Vec<_>>();
                (Some(head), rows)
            }
            None => {
                // No live report: fall back to whatever the cursor's node has cached, which
                // is stored from Black's perspective.
                let cursor = state.cursor();
                let tree = state.tree();
                match tree.node(cursor).analysis.as_ref() {
                    Some(a) => {
                        let head = Headline {
                            color: to_play,
                            visits: a.visits,
                            winrate: perspective(a.winrate, to_play),
                            score: a.score_lead * to_play.sign(),
                            stdev: a.score_stdev,
                            speed: None,
                        };
                        let rows = a
                            .candidates
                            .iter()
                            .take(max.max(1))
                            .map(|c| Row {
                                point: c.mv,
                                pv_first: c.pv.first().copied().unwrap_or(c.mv),
                                winrate: perspective(c.winrate, to_play),
                                score: c.score_lead * to_play.sign(),
                                visits: c.visits,
                                prior: c.prior,
                                pv: pv_text(size, &c.pv),
                            })
                            .collect::<Vec<_>>();
                        (Some(head), rows)
                    }
                    None => (None, Vec::new()),
                }
            }
        };

        let inner = self.inner();
        match headline {
            Some(h) => {
                let letter = h.color.katago();
                inner.readout.set_label(&format!(
                    "{letter} {}%   {letter}{}",
                    pct1(h.winrate),
                    signed1(h.score)
                ));
                let speed = match h.speed {
                    Some(rate) => format!(" · {}", visits_per_second(rate)),
                    None => String::new(),
                };
                inner.detail.set_label(&format!(
                    "{} visits{speed} · ±{:.1} points · {} to play",
                    si_visits(h.visits),
                    h.stdev,
                    color_name(h.color)
                ));
            }
            None => {
                inner.readout.set_label("No analysis");
                inner.detail.set_label(if self.state().engine().is_some() {
                    "Turn on live analysis, or analyse the whole game"
                } else {
                    "No engine"
                });
            }
        }

        let objects: Vec<CandidateObject> = rows.into_iter().map(|r| r.into_object(size)).collect();
        let previous = inner.selection.selected();
        self.imp().splicing.set(true);
        inner
            .store
            .splice(0, inner.store.n_items(), &objects);
        // A splice clears the selection; put it back so a pinned PV survives the next
        // report. The candidate at that rank may have changed, which is the intent — the
        // preview follows the rank, not the move.
        if previous != gtk::INVALID_LIST_POSITION && (previous as usize) < objects.len() {
            inner.selection.set_selected(previous);
        }
        self.imp().splicing.set(false);
        self.sync_pv();
    }

    /// Reports the current selection to the PV hooks, if it changed.
    fn sync_pv(&self) {
        let selected = self.inner().selection.selected();
        if self.imp().last_pv.replace(selected) == selected {
            return;
        }
        let index = (selected != gtk::INVALID_LIST_POSITION).then_some(selected as usize);
        for hook in self.imp().pv_hooks.borrow().iter() {
            hook(index);
        }
    }

    // -- blunders -----------------------------------------------------------------------

    /// Fills the blunder list from a completed whole-game analysis.
    pub fn set_blunders(&self, rows: Vec<Blunder>) {
        let inner = self.inner();
        clear_list(&inner.blunder_list);
        let size = self.state().tree().info.size;

        let mut nodes = Vec::with_capacity(rows.len());
        for b in &rows {
            let subtitle = match b.best {
                Some(best) => format!(
                    "−{}% · played {} · best {}",
                    pct1(b.drop),
                    gtp(size, b.played),
                    gtp(size, best)
                ),
                None => format!("−{}% · played {}", pct1(b.drop), gtp(size, b.played)),
            };
            let row = adw::ActionRow::builder()
                .title(format!("{} · {}", b.move_number, color_name(b.player)))
                .subtitle(subtitle)
                .activatable(true)
                .build();
            row.add_css_class(severity_class(b.drop));
            inner.blunder_list.append(&row);
            nodes.push(b.node);
        }
        *self.imp().blunder_nodes.borrow_mut() = nodes;

        inner
            .blunder_expander
            .set_label(Some(&format!("Blunders ({})", rows.len())));
        inner.blunder_expander.set_visible(!rows.is_empty());
    }

    pub fn clear_blunders(&self) {
        let inner = self.inner();
        clear_list(&inner.blunder_list);
        self.imp().blunder_nodes.borrow_mut().clear();
        inner.blunder_expander.set_label(Some("Blunders"));
        inner.blunder_expander.set_visible(false);
    }
}

/// The root numbers, already in the mover's perspective.
struct Headline {
    color: Color,
    visits: u32,
    winrate: f32,
    score: f32,
    stdev: f32,
    /// Visits per second, when a live search is producing the numbers.
    speed: Option<f32>,
}

fn perspective(black_value: f32, c: Color) -> f32 {
    match c {
        Color::Black => black_value,
        Color::White => 1.0 - black_value,
    }
}

fn color_name(c: Color) -> &'static str {
    match c {
        Color::Black => "Black",
        Color::White => "White",
    }
}

fn severity_class(drop: f32) -> &'static str {
    if drop >= 0.10 {
        "mirai-blunder-major"
    } else if drop >= 0.05 {
        "mirai-blunder-medium"
    } else {
        "mirai-blunder-minor"
    }
}

fn pv_text(size: mirai_core::Size, pv: &[Point]) -> String {
    let mut out = String::with_capacity(pv.len() * 5);
    for (i, &p) in pv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&size.to_gtp(p));
    }
    out
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

/// A single-label column whose text is derived from the row object.
fn text_column(
    title: &str,
    xalign: f32,
    expand: bool,
    css: Option<&'static str>,
    text: impl Fn(&CandidateObject) -> String + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder()
            .xalign(xalign)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        if let Some(class) = css {
            label.add_css_class(class);
        }
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(row), Some(label)) = (
            item.item().and_downcast::<CandidateObject>(),
            item.child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        label.set_label(&text(&row));
    });
    gtk::ColumnViewColumn::builder()
        .title(title)
        .factory(&factory)
        .expand(expand)
        .resizable(true)
        .build()
}
