// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The Analysis sidebar page: the root readout, the candidate list and the blunder list.
//! Step 10 / Step 12.
//!
//! Candidate objects are updated in place at report rate; expression bindings preserve row
//! hover and PV selection without replacing the model.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;
use gtk::subclass::prelude::*;
use gtk::{CompositeTemplate, gio, glib, glib::clone};

use mirai_core::{Color, Point, Size};

use crate::app::{AppState, EngineState};
use crate::batch::Blunder;
use crate::util::{pct1, si_visits, signed1, visits_per_second};
use crate::widgets::winrate::severity_of_drop;

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
        /// How much this move loses against the pick, in KataGo utility — the number the
        /// colour is made of. Infinite when the record predates MRAI v2 and has no utility,
        /// which is why the pspec's range has to be widened: GLib validates a double against
        /// its bounds, and the default upper bound is `f64::MAX`, so an infinity is rejected.
        #[property(get, set, minimum = 0.0, maximum = f64::INFINITY)]
        pub loss: Cell<f64>,
        /// KataGo's own rank for this move, 1-based — the number in the badge.
        #[property(get, set)]
        pub rank: Cell<u32>,
        /// Which [`crate::palette`] stop this move's loss falls on — the badge's colour, and
        /// the colour of its blob on the board. [`crate::palette::UNKNOWN_STOP`] when the
        /// search behind it is not enough for the loss to mean anything.
        #[property(get, set)]
        pub grade: Cell<u32>,

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
    /// Position in KataGo's own ordering, 1-based — the badge's number.
    rank: u32,
    /// The nearest [`crate::palette`] stop to this move's loss — the badge's colour.
    grade: u32,
    point: Point,
    pv_first: Point,
    winrate: f32,
    score: f32,
    /// Side-to-move KataGo utility. `None` on MRAI v1 cache, which falls back to the means.
    utility: Option<f32>,
    /// Utility lost against the pick; `f32::INFINITY` when this row has no utility.
    loss: f32,
    visits: u32,
    prior: f32,
}

impl Row {
    fn to_object(&self, size: mirai_core::Size) -> CandidateObject {
        let object = CandidateObject::default();
        self.apply(&object, size);
        object
    }

    /// Writes this row's numbers onto an existing object, touching only what moved.
    ///
    /// The derive-generated setters notify unconditionally, and every notify re-evaluates a
    /// column expression and re-measures a label; a report that leaves a value alone should
    /// cost nothing. `mv` and `pv` are compared before the string is even built.
    fn apply(&self, object: &CandidateObject, size: mirai_core::Size) {
        if object.rank() != self.rank {
            object.set_rank(self.rank);
        }
        if object.grade() != self.grade {
            object.set_grade(self.grade);
        }
        let mv = size.to_gtp(self.point);
        if object.mv() != mv.as_str() {
            object.set_mv(mv.as_str());
        }
        if object.winrate() != self.winrate as f64 {
            object.set_winrate(self.winrate as f64);
        }
        if object.score() != self.score as f64 {
            object.set_score(self.score as f64);
        }
        if object.visits() != self.visits {
            object.set_visits(self.visits);
        }
        if object.prior() != self.prior as f64 {
            object.set_prior(self.prior as f64);
        }
        if object.loss() != self.loss as f64 {
            object.set_loss(self.loss as f64);
        }
        object.imp().point.set(self.point.0);
        object.imp().pv_first.set(self.pv_first.0);
    }
}

/// Grades every row by how much it loses against the first one — KataGo's pick — which is the
/// same reading the board gives that move's blob ([`crate::palette::colour_stop`]). The pick
/// itself is never unknown grey.
fn grade_rows(rows: &mut [Row]) {
    let Some(pick) = rows.first() else { return };
    let (winrate, score, utility) = (pick.winrate, pick.score, pick.utility);
    for (i, row) in rows.iter_mut().enumerate() {
        row.loss = match (utility, row.utility) {
            (Some(p), Some(c)) => (p - c).max(0.0),
            _ => f32::INFINITY,
        };
        let g = match row.loss.is_finite() {
            true => crate::palette::grade(row.loss),
            false => crate::palette::grade_means(winrate - row.winrate, score - row.score),
        };
        row.grade = crate::palette::colour_stop(g, i, row.visits);
    }
}

// -- the panel --------------------------------------------------------------------------

/// The widgets the panel has to reach back into after construction.
///
/// `pub` because it appears in a `pub` field of the `imp` struct, which the
/// `ObjectSubclass` impl makes publicly reachable; its own fields stay private.
pub struct Inner {
    readout: gtk::Label,
    metrics: gtk::Label,
    detail: gtk::Label,
    columns: gtk::ColumnView,
    rank_column: gtk::ColumnViewColumn,
    loss_column: gtk::ColumnViewColumn,
    prior_column: gtk::ColumnViewColumn,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    blunder_group: gtk::Box,
    blunder_expander: gtk::Expander,
    blunder_list: gtk::ListBox,
}

/// A callback fired with the selected candidate's index, or `None` when nothing is
/// selected, so the board can follow the pinned PV.
pub(crate) type PvHook = Box<dyn Fn(Option<usize>)>;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/panels/analysis.blp")]
    pub struct AnalysisPanel {
        #[template_child]
        pub readout: TemplateChild<gtk::Label>,
        #[template_child]
        pub metrics: TemplateChild<gtk::Label>,
        #[template_child]
        pub detail: TemplateChild<gtk::Label>,
        #[template_child]
        pub columns: TemplateChild<gtk::ColumnView>,
        #[template_child]
        pub blunder_group: TemplateChild<gtk::Box>,
        #[template_child]
        pub blunder_expander: TemplateChild<gtk::Expander>,
        #[template_child]
        pub blunder_list: TemplateChild<gtk::ListBox>,
        pub state: OnceCell<AppState>,
        pub inner: OnceCell<super::Inner>,
        pub pv_hooks: RefCell<Vec<PvHook>>,
        /// Keep row widgets alive across batch results so pointer hover and activation
        /// survive updates to the graph or to another blunder.
        pub blunder_rows: RefCell<Vec<(Blunder, adw::ActionRow)>>,
        /// The selection index last reported to the PV hooks.
        pub last_pv: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AnalysisPanel {
        const NAME: &'static str = "MiraiAnalysisPanel";
        type Type = super::AnalysisPanel;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
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
        let this: AnalysisPanel = glib::Object::new();
        this.imp()
            .state
            .set(state.clone())
            .expect("a fresh AnalysisPanel cannot already hold a state");
        this.imp().last_pv.set(gtk::INVALID_LIST_POSITION);
        this.build();
        this
    }

    /// The window state this panel reads, held directly rather than fetched back through the
    /// window on every report.
    pub fn state(&self) -> &AppState {
        self.imp()
            .state
            .get()
            .expect("AnalysisPanel was built without a state")
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
        let imp = self.imp();
        let readout = imp.readout.get();
        let metrics = imp.metrics.get();
        let detail = imp.detail.get();
        let columns = imp.columns.get();
        let blunder_group = imp.blunder_group.get();
        let blunder_expander = imp.blunder_expander.get();
        let blunder_list = imp.blunder_list.get();

        let store = gio::ListStore::new::<CandidateObject>();
        // The user's sort sits between the store and the selection, so the store stays in
        // KataGo's `order` and everything that indexes into the engine's move list still can.
        // With no column selected the sort model is a pass-through and the list is `order`.
        let sorted = gtk::SortListModel::new(Some(store.clone()), columns.sorter());
        let selection = gtk::SingleSelection::builder()
            .model(&sorted)
            .autoselect(false)
            .can_unselect(true)
            .build();
        selection.set_selected(gtk::INVALID_LIST_POSITION);
        columns.set_model(Some(&selection));
        let rank_column = rank_column();
        columns.append_column(&rank_column);
        // The move itself is the one column with nothing to rank by, so its header is inert.
        columns.append_column(&column(
            Col {
                title: "Move",
                property: "mv",
                width_chars: 4,
                ..Col::default()
            },
            |mv: String| mv,
        ));
        columns.append_column(&column(
            Col {
                title: "Win",
                property: "winrate",
                width_chars: 6,
                sortable: true,
                ..Col::default()
            },
            |winrate: f64| format!("{}%", pct1(winrate as f32)),
        ));
        columns.append_column(&column(
            Col {
                title: "Score",
                property: "score",
                width_chars: 6,
                sortable: true,
                ..Col::default()
            },
            |score: f64| signed1(score as f32),
        ));
        // The loss against the pick is what the colour is made of. Raw utility is a reading of
        // the position rather than of the move, so it is not useful on its own here.
        let loss_column = column(
            Col {
                title: "Loss",
                property: "loss",
                width_chars: 5,
                sortable: true,
                ..Col::default()
            },
            |loss: f64| {
                if loss.is_finite() {
                    format!("{loss:.2}")
                } else {
                    "—".to_string()
                }
            },
        );
        loss_column.set_visible(false);
        columns.append_column(&loss_column);
        columns.append_column(&column(
            Col {
                title: "Visits",
                property: "visits",
                expand: false,
                width_chars: 5,
                sortable: true,
            },
            |visits: u32| si_visits(visits),
        ));
        let prior_column = column(
            Col {
                title: "Prior",
                property: "prior",
                width_chars: 6,
                expand: false,
                sortable: true,
            },
            |prior: f64| format!("{}%", pct1(prior as f32)),
        );
        prior_column.set_visible(false);
        columns.append_column(&prior_column);

        // Selecting a row pins the PV preview; activating it plays the move. The model is
        // never replaced, so a selection change is always the user's.
        selection.connect_selected_notify(clone!(
            #[weak(rename_to = panel)]
            self,
            move |_| panel.sync_pv()
        ));
        columns.connect_activate(clone!(
            #[weak(rename_to = panel)]
            self,
            #[weak]
            selection,
            move |_, position| {
                // The position is the view's, which is the sorted one.
                let Some(row) = selection.item(position).and_downcast::<CandidateObject>() else {
                    return;
                };
                panel.play(row.pv_first());
            }
        ));
        blunder_list.connect_row_activated(clone!(
            #[weak(rename_to = panel)]
            self,
            move |_, row| {
                let node = panel
                    .imp()
                    .blunder_rows
                    .borrow()
                    .get(row.index().max(0) as usize)
                    .map(|(blunder, _)| blunder.node);
                if let Some(node) = node
                    && let Some(id) = panel.state().resolve_node(node)
                {
                    panel.state().set_cursor(id);
                }
            }
        ));

        let _ = self.imp().inner.set(Inner {
            readout,
            metrics,
            detail,
            columns,
            rank_column,
            loss_column,
            prior_column,
            store,
            selection,
            blunder_group,
            blunder_expander,
            blunder_list,
        });
    }

    pub(crate) fn set_detailed_columns(&self, detailed: bool) {
        let inner = self.inner();
        if !detailed
            && let Some(sorter) = inner
                .columns
                .sorter()
                .and_downcast::<gtk::ColumnViewSorter>()
            && sorter
                .primary_sort_column()
                .is_some_and(|column| column == inner.loss_column || column == inner.prior_column)
        {
            inner
                .columns
                .sort_by_column(Some(&inner.rank_column), gtk::SortType::Ascending);
        }
        inner.loss_column.set_visible(detailed);
        inner.prior_column.set_visible(detailed);
    }

    fn play(&self, p: Point) {
        let point = u32::from(p.0);
        let _ = self.activate_action("win.play-at", Some(&glib::Variant::from(point)));
    }

    // -- refreshing ---------------------------------------------------------------------

    pub(crate) fn refresh(&self) {
        let state = self.state();
        let size = state.tree().info.size;
        let to_play = state.to_play();
        let limit = state.config().analysis.suggestion_limit();

        let (headline, mut rows) = match state.last_report() {
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
                // `Report::moves` is already sorted by KataGo's `order`, so the position in this
                // list *is* the rank, and `moves[0]` is the pick every loss is measured against.
                let rows = report
                    .moves
                    .iter()
                    .take(limit)
                    .enumerate()
                    .map(|(i, m)| Row {
                        rank: i as u32 + 1,
                        grade: 0,
                        point: m.mv,
                        pv_first: m.pv.first().copied().unwrap_or(m.mv),
                        winrate: m.winrate_for(mover),
                        score: m.score_lead_for(mover),
                        utility: Some(m.utility_for(mover)),
                        loss: f32::INFINITY,
                        visits: m.visits,
                        prior: m.prior_f32(),
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
                            winrate: to_play.winrate_for(a.winrate),
                            score: a.score_lead * to_play.sign(),
                            stdev: a.score_stdev,
                            speed: None,
                        };
                        let rows = a
                            .candidates
                            .iter()
                            .take(limit)
                            .enumerate()
                            .map(|(i, c)| Row {
                                rank: i as u32 + 1,
                                grade: 0,
                                point: c.mv,
                                pv_first: c.pv.first().copied().unwrap_or(c.mv),
                                winrate: to_play.winrate_for(c.winrate),
                                score: c.score_lead * to_play.sign(),
                                utility: c.utility.map(|u| to_play.utility_for(u)),
                                loss: f32::INFINITY,
                                visits: c.visits,
                                prior: c.prior,
                            })
                            .collect::<Vec<_>>();
                        (Some(head), rows)
                    }
                    None => (None, Vec::new()),
                }
            }
        };
        grade_rows(&mut rows);

        let inner = self.inner();
        match headline {
            Some(h) => {
                inner
                    .readout
                    .set_label(&format!("{} to Play", h.color.name()));
                inner.metrics.set_label(&format!(
                    "{}% · {} points",
                    pct1(h.winrate),
                    signed1(h.score),
                ));
                inner.metrics.set_visible(true);
                let speed = match h.speed {
                    Some(rate) => format!(" · {}", visits_per_second(rate)),
                    None => String::new(),
                };
                inner.detail.set_label(&format!(
                    "{} visits{speed} · ±{:.1} points",
                    si_visits(h.visits),
                    h.stdev,
                ));
            }
            None => {
                inner.readout.set_label("No Analysis");
                inner.metrics.set_visible(false);
                let detail = match self.state().engine_state() {
                    EngineState::Starting { profile } => format!("Starting {profile}…"),
                    EngineState::Failed { message, .. } => message,
                    EngineState::Ready { .. } => {
                        "Turn on live analysis, or analyse the whole game".to_string()
                    }
                    EngineState::None => "No engine".to_string(),
                };
                inner.detail.set_label(&detail);
            }
        }

        // Update the rows in place. `GtkColumnView` re-creates every row widget when the model
        // hands it different objects, which costs a full list re-layout — measured at 7–17 ms
        // per report, ten times a second, and it is also what made a hovered row flicker. The
        // objects therefore live as long as the list is that long: only the tail moves.
        let store = &inner.store;
        let had = store.n_items() as usize;
        for (i, row) in rows.iter().take(had).enumerate() {
            let object = store
                .item(i as u32)
                .and_downcast::<CandidateObject>()
                .expect("the candidate store holds only CandidateObjects");
            row.apply(&object, size);
        }
        if rows.len() > had {
            let extra: Vec<CandidateObject> =
                rows[had..].iter().map(|r| r.to_object(size)).collect();
            store.extend_from_slice(&extra);
        } else if rows.len() < had {
            store.splice(
                rows.len() as u32,
                (had - rows.len()) as u32,
                &[] as &[CandidateObject],
            );
        }
        self.resort();
        self.sync_pv();
    }

    /// Tells the sort model that the numbers under it moved.
    ///
    /// The rows are mutated in place, and a `GtkSortListModel` re-sorts when its sorter says
    /// so or when items are added and removed — never on a property it cannot know about. So
    /// the panel says so: the sorter being poked is the one this panel built for that column,
    /// which is what `gtk_sorter_changed` is for. Nothing happens while the list is in
    /// KataGo's `order`, which is the default and has no primary column.
    fn resort(&self) {
        let Some(column) = self
            .inner()
            .columns
            .sorter()
            .and_downcast::<gtk::ColumnViewSorter>()
            .and_then(|sorter| sorter.primary_sort_column())
        else {
            return;
        };
        if let Some(sorter) = column.sorter() {
            sorter.changed(gtk::SorterChange::Different);
        }
    }

    /// Reports the selected candidate to the PV hooks, if it changed.
    ///
    /// The hooks index into the engine's own move list, so this is the row's rank and not its
    /// position: under a user sort the two are different, and the board previews the move the
    /// user clicked either way.
    fn sync_pv(&self) {
        let index = self
            .inner()
            .selection
            .selected_item()
            .and_downcast::<CandidateObject>()
            .map(|row| row.rank().saturating_sub(1));
        let reported = index.unwrap_or(gtk::INVALID_LIST_POSITION);
        if self.imp().last_pv.replace(reported) == reported {
            return;
        }
        let index = index.map(|i| i as usize);
        for hook in self.imp().pv_hooks.borrow().iter() {
            hook(index);
        }
    }

    // -- blunders -----------------------------------------------------------------------

    /// Syncs the stored main-line blunders without replacing rows on each batch result.
    pub fn set_blunders(&self, rows: Vec<Blunder>) {
        let inner = self.inner();
        let size = self.state().tree().info.size;
        let mut shown = self.imp().blunder_rows.borrow_mut();
        let old_len = shown.len();
        let new_len = rows.len();
        for (index, blunder) in rows.into_iter().enumerate() {
            if let Some((previous, row)) = shown.get_mut(index) {
                if *previous != blunder {
                    update_blunder_row(row, blunder, size);
                    *previous = blunder;
                }
            } else {
                let row = adw::ActionRow::builder()
                    .activatable(true)
                    .title_lines(1)
                    .build();
                update_blunder_row(&row, blunder, size);
                inner.blunder_list.append(&row);
                shown.push((blunder, row));
            }
        }
        for (_, row) in shown.drain(new_len..) {
            inner.blunder_list.remove(&row);
        }
        if old_len != new_len {
            if new_len == 0 {
                inner.blunder_group.set_visible(false);
                inner.blunder_expander.set_label(Some("Blunders"));
            } else {
                inner
                    .blunder_expander
                    .set_label(Some(&format!("Blunders ({new_len})")));
                inner.blunder_group.set_visible(true);
            }
        }
    }
}

fn update_blunder_row(row: &adw::ActionRow, b: Blunder, size: Size) {
    let detail = match b.best {
        Some(best) => format!(
            "−{}% · played {} · best {}",
            pct1(b.drop),
            size.to_gtp(b.played),
            size.to_gtp(best)
        ),
        None => format!("−{}% · played {}", pct1(b.drop), size.to_gtp(b.played)),
    };
    row.set_title(&format!("{} {} · {detail}", stone(b.player), b.move_number));
    for class in [
        "mirai-blunder-minor",
        "mirai-blunder-medium",
        "mirai-blunder-major",
    ] {
        row.remove_css_class(class);
    }
    if let Some(class) = severity_class(severity_of_drop(b.drop)) {
        row.add_css_class(class);
    }
    // The emoji stone is a picture; the accessible label must name the player.
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&format!(
            "Move {} · {} · {detail}",
            b.move_number,
            b.player.name()
        ))]);
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

/// The mover of a blunder, as the stone they played.
///
/// These two are `Emoji_Presentation=Yes`, so they keep their own black and white under
/// the row's severity foreground colour.
fn stone(color: Color) -> &'static str {
    match color {
        Color::Black => "⚫",
        Color::White => "⚪",
    }
}

fn severity_class(severity: crate::widgets::winrate::Severity) -> Option<&'static str> {
    use crate::widgets::winrate::Severity;
    match severity {
        Severity::None => None,
        Severity::Minor => Some("mirai-blunder-minor"),
        Severity::Medium => Some("mirai-blunder-medium"),
        Severity::Major => Some("mirai-blunder-major"),
    }
}

/// One column of the candidate list: where its text comes from, and how wide it sits.
#[derive(Clone, Copy)]
struct Col {
    title: &'static str,
    /// The [`CandidateObject`] property the cell follows.
    property: &'static str,
    /// Keep the trailing numeric columns compact while preceding columns share spare width.
    expand: bool,
    /// Natural width in characters, GTK's own `-1` for "as wide as the text".
    ///
    /// Pinning it is what keeps a column from re-measuring when `1.4k` becomes `12.7k`: the
    /// cell's size request stops depending on the digits, so ten reports a second cost the
    /// list nothing but new glyphs. It is in characters rather than pixels so it still tracks
    /// the font.
    width_chars: i32,
    /// Whether the header sorts the list. Every column but the move itself has something to
    /// rank by.
    sortable: bool,
}

impl Default for Col {
    fn default() -> Col {
        Col {
            title: "",
            property: "",
            expand: true,
            width_chars: -1,
            sortable: false,
        }
    }
}

/// A single-label column whose text follows one property of the row object.
///
/// The label is bound once, at `setup`, through a `gtk::Expression` chain: the list item's
/// `item` property, then `property` on the row object, then a formatter. GTK re-evaluates that
/// chain on `notify`, so a row that changes its numbers **in place** updates its labels and
/// nothing else. Formatting from a `bind` handler instead would only be re-run when the model
/// handed the view a different object — which is what used to force a splice per report, and a
/// splice re-creates every row widget: a full re-layout of the list, and the row under the
/// pointer loses its hover for a frame, ten times a second.
fn column<T>(col: Col, text: impl Fn(T) -> String + 'static) -> gtk::ColumnViewColumn
where
    T: for<'a> glib::value::FromValue<'a> + 'static,
{
    let text = Rc::new(text);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            // Both bounds, not just the minimum: a cell whose *natural* width still tracked
            // its digits would go on re-measuring the column.
            .width_chars(col.width_chars)
            .max_width_chars(col.width_chars)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        item.set_child(Some(&label));
        let text = text.clone();
        gtk::ListItem::this_expression("item")
            .chain_property::<CandidateObject>(col.property)
            .chain_closure_with_callback(move |values| {
                let value = values[1]
                    .get::<T>()
                    .expect("a column's formatter takes its own property's type");
                text(value)
            })
            .bind(&label, "label", Some(item));
    });
    let this = gtk::ColumnViewColumn::builder()
        .title(col.title)
        .factory(&factory)
        .expand(col.expand)
        .resizable(true)
        .build();
    if col.sortable {
        // One expression, two jobs: the header becomes a button with an arrow, and the
        // `GtkSortListModel` behind the view has something to order by.
        let expr = gtk::PropertyExpression::new(
            CandidateObject::static_type(),
            None::<gtk::Expression>,
            col.property,
        );
        this.set_sorter(Some(&gtk::NumericSorter::new(Some(expr))));
    }
    this
}

/// The rank badge: a pill carrying KataGo's own ranking, in the colour the board gives that
/// move's blob ([`crate::palette`]) — the number is the rank, the colour is what the move loses
/// (or unknown grey, when it has not been searched enough for that loss to mean anything).
///
/// Two bindings on two properties of the same label. `css-classes` is an ordinary widget
/// property, so the class that carries the colour rides the `notify::grade` GTK already watches —
/// no bind/unbind bookkeeping, and nothing to forget when a row is recycled onto another move.
fn rank_column() -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder()
            .width_chars(2)
            .single_line_mode(true)
            .build();
        item.set_child(Some(&label));
        let this = gtk::ListItem::this_expression("item");
        this.chain_property::<CandidateObject>("rank")
            .chain_closure_with_callback(|values| {
                let rank = values[1].get::<u32>().unwrap_or_default();
                if rank == 0 {
                    String::new()
                } else {
                    rank.to_string()
                }
            })
            .bind(&label, "label", Some(item));
        this.chain_property::<CandidateObject>("grade")
            .chain_closure_with_callback(|values| {
                let grade = values[1].get::<u32>().unwrap_or_default();
                glib::StrV::from(vec![
                    glib::GString::from("mirai-rank"),
                    glib::GString::from(crate::palette::grade_class(grade)),
                ])
            })
            .bind(&label, "css-classes", Some(item));
    });
    let this = gtk::ColumnViewColumn::builder()
        .title("#")
        .factory(&factory)
        .resizable(false)
        .build();
    // Sortable so that there is a way back: this column *is* KataGo's order, so clicking it
    // undoes whatever the user sorted by.
    let expr = gtk::PropertyExpression::new(
        CandidateObject::static_type(),
        None::<gtk::Expression>,
        "rank",
    );
    this.set_sorter(Some(&gtk::NumericSorter::new(Some(expr))));
    this
}
