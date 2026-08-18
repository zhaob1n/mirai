// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The application window: layout, actions, accelerators, SGF I/O and autosave. Step 8.
//!
//! Everything the user can trigger lives here as a `win.*` action, so the header menu, the
//! bottom bar buttons, the keyboard accelerators and the shortcuts dialog all name the same
//! thing. Widgets never talk to each other; they observe [`AppState`].

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use mirai_core::{DeadSet, GameInfo, GameTree, NodeId, Point, sgf};
use mirai_engine::{Report, SubEvent, Want};

use crate::app::{AppState, Change, NodeRef};
use crate::batch::BatchAnalysis;
use crate::config::Config;
use crate::panels::AnalysisPanel;
use crate::play::{PlayController, PlayState};
use crate::util;
use crate::widgets::{BoardView, MoveTreeView, WinrateGraph};
use crate::window_shell::MiraiWindow;

/// Autosave cadence, in seconds.
const AUTOSAVE_SECS: u32 = 30;
/// Visit cap for the one-off score-estimate query.
const SCORE_VISITS: u32 = 400;
/// Ownership magnitude above which a stone counts as dead, matching KataGo's own default.
const DEAD_THRESHOLD: f32 = 0.4;
#[derive(Default)]
struct TaskSlot(RefCell<Option<glib::JoinHandle<()>>>);

impl TaskSlot {
    fn replace(&self, task: glib::JoinHandle<()>) {
        self.abort();
        *self.0.borrow_mut() = Some(task);
    }

    fn abort(&self) {
        if let Some(task) = self.0.borrow_mut().take() {
            task.abort();
        }
    }
}

impl Drop for TaskSlot {
    fn drop(&mut self) {
        if let Some(task) = self.0.get_mut().take() {
            task.abort();
        }
    }
}

#[derive(Default)]
struct SourceSlot(Cell<Option<glib::SourceId>>);

impl SourceSlot {
    fn replace(&self, source: glib::SourceId) {
        self.remove();
        self.0.set(Some(source));
    }

    fn remove(&self) {
        if let Some(source) = self.0.take() {
            source.remove();
        }
    }
}

impl Drop for SourceSlot {
    fn drop(&mut self) {
        if let Some(source) = self.0.take() {
            source.remove();
        }
    }
}

#[derive(Default)]
struct WindowTasks {
    score: TaskSlot,
    autosave: SourceSlot,
}

impl WindowTasks {
    fn abort_all(&self) {
        self.score.abort();
        self.autosave.remove();
    }
}

/// Per-window Rust state. The corresponding [`MiraiWindow`] owns exactly one value.
pub struct Ui {
    window: glib::WeakRef<MiraiWindow>,
    pub state: AppState,
    pub toasts: adw::ToastOverlay,
    pub comment: gtk::TextView,
    pub play: PlayController,
    pub batch: BatchAnalysis,
    pub readout: gtk::Label,
    pub move_scale: gtk::Scale,
    pub engine_menu: gtk::MenuButton,
    pub analysis: AnalysisPanel,
    board: glib::WeakRef<BoardView>,
    winrate: glib::WeakRef<WinrateGraph>,
    move_tree: glib::WeakRef<MoveTreeView>,
    pub file: RefCell<Option<PathBuf>>,
    title: adw::WindowTitle,
    clock_box: gtk::Box,
    clock_black: gtk::Label,
    clock_white: gtk::Label,
    play_controls: gtk::Box,
    pass_button: gtk::Button,
    undo_button: gtk::Button,
    resign_button: gtk::Button,
    analysis_stack: gtk::Stack,
    /// This window's Fox picker, built the first time it is asked for. One per
    /// window: libadwaita refuses to present one dialog in two windows at once.
    fox_picker: RefCell<Option<crate::fox_picker::FoxPickerDialog>>,
    comment_node: Cell<Option<NodeRef>>,
    scale_guard: Cell<bool>,
    pending_auto_analyse: Cell<bool>,
    tasks: WindowTasks,
    autosave: Option<AutosaveFile>,
}

impl Ui {
    fn window(&self) -> MiraiWindow {
        self.window
            .upgrade()
            .expect("window state outlived its GObject owner")
    }

    fn weak_window(&self) -> glib::WeakRef<MiraiWindow> {
        self.window.clone()
    }
}

impl Drop for Ui {
    /// Dropping the state *is* the release, so no exit path can forget it: the window's
    /// `close-request`, its `dispose` and the application's `shutdown` all reduce to
    /// [`MiraiWindow::take_ui`].
    ///
    /// Nothing here may reach back through `self.window`. A `Ui` is dropped from
    /// `MiraiWindow::dispose`, where the weak reference may already be cleared, and it is
    /// taken out of the window before it drops, so every [`AppState`] hook that tries to
    /// re-enter finds no state and does nothing. Work that genuinely needs the widget tree
    /// belongs at the call site, ahead of the drop.
    fn drop(&mut self) {
        // Ordering, not necessity: the slots would release themselves when `tasks` drops,
        // but the timers must not fire while the tree is being flushed.
        self.tasks.abort_all();
        flush_comment(self);
        self.batch.cancel();
        self.play.stop();
        self.state.cancel_tasks();
        self.state.set_live_analysis(false);
        self.state.save_config();
        self.state.set_engine(None);
        // `autosave` deletes its file as it drops, with the rest of the fields.
    }
}

impl MiraiWindow {
    /// Releases this window's state exactly once. The release itself is [`Ui`]'s `Drop`.
    pub(crate) fn shutdown(&self) {
        if self.begin_shutdown() {
            drop(self.take_ui());
        }
    }
}

/// Builds and presents a window for `app`, optionally opening `path`.
///
/// Every window owns its own `AppState`; the engines are the one thing they share, through
/// `pool`, so a second window does not start a second KataGo.
pub fn present(
    app: &adw::Application,
    runtime: tokio::runtime::Handle,
    pool: Rc<crate::engines::EnginePool>,
    path: Option<PathBuf>,
) {
    let config_path = Config::default_path().unwrap_or_else(|_| PathBuf::from("mirai.toml"));
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(%e, "falling back to a seeded configuration");
            Config::seeded()
        }
    };
    let state = AppState::new(config, config_path, runtime, pool);
    let window = MiraiWindow::new(app);
    let toasts = window.toasts();
    let board = BoardView::new(&window, &state);
    let winrate = WinrateGraph::new(&window);
    let move_tree = MoveTreeView::new(&window);
    let analysis = AnalysisPanel::new(&window);
    let comment = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    let play = PlayController::new(&state, &window);
    let batch = BatchAnalysis::new(&state, &window);

    // The custom GPU-rendered views are stateful Rust widgets. Blueprint owns their static
    // containers; Rust only inserts the dynamic instances.
    board.set_hexpand(true);
    board.set_vexpand(true);
    winrate.set_hexpand(true);
    winrate.set_size_request(-1, 170);
    let content = window.content_paned();
    content.set_start_child(Some(&board));
    content.set_end_child(Some(&winrate));
    window.banner_slot().append(batch.banner());

    let analysis_stack = gtk::Stack::new();
    analysis_stack.add_named(&analysis, Some("panel"));
    analysis_stack.add_named(&crate::prefs::no_engine_status_page(), Some("empty"));
    analysis_stack.set_vexpand(true);

    let sidebar_stack = window.sidebar_stack();
    sidebar_stack.add_titled_with_icon(
        &analysis_stack,
        Some("analysis"),
        "Analysis",
        "view-list-symbolic",
    );
    let tree_scroller = move_tree.in_scroller();
    tree_scroller.set_hexpand(true);
    tree_scroller.set_vexpand(true);
    sidebar_stack.add_titled_with_icon(
        &tree_scroller,
        Some("moves"),
        "Moves",
        "view-grid-symbolic",
    );
    let comment_scroll = gtk::ScrolledWindow::builder().child(&comment).build();
    comment_scroll.set_vexpand(true);
    sidebar_stack.add_titled_with_icon(
        &comment_scroll,
        Some("comment"),
        "Comment",
        "text-editor-symbolic",
    );

    let live_toggle = window.live_toggle();
    state
        .bind_property("live-analysis", &live_toggle, "active")
        .bidirectional()
        .sync_create()
        .build();
    live_toggle.connect_active_notify(|button| {
        if button.is_active() {
            button.set_icon_name("media-playback-stop-symbolic");
            button.set_tooltip_text(Some("Stop Live Analysis (Space)"));
        } else {
            button.set_icon_name("media-playback-start-symbolic");
            button.set_tooltip_text(Some("Start Live Analysis (Space)"));
        }
    });

    let engine_menu = window.engine_menu();
    let engine_content = window.engine_content();
    state
        .bind_property("engine-label", &engine_content, "label")
        .sync_create()
        .build();

    let split = window.split();
    let sidebar_toggle = window.sidebar_toggle();
    split
        .bind_property("show-sidebar", &sidebar_toggle, "active")
        .bidirectional()
        .sync_create()
        .build();
    // One button for both directions: the check state is the only affordance the icon set
    // offers, so the tooltip carries the verb.
    sidebar_toggle.connect_active_notify(|button| {
        button.set_tooltip_text(Some(if button.is_active() {
            "Hide Sidebar (F9)"
        } else {
            "Show Sidebar (F9)"
        }));
    });
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        900.0,
        adw::LengthUnit::Sp,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    breakpoint.add_setter(&split, "show-sidebar", Some(&false.to_value()));
    window.add_breakpoint(breakpoint);

    let title = window.title_widget();
    let clock_box = window.clock_box();
    let clock_black = window.clock_black();
    let clock_white = window.clock_white();
    let play_controls = window.play_controls();
    let pass_button = window.pass_button();
    let undo_button = window.undo_button();
    let resign_button = window.resign_button();
    let move_scale = window.move_scale();
    let readout = window.readout();
    move_scale.set_increments(1.0, 10.0);

    window.install_ui(Ui {
        window: window.downgrade(),
        state: state.clone(),
        toasts: toasts.clone(),
        comment,
        play,
        batch,
        analysis: analysis.clone(),
        readout,
        board: board.downgrade(),
        winrate: winrate.downgrade(),
        move_tree: move_tree.downgrade(),
        move_scale,
        engine_menu,
        file: RefCell::new(None),
        title,
        clock_box,
        clock_black,
        clock_white,
        play_controls,
        pass_button,
        undo_button,
        resign_button,
        analysis_stack,
        fox_picker: RefCell::new(None),
        comment_node: Cell::new(None),
        scale_guard: Cell::new(false),
        pending_auto_analyse: Cell::new(false),
        tasks: WindowTasks::default(),
        autosave: next_autosave_file(),
    });
    window.with_ui(|ui| {
        install_actions(ui);
        connect_state(ui);
        connect_comment(ui);
        connect_scale(ui);
    });
    {
        let weak = window.downgrade();
        state.set_change_hook(move |change| {
            with_window_ui(&weak, |ui| handle_change(ui, change));
        });
    }

    // Cross-widget wiring the two panels cannot do for themselves.
    window.with_ui(|ui| ui.play.attach_board(&board));
    {
        let board = board.clone();
        analysis.connect_pv_preview(move |index| board.set_pv_preview(index));
    }
    window.with_ui(|ui| {
        let weak = ui.weak_window();
        ui.play.set_analyse_hook(move || {
            with_window_ui(&weak, |ui| ui.batch.start());
        });
    });
    window.with_ui(|ui| {
        refresh_engine_menu(ui);
        update_analysis_page(ui);
        update_play_controls(ui);
        update_scale(ui);
        update_readout(ui);
        update_title(ui);
        update_subtitle(ui);
        load_comment(ui);
        ui.analysis.refresh();
        if let Some(tree) = ui.move_tree.upgrade() {
            tree.refresh();
        }
        if let Some(graph) = ui.winrate.upgrade() {
            graph.refresh();
        }
    });

    // Whatever an earlier run left behind, one record per window, most recent first. A
    // window that closed cleanly deleted its file, so anything here really is a leftover.
    let stale_autosave = stale_autosaves()
        .lock()
        .ok()
        .and_then(|mut stale| stale.pop());

    window.with_ui(|ui| {
        install_autosave(ui);
        connect_close(ui);
    });

    // Start the configured engine, if any.
    let active = state.config().active_profile().map(|p| p.name.clone());
    if let Some(name) = active {
        state.activate_profile(&name);
    }

    window.present();

    window.with_ui(|ui| {
        if let Some(path) = path {
            load_sgf(ui, &path, true);
        } else if let Some(autosave) = stale_autosave {
            offer_restore(ui, autosave);
        }
    });

    if state.config().engine_profiles.is_empty() {
        crate::prefs::present(&window, &state);
    }
}

fn refresh_engine_menu(ui: &Ui) {
    let model = crate::prefs::engine_menu_model(&ui.state);
    ui.engine_menu.set_menu_model(Some(&model));
}

// -- signal wiring ----------------------------------------------------------------------

/// Runs `f` with the state of a live window. Handlers never own that state.
fn with_window_ui(weak: &glib::WeakRef<MiraiWindow>, f: impl FnOnce(&Ui)) {
    if let Some(window) = weak.upgrade() {
        window.with_ui(f);
    }
}
fn connect_state(ui: &Ui) {
    let weak = ui.weak_window();
    let state = ui.state.clone();
    {
        let weak = weak.clone();
        state.connect_modified_notify(move |_| with_window_ui(&weak, update_title));
    }
    {
        let weak = weak.clone();
        state.connect_status_notify(move |_| {
            with_window_ui(&weak, |ui| {
                update_subtitle(ui);
                update_readout(ui);
            });
        });
    }
    state.connect_engine_label_notify(move |_| with_window_ui(&weak, update_subtitle));
}

fn handle_change(ui: &Ui, change: Change) {
    match change {
        Change::Tree => {
            update_scale(ui);
            update_title(ui);
            if let Some(board) = ui.board.upgrade() {
                board.refresh_tree();
            }
            if let Some(tree) = ui.move_tree.upgrade() {
                tree.refresh();
            }
            if let Some(graph) = ui.winrate.upgrade() {
                graph.refresh();
            }
            ui.analysis.clear_blunders();
            ui.analysis.refresh();
        }
        Change::Cursor => {
            flush_comment(ui);
            load_comment(ui);
            update_scale(ui);
            update_readout(ui);
            update_clocks(ui);
            if let Some(board) = ui.board.upgrade() {
                board.refresh_cursor();
            }
            if let Some(tree) = ui.move_tree.upgrade() {
                tree.refresh();
            }
            if let Some(graph) = ui.winrate.upgrade() {
                graph.refresh_cursor();
            }
            ui.analysis.refresh();
        }
        Change::Report => {
            update_readout(ui);
            if let Some(board) = ui.board.upgrade() {
                board.refresh_report();
            }
            if let Some(graph) = ui.winrate.upgrade() {
                graph.refresh();
            }
            ui.analysis.refresh();
        }
        Change::Engine => {
            refresh_engine_menu(ui);
            update_analysis_page(ui);
            update_subtitle(ui);
            maybe_auto_analyse(ui);
        }
        Change::Toast(text) => ui.toasts.add_toast(adw::Toast::new(&text)),
        Change::Play => {
            update_clocks(ui);
            update_play_controls(ui);
        }
        Change::BatchProgress(_, _) => {
            if let Some(graph) = ui.winrate.upgrade() {
                graph.refresh();
            }
        }
    }
}

fn connect_scale(ui: &Ui) {
    let weak = ui.weak_window();
    ui.move_scale.connect_value_changed(move |scale| {
        with_window_ui(&weak, |ui| {
            if ui.scale_guard.get() {
                return;
            }
            let want = scale.value().round().max(0.0) as usize;
            let cursor = ui.state.cursor();
            let target = ui
                .state
                .with_tree_cached(|t| current_line(t, cursor).get(want).copied());
            if let Some(id) = target {
                ui.state.set_cursor(id);
            }
        });
    });
}

fn connect_comment(ui: &Ui) {
    let focus = gtk::EventControllerFocus::new();
    let weak = ui.weak_window();
    focus.connect_leave(move |_| with_window_ui(&weak, flush_comment));
    ui.comment.add_controller(focus);
}

fn connect_close(ui: &Ui) {
    let window = ui.window();
    let weak = window.downgrade();
    window.connect_close_request(move |_| {
        if let Some(window) = weak.upgrade() {
            window.shutdown();
        }
        glib::Propagation::Proceed
    });
}

fn install_autosave(ui: &Ui) {
    let weak = ui.weak_window();
    let id = glib::timeout_add_seconds_local(AUTOSAVE_SECS, move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        with_window_ui(&weak, write_autosave);
        glib::ControlFlow::Continue
    });
    ui.tasks.autosave.replace(id);
}

// -- refreshers -------------------------------------------------------------------------

/// The line through the cursor: root → cursor, then main-line continuations.
fn current_line(tree: &GameTree, cursor: NodeId) -> Vec<NodeId> {
    let mut line = tree.path_to(cursor);
    let mut id = cursor;
    while let Some(&child) = tree.children(id).first() {
        line.push(child);
        id = child;
    }
    line
}

fn update_scale(ui: &Ui) {
    let cursor = ui.state.cursor();
    let (upper, index) = ui.state.with_tree_cached(|t| {
        let line = current_line(t, cursor);
        let index = line.iter().position(|&n| n == cursor).unwrap_or(0);
        (line.len().saturating_sub(1), index)
    });
    ui.scale_guard.set(true);
    ui.move_scale.set_range(0.0, upper.max(1) as f64);
    ui.move_scale.set_value(index as f64);
    ui.scale_guard.set(false);
    ui.move_scale
        .set_tooltip_text(Some(&format!("Move {index} of {upper}")));
}

fn update_readout(ui: &Ui) {
    let text = match ui.state.last_report() {
        Some(report) => {
            let to_play = ui.state.to_play();
            let speed = match ui.state.analysis_speed() {
                Some(rate) => format!("  {}", util::visits_per_second(rate)),
                None => String::new(),
            };
            format!(
                "{} {}%  {}  {}{speed}",
                to_play.katago(),
                util::pct1(report.root.winrate_for(to_play)),
                util::signed1(report.root.score_lead_for(to_play)),
                util::si_visits(report.root.visits),
            )
        }
        None => {
            let status = ui.state.status();
            if status.is_empty() {
                "—".to_string()
            } else {
                status
            }
        }
    };
    ui.readout.set_label(&text);
}

fn update_clocks(ui: &Ui) {
    let Some((black, white)) = ui.play.clocks() else {
        ui.clock_box.set_visible(false);
        return;
    };
    ui.clock_black.set_label(&format!("● {black}"));
    ui.clock_white.set_label(&format!("○ {white}"));
    ui.clock_box.set_visible(true);

    let to_play = ui.state.to_play();
    for (label, mine) in [
        (&ui.clock_black, to_play == mirai_core::Color::Black),
        (&ui.clock_white, to_play == mirai_core::Color::White),
    ] {
        if mine {
            label.add_css_class("active");
        } else {
            label.remove_css_class("active");
        }
    }
}

fn update_play_controls(ui: &Ui) {
    let state = ui.play.play_state();
    let in_progress = matches!(state, PlayState::HumanTurn | PlayState::AiThinking);
    ui.play_controls.set_visible(in_progress);
    ui.pass_button
        .set_sensitive(matches!(state, PlayState::HumanTurn));
    ui.undo_button.set_sensitive(ui.play.is_active());
    ui.resign_button
        .set_visible(in_progress && ui.play.is_human_vs_engine());
}

/// The name for a record with no backing file: who played it, else the event it came from.
fn record_label(info: &mirai_core::GameInfo) -> Option<String> {
    let black = info.players[0].name.trim();
    let white = info.players[1].name.trim();
    if black.is_empty() && white.is_empty() {
        let event = info.event.trim();
        return (!event.is_empty()).then(|| event.to_string());
    }
    Some(format!(
        "{} vs {}",
        if black.is_empty() { "?" } else { black },
        if white.is_empty() { "?" } else { white }
    ))
}

fn update_title(ui: &Ui) {
    // A saved record is named by its file; an imported one by the only identity it has.
    let name = ui
        .file
        .borrow()
        .as_ref()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .or_else(|| record_label(&ui.state.tree().info))
        .unwrap_or_else(|| "Untitled".to_string());
    let shown = if ui.state.modified() {
        format!("{name} •")
    } else {
        name
    };
    ui.title.set_title(&shown);
    ui.window().set_title(Some(&format!("{shown} — mirai")));
}

fn update_subtitle(ui: &Ui) {
    let status = ui.state.status();
    let subtitle = if status.is_empty() {
        ui.state.engine_label()
    } else {
        status
    };
    ui.title.set_subtitle(&subtitle);
}

fn update_analysis_page(ui: &Ui) {
    let empty = ui.state.config().engine_profiles.is_empty();
    ui.analysis_stack
        .set_visible_child_name(if empty { "empty" } else { "panel" });
}

// -- comment pane -----------------------------------------------------------------------

fn flush_comment(ui: &Ui) {
    let Some(node) = ui.comment_node.get() else {
        return;
    };
    let Some(id) = ui.state.resolve_node(node) else {
        ui.comment_node.set(None);
        return;
    };
    let buffer = ui.comment.buffer();
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string();
    let unchanged = {
        let tree = ui.state.tree();
        tree.node(id).comment == text
    };
    if unchanged {
        return;
    }
    ui.comment_node.set(None);
    ui.state.with_tree_mut(|t| t.set_comment(id, text));
    ui.comment_node.set(Some(node));
}

fn load_comment(ui: &Ui) {
    let node = ui.state.cursor_ref();
    let text = {
        let tree = ui.state.tree();
        tree.node(node.id).comment.clone()
    };
    ui.comment_node.set(None);
    ui.comment.buffer().set_text(&text);
    ui.comment_node.set(Some(node));
}

// -- SGF I/O ----------------------------------------------------------------------------

fn sgf_filters() -> (gio::ListStore, gtk::FileFilter) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("SGF game records"));
    filter.add_pattern("*.sgf");
    filter.add_suffix("sgf");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&filter);
    (store, filter)
}

fn sgf_text(ui: &Ui) -> String {
    let include = ui.state.config().ui.save_analysis_in_sgf;
    let tree = ui.state.tree();
    sgf::write(&tree, include)
}

/// Installs `tree` as the current game. `path` is remembered for plain Save.
fn adopt(ui: &Ui, tree: GameTree, path: Option<PathBuf>) {
    ui.play.stop();
    // Batch workers hold node IDs from this tree; stop them before replacing its arena.
    ui.batch.cancel();
    ui.comment_node.set(None);
    *ui.file.borrow_mut() = path.clone();
    ui.state.set_file_path(
        path.as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    );
    ui.state.set_tree(tree, None);
    load_comment(ui);
    update_scale(ui);
    update_readout(ui);
    update_title(ui);
    update_clocks(ui);
    ui.pending_auto_analyse
        .set(ui.state.config().analysis.auto_analyse_on_open);
    maybe_auto_analyse(ui);
}

/// Starts a whole-game sweep if the user asked for one on open and an engine is ready.
fn maybe_auto_analyse(ui: &Ui) {
    if !ui.pending_auto_analyse.get() {
        return;
    }
    if ui.state.engine().is_none() {
        return;
    }
    ui.pending_auto_analyse.set(false);
    if ui.state.tree().main_line().len() >= 2 {
        ui.batch.start();
    }
}

fn load_sgf(ui: &Ui, path: &Path, remember: bool) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            ui.state.toast(format!("{}: {e}", path.display()));
            return;
        }
    };
    let trees = match sgf::parse(&bytes) {
        Ok(t) => t,
        Err(e) => {
            ui.state.toast(format!("{}: {e}", path.display()));
            return;
        }
    };
    let remembered = remember.then(|| path.to_path_buf());
    match trees.len() {
        0 => ui.state.toast("That file holds no game records"),
        1 => {
            let tree = trees.into_iter().next().expect("length checked");
            let moves = tree.main_line().len().saturating_sub(1);
            adopt(ui, tree, remembered);
            ui.state
                .toast(format!("Opened {} ({moves} moves)", file_label(path)));
        }
        _ => choose_game(ui, trees, remembered, file_label(path)),
    }
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn game_row(tree: &GameTree) -> adw::ActionRow {
    let info = &tree.info;
    let who = |p: &mirai_core::PlayerInfo| -> String {
        match (p.name.as_str(), p.rank.as_str()) {
            ("", "") => "?".to_string(),
            (name, "") => name.to_string(),
            ("", rank) => rank.to_string(),
            (name, rank) => format!("{name} ({rank})"),
        }
    };
    let moves = tree.main_line().len().saturating_sub(1);
    let mut bits = vec![
        format!("{}×{}", info.size.w, info.size.h),
        format!("{moves} moves"),
    ];
    if !info.result.is_empty() {
        bits.push(info.result.clone());
    }
    if !info.date.is_empty() {
        bits.push(info.date.clone());
    }
    adw::ActionRow::builder()
        .title(format!(
            "{} vs {}",
            who(&info.players[0]),
            who(&info.players[1])
        ))
        .subtitle(bits.join(" · "))
        .build()
}

fn choose_game(ui: &Ui, trees: Vec<GameTree>, path: Option<PathBuf>, label: String) {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    for tree in &trees {
        list.append(&game_row(tree));
    }
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .min_content_height(260)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();

    let count = trees.len();
    let dialog = adw::AlertDialog::new(
        Some("Games in file"),
        Some(&format!("{label} holds {count} game records.")),
    );
    dialog.set_extra_child(Some(&scroll));
    dialog.add_responses(&[("cancel", "Cancel"), ("open", "Open")]);
    dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("open"));
    dialog.set_close_response("cancel");

    let trees = RefCell::new(trees);
    let weak = ui.weak_window();
    dialog.connect_response(None, move |_, response| {
        if response != "open" {
            return;
        }
        let index = list.selected_row().map(|r| r.index() as usize).unwrap_or(0);
        let mut trees = trees.borrow_mut();
        if index < trees.len() {
            let tree = trees.remove(index);
            drop(trees);
            with_window_ui(&weak, |ui| adopt(ui, tree, path.clone()));
        }
    });
    dialog.present(Some(&ui.window()));
}

fn do_open(ui: &Ui) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title("Open SGF");
    let (filters, default) = sgf_filters();
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&default));
    let future = dialog.open_future(Some(&ui.window()));
    let weak = ui.weak_window();
    glib::spawn_future_local(async move {
        match future.await {
            Ok(file) => match file.path() {
                Some(path) => with_window_ui(&weak, |ui| load_sgf(ui, &path, true)),
                None => with_window_ui(&weak, |ui| {
                    ui.state.toast("That location is not a local file");
                }),
            },
            Err(e) => {
                if !e.matches(gtk::DialogError::Dismissed) {
                    with_window_ui(&weak, |ui| {
                        ui.state.toast(format!("Could not open: {e}"));
                    });
                }
            }
        }
    });
}

fn do_download_fox(ui: &Ui) {
    let weak = ui.weak_window();
    crate::fox::present(&ui.window(), &ui.fox_picker, move |download| {
        with_window_ui(&weak, |ui| {
            let moves = download.tree.main_line().len().saturating_sub(1);
            adopt(ui, download.tree, None);
            ui.state.set_modified(true);
            update_title(ui);
            ui.state
                .toast(format!("Downloaded {} ({moves} moves)", download.label));
        });
    });
}

/// Empty board, same size / rules / komi as `tree`. Identity and stones do not carry over.
fn blank_tree(tree: &GameTree) -> GameTree {
    let mut info = GameInfo::new(tree.info.size, tree.info.rules);
    info.komi = tree.info.komi;
    GameTree::new(info)
}

fn do_clear_board(ui: &Ui) {
    let tree = blank_tree(&ui.state.tree());
    adopt(ui, tree, None);
    // This is a new blank record, not an opened one.
    ui.pending_auto_analyse.set(false);
}

fn write_to(ui: &Ui, path: &Path) {
    let text = sgf_text(ui);
    match std::fs::write(path, text) {
        Ok(()) => {
            *ui.file.borrow_mut() = Some(path.to_path_buf());
            ui.state.set_file_path(path.display().to_string());
            ui.state.set_modified(false);
            update_title(ui);
            ui.state.toast(format!("Saved {}", file_label(path)));
        }
        Err(e) => ui.state.toast(format!("{}: {e}", path.display())),
    }
}

fn do_save(ui: &Ui) {
    flush_comment(ui);
    let existing = ui.file.borrow().clone();
    match existing {
        Some(path) => write_to(ui, &path),
        None => do_save_as(ui),
    }
}

fn do_save_as(ui: &Ui) {
    flush_comment(ui);
    let dialog = gtk::FileDialog::new();
    dialog.set_title("Save SGF");
    let (filters, default) = sgf_filters();
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&default));
    let suggested = ui
        .file
        .borrow()
        .as_ref()
        .map(|p| file_label(p))
        .unwrap_or_else(|| "game.sgf".to_string());
    dialog.set_initial_name(Some(&suggested));
    let future = dialog.save_future(Some(&ui.window()));
    let weak = ui.weak_window();
    glib::spawn_future_local(async move {
        match future.await {
            Ok(file) => match file.path() {
                Some(path) => with_window_ui(&weak, |ui| write_to(ui, &path)),
                None => with_window_ui(&weak, |ui| {
                    ui.state.toast("That location is not a local file");
                }),
            },
            Err(e) => {
                if !e.matches(gtk::DialogError::Dismissed) {
                    with_window_ui(&weak, |ui| {
                        ui.state.toast(format!("Could not save: {e}"));
                    });
                }
            }
        }
    });
}

// -- autosave ---------------------------------------------------------------------------

/// This window's autosave file, `autosave-<pid>-<start>-<n>.sgf` in the data directory.
///
/// One file per window, not per process: windows used to share `autosave.sgf` and overwrite
/// each other, and the companion `clean-exit` flag could not say *which* window had exited
/// cleanly. The path is owned by the window's [`Ui`] and the file is removed when that value
/// drops, so "a window deletes its own file as it closes" holds for every exit path instead
/// of only the one that remembers to. Whatever is left on disk is by definition what a crash
/// left behind.
struct AutosaveFile(PathBuf);

impl AutosaveFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for AutosaveFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn next_autosave_file() -> Option<AutosaveFile> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    Some(AutosaveFile(
        Config::data_dir()
            .ok()?
            .join(format!("{}{n}.sgf", *AUTOSAVE_PREFIX)),
    ))
}

/// Identifies this process's autosaves. The start time is in there because a pid alone is
/// reused, and a stale file wrongly taken for ours would never be offered back to the user.
static AUTOSAVE_PREFIX: LazyLock<String> = LazyLock::new(|| {
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("autosave-{}-{start}-", std::process::id())
});

/// Autosaves left by an earlier run, newest last, each offered to at most one window.
///
/// Scanned once per process. Files that no longer parse, or hold nothing worth restoring,
/// are deleted here rather than shown.
fn stale_autosaves() -> &'static Mutex<Vec<PathBuf>> {
    static STALE: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(|| {
        let Ok(dir) = Config::data_dir() else {
            return Mutex::new(Vec::new());
        };
        // A pre-per-window autosave is stale exactly when the old clean-exit flag is absent.
        let legacy = dir.join("autosave.sgf");
        let flag = dir.join("clean-exit");
        let mut found: Vec<PathBuf> = Vec::new();
        if legacy.exists() && !flag.exists() {
            found.push(legacy);
        }
        let _ = std::fs::remove_file(&flag);

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with("autosave-")
                    && name.ends_with(".sgf")
                    && !name.starts_with(AUTOSAVE_PREFIX.as_str())
                {
                    found.push(entry.path());
                }
            }
        }
        found.retain(|path| {
            match std::fs::read(path)
                .ok()
                .and_then(|bytes| sgf::parse(&bytes).ok())
            {
                Some(games) => games.iter().any(tree_has_content),
                None => {
                    let _ = std::fs::remove_file(path);
                    false
                }
            }
        });
        found.sort();
        Mutex::new(found)
    });
    &STALE
}

/// A game record is only worth autosaving if it holds something the user would miss: a
/// move, a setup stone, or a comment. Restoring a blank board is pure noise, and it is
/// exactly what a session where the user did nothing would otherwise leave behind.
fn tree_has_content(tree: &mirai_core::GameTree) -> bool {
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        if node.mv.is_some() || !node.setup.is_empty() || !node.comment.trim().is_empty() {
            return true;
        }
        stack.extend_from_slice(tree.children(id));
    }
    false
}

fn write_autosave(ui: &Ui) {
    let Some(autosave) = ui.autosave.as_ref().map(AutosaveFile::path) else {
        return;
    };
    if !tree_has_content(&ui.state.tree()) {
        // Leave no bait for the restore prompt.
        let _ = std::fs::remove_file(autosave);
        return;
    }
    if let Some(dir) = autosave.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!(%e, "could not create the data directory");
        return;
    }
    let text = sgf_text(ui);
    if let Err(e) = std::fs::write(autosave, text) {
        tracing::warn!(%e, "could not write the autosave");
    }
}

fn offer_restore(ui: &Ui, autosave: PathBuf) {
    let dialog = adw::AlertDialog::new(
        Some("Restore the Last Game?"),
        Some(
            "mirai did not shut down cleanly. An autosaved copy of the game record you were \
             looking at is available.",
        ),
    );
    dialog.add_responses(&[("discard", "Discard"), ("restore", "Restore")]);
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("restore"));
    dialog.set_close_response("discard");
    let weak = ui.weak_window();
    dialog.connect_response(None, move |_, response| {
        if response == "restore" {
            with_window_ui(&weak, |ui| load_sgf(ui, &autosave, false));
        }
        let _ = std::fs::remove_file(&autosave);
    });
    dialog.present(Some(&ui.window()));
}

// -- score estimate ---------------------------------------------------------------------

fn do_score(ui: &Ui) {
    let Some(engine) = ui.state.engine() else {
        ui.state.toast("No engine to estimate the score with");
        return;
    };
    ui.tasks.score.abort();
    let target = ui.state.cursor_ref();
    let mut req = ui
        .state
        .request_for_node(target.id, Some(SCORE_VISITS), Want::OWNERSHIP);
    req.report_every_ms = None;
    req.priority = 8;
    let mut sub = engine.subscribe(req);

    ui.state.set_status("Estimating the score…".to_string());
    let weak = ui.weak_window();
    let handle = glib::spawn_future_local(async move {
        let mut last: Option<Arc<Report>> = None;
        while let Some(event) = sub.next().await {
            match event {
                SubEvent::Pending => {}
                SubEvent::Report(r) => last = Some(r),
                SubEvent::Done(r) => {
                    last = Some(r);
                    break;
                }
                SubEvent::Failed(e) => {
                    with_window_ui(&weak, |ui| {
                        ui.state.set_status(String::new());
                        ui.state.on_engine_error(e);
                    });
                    return;
                }
            }
        }
        with_window_ui(&weak, |ui| {
            ui.state.set_status(String::new());
            if ui.state.resolve_node(target) != Some(ui.state.cursor()) {
                return;
            }
            match last {
                Some(report) => show_estimate(ui, &report),
                None => ui.state.toast("The engine returned no estimate"),
            }
        });
    });
    ui.tasks.score.replace(handle);
}

fn show_estimate(ui: &Ui, report: &Report) {
    let position = ui.state.position();
    let board = &position.board;
    let dead = match report.ownership.as_ref() {
        Some(o) if o.len() == board.size.points() => {
            let owner: Vec<f32> = o.iter().map(|&v| v as f32 / 127.0).collect();
            DeadSet::from_ownership(board, &owner, DEAD_THRESHOLD)
        }
        _ => DeadSet::empty(board.size),
    };
    let (rules, komi, handicap) = {
        let tree = ui.state.tree();
        (tree.info.rules.rules(), tree.info.komi, tree.info.handicap)
    };
    let result = mirai_core::score(board, &rules, komi, handicap, &dead);
    let lead = report.root.score_lead_for(mirai_core::Color::Black);
    let body = format!(
        "{}{}\n\nBlack {:.1} — White {:.1}\nKataGo lead after {} visits: {}",
        result.result_string(),
        if result.approximate {
            " (estimated)"
        } else {
            ""
        },
        result.black,
        result.white,
        util::si_visits(report.root.visits),
        util::signed1(lead),
    );
    let dialog = adw::AlertDialog::new(Some("Score estimate"), Some(&body));
    dialog.add_responses(&[("close", "Close")]);
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(&ui.window()));
}

// -- dialogs ----------------------------------------------------------------------------

fn show_shortcuts(ui: &Ui) {
    let dialog = adw::ShortcutsDialog::new();
    for (title, items) in [
        (
            "Navigation",
            &[
                ("First Move", "win.first"),
                ("Last Move", "win.last"),
                ("Previous Move", "win.prev"),
                ("Next Move", "win.next"),
                ("Back Ten Moves", "win.prev10"),
                ("Forward Ten Moves", "win.next10"),
                ("Previous Variation", "win.branch-prev"),
                ("Next Variation", "win.branch-next"),
            ][..],
        ),
        (
            "Analysis",
            &[
                ("Live Analysis", "win.toggle-analysis"),
                ("Analyse Whole Game", "win.analyse-game"),
                ("Estimate Score", "win.score"),
                ("Ownership Overlay", "win.toggle-ownership"),
                ("Policy Overlay", "win.toggle-policy"),
                ("Coordinates", "win.toggle-coords"),
                ("Move Numbers", "win.toggle-move-numbers"),
                ("Sidebar", "win.toggle-sidebar"),
            ][..],
        ),
        (
            "Game",
            &[
                ("New Game", "win.new-game"),
                ("Pass", "win.pass"),
                ("Undo", "win.undo"),
                ("Delete Branch", "win.delete-branch"),
            ][..],
        ),
        (
            "File",
            &[
                ("Open", "win.open"),
                ("Download from Fox", "win.download-fox"),
                ("Clear Board", "win.clear-board"),
                ("Save", "win.save"),
                ("Save As", "win.save-as"),
                ("Copy SGF", "win.copy-sgf"),
                ("Paste SGF", "win.paste-sgf"),
            ][..],
        ),
    ] {
        let section = adw::ShortcutsSection::new(Some(title));
        for (label, action) in items {
            section.add(adw::ShortcutsItem::from_action(label, action));
        }
        dialog.add(section);
    }
    dialog.present(Some(&ui.window()));
}

fn show_about(ui: &Ui) {
    let about = adw::AboutDialog::builder()
        .application_name("mirai")
        .application_icon("io.github.mirai.Mirai")
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Huang Zhaobin")
        .comments("A KataGo analysis and playing board for GNOME.")
        .build();
    about.present(Some(&ui.window()));
}

// -- actions ----------------------------------------------------------------------------

/// Deletes the branch starting at the cursor and steps back to its parent.
fn delete_branch(ui: &Ui) {
    let id = ui.state.cursor();
    let parent = ui.state.tree().parent(id);
    let Some(parent) = parent else {
        ui.state.toast("The root node cannot be deleted");
        return;
    };
    ui.comment_node.set(None);
    ui.state.set_cursor(parent);
    ui.comment_node.set(None);
    ui.state.with_tree_mut(|t| t.delete_branch(id));
    load_comment(ui);
    update_scale(ui);
}

/// The body of a window action.
type UiAction = Box<dyn Fn(&Ui)>;

fn install_actions(ui: &Ui) {
    let group = gio::SimpleActionGroup::new();
    let weak = ui.weak_window();
    let add = |name: &str, f: UiAction| {
        let action = gio::SimpleAction::new(name, None);
        let weak = weak.clone();
        action.connect_activate(move |_, _| with_window_ui(&weak, |ui| f(ui)));
        group.add_action(&action);
    };

    add("first", Box::new(|ui| ui.state.go_first()));
    add("last", Box::new(|ui| ui.state.go_last()));
    add("prev", Box::new(|ui| ui.state.go_prev()));
    add("next", Box::new(|ui| ui.state.go_next()));
    add("prev10", Box::new(|ui| ui.state.go_back(10)));
    add("next10", Box::new(|ui| ui.state.go_forward(10)));
    add("branch-prev", Box::new(|ui| ui.state.go_sibling(-1)));
    add("branch-next", Box::new(|ui| ui.state.go_sibling(1)));

    add(
        "toggle-analysis",
        Box::new(|ui| ui.state.set_live_analysis(!ui.state.live_analysis())),
    );
    // The view toggles are stateful so the View menu renders a check mark next to whatever
    // is on. State follows the source of truth — `AppState`, or the split view — rather than
    // a copy kept here, so a change made by an accelerator, the header button or the menu
    // shows up in all three.
    let toggle = |name: &str, initial: bool, flip: UiAction| {
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        let weak = weak.clone();
        action.connect_activate(move |_, _| with_window_ui(&weak, |ui| flip(ui)));
        group.add_action(&action);
        action
    };

    let ownership = toggle(
        "toggle-ownership",
        ui.state.ownership_overlay(),
        Box::new(|ui| {
            ui.state
                .set_ownership_overlay(!ui.state.ownership_overlay())
        }),
    );
    ui.state.connect_ownership_overlay_notify(move |state| {
        ownership.set_state(&state.ownership_overlay().to_variant());
    });

    let policy = toggle(
        "toggle-policy",
        ui.state.policy_overlay(),
        Box::new(|ui| ui.state.set_policy_overlay(!ui.state.policy_overlay())),
    );
    ui.state.connect_policy_overlay_notify(move |state| {
        policy.set_state(&state.policy_overlay().to_variant());
    });

    let coords = toggle(
        "toggle-coords",
        ui.state.show_coordinates(),
        Box::new(|ui| ui.state.set_show_coordinates(!ui.state.show_coordinates())),
    );
    ui.state.connect_show_coordinates_notify(move |state| {
        coords.set_state(&state.show_coordinates().to_variant());
    });

    let numbers = toggle(
        "toggle-move-numbers",
        ui.state.show_move_numbers(),
        Box::new(|ui| {
            ui.state
                .set_show_move_numbers(!ui.state.show_move_numbers())
        }),
    );
    ui.state.connect_show_move_numbers_notify(move |state| {
        numbers.set_state(&state.show_move_numbers().to_variant());
    });

    let split = ui.window().split();
    let sidebar = toggle(
        "toggle-sidebar",
        split.shows_sidebar(),
        Box::new(|ui| {
            let split = ui.window().split();
            split.set_show_sidebar(!split.shows_sidebar());
        }),
    );
    split.connect_show_sidebar_notify(move |split| {
        sidebar.set_state(&split.shows_sidebar().to_variant());
    });

    add(
        "pass",
        Box::new(|ui| {
            if ui.play.is_active() {
                ui.play.pass();
                return;
            }
            let to_play = ui.state.to_play();
            if let Err(error) = ui.state.play_move(to_play, Point::PASS) {
                ui.state.toast_illegal_move(error);
            }
        }),
    );
    add(
        "undo",
        Box::new(|ui| {
            if ui.play.is_active() {
                ui.play.undo();
            } else {
                delete_branch(ui);
            }
        }),
    );
    add("delete-branch", Box::new(delete_branch));
    add("resign", Box::new(|ui| ui.play.resign()));

    add("open", Box::new(do_open));
    add("download-fox", Box::new(do_download_fox));
    add("clear-board", Box::new(do_clear_board));
    add("save", Box::new(do_save));
    add("save-as", Box::new(do_save_as));

    add(
        "copy-sgf",
        Box::new(|ui| {
            flush_comment(ui);
            let text = sgf_text(ui);
            match gdk::Display::default() {
                Some(display) => {
                    display.clipboard().set_text(&text);
                    ui.state.toast("Game record copied");
                }
                None => ui.state.toast("No display to copy through"),
            }
        }),
    );
    add(
        "paste-sgf",
        Box::new(|ui| {
            let Some(display) = gdk::Display::default() else {
                return;
            };
            let clipboard = display.clipboard();
            let weak = ui.weak_window();
            glib::spawn_future_local(async move {
                let text = match clipboard.read_text_future().await {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        with_window_ui(&weak, |ui| {
                            ui.state.toast("The clipboard holds no text");
                        });
                        return;
                    }
                    Err(e) => {
                        with_window_ui(&weak, |ui| {
                            ui.state.toast(format!("Clipboard: {e}"));
                        });
                        return;
                    }
                };
                let parsed = sgf::parse_str(&text);
                with_window_ui(&weak, |ui| match parsed {
                    Ok(mut trees) if !trees.is_empty() => {
                        let tree = trees.remove(0);
                        let moves = tree.main_line().len().saturating_sub(1);
                        adopt(ui, tree, None);
                        // Nothing on disk holds this record: it is unsaved from the start.
                        ui.state.set_modified(true);
                        ui.state.toast(format!("Pasted a game of {moves} moves"));
                    }
                    Ok(_) => ui.state.toast("The clipboard holds no game record"),
                    Err(e) => ui.state.toast(format!("Clipboard: {e}")),
                });
            });
        }),
    );

    add("analyse-game", Box::new(|ui| ui.batch.start()));
    add("score", Box::new(do_score));
    add(
        "new-game",
        Box::new(|ui| {
            let weak = ui.weak_window();
            crate::new_game::present(&ui.window(), &ui.state, move |setup| {
                with_window_ui(&weak, |ui| {
                    ui.batch.cancel();
                    ui.comment_node.set(None);
                    *ui.file.borrow_mut() = None;
                    ui.state.set_file_path(String::new());
                    ui.play.start(setup);
                });
            });
        }),
    );
    add(
        "preferences",
        Box::new(|ui| crate::prefs::present(&ui.window(), &ui.state)),
    );
    add("shortcuts", Box::new(show_shortcuts));
    add("about", Box::new(show_about));

    // Stateful so the engine menu can render a radio dot next to the live profile.
    let initial = ui.state.config().active_engine.clone().unwrap_or_default();
    let set_engine = gio::SimpleAction::new_stateful(
        "set-engine",
        Some(glib::VariantTy::STRING),
        &initial.to_variant(),
    );
    {
        let weak = weak.clone();
        set_engine.connect_activate(move |action, param| {
            let Some(name) = param.and_then(|p| p.str()) else {
                return;
            };
            action.set_state(&name.to_variant());
            with_window_ui(&weak, |ui| ui.state.activate_profile(name));
        });
    }
    group.add_action(&set_engine);

    let window = ui.window();
    window.insert_action_group("win", Some(&group));

    if let Some(app) = window.application() {
        for (action, accels) in [
            ("win.first", &["Home"][..]),
            ("win.last", &["End"]),
            ("win.prev", &["Left"]),
            ("win.next", &["Right"]),
            ("win.prev10", &["Page_Up"]),
            ("win.next10", &["Page_Down"]),
            ("win.branch-prev", &["Up"]),
            ("win.branch-next", &["Down"]),
            ("win.toggle-analysis", &["space"]),
            ("win.pass", &["p"]),
            ("win.undo", &["<Control>z"]),
            ("win.delete-branch", &["Delete"]),
            ("win.open", &["<Control>o"]),
            ("win.download-fox", &["<Control><Shift>o"]),
            ("win.clear-board", &["<Control><Shift>n"]),
            ("win.save", &["<Control>s"]),
            ("win.save-as", &["<Control><Shift>s"]),
            ("win.copy-sgf", &["<Control>c"]),
            ("win.paste-sgf", &["<Control>v"]),
            ("win.toggle-ownership", &["o"]),
            ("win.toggle-policy", &["y"]),
            ("win.toggle-coords", &["c"]),
            ("win.toggle-move-numbers", &["n"]),
            ("win.toggle-sidebar", &["F9"]),
            ("win.analyse-game", &["<Control>a"]),
            ("win.new-game", &["<Control>n"]),
            ("win.score", &["<Control>e"]),
        ] {
            app.set_accels_for_action(action, accels);
        }
    }
}

#[cfg(test)]
mod tests {

    use super::{blank_tree, record_label, tree_has_content};
    use mirai_core::{Color, GameInfo, GameTree, MarkKind, Point, RuleSet, Size};

    fn empty_tree() -> GameTree {
        GameTree::new(GameInfo::new(Size::square(19), RuleSet::Chinese))
    }

    #[test]
    fn a_blank_record_is_not_worth_autosaving() {
        // This is the case that used to greet users with "mirai did not shut down
        // cleanly" after a session in which they did nothing at all.
        assert!(!tree_has_content(&empty_tree()));
    }

    #[test]
    fn a_single_move_makes_a_record_worth_saving() {
        let mut tree = empty_tree();
        let root = tree.root();
        tree.play(root, Color::Black, Size::square(19).point(3, 3))
            .expect("D16 is legal on an empty board");
        assert!(tree_has_content(&tree));
    }

    #[test]
    fn a_pass_still_counts_as_a_move() {
        let mut tree = empty_tree();
        let root = tree.root();
        tree.play(root, Color::Black, Point::PASS).expect("pass");
        assert!(tree_has_content(&tree));
    }

    #[test]
    fn a_comment_or_setup_stone_alone_is_enough() {
        let mut commented = empty_tree();
        let root = commented.root();
        commented.set_comment(root, "  ");
        assert!(
            !tree_has_content(&commented),
            "whitespace is not real content"
        );
        commented.set_comment(root, "study this");
        assert!(tree_has_content(&commented));

        let mut setup = empty_tree();
        let root = setup.root();
        setup.set_setup_stone(root, Size::square(19).point(3, 3), Some(Color::Black));
        assert!(tree_has_content(&setup));
    }

    #[test]
    fn content_deep_in_a_variation_is_found() {
        let mut tree = empty_tree();
        let root = tree.root();
        // An empty node chain with a move only at the very end.
        let a = tree.add_child(root);
        let b = tree.add_child(a);
        assert!(!tree_has_content(&tree));
        tree.play(b, Color::White, Size::square(19).point(15, 15))
            .expect("Q4 is legal");
        assert!(tree_has_content(&tree));
    }

    #[test]
    fn marks_alone_do_not_arm_the_restore_prompt() {
        // Marks are review annotations on an otherwise blank board; they are not worth
        // interrupting the next launch for.
        let mut tree = empty_tree();
        let root = tree.root();
        tree.toggle_mark(root, MarkKind::Triangle, Size::square(19).point(3, 3));
        assert!(!tree_has_content(&tree));
    }

    #[test]
    fn an_unsaved_record_is_named_by_who_played_it() {
        // A download or a paste has no file to be named after; the header bar used to say
        // "Untitled" even though the record knows both players.
        let mut info = GameInfo::new(Size::square(19), RuleSet::Chinese);
        info.players[0].name = "柯洁".to_string();
        info.players[1].name = "申真谞".to_string();
        assert_eq!(record_label(&info).as_deref(), Some("柯洁 vs 申真谞"));

        info.players[1].name.clear();
        assert_eq!(record_label(&info).as_deref(), Some("柯洁 vs ?"));

        info.players[0].name.clear();
        assert_eq!(record_label(&info), None, "nameless falls back to Untitled");

        info.event = "LG Cup".to_string();
        assert_eq!(record_label(&info).as_deref(), Some("LG Cup"));
    }

    #[test]
    fn clearing_the_board_keeps_size_rules_and_komi() {
        let mut info = GameInfo::new(Size::square(9), RuleSet::Japanese);
        info.komi = 0.5;
        info.players[0].name = "Black".to_string();
        let mut tree = GameTree::new(info);
        let root = tree.root();
        tree.play(root, Color::Black, Size::square(9).point(2, 2))
            .expect("C7 is legal");

        let blank = blank_tree(&tree);
        assert_eq!(blank.info.size, Size::square(9));
        assert_eq!(blank.info.rules, RuleSet::Japanese);
        assert_eq!(blank.info.komi, 0.5);
        assert!(
            blank.info.players[0].name.is_empty(),
            "a cleared board is a new untitled record"
        );
        assert!(!tree_has_content(&blank));
    }
}
