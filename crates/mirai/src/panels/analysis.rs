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
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;
use gtk::subclass::prelude::*;
use gtk::{CompositeTemplate, gio, glib, glib::clone};

use mirai_core::{Color, Point};

use crate::app::{AppState, EngineState, NodeRef};
use crate::batch::Blunder;
use crate::util::{pct1, si_visits, signed1, visits_per_second};
use crate::widgets::winrate::{Severity, severity_of_drop};

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
    visits: u32,
    prior: f32,
    pv: String,
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
        if object.pv() != self.pv {
            object.set_pv(self.pv.as_str());
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
        let g = match (utility, row.utility) {
            (Some(p), Some(c)) => crate::palette::grade(p - c),
            _ => crate::palette::grade_means(winrate - row.winrate, score - row.score),
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

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/panels/analysis.blp")]
    pub struct AnalysisPanel {
        #[template_child]
        pub readout: TemplateChild<gtk::Label>,
        #[template_child]
        pub detail: TemplateChild<gtk::Label>,
        #[template_child]
        pub columns: TemplateChild<gtk::ColumnView>,
        #[template_child]
        pub blunder_expander: TemplateChild<gtk::Expander>,
        #[template_child]
        pub blunder_list: TemplateChild<gtk::ListBox>,
        pub state: OnceCell<AppState>,
        pub inner: OnceCell<super::Inner>,
        pub pv_hooks: RefCell<Vec<PvHook>>,
        /// The nodes the blunder rows jump to, parallel to the list box's children.
        pub blunder_nodes: RefCell<Vec<NodeRef>>,
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
        let detail = imp.detail.get();
        let columns = imp.columns.get();
        let blunder_expander = imp.blunder_expander.get();
        let blunder_list = imp.blunder_list.get();

        let store = gio::ListStore::new::<CandidateObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        selection.set_selected(gtk::INVALID_LIST_POSITION);
        columns.set_model(Some(&selection));
        columns.append_column(&rank_column());
        columns.append_column(&column(
            Col {
                title: "Move",
                property: "mv",
                xalign: 0.0,
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
                ..Col::default()
            },
            |winrate: f64| format!("{}%", pct1(winrate as f32)),
        ));
        columns.append_column(&column(
            Col {
                title: "Score",
                property: "score",
                width_chars: 6,
                ..Col::default()
            },
            |score: f64| signed1(score as f32),
        ));
        columns.append_column(&column(
            Col {
                title: "Visits",
                property: "visits",
                width_chars: 5,
                ..Col::default()
            },
            |visits: u32| si_visits(visits),
        ));
        columns.append_column(&column(
            Col {
                title: "Prior",
                property: "prior",
                width_chars: 6,
                ..Col::default()
            },
            |prior: f64| format!("{}%", pct1(prior as f32)),
        ));
        columns.append_column(&column(
            Col {
                title: "PV",
                property: "pv",
                xalign: 0.0,
                expand: true,
                css: Some("mirai-pv-label"),
                ..Col::default()
            },
            |pv: String| pv,
        ));

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
                    let state = panel.state();
                    if let Some(id) = state.resolve_node(node) {
                        state.set_cursor(id);
                    }
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

    fn play(&self, p: Point) {
        let state = self.state();
        if let Err(error) = state.play_move(p) {
            state.toast_illegal_move(error);
        }
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
        grade_rows(&mut rows);

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
                    h.color.name()
                ));
            }
            None => {
                inner.readout.set_label("No analysis");
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

    /// Fills the blunder list from stored main-line analyses.
    pub fn set_blunders(&self, rows: Vec<Blunder>) {
        self.clear_blunders();
        if rows.is_empty() {
            return;
        }
        let inner = self.inner();
        let size = self.state().tree().info.size;

        let mut nodes = Vec::with_capacity(rows.len());
        for b in &rows {
            let detail = match b.best {
                Some(best) => format!(
                    "−{}% · played {} · best {}",
                    pct1(b.drop),
                    size.to_gtp(b.played),
                    size.to_gtp(best)
                ),
                None => format!("−{}% · played {}", pct1(b.drop), size.to_gtp(b.played)),
            };
            // One line: stone, move number, what it cost. A move number on a line of its own
            // spent a row's height saying nothing the eye had to read.
            let row = adw::ActionRow::builder()
                .title(format!("{} {} · {detail}", stone(b.player), b.move_number))
                .activatable(true)
                .build();
            if let Some(class) = severity_class(severity_of_drop(b.drop)) {
                row.add_css_class(class);
            }
            // The stone is a picture; a screen reader sees "black circle" at best, so the row
            // carries the word. `AdwActionRow` is not declared `Accessible` in the bindings,
            // its widget is.
            row.upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "Move {} · {} · {detail}",
                    b.move_number,
                    b.player.name()
                ))]);
            inner.blunder_list.append(&row);
            nodes.push(b.node);
        }
        *self.imp().blunder_nodes.borrow_mut() = nodes;

        inner
            .blunder_expander
            .set_label(Some(&format!("Blunders ({})", rows.len())));
        inner.blunder_expander.set_visible(true);
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

/// The mover of a blunder, as the stone they played.
///
/// A reviewer reading the list is looking at a board, where the player *is* a colour, so
/// "Black"/"White" made them translate a word back into the thing in front of them.
///
/// These two are `Emoji_Presentation=Yes`, so Pango renders them from the colour emoji font
/// and they keep their own black and white — which matters here, because the row carries a
/// `severity_class` that sets `color`, and a monochrome `●`/`○` would come out red.
fn stone(color: Color) -> &'static str {
    match color {
        Color::Black => "⚫",
        Color::White => "⚪",
    }
}

/// The CSS class for a blunder's severity, or `None` when the drop is below the noise
/// floor and the row must not be tinted at all.
fn severity_class(severity: Severity) -> Option<&'static str> {
    match severity {
        Severity::None => None,
        Severity::Minor => Some("mirai-blunder-minor"),
        Severity::Medium => Some("mirai-blunder-medium"),
        Severity::Major => Some("mirai-blunder-major"),
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

/// One column of the candidate list: where its text comes from, and how wide it sits.
#[derive(Clone, Copy)]
struct Col {
    title: &'static str,
    /// The [`CandidateObject`] property the cell follows.
    property: &'static str,
    xalign: f32,
    /// Natural width in characters, GTK's own `-1` for "as wide as the text".
    ///
    /// Pinning it is what keeps a column from re-measuring when `1.4k` becomes `12.7k`: the
    /// cell's size request stops depending on the digits, so ten reports a second cost the
    /// list nothing but new glyphs. It is in characters rather than pixels so it still tracks
    /// the font.
    width_chars: i32,
    /// Only the PV column takes the leftover width.
    expand: bool,
    css: Option<&'static str>,
}

impl Default for Col {
    fn default() -> Col {
        Col {
            title: "",
            property: "",
            xalign: 1.0,
            width_chars: -1,
            expand: false,
            css: None,
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
            .xalign(col.xalign)
            // Both bounds, not just the minimum: a cell whose *natural* width still tracked
            // its digits would go on re-measuring the column, and would also take width the
            // elastic PV column wants.
            .width_chars(col.width_chars)
            .max_width_chars(col.width_chars)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        if let Some(class) = col.css {
            label.add_css_class(class);
        }
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
    gtk::ColumnViewColumn::builder()
        .title(col.title)
        .factory(&factory)
        .expand(col.expand)
        .resizable(true)
        .build()
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
    gtk::ColumnViewColumn::builder()
        .title("#")
        .factory(&factory)
        .resizable(false)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list and the win-rate strip must agree on what counts as a blunder at all.
    /// `severity_class` used to re-derive its own thresholds with no noise floor, so a
    /// 1 % drop came back tinted as a minor blunder.
    #[test]
    fn a_drop_below_the_noise_floor_gets_no_class() {
        assert_eq!(severity_class(severity_of_drop(0.01)), None);
        assert_eq!(severity_class(severity_of_drop(f32::NAN)), None);
        assert_eq!(
            severity_class(severity_of_drop(0.03)),
            Some("mirai-blunder-minor")
        );
        assert_eq!(
            severity_class(severity_of_drop(0.07)),
            Some("mirai-blunder-medium")
        );
        assert_eq!(
            severity_class(severity_of_drop(0.12)),
            Some("mirai-blunder-major")
        );
    }
}
