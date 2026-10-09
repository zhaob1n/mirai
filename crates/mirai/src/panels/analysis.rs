// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The Analysis sidebar page: a one-line search status, the candidate list and the blunder list.
//! Step 10 / Step 12.
//!
//! Candidate objects are updated in place at report rate, and a row a report has no move
//! for is blanked rather than removed; expression bindings preserve row hover and PV
//! selection without replacing the model. `Change::Cursor` clears that selection, as it
//! clears the board's pin: the row is about to name a different position's candidate.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, glib::clone};

use mirai_core::{Color, Point, Size, candidate_grade};

use crate::app::{AppState, EngineState};
use crate::batch::Blunder;
use crate::i18n;
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
        /// Which [`crate::palette`] level this move's loss is drawn at — the badge's colour,
        /// and the colour of its blob on the board. [`crate::palette::UNKNOWN_LEVEL`] when the
        /// search behind it is not enough for the loss to mean anything.
        #[property(get, set)]
        pub grade: Cell<u32>,
        /// Whether the row shows a candidate. A row the current report has no move for is
        /// kept blank rather than removed: see [`super::AnalysisPanel::refresh`].
        #[property(get, set)]
        pub filled: Cell<bool>,

        /// `(width, height, point)` last written into `mv`. `None` until the first
        /// apply: intersection 0 and [`Point::PASS`] are both real inputs, so neither
        /// can be the unset mark.
        pub mv_key: Cell<Option<(u8, u8, u16)>>,
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
    /// The first move of this row's PV — the candidate's move unless the engine sent a
    /// PV that starts elsewhere.
    pub fn pv_first(&self) -> Point {
        Point(self.imp().pv_first.get())
    }
}

/// The numbers one candidate contributes, already in the side-to-move's perspective.
struct Row {
    /// Position in KataGo's own ordering, 1-based — the badge's number.
    rank: u32,
    /// The [`crate::palette`] level this move's loss is drawn at — the badge's colour.
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
    /// cost nothing. The GTP string is built only when the point or the board size changed:
    /// `object.mv()` allocates a `GString`, and `to_gtp` allocates the coordinate.
    fn apply(&self, object: &CandidateObject, size: mirai_core::Size) {
        if object.rank() != self.rank {
            object.set_rank(self.rank);
        }
        if object.grade() != self.grade {
            object.set_grade(self.grade);
        }
        let key = (size.w, size.h, self.point.0);
        if object.imp().mv_key.get() != Some(key) {
            object.set_mv(size.to_gtp(self.point).as_str());
            object.imp().mv_key.set(Some(key));
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
        object.imp().pv_first.set(self.pv_first.0);
        // Last: a blank row's cells re-format on this one notify, already from the new
        // numbers, rather than once per number from the move it showed before.
        if !object.filled() {
            object.set_filled(true);
        }
    }
}

/// Grades every row by how much it loses against the first one — KataGo's pick — which is the
/// same reading the board gives that move's blob ([`crate::palette::colour_level`]). The pick
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
            true => candidate_grade::grade(row.loss),
            false => candidate_grade::grade_means(winrate - row.winrate, score - row.score),
        };
        row.grade = crate::palette::colour_level(g, i, row.visits);
    }
}

// -- the panel --------------------------------------------------------------------------

/// Widgets reached after construction. Each builder `name` is the harness id (`focus:`, cropped shots).
///
/// `pub` because it appears in a `pub` field of the `imp` struct, which the
/// `ObjectSubclass` impl makes publicly reachable; its own fields stay private.
pub struct Inner {
    status: gtk::Box,
    to_move: gtk::Label,
    detail: gtk::Label,
    start_actions: gtk::Box,
    analyse_game_button: gtk::Button,
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

    #[derive(Default)]
    pub struct AnalysisPanel {
        pub state: OnceCell<AppState>,
        pub inner: OnceCell<super::Inner>,
        pub pv_hooks: RefCell<Vec<PvHook>>,
        /// Keep row widgets alive across batch results so pointer hover and activation
        /// survive updates to the graph or to another blunder.
        pub blunder_rows: RefCell<Vec<(Blunder, adw::ActionRow, gtk::Label)>>,
        /// The selection index last reported to the PV hooks.
        pub last_pv: Cell<u32>,
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
        // The stone stays out of `detail` so dim-label does not fade it. Role img: AT reads
        // the accessible name, not the emoji. Win rate is the graph's; the pick is row one.
        let to_move = gtk::Label::builder()
            .name("to_move")
            .visible(false)
            .accessible_role(gtk::AccessibleRole::Img)
            .css_classes(["caption"])
            .build();
        let detail = gtk::Label::builder()
            .name("detail")
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["caption", "dim-label", "mirai-position"])
            .build();
        let status = gtk::Box::builder().name("status").spacing(4).build();
        status.append(&to_move);
        status.append(&detail);

        // Stacked: side by side the labels need more than the 300 sp sidebar.
        let analyse_position = gtk::Button::builder()
            .label(i18n::gettext("Analyse Position"))
            .action_name("win.toggle-analysis")
            .tooltip_text(i18n::gettext(
                "Analyse this position and each one you move to (Space)",
            ))
            .css_classes(["suggested-action"])
            .build();
        let analyse_game_button = gtk::Button::builder()
            .name("analyse_game_button")
            .label(i18n::gettext("Analyse Game"))
            .action_name("win.analyse-game")
            .tooltip_text(i18n::gettext(
                "Analyse every move of the main line (Ctrl+A)",
            ))
            .build();
        let start_actions = gtk::Box::builder()
            .name("start_actions")
            .visible(false)
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .build();
        start_actions.append(&analyse_position);
        start_actions.append(&analyse_game_button);

        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        header.append(&status);
        header.append(&start_actions);

        let columns = gtk::ColumnView::builder()
            .name("columns")
            .vexpand(true)
            .css_classes(["mirai-candidates"])
            .build();
        let columns_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&columns)
            .build();

        let blunder_list = gtk::ListBox::builder()
            .name("blunder_list")
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let blunder_scroll = gtk::ScrolledWindow::builder()
            .propagate_natural_height(true)
            .max_content_height(240)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&blunder_list)
            .build();
        let blunder_expander = gtk::Expander::builder()
            .name("blunder_expander")
            .label(i18n::gettext("Blunders"))
            .expanded(true)
            .css_classes(["mirai-blunders"])
            .child(&blunder_scroll)
            .build();
        let blunder_group = gtk::Box::builder()
            .name("blunder_group")
            .visible(false)
            .orientation(gtk::Orientation::Vertical)
            .build();
        blunder_group.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        blunder_group.append(&blunder_expander);

        self.append(&header);
        self.append(&columns_scroll);
        self.append(&blunder_group);

        let store = gio::ListStore::new::<CandidateObject>();
        // The user's sort sits between the store and the selection, so the store stays in
        // KataGo's `order` and everything that indexes into the engine's move list still can.
        // Blank rows sort last under any column, so moving the focus through the moves never
        // crosses one. They are only ever the store's tail, so with no column selected the
        // stable sort leaves the list in `order`.
        let filled_first = gtk::NumericSorter::new(Some(gtk::PropertyExpression::new(
            CandidateObject::static_type(),
            None::<gtk::Expression>,
            "filled",
        )));
        filled_first.set_sort_order(gtk::SortType::Descending);
        let sorter = gtk::MultiSorter::new();
        sorter.append(filled_first);
        if let Some(columns) = columns.sorter() {
            sorter.append(columns);
        }
        let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter));
        let selection = gtk::SingleSelection::builder()
            .model(&sorted)
            .autoselect(false)
            .can_unselect(true)
            .build();
        selection.set_selected(gtk::INVALID_LIST_POSITION);
        columns.set_model(Some(&selection));
        columns.set_row_factory(Some(&row_factory()));
        let rank_column = rank_column();
        columns.append_column(&rank_column);
        // The move itself is the one column with nothing to rank by, so its header is inert.
        columns.append_column(&column(
            Col {
                title: i18n::pgettext("column", "Move"),
                property: "mv",
                width_chars: 4,
                ..Col::default()
            },
            |mv: String| mv,
        ));
        columns.append_column(&column(
            Col {
                title: i18n::pgettext("column", "Win"),
                property: "winrate",
                width_chars: 6,
                sortable: true,
            },
            |winrate: f64| format!("{}%", pct1(winrate as f32)),
        ));
        columns.append_column(&column(
            Col {
                title: i18n::pgettext("column", "Score"),
                property: "score",
                width_chars: 6,
                sortable: true,
            },
            |score: f64| signed1(score as f32),
        ));
        // The loss against the pick is what the colour is made of. Raw utility is a reading of
        // the position rather than of the move, so it is not useful on its own here.
        let loss_column = column(
            Col {
                title: i18n::pgettext("column", "Loss"),
                property: "loss",
                width_chars: 5,
                sortable: true,
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
                title: i18n::pgettext("column", "Visits"),
                property: "visits",
                width_chars: 5,
                sortable: true,
            },
            |visits: u32| si_visits(visits),
        ));
        let prior_column = column(
            Col {
                title: i18n::pgettext("column", "Prior"),
                property: "prior",
                width_chars: 6,
                sortable: true,
            },
            |prior: f64| format!("{}%", pct1(prior as f32)),
        );
        prior_column.set_visible(false);
        columns.append_column(&prior_column);
        // Which columns to show is a question about the columns, so it is asked on their
        // headers (right-click) as well as in Main Menu → View.
        let section = gio::Menu::new();
        section.append(
            Some(&i18n::gettext("_Loss and Prior Columns")),
            Some("win.toggle-candidate-details"),
        );
        let column_menu = gio::Menu::new();
        column_menu.append_section(None, &section);
        for column in columns.columns().iter::<gtk::ColumnViewColumn>().flatten() {
            column.set_header_menu(Some(&column_menu));
        }
        // Turning live analysis on is what empties the panel of its start buttons.
        self.state().connect_live_analysis_notify(clone!(
            #[weak(rename_to = panel)]
            self,
            move |_| panel.refresh()
        ));

        // Selecting a row pins the PV preview; activating it plays the move. The model is
        // never replaced, so the selection changes only by the user's hand or when `refresh`
        // blanks the selected row.
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
                let Some(row) = selection
                    .item(position)
                    .and_downcast::<CandidateObject>()
                    .filter(CandidateObject::filled)
                else {
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
                    .map(|(blunder, ..)| blunder.node);
                if let Some(node) = node
                    && let Some(id) = panel.state().resolve_node(node)
                {
                    panel.state().set_cursor(id);
                }
            }
        ));

        let _ = self.imp().inner.set(Inner {
            status,
            to_move,
            detail,
            start_actions,
            analyse_game_button,
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
        // Between frames, at report rate: rows the view creates or destroys are created and
        // destroyed in here, synchronously, inside the store's `items-changed`.
        let _t = crate::render_probe::Timer::new("candidates");
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
                let spread = format!("{:.1}", h.stdev);
                let (name, tooltip) = match h.color {
                    Color::Black => (
                        i18n::gettext("Black to play"),
                        // Translators: {points} is how widely the predicted final score spreads, such as 3.2.
                        i18n::gettext_f(
                            "Black to play · final score spread ±{points} points",
                            &[("points", &spread)],
                        ),
                    ),
                    Color::White => (
                        i18n::gettext("White to play"),
                        // Translators: {points} is how widely the predicted final score spreads, such as 3.2.
                        i18n::gettext_f(
                            "White to play · final score spread ±{points} points",
                            &[("points", &spread)],
                        ),
                    ),
                };
                inner.to_move.set_label(stone(h.color));
                inner.to_move.set_visible(true);
                inner
                    .to_move
                    .update_property(&[gtk::accessible::Property::Label(&name)]);
                let visits = si_visits(h.visits);
                let detail = match h.speed {
                    Some(rate) => {
                        let speed = visits_per_second(rate);
                        // Translators: {visits} is an abbreviated count such as 1.2k; {speed} is a rate such as 840/s.
                        i18n::ngettext_f(
                            "{visits} visit · {speed}",
                            "{visits} visits · {speed}",
                            h.visits as u64,
                            &[("visits", &visits), ("speed", &speed)],
                        )
                    }
                    None => {
                        // Translators: {visits} is an abbreviated count such as 1.2k.
                        i18n::ngettext_f(
                            "{visits} visit",
                            "{visits} visits",
                            h.visits as u64,
                            &[("visits", &visits)],
                        )
                    }
                };
                inner.detail.set_label(&detail);
                inner.detail.set_visible(true);
                // KataGo's spread of the final score is easily read as the error of the
                // lead, so it stays out of the line and is there for whoever asks.
                inner.status.set_tooltip_text(Some(&tooltip));
                inner.status.set_visible(true);
                inner.start_actions.set_visible(false);
            }
            None => {
                inner.to_move.set_visible(false);
                inner.status.set_tooltip_text(None);
                let live = self.state().live_analysis();
                let engine = self.state().engine_state();
                // A whole-game sweep needs the engine itself, so it is offered once it is up.
                inner
                    .analyse_game_button
                    .set_visible(matches!(engine, EngineState::Ready { .. }));
                let (detail, offer) = match engine {
                    // A failed or missing engine says so even with live analysis on: it was
                    // likely turned on while the engine was still starting.
                    EngineState::Failed { message, .. } => (message, false),
                    EngineState::None => (i18n::gettext("No engine"), false),
                    _ if live => (i18n::gettext("Analysing…"), false),
                    // Live analysis may be asked for early: it starts once the net loads.
                    EngineState::Starting { profile } => (
                        i18n::gettext_f("Starting {profile}…", &[("profile", &profile)]),
                        true,
                    ),
                    // The buttons say it all.
                    EngineState::Ready { .. } => (String::new(), true),
                };
                inner.detail.set_visible(!detail.is_empty());
                inner.detail.set_label(&detail);
                inner.status.set_visible(!detail.is_empty());
                inner.start_actions.set_visible(offer);
            }
        }

        // Update the rows in place. `GtkColumnView` creates a row widget for every object the
        // model gains and destroys one for every object it loses, and either costs a list
        // re-layout — replacing the model per report measured 7–17 ms, ten times a second,
        // and made a hovered row flicker. So the objects outlive the moves they show, and a
        // row this report has no move for is blanked rather than removed: stepping through a
        // game under live analysis empties the list until the first report and refills it a
        // few moves a report, and that used to destroy and re-create the rows each time. A
        // blank row takes no space on screen ([`row_factory`]); only a lowered suggestion
        // limit drops rows.
        let store = &inner.store;
        let had = store.n_items() as usize;
        let keep = had.min(limit);
        if keep < had {
            store.splice(keep as u32, (had - keep) as u32, &[] as &[CandidateObject]);
        }
        for i in 0..keep {
            let object = store
                .item(i as u32)
                .and_downcast::<CandidateObject>()
                .expect("the candidate store holds only CandidateObjects");
            match rows.get(i) {
                Some(row) => row.apply(&object, size),
                None if object.filled() => object.set_filled(false),
                None => {}
            }
        }
        if rows.len() > keep {
            let extra: Vec<CandidateObject> =
                rows[keep..].iter().map(|r| r.to_object(size)).collect();
            store.extend_from_slice(&extra);
        }
        // A blank row must not stay selected: the PV it pins is the move it no longer shows.
        let selection = &inner.selection;
        if selection
            .selected_item()
            .and_downcast::<CandidateObject>()
            .is_some_and(|row| !row.filled())
        {
            selection.set_selected(gtk::INVALID_LIST_POSITION);
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

    /// Unselects the candidate. The dispatcher calls this on every `Change::Cursor`, the
    /// same event on which the board drops its pin; unselecting notifies `sync_pv`.
    pub(crate) fn clear_selection(&self) {
        let selection = &self.inner().selection;
        if selection.selected() != gtk::INVALID_LIST_POSITION {
            selection.set_selected(gtk::INVALID_LIST_POSITION);
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
            if let Some((previous, row, drop)) = shown.get_mut(index) {
                if *previous != blunder {
                    update_blunder_row(row, drop, blunder, size);
                    *previous = blunder;
                }
            } else {
                let row = adw::ActionRow::builder()
                    .activatable(true)
                    .title_lines(1)
                    .build();
                let drop = gtk::Label::builder().valign(gtk::Align::Center).build();
                row.add_suffix(&drop);
                update_blunder_row(&row, &drop, blunder, size);
                inner.blunder_list.append(&row);
                shown.push((blunder, row, drop));
            }
        }
        for (_, row, _) in shown.drain(new_len..) {
            inner.blunder_list.remove(&row);
        }
        if old_len != new_len {
            if new_len == 0 {
                inner.blunder_group.set_visible(false);
                inner
                    .blunder_expander
                    .set_label(Some(&i18n::gettext("Blunders")));
            } else {
                let count = new_len.to_string();
                let label = i18n::ngettext_f(
                    "Blunder ({count})",
                    "Blunders ({count})",
                    new_len as u64,
                    &[("count", &count)],
                );
                inner.blunder_expander.set_label(Some(&label));
                inner.blunder_group.set_visible(true);
            }
        }
    }
}

/// The title is the move in the list's ink; the loss is a badge in the colour the graph's
/// strip ticks that move with, the same pill the candidate list ranks in.
fn update_blunder_row(row: &adw::ActionRow, drop: &gtk::Label, b: Blunder, size: Size) {
    let played = size.to_gtp(b.played);
    let moves = match b.best {
        Some(best) => format!("{played} → {}", size.to_gtp(best)),
        None => played.to_string(),
    };
    let loss = format!("−{}%", pct1(b.drop));
    row.set_title(&format!("{}  {}  {moves}", stone(b.player), b.move_number));
    drop.set_label(&loss);
    let mut classes = vec!["mirai-rank", "mirai-drop"];
    let grade = severity_of_drop(b.drop)
        .ramp_stop()
        .map(|stop| crate::palette::grade_class(crate::palette::stop_level(stop)));
    if let Some(grade) = &grade {
        classes.push(grade);
    }
    drop.set_css_classes(&classes);
    // The emoji stone is a picture and the arrow a glyph; the accessible label says both.
    let number = b.move_number.to_string();
    let accessible = match (b.player, b.best) {
        (Color::Black, Some(best)) => {
            let best = size.to_gtp(best);
            // Translators: {loss} is a percentage such as −3.2%; {played} and {best} are coordinates such as D4.
            i18n::gettext_f(
                "Move {number} · Black · {loss} · played {played} · best {best}",
                &[
                    ("number", &number),
                    ("loss", &loss),
                    ("played", played.as_str()),
                    ("best", best.as_str()),
                ],
            )
        }
        (Color::White, Some(best)) => {
            let best = size.to_gtp(best);
            // Translators: {loss} is a percentage such as −3.2%; {played} and {best} are coordinates such as D4.
            i18n::gettext_f(
                "Move {number} · White · {loss} · played {played} · best {best}",
                &[
                    ("number", &number),
                    ("loss", &loss),
                    ("played", played.as_str()),
                    ("best", best.as_str()),
                ],
            )
        }
        (Color::Black, None) => {
            // Translators: {loss} is a percentage such as −3.2%; {played} is a coordinate such as D4.
            i18n::gettext_f(
                "Move {number} · Black · {loss} · played {played}",
                &[
                    ("number", &number),
                    ("loss", &loss),
                    ("played", played.as_str()),
                ],
            )
        }
        (Color::White, None) => {
            // Translators: {loss} is a percentage such as −3.2%; {played} is a coordinate such as D4.
            i18n::gettext_f(
                "Move {number} · White · {loss} · played {played}",
                &[
                    ("number", &number),
                    ("loss", &loss),
                    ("played", played.as_str()),
                ],
            )
        }
    };
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&accessible)]);
}

/// The root reading the status line needs. The root's win rate and lead are the graph's.
struct Headline {
    color: Color,
    visits: u32,
    stdev: f32,
    /// Visits per second, when a live search is producing the numbers.
    speed: Option<f32>,
}

/// The mover of a blunder, as the stone they played.
///
/// These two are `Emoji_Presentation=Yes`, so they keep their own black and white under
/// any foreground colour, dim-label included.
fn stone(color: Color) -> &'static str {
    match color {
        Color::Black => "⚫",
        Color::White => "⚪",
    }
}

/// One column of the candidate list: where its text comes from, and how wide it sits.
struct Col {
    title: String,
    /// The [`CandidateObject`] property the cell follows.
    property: &'static str,
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
            title: String::new(),
            property: "",
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
/// pointer loses its hover for a frame, ten times a second. A blank row hides the label
/// (see [`row_factory`]).
fn column<T>(col: Col, text: impl Fn(T) -> String + 'static) -> gtk::ColumnViewColumn
where
    T: for<'a> glib::value::FromValue<'a> + 'static,
{
    let text = Rc::new(text);
    let width_chars = col.width_chars;
    let property = col.property;
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
            .width_chars(width_chars)
            .max_width_chars(width_chars)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        item.set_child(Some(&label));
        let text = text.clone();
        let row = gtk::ListItem::this_expression("item");
        row.chain_property::<CandidateObject>(property)
            .chain_closure_with_callback(move |values| {
                let value = values[1]
                    .get::<T>()
                    .expect("a column's formatter takes its own property's type");
                text(value)
            })
            .bind(&label, "label", Some(item));
        row.chain_property::<CandidateObject>("filled")
            .bind(&label, "visible", Some(item));
    });
    let this = gtk::ColumnViewColumn::builder()
        .title(col.title)
        .factory(&factory)
        .expand(true)
        // All non-rank columns start at the same width; GTK divides the remaining space
        // equally among the visible columns, regardless of their labels or values.
        .fixed_width(0)
        .resizable(true)
        .build();
    // Dragging a header sets its fixed width. Stop expanding that column so the
    // requested size sticks; untouched columns still share the remaining space.
    this.connect_fixed_width_notify(|column| column.set_expand(column.fixed_width() == 0));
    if col.sortable {
        // One expression, two jobs: the header becomes a button with an arrow, and the
        // `GtkSortListModel` behind the view has something to order by.
        let expr = gtk::PropertyExpression::new(
            CandidateObject::static_type(),
            None::<gtk::Expression>,
            property,
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
/// A blank row hides the label, pill and all.
fn rank_column() -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        // One per row widget the view creates, so `frame-stats.py` counts them per step: a
        // list that only updates in place creates none after its first report.
        crate::render_probe::trace("candidate-row", 0.0);
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
        this.chain_property::<CandidateObject>("filled")
            .bind(&label, "visible", Some(item));
    });
    let this = gtk::ColumnViewColumn::builder()
        .title(i18n::pgettext("column", "#"))
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

/// Rows that a blank candidate leaves unselectable, inactivatable and unfocusable, so a click
/// or a key where it sits does nothing. Not being activatable drops the row's `.activatable`
/// class, which is what takes it out of the theme's hover highlight and what the stylesheet
/// keys on to fold a blank row to no height: its labels are hidden, and that leaves only the
/// cells' padding. A blank row therefore looks like no row at all — the list shows as many
/// moves as the report has, as it did when rows were removed — and lengthens no scroll.
fn row_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, row| {
        let Some(row) = row.downcast_ref::<gtk::ColumnViewRow>() else {
            return;
        };
        let filled =
            gtk::ColumnViewRow::this_expression("item").chain_property::<CandidateObject>("filled");
        for property in ["selectable", "activatable", "focusable"] {
            filled.bind(row, property, Some(row));
        }
    });
    factory
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(point: Point) -> Row {
        Row {
            rank: 1,
            grade: 0,
            point,
            pv_first: point,
            winrate: 0.5,
            score: 1.5,
            utility: None,
            loss: 0.0,
            visits: 20,
            prior: 0.1,
        }
    }

    /// The top-left intersection and [`Point::PASS`] are both valid moves: neither can
    /// stand for an object whose coordinate has not been set yet.
    #[test]
    fn a_fresh_row_is_not_already_showing_its_move() {
        let size = Size::square(19);
        let object = CandidateObject::default();
        assert!(object.mv().is_empty());

        row(size.point(0, 0)).apply(&object, size);
        assert_eq!(object.mv().as_str(), "A19");

        row(Point::PASS).apply(&object, size);
        assert_eq!(object.mv().as_str(), "pass");

        // The same index on a smaller board is a different coordinate. Objects are reused
        // across refreshes, so the size is part of the key.
        let nine = Size::square(9);
        row(nine.point(0, 0)).apply(&object, nine);
        assert_eq!(object.mv().as_str(), "A9");
    }
}
