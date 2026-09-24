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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use mirai_client::dead_from_ownership;
use mirai_core::{Color, GameInfo, GameTree, MarkKind, NodeId, Point, sgf};
use mirai_engine::{Report, SubEvent, Want};

use crate::app::{AppState, Change, EditorTool, NodeRef};
use crate::batch::BatchAnalysis;
use crate::config::Config;
use crate::panels::AnalysisPanel;
use crate::play::{PlayController, PlayState};
use crate::util;
use crate::widgets::board::BoardClick;
use crate::widgets::{BoardView, MoveTreeView, WinrateGraph};
use crate::window_shell::MiraiWindow;

/// Autosave cadence, in seconds.
const AUTOSAVE_SECS: u32 = 30;
/// Visit cap for the one-off score-estimate query.
const SCORE_VISITS: u32 = 400;
#[derive(Default)]
struct TaskSlot(RefCell<Option<glib::JoinHandle<()>>>);

impl TaskSlot {
    fn replace(&self, task: glib::JoinHandle<()>) {
        self.abort();
        *self.0.borrow_mut() = Some(task);
    }

    fn abort(&self) -> bool {
        let Some(task) = self.0.borrow_mut().take() else {
            return false;
        };
        task.abort();
        true
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
    /// Applying an open. Aborted on close so a result cannot land on a window that is gone.
    load: TaskSlot,
    /// Waiting for the stale-autosave scan. Aborted on close so this window does not
    /// consume a leftover another window should be offered.
    restore: TaskSlot,
    /// The glib side of an autosave write. The blocking write is gated separately, because
    /// aborting this future must not cancel a write shutdown still has to wait out.
    autosave_write: TaskSlot,
}

impl WindowTasks {
    fn abort_all(&self) {
        self.score.abort();
        self.autosave.remove();
        self.load.abort();
        self.restore.abort();
        self.autosave_write.abort();
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
    move_position: gtk::Label,
    pub move_scale: gtk::Scale,
    pub engine_menu: gtk::MenuButton,
    pub analysis: AnalysisPanel,
    board: BoardView,
    winrate: WinrateGraph,
    move_tree: MoveTreeView,
    pub file: RefCell<Option<PathBuf>>,
    title: adw::WindowTitle,
    clock_box: gtk::Box,
    play_bar: gtk::Box,
    clock_black: gtk::Label,
    clock_white: gtk::Label,
    play_controls: gtk::Box,
    pass_button: gtk::Button,
    undo_button: gtk::Button,
    retry_button: gtk::Button,
    resign_button: gtk::Button,
    analysis_stack: adw::ViewStack,
    /// This window's Fox picker, built the first time it is asked for. One per
    /// window: libadwaita refuses to present one dialog in two windows at once.
    fox_picker: RefCell<Option<crate::fox_picker::FoxPickerDialog>>,
    comment_node: Cell<Option<NodeRef>>,
    scale_guard: Cell<bool>,
    pending_auto_analyse: Cell<bool>,
    /// `Some(sidebar was shown)` while a game hides the review surfaces; see
    /// [`sync_play_layout`].
    play_layout: Cell<Option<bool>>,
    /// The View menu's Win-Rate Graph switch; see [`sync_graph`].
    show_graph: Cell<bool>,
    /// Bumped on every open and on every wholesale record replacement. A slower
    /// earlier read must not replace a later one, or a New Game the user has since
    /// started. In-place edits are caught separately, by the document token.
    load_generation: Cell<u64>,
    tasks: WindowTasks,
    autosave: Option<AutosaveFile>,
    win_actions: gio::SimpleActionGroup,
}

impl Ui {
    /// This window, while it still exists.
    ///
    /// A `WeakRef` *is* an `Option`, and `expect`ing that away was the one sharp edge left in
    /// `Ui`: the value is dropped from `MiraiWindow::dispose`, where the reference may already
    /// be cleared, so any future line in the release path that wanted the window would have
    /// panicked during teardown. Callers that only need a dialog parent pass this straight
    /// through as `Option`.
    fn window(&self) -> Option<MiraiWindow> {
        self.window.upgrade()
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
    /// The window itself is unreachable from here in every sense that matters. `take_ui`
    /// runs first, so every [`AppState`] hook that tries to re-enter through `with_ui` finds
    /// no state and does nothing, and [`Ui::window`] hands back an `Option` that is already
    /// `None` once `dispose` has cleared the weak reference. Work that genuinely needs the
    /// widget tree therefore belongs at the call site, ahead of the drop — it cannot be made
    /// to work by reaching for it in here.
    fn drop(&mut self) {
        // Ordering, not necessity: the slots would release themselves when `tasks` drops,
        // but the timers must not fire while the tree is being flushed.
        self.tasks.abort_all();
        flush_comment(self);
        self.batch.cancel();
        self.play.stop();
        self.state.cancel_tasks();
        self.state.set_live_analysis(false);
        // A plain value by now: the graph recorded its height on its last allocation.
        self.state.config_mut().ui.graph_height =
            u16::try_from(self.winrate.preferred_height()).unwrap_or(u16::MAX);
        self.state.config_mut().ui.show_graph = self.show_graph.get();
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
    // Reading every leftover autosave is the slow part of the first window. Start it
    // before the widgets exist so that work is not on the GTK thread.
    ensure_stale_scan(&runtime);
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
    let board = BoardView::new(&state);
    let winrate = WinrateGraph::new(&state);
    let move_tree = MoveTreeView::new(&state);
    let analysis = AnalysisPanel::new(&state);
    let comment = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    comment.buffer().set_enable_undo(true);
    let play = PlayController::new(&state, &window);
    let batch = BatchAnalysis::new(&state, &window);

    // The custom GPU-rendered views are stateful Rust widgets. Blueprint owns their static
    // containers; Rust only inserts the dynamic instances.
    board.set_hexpand(true);
    board.set_vexpand(true);
    winrate.set_hexpand(true);
    let show_graph = state.config().ui.show_graph;
    winrate.set_visible(show_graph);
    let content = window.content_paned();
    content.set_start_child(Some(&board));
    content.set_end_child(Some(&winrate));
    window.banner_slot().append(batch.banner());

    let analysis_stack = adw::ViewStack::new();
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
    engine_menu.set_tooltip_text(Some(&format!("Analysis Engine — {}", state.engine_label())));

    let split = window.split();
    set_candidate_sidebar_width(&split, false);
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
        SIDEBAR_BREAKPOINT_SP,
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
    let retry_button = window.retry_button();
    let resign_button = window.resign_button();
    let move_scale = window.move_scale();
    let move_position = window.move_position();
    move_scale.set_increments(1.0, 10.0);

    window.install_ui(Ui {
        window: window.downgrade(),
        state: state.clone(),
        toasts: toasts.clone(),
        comment,
        play,
        batch,
        analysis: analysis.clone(),
        move_position,
        board: board.clone(),
        winrate: winrate.clone(),
        move_tree: move_tree.clone(),
        move_scale,
        engine_menu,
        file: RefCell::new(None),
        title,
        clock_box,
        play_bar: window.play_bar(),
        clock_black,
        clock_white,
        play_controls,
        pass_button,
        undo_button,
        retry_button,
        resign_button,
        analysis_stack,
        fox_picker: RefCell::new(None),
        comment_node: Cell::new(None),
        scale_guard: Cell::new(false),
        pending_auto_analyse: Cell::new(false),
        play_layout: Cell::new(None),
        show_graph: Cell::new(show_graph),
        load_generation: Cell::new(0),
        tasks: WindowTasks::default(),
        autosave: next_autosave_file(),
        win_actions: gio::SimpleActionGroup::new(),
    });
    window.with_ui(|ui| {
        install_actions(&window, ui);
        connect_state(ui);
        connect_comment(ui);
        connect_scale(ui);
        connect_editor_tools(&window, ui);
    });
    {
        let weak = window.downgrade();
        state.set_change_hook(move |change| {
            with_window_ui(&weak, |ui| handle_change(ui, change));
        });
    }

    // Cross-widget wiring the two panels cannot do for themselves.
    window.with_ui(|ui| ui.play.attach_board(&board));
    window.with_ui(install_board_hook);
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
        sync_play_editability(ui);
        update_editor_actions(ui);
        update_scale(ui);
        update_title(ui);
        update_subtitle(ui);
        load_comment(ui);
        ui.analysis.refresh();
        ui.move_tree.refresh();
        ui.winrate.refresh();
    });

    window.with_ui(install_autosave);
    connect_close(&window);

    // Start the configured engine, if any.
    let active = state.config().active_profile().map(|p| p.name.clone());
    if let Some(name) = active {
        state.activate_profile(&name);
    }

    fit_default_size(&window, &winrate);
    window.present();

    crate::render_probe::install(&window);

    window.with_ui(|ui| {
        if let Some(path) = path {
            load_sgf(ui, &path, Origin::File);
        } else {
            offer_stale_when_ready(ui);
        }
    });
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
        state.connect_status_notify(move |_| with_window_ui(&weak, update_subtitle));
    }
    state.connect_engine_label_notify(move |state| {
        with_window_ui(&weak, |ui| {
            ui.engine_menu
                .set_tooltip_text(Some(&format!("Analysis Engine — {}", state.engine_label())));
        });
    });
}

fn handle_change(ui: &Ui, change: Change) {
    match change {
        Change::BeforeEdit => flush_comment(ui),
        Change::Edit {
            positions_changed,
            structure_changed,
        } => {
            if positions_changed || structure_changed {
                ui.batch.cancel();
            }
            if positions_changed && ui.tasks.score.abort() {
                ui.state.set_status(String::new());
            }
            update_editor_actions(ui);
        }
        Change::Editor => {
            if ui.state.editor_tool() != EditorTool::Play {
                set_editor_visible(ui, true);
            }
            update_editor_actions(ui);
            if ui.state.editor_tool() != EditorTool::Play {
                ui.board.clear_preview();
            }
            // The ghost stone under the pointer follows the tool.
            ui.board.queue_draw();
        }
        Change::Tree => {
            ui.board.close_menu();
            update_scale(ui);
            update_analysis_page(ui);
            update_title(ui);
            ui.board.refresh_tree();
            ui.move_tree.refresh();
            ui.winrate.refresh();
            refresh_blunders(ui);
            ui.analysis.refresh();
            update_editor_actions(ui);
        }
        Change::Cursor => {
            ui.board.close_menu();
            load_comment(ui);
            update_scale(ui);
            update_analysis_page(ui);
            update_clocks(ui);
            ui.board.refresh_cursor();
            ui.move_tree.refresh();
            ui.winrate.refresh_cursor();
            ui.analysis.refresh();
            update_editor_actions(ui);
        }
        Change::Report => {
            ui.board.refresh_report();
            ui.winrate.refresh();
            ui.analysis.refresh();
            update_analysis_page(ui);
        }
        Change::Engine => {
            refresh_engine_menu(ui);
            update_analysis_page(ui);
            ui.analysis.refresh();
            update_subtitle(ui);
            maybe_auto_analyse(ui);
            ui.play.retry_if_engine_ready();
        }
        Change::Toast(text) => ui.toasts.add_toast(adw::Toast::new(&text)),
        Change::Play => {
            update_clocks(ui);
            update_play_controls(ui);
            sync_play_editability(ui);
            ui.board.set_play_locked(
                ui.play.is_active() && !matches!(ui.play.play_state(), PlayState::HumanTurn),
            );
            update_editor_actions(ui);
        }
        Change::BatchProgress(_, _) => schedule_batch_projection(ui),
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
            let target = ui.state.with_tree_cached(|t| line_node(t, cursor, want));
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

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = ui.weak_window();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(redo) = comment_buffer_op(key, modifiers) else {
            return glib::Propagation::Proceed;
        };
        with_window_ui(&weak, |ui| apply_comment_buffer_op(ui, redo));
        glib::Propagation::Stop
    });
    ui.comment.add_controller(keys);

    // Application accels capture at the window; stop them while the comment
    // view has focus even if the buffer can no longer undo.
    if let Some(window) = ui.window() {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = ui.weak_window();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(redo) = comment_buffer_op(key, modifiers) else {
                return glib::Propagation::Proceed;
            };
            let Some(window) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let handled = window
                .with_ui(|ui| {
                    if !ui.comment.has_focus() {
                        return false;
                    }
                    apply_comment_buffer_op(ui, redo);
                    true
                })
                .unwrap_or(false);
            if handled {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(keys);
    }
}

/// `Some(true)` is redo, `Some(false)` is undo.
fn comment_buffer_op(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<bool> {
    if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
        return None;
    }
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    if key == gdk::Key::z || key == gdk::Key::Z {
        Some(shift)
    } else if key == gdk::Key::y || key == gdk::Key::Y {
        Some(true)
    } else {
        None
    }
}

fn apply_comment_buffer_op(ui: &Ui, redo: bool) {
    let buffer = ui.comment.buffer();
    if redo {
        buffer.redo();
    } else {
        buffer.undo();
    }
}

fn win_simple(ui: &Ui, name: &str) -> Option<gio::SimpleAction> {
    ui.win_actions
        .lookup_action(name)
        .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
}

fn color_name(color: Color) -> &'static str {
    match color {
        Color::Black => "black",
        Color::White => "white",
    }
}

fn parse_color(name: &str) -> Option<Color> {
    match name {
        "black" => Some(Color::Black),
        "white" => Some(Color::White),
        _ => None,
    }
}

fn connect_editor_tools(window: &MiraiWindow, ui: &Ui) {
    for group in [window.stone_tools(), window.mark_tools()] {
        let weak = ui.weak_window();
        group.connect_active_name_notify(move |group| {
            let Some(name) = group.active_name() else {
                return;
            };
            with_window_ui(&weak, |ui| {
                // Projection updates select the already-current tool, not a new edit.
                if name != ui.state.editor_tool().as_str() {
                    let _ = group.activate_action("win.edit-tool", Some(&name.to_variant()));
                }
            });
        });
    }
}

fn update_editor_actions(ui: &Ui) {
    let active = ui.play.is_active();
    let human = matches!(ui.play.play_state(), PlayState::HumanTurn);
    if let Some(action) = win_simple(ui, "undo") {
        action.set_enabled(if active { true } else { ui.state.can_undo() });
    }
    if let Some(action) = win_simple(ui, "redo") {
        action.set_enabled(!active && ui.state.can_redo());
    }
    if let Some(action) = win_simple(ui, "to-play") {
        action.set_enabled(!active);
        action.set_state(&color_name(ui.state.to_play()).to_variant());
    }
    if let Some(window) = ui.window() {
        let (icon, label, tooltip) = match ui.state.to_play() {
            Color::Black => (
                "mirai-play-black",
                "Play — Black to Play",
                "Play — Black to Play; Left-click to Play, Right-click to Take Back and Delete the Current Branch",
            ),
            Color::White => (
                "mirai-play-white",
                "Play — White to Play",
                "Play — White to Play; Left-click to Play, Right-click to Take Back and Delete the Current Branch",
            ),
        };
        let button = window.play_tool();
        window.play_tool_icon().set_icon_name(Some(icon));
        button.set_tooltip(tooltip);
        button.set_label(Some(label));
        let name = ui.state.editor_tool().as_str();
        for group in [window.stone_tools(), window.mark_tools()] {
            group.set_sensitive(!active);
            group.set_active_name(group.toggle_by_name(name).map(|_| name));
        }
    }
    if let Some(action) = win_simple(ui, "edit-tool") {
        action.set_enabled(!active);
        action.set_state(&ui.state.editor_tool().as_str().to_variant());
    }
    if let Some(action) = win_simple(ui, "play-at") {
        action.set_enabled(!active || human);
    }
    if let Some(action) = win_simple(ui, "promote-line-at") {
        action.set_enabled(!active);
    }
    if let Some(action) = win_simple(ui, "delete-branch-at") {
        action.set_enabled(!active);
    }
    if let Some(action) = win_simple(ui, "delete-branch") {
        action.set_enabled(!active);
    }
}

fn set_editor_visible(ui: &Ui, visible: bool) {
    let Some(window) = ui.window() else { return };
    let revealer = window.editor_revealer();
    if revealer.reveals_child() == visible {
        return;
    }
    if !visible {
        ui.state.set_editor_tool(EditorTool::Play);
        if gtk::prelude::GtkWindowExt::focus(&window)
            .is_some_and(|focused| focused.is_ancestor(&window.editor_toolbar()))
        {
            window.editor_toggle().grab_focus();
        }
    }
    revealer.set_reveal_child(visible);
}

fn sync_play_editability(ui: &Ui) {
    let active = ui.play.is_active();
    if active {
        set_editor_visible(ui, false);
        ui.state.set_editor_tool(EditorTool::Play);
    }
    if let Some(action) = win_simple(ui, "toggle-editor") {
        action.set_enabled(!active);
    }
    if let Some(window) = ui.window() {
        window.editor_toggle().set_sensitive(!active);
    }
    if active {
        if ui.comment.is_editable() {
            ui.comment_node.set(None);
            ui.comment.set_editable(false);
            load_comment(ui);
        }
    } else if !ui.comment.is_editable() {
        ui.comment.set_editable(true);
    }
}

fn install_board_hook(ui: &Ui) {
    let weak = ui.weak_window();
    ui.board.set_click_hook(move |click: BoardClick| {
        with_window_ui(&weak, |ui| on_board_click(ui, click));
    });
}

fn on_board_click(ui: &Ui, click: BoardClick) {
    let p = click.point;
    if p.is_pass() {
        return;
    }
    let shift = click.modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    let primary = click.button == gdk::BUTTON_PRIMARY;
    let secondary = click.button == gdk::BUTTON_SECONDARY;

    if secondary && shift {
        ui.board.show_menu(Some(p), None, !ui.play.is_active());
        return;
    }

    if ui.play.is_active() {
        if primary {
            ui.play.on_board_click(p);
        }
        return;
    }

    if secondary {
        ui.board.clear_preview();
        match ui.state.editor_tool() {
            EditorTool::Play => delete_branch(ui),
            EditorTool::Setup(color) => toggle_setup_stone(ui, p, color.other()),
            _ => {}
        }
        return;
    }

    if !primary {
        return;
    }

    if ui.state.editor_tool() != EditorTool::Play {
        ui.board.clear_preview();
    }

    match ui.state.editor_tool() {
        EditorTool::Play => {
            if let Err(error) = ui.state.play_move(p) {
                ui.state.toast_illegal_move(error);
            }
        }
        EditorTool::Setup(color) => toggle_setup_stone(ui, p, color),
        EditorTool::Triangle => {
            ui.state
                .with_edit_session(|session| session.toggle_mark(MarkKind::Triangle, p));
        }
        EditorTool::Square => {
            ui.state
                .with_edit_session(|session| session.toggle_mark(MarkKind::Square, p));
        }
        EditorTool::Circle => {
            ui.state
                .with_edit_session(|session| session.toggle_mark(MarkKind::Circle, p));
        }
        EditorTool::Cross => {
            ui.state
                .with_edit_session(|session| session.toggle_mark(MarkKind::Cross, p));
        }
        EditorTool::Label => {
            if let Some(window) = ui.window() {
                crate::label_editor::present(&window, p);
            }
        }
        EditorTool::EraseMark => {
            ui.state.with_edit_session(|session| session.clear_mark(p));
        }
    }
}

fn toggle_setup_stone(ui: &Ui, point: Point, color: Color) {
    ui.state.with_edit_session(|session| {
        let target = (session.position().board.at(point) != Some(color)).then_some(color);
        session.set_setup_stone(point, target)
    });
}

fn connect_close(window: &MiraiWindow) {
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

/// How many moves back the root is — which is also the cursor's index on its own line.
fn node_depth(tree: &GameTree, id: NodeId) -> usize {
    let mut depth = 0;
    let mut cur = id;
    while let Some(parent) = tree.parent(cur) {
        depth += 1;
        cur = parent;
    }
    depth
}

/// The node at index `want` along the line through the cursor: root → cursor, then main-line
/// continuations. `None` past either end.
///
/// Walked rather than materialised: the slider hands over one index per motion event, and
/// only ever wanted the one node.
fn line_node(tree: &GameTree, cursor: NodeId, want: usize) -> Option<NodeId> {
    let depth = node_depth(tree, cursor);
    let mut id = cursor;
    if want <= depth {
        for _ in 0..depth - want {
            id = tree.parent(id)?;
        }
        return Some(id);
    }
    for _ in 0..want - depth {
        id = *tree.children(id).first()?;
    }
    Some(id)
}

/// The last index on the line through the cursor, and the cursor's own index on it — the
/// move slider's range and value. Counted, not collected: this runs on every navigation step
/// and the `Vec` it used to build was read for its length alone.
fn line_extent(tree: &GameTree, cursor: NodeId) -> (usize, usize) {
    let index = node_depth(tree, cursor);
    let mut last = index;
    let mut id = cursor;
    while let Some(&child) = tree.children(id).first() {
        last += 1;
        id = child;
    }
    (last, index)
}

fn update_scale(ui: &Ui) {
    let cursor = ui.state.cursor();
    let (upper, index) = ui.state.with_tree_cached(|t| line_extent(t, cursor));
    ui.scale_guard.set(true);
    ui.move_scale.set_range(0.0, upper.max(1) as f64);
    ui.move_scale.set_value(index as f64);
    ui.scale_guard.set(false);
    ui.move_scale
        .set_tooltip_text(Some(&format!("Move {index} of {upper}")));
    let color = ui.state.to_play();
    ui.move_position
        .set_label(&format!("{index} / {upper} · {}", color.katago()));
    let tip = format!(
        "Move {index} of {upper} · {} to play",
        if color == Color::Black {
            "Black"
        } else {
            "White"
        }
    );
    ui.move_position.set_tooltip_text(Some(&tip));
    ui.move_position
        .update_property(&[gtk::accessible::Property::Label(&tip)]);
}

fn update_clocks(ui: &Ui) {
    let Some((black, white)) = ui.play.clocks() else {
        ui.clock_box.set_visible(false);
        ui.play_bar.set_visible(ui.play_controls.is_visible());
        return;
    };
    ui.clock_black.set_label(&format!("● {black}"));
    ui.clock_white.set_label(&format!("○ {white}"));
    ui.clock_box.set_visible(true);
    ui.play_bar.set_visible(true);

    let to_play = ui.state.to_play();
    for (label, mine) in [
        (&ui.clock_black, to_play == Color::Black),
        (&ui.clock_white, to_play == Color::White),
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
    let in_progress = matches!(
        state,
        PlayState::HumanTurn | PlayState::AiThinking | PlayState::AiStalled(_)
    );
    ui.play_controls.set_visible(in_progress);
    ui.pass_button
        .set_sensitive(matches!(state, PlayState::HumanTurn));
    ui.retry_button
        .set_visible(matches!(state, PlayState::AiStalled(_)));
    ui.undo_button.set_sensitive(ui.play.is_active());
    ui.resign_button
        .set_visible(in_progress && ui.play.is_human_vs_engine());
    ui.play_bar
        .set_visible(in_progress || ui.clock_box.is_visible());
    sync_play_layout(ui);
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
    if let Some(window) = ui.window() {
        window.set_title(Some(&format!("{shown} — mirai")));
    }
}

fn update_subtitle(ui: &Ui) {
    ui.title.set_subtitle(&ui.state.status());
}

fn update_analysis_page(ui: &Ui) {
    let has_analysis =
        !ui.state.config().engine_profiles.is_empty() || ui.state.last_report().is_some() || {
            let tree = ui.state.tree();
            tree.node(ui.state.cursor()).analysis.is_some()
        };
    ui.analysis_stack
        .set_visible_child_name(if has_analysis { "panel" } else { "empty" });
}

/// Rebuilds the sidebar blunder list from analyses already stored on the main line.
///
/// The list is a projection of the tree, not a leftover of the last sweep. Clearing it
/// on every `Change::Tree` made a finished review vanish when a comment flushed or a
/// result was written.
fn refresh_blunders(ui: &Ui) {
    let rows = crate::batch::blunders(&ui.state.tree(), ui.state.tree_epoch());
    ui.analysis.set_blunders(rows);
}

/// How long a burst of sweep results may wait before the graph and blunder list catch up.
/// One rebuild of the main line per tick, not one per result.
const BATCH_PROJECTION_MS: u64 = 250;

fn schedule_batch_projection(ui: &Ui) {
    if !ui.batch.note_projection_dirty() {
        return;
    }
    let weak = ui.weak_window();
    let id = glib::timeout_add_local(
        std::time::Duration::from_millis(BATCH_PROJECTION_MS),
        move || {
            with_window_ui(&weak, flush_batch_projection);
            glib::ControlFlow::Break
        },
    );
    ui.batch.store_projection_source(id);
}

fn flush_batch_projection(ui: &Ui) {
    // The source is completing; removing it again would warn.
    ui.batch.forget_projection_source();
    if ui.batch.take_projection_dirty() {
        ui.winrate.refresh();
        refresh_blunders(ui);
    }
}

// -- comment pane -----------------------------------------------------------------------

fn flush_comment(ui: &Ui) {
    if ui.play.is_active() {
        return;
    }
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
    ui.state.set_comment_at(node, &text);
    ui.comment_node.set(Some(node));
}

fn load_comment(ui: &Ui) {
    let node = ui.state.cursor_ref();
    let text = {
        let tree = ui.state.tree();
        tree.node(node.id).comment.clone()
    };
    ui.comment_node.set(None);
    let buffer = ui.comment.buffer();
    buffer.begin_irreversible_action();
    buffer.set_text(&text);
    buffer.end_irreversible_action();
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

/// Installs `tree` as the current game. `path` is remembered for plain Save; `unsaved`
/// marks a record nothing on disk holds — a paste or a download — which must go through
/// Save As and is dirty from the start.
fn adopt(ui: &Ui, tree: GameTree, path: Option<PathBuf>, unsaved: bool) {
    // A replacement the user asked for wins over an open that has not landed.
    supersede_open(ui);
    ui.play.stop();
    // Batch workers hold node IDs from this tree; stop them before replacing its arena.
    ui.batch.cancel();
    ui.comment_node.set(None);
    *ui.file.borrow_mut() = path.clone();
    if unsaved {
        ui.state.adopt_unsaved(tree);
    } else {
        ui.state
            .adopt_record(tree, path.as_ref().map(|p| p.display().to_string()));
    }
    load_comment(ui);
    update_scale(ui);
    update_analysis_page(ui);
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

/// Where a record being loaded came from.
///
/// An autosave is not the user's file: it has no Save target, and it must stay dirty until
/// Save As, or the title claims a recovered record is safely on disk when nothing holds it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    File,
    Recovered,
}

struct LoadedSgf {
    path: PathBuf,
    trees: Vec<GameTree>,
}

fn load_sgf(ui: &Ui, path: &Path, origin: Origin) {
    let generation = ui.load_generation.get().wrapping_add(1);
    ui.load_generation.set(generation);
    let document = ui.state.document_token();
    let path = path.to_path_buf();
    let runtime = ui.state.runtime();
    let weak = ui.weak_window();
    let join = runtime.spawn_blocking(move || -> Result<LoadedSgf, String> {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let trees = sgf::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(LoadedSgf { path, trees })
    });
    let handle = glib::spawn_future_local(async move {
        let result = join.await;
        with_window_ui(&weak, |ui| {
            if ui.load_generation.get() != generation {
                return;
            }
            ui.tasks.load.0.borrow_mut().take();
            if ui.state.document_token() != document {
                ui.state
                    .toast("The record changed while that file was opening, so it was not loaded");
                return;
            }
            match result {
                Ok(Ok(loaded)) => finish_load(ui, loaded, origin),
                Ok(Err(message)) => ui.state.toast(message),
                Err(error) => tracing::warn!(%error, "reading an SGF failed"),
            }
        });
    });
    ui.tasks.load.replace(handle);
}

/// An open that has not landed must not replace a record the user has since asked for.
fn supersede_open(ui: &Ui) {
    ui.load_generation
        .set(ui.load_generation.get().wrapping_add(1));
}

fn finish_load(ui: &Ui, loaded: LoadedSgf, origin: Origin) {
    let LoadedSgf { path, trees } = loaded;
    let remembered = (origin == Origin::File).then(|| path.clone());
    // A restored autosave is deleted only once its record is in this window. Deleting it
    // any earlier — before the read, before a superseded or cancelled load has been
    // dropped, or before a game is picked from a multi-game file — loses the only copy of
    // an unsaved game; left alone, it is offered again on the next start.
    let recovered = (origin == Origin::Recovered).then(|| path.clone());
    match trees.len() {
        0 => ui.state.toast("That file holds no game records"),
        1 => {
            let tree = trees.into_iter().next().expect("length checked");
            let moves = tree.main_line().len().saturating_sub(1);
            adopt(ui, tree, remembered, recovered.is_some());
            if let Some(autosave) = &recovered {
                let _ = std::fs::remove_file(autosave);
            }
            let label = file_label(&path);
            ui.state.toast(match origin {
                Origin::File => format!("Opened {label} ({moves} moves)"),
                Origin::Recovered => {
                    format!("Restored {moves} moves — use Save As to keep them")
                }
            });
        }
        _ => choose_game(ui, trees, remembered, recovered, file_label(&path)),
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

/// `recovered` is the autosave the records came from, deleted only once a game from it
/// has been adopted: Cancel keeps it for the next start.
fn choose_game(
    ui: &Ui,
    trees: Vec<GameTree>,
    path: Option<PathBuf>,
    recovered: Option<PathBuf>,
    label: String,
) {
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
            with_window_ui(&weak, |ui| {
                adopt(ui, tree, path.clone(), recovered.is_some());
                if let Some(autosave) = &recovered {
                    let _ = std::fs::remove_file(autosave);
                }
            });
        }
    });
    dialog.present(ui.window().as_ref());
}

fn do_open(ui: &Ui) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title("Open SGF");
    let (filters, default) = sgf_filters();
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&default));
    let future = dialog.open_future(ui.window().as_ref());
    let weak = ui.weak_window();
    glib::spawn_future_local(async move {
        match future.await {
            Ok(file) => match file.path() {
                Some(path) => with_window_ui(&weak, |ui| load_sgf(ui, &path, Origin::File)),
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
    let Some(window) = ui.window() else { return };
    let weak = ui.weak_window();
    crate::fox::present(&window, &ui.fox_picker, move |download| {
        with_window_ui(&weak, |ui| {
            let moves = download.tree.main_line().len().saturating_sub(1);
            adopt(ui, download.tree, None, true);
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
    adopt(ui, tree, None, false);
    // This is a new blank record, not an opened one.
    ui.pending_auto_analyse.set(false);
}

fn write_to(ui: &Ui, path: &Path) {
    let text = sgf_text(ui);
    match mirai_proto::atomic::write_atomic(path, text.as_bytes()) {
        Ok(()) => {
            *ui.file.borrow_mut() = Some(path.to_path_buf());
            ui.state.saved_to(path.display().to_string());
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
    let future = dialog.save_future(ui.window().as_ref());
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
/// drops — after any in-flight write has either finished or seen the cancellation — so a
/// clean close cannot leave a file a crash would be offered. Whatever is left on disk is by
/// definition what a crash left behind.
struct AutosaveGate {
    /// Held for the whole off-thread write. Shutdown takes it after setting `cancel`, so
    /// the delete cannot race a write that already decided to proceed.
    lock: Mutex<()>,
    cancel: AtomicBool,
    /// Set while a write is in flight. A tick that finds it set skips itself.
    in_flight: AtomicBool,
}

struct AutosaveFile {
    path: PathBuf,
    gate: Arc<AutosaveGate>,
}

impl AutosaveFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AutosaveFile {
    fn drop(&mut self) {
        self.gate.cancel.store(true, Ordering::Release);
        let _guard = self
            .gate
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = std::fs::remove_file(&self.path);
    }
}

fn next_autosave_file() -> Option<AutosaveFile> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    Some(AutosaveFile {
        path: Config::data_dir()
            .ok()?
            .join(format!("{}{n}.sgf", *AUTOSAVE_PREFIX)),
        gate: Arc::new(AutosaveGate {
            lock: Mutex::new(()),
            cancel: AtomicBool::new(false),
            in_flight: AtomicBool::new(false),
        }),
    })
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
/// Scanned once per process, on the runtime's blocking pool: the first window must not
/// stall the GTK thread reading and parsing every leftover. Files that no longer parse,
/// or hold nothing worth restoring, are deleted here rather than shown.
fn collect_stale_autosaves() -> Vec<PathBuf> {
    let Ok(dir) = Config::data_dir() else {
        return Vec::new();
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
            Some(games) => games.iter().any(GameTree::has_content),
            None => {
                let _ = std::fs::remove_file(path);
                false
            }
        }
    });
    sort_autosaves(&mut found);
    found
}

enum StaleScan {
    Pending(Vec<tokio::sync::oneshot::Sender<()>>),
    Ready(Vec<PathBuf>),
}

static STALE: LazyLock<Mutex<StaleScan>> =
    LazyLock::new(|| Mutex::new(StaleScan::Pending(Vec::new())));
static STALE_STARTED: AtomicBool = AtomicBool::new(false);

fn ensure_stale_scan(runtime: &tokio::runtime::Handle) {
    if STALE_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    runtime.spawn_blocking(|| publish_stale(collect_stale_autosaves()));
}

fn publish_stale(found: Vec<PathBuf>) {
    let waiters = {
        let mut slot = STALE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match std::mem::replace(&mut *slot, StaleScan::Ready(found)) {
            StaleScan::Pending(waiters) => waiters,
            StaleScan::Ready(_) => Vec::new(),
        }
    };
    for waiter in waiters {
        let _ = waiter.send(());
    }
}

async fn wait_for_stale_scan() {
    let rx = {
        let mut slot = STALE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &mut *slot {
            StaleScan::Ready(_) => None,
            StaleScan::Pending(waiters) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                waiters.push(tx);
                Some(rx)
            }
        }
    };
    if let Some(rx) = rx {
        let _ = rx.await;
    }
}

fn pop_stale() -> Option<PathBuf> {
    let mut slot = STALE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match &mut *slot {
        StaleScan::Ready(files) => files.pop(),
        StaleScan::Pending(_) => None,
    }
}

/// Offers one leftover autosave once the background scan has finished — unless the user
/// has already moved on. The scan is asynchronous, so a game started or an edit made in
/// the meantime would otherwise be replaced by accepting a prompt about an older record.
/// A skipped leftover stays on disk and in the list, for another window or the next start.
fn offer_stale_when_ready(ui: &Ui) {
    let weak = ui.weak_window();
    let document = ui.state.document_token();
    let generation = ui.load_generation.get();
    let handle = glib::spawn_future_local(async move {
        wait_for_stale_scan().await;
        with_window_ui(&weak, |ui| {
            ui.tasks.restore.0.borrow_mut().take();
            if ui.state.document_token() != document || ui.load_generation.get() != generation {
                return;
            }
            if let Some(autosave) = pop_stale() {
                offer_restore(ui, autosave);
            }
        });
    });
    ui.tasks.restore.replace(handle);
}

/// Newest last, so `pop` offers the most recent leftover first.
///
/// `autosave-<pid>-<start>-<n>.sgf` is ordered by `(start, n)`, not by the pid
/// that happens to come first in the name. Unparsable names sort first. The
/// legacy `autosave.sgf` is older than any stamped file.
fn autosave_rank(name: &str) -> (u8, u64, u32) {
    if let Some((start, n)) = autosave_stamp(name) {
        return (2, start, n);
    }
    if name == "autosave.sgf" {
        return (1, 0, 0);
    }
    (0, 0, 0)
}

fn autosave_stamp(name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix("autosave-")?.strip_suffix(".sgf")?;
    let (pid, rest) = rest.split_once('-')?;
    let (start, n) = rest.split_once('-')?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) || n.contains('-') {
        return None;
    }
    Some((start.parse().ok()?, n.parse().ok()?))
}

fn cmp_autosave_names(a: &str, b: &str) -> std::cmp::Ordering {
    autosave_rank(a)
        .cmp(&autosave_rank(b))
        .then_with(|| a.cmp(b))
}

fn sort_autosaves(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let an = a.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let bn = b.file_name().and_then(|name| name.to_str()).unwrap_or("");
        cmp_autosave_names(an, bn)
    });
}

fn write_autosave(ui: &Ui) {
    let Some(autosave) = ui.autosave.as_ref() else {
        return;
    };
    let gate = &autosave.gate;
    if gate.cancel.load(Ordering::Acquire) || gate.in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    let path = autosave.path().to_path_buf();
    if !ui.state.tree().has_content() {
        gate.in_flight.store(false, Ordering::Release);
        let _ = std::fs::remove_file(&path);
        return;
    }
    // The blocking task cannot borrow the tree. Clone what it needs while this thread
    // still holds the session; serialise, compress and write off the GTK thread.
    let include = ui.state.config().ui.save_analysis_in_sgf;
    let tree = ui.state.tree().clone();
    let gate = Arc::clone(&autosave.gate);
    let join = ui.state.runtime().spawn_blocking(move || {
        let _guard = gate
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _clear = ClearInFlight(&gate.in_flight);
        if gate.cancel.load(Ordering::Acquire) {
            return;
        }
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            tracing::warn!(%e, "could not create the data directory");
            return;
        }
        let text = sgf::write(&tree, include);
        if gate.cancel.load(Ordering::Acquire) {
            return;
        }
        if let Err(e) = mirai_proto::atomic::write_atomic(&path, text.as_bytes()) {
            tracing::warn!(%e, "could not write the autosave");
        }
    });
    let handle = glib::spawn_future_local(async move {
        let _ = join.await;
    });
    ui.tasks.autosave_write.replace(handle);
}

/// Clears the in-flight flag even if serialising panics, so a later tick is not stuck
/// skipping forever.
struct ClearInFlight<'a>(&'a AtomicBool);

impl Drop for ClearInFlight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
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
            with_window_ui(&weak, |ui| load_sgf(ui, &autosave, Origin::Recovered));
        } else {
            let _ = std::fs::remove_file(&autosave);
        }
    });
    dialog.present(ui.window().as_ref());
}

// -- score estimate ---------------------------------------------------------------------

fn do_score(ui: &Ui) {
    let Some(engine) = ui.state.engine() else {
        ui.state.toast("No engine to estimate the score with");
        return;
    };
    ui.tasks.score.abort();
    let target = ui.state.cursor_ref();
    let revision = ui.state.position_revision();
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
                        ui.tasks.score.0.borrow_mut().take();
                        ui.state.set_status(String::new());
                        ui.state.on_engine_error(e);
                    });
                    return;
                }
            }
        }
        with_window_ui(&weak, |ui| {
            ui.tasks.score.0.borrow_mut().take();
            ui.state.set_status(String::new());
            if ui.state.resolve_node(target) != Some(ui.state.cursor())
                || ui.state.position_revision() != revision
            {
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
    let dead = dead_from_ownership(board, report.ownership.as_deref().unwrap_or(&[]));
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
    dialog.present(ui.window().as_ref());
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
                ("Win-Rate Graph", "win.toggle-graph"),
            ][..],
        ),
        (
            "Game",
            &[
                ("New Game", "win.new-game"),
                ("Pass", "win.pass"),
                ("Undo Last Edit", "win.undo"),
                ("Redo", "win.redo"),
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
    dialog.present(ui.window().as_ref());
}

fn show_about(ui: &Ui) {
    let about = adw::AboutDialog::builder()
        .application_name("mirai")
        .application_icon("io.github.mirai.Mirai")
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Huang Zhaobin")
        .comments("A KataGo analysis and playing board for GNOME.")
        .build();
    about.present(ui.window().as_ref());
}

// -- actions ----------------------------------------------------------------------------

fn delete_branch(ui: &Ui) {
    delete_branch_id(ui, ui.state.cursor());
}

fn delete_branch_id(ui: &Ui, id: NodeId) {
    if ui.play.is_active() {
        return;
    }
    if !ui.state.tree().contains(id) {
        return;
    }
    if ui.state.tree().parent(id).is_none() {
        ui.state.toast("The root node cannot be deleted");
        return;
    }
    flush_comment(ui);
    ui.comment_node.set(None);
    ui.state
        .with_edit_session(|session| session.delete_branch_at(id));
    load_comment(ui);
}

fn promote_line_id(ui: &Ui, id: NodeId) {
    if ui.play.is_active() {
        return;
    }
    if !ui.state.tree().contains(id) {
        return;
    }
    ui.state
        .with_edit_session(|session| session.promote_to_main_line_at(id));
}

fn play_at_point(ui: &Ui, p: Point) {
    if ui.play.is_active() {
        if matches!(ui.play.play_state(), PlayState::HumanTurn) {
            ui.play.on_board_click(p);
        }
        return;
    }
    if let Err(error) = ui.state.play_move(p) {
        ui.state.toast_illegal_move(error);
    }
}

fn do_undo(ui: &Ui) {
    if ui.play.is_active() {
        ui.play.undo();
        return;
    }
    flush_comment(ui);
    ui.comment_node.set(None);
    ui.state.with_edit_session(|session| {
        session.undo();
    });
    load_comment(ui);
}

fn do_redo(ui: &Ui) {
    if ui.play.is_active() {
        return;
    }
    flush_comment(ui);
    ui.comment_node.set(None);
    ui.state.with_edit_session(|session| {
        session.redo();
    });
    load_comment(ui);
}

fn point_from_u32(raw: u32) -> Option<Point> {
    u16::try_from(raw).ok().map(Point)
}

/// The body of a window action.
type UiAction = Box<dyn Fn(&Ui)>;
type PointAction = Box<dyn Fn(&Ui, u32)>;
type ChoiceAction = Box<dyn Fn(&Ui, &str)>;

fn set_candidate_sidebar_width(split: &adw::OverlaySplitView, detailed: bool) {
    let width = if detailed { 386.0 } else { 300.0 };
    split.set_min_sidebar_width(width);
    split.set_max_sidebar_width(width);
}

/// At this width or narrower the sidebar folds into an overlay.
const SIDEBAR_BREAKPOINT_SP: f64 = 926.0;

/// The tallest default window: the board is as large as this makes it, and no larger.
const DEFAULT_HEIGHT_CAP: i32 = 960;

/// Sizes a new window so the board fills its area exactly.
///
/// The board is square, so any other aspect ratio leaves bare background beside or under
/// it. The window takes most of the shortest monitor's height, up to [`DEFAULT_HEIGHT_CAP`];
/// the board gets what is left after the header, the navigation bar and the graph; the width
/// is that board plus the sidebar. The chrome is measured rather than assumed, so text
/// scaling and a remembered graph height stay square. Below the sidebar breakpoint the
/// sidebar is an overlay and the window is only as wide as the board. A tiling compositor
/// ignores all of this.
fn fit_default_size(window: &MiraiWindow, graph: &WinrateGraph) {
    let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Vertical, -1).1;
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let monitors = display.monitors();
    let Some(monitor_height) = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .map(|m| m.geometry().height())
        .min()
    else {
        return;
    };
    let height = (monitor_height * 17 / 20).min(DEFAULT_HEIGHT_CAP);

    let board_view = window.board_view();
    let board = window
        .content_paned()
        .start_child()
        .expect("present() packs the board first");
    let chrome = natural(window.header_bar().upcast_ref()) + natural(board_view.upcast_ref())
        - natural(&board);

    let settings = window.settings();
    let split = window.split();
    let sidebar = adw::LengthUnit::Sp.to_px(split.max_sidebar_width(), Some(&settings));
    let breakpoint = adw::LengthUnit::Sp.to_px(SIDEBAR_BREAKPOINT_SP, Some(&settings));
    // The narrowest board that still keeps the sidebar docked beside it.
    let docked_side = (breakpoint - sidebar).floor() as i32 + 1;

    // A graph remembered from a taller screen may not fit this one. It gets at most a third
    // of what it shares with the board, and no more than leaves the board wide enough to
    // dock the sidebar -- unless that would take it under a quarter, when the sidebar
    // cannot dock at this height anyway. A hidden graph takes no room at all.
    let graph_height = if graph.is_visible() {
        natural(graph.upcast_ref())
    } else {
        0
    };
    let room = height - (chrome - graph_height);
    let cap = (room / 3).min((room - docked_side).max(room / 4));
    let graph_height = if graph.is_visible() {
        graph.limit_height(graph_height.min(cap))
    } else {
        0
    };
    let side = (room - graph_height).max(1);
    let chrome = height - side;

    let width = if side >= docked_side {
        (f64::from(side) + sidebar).round() as i32
    } else {
        side
    };
    tracing::debug!(width, height, chrome, "default window size");
    window.set_default_size(width, height);
}

/// A game shows the board and the play bar only. The graph, the navigation row and the
/// sidebar are review tools, and the analysis in them would be hints; they return when the
/// game is over, the sidebar as it was before the game.
fn sync_play_layout(ui: &Ui) {
    let playing = matches!(
        ui.play.play_state(),
        PlayState::HumanTurn | PlayState::AiThinking | PlayState::AiStalled(_) | PlayState::Scoring
    );
    if playing == ui.play_layout.get().is_some() {
        return;
    }
    let Some(window) = ui.window() else {
        return;
    };
    let split = window.split();
    // `play_layout` is what `sync_graph` and the sidebar guard read, so it changes first.
    let sidebar_shown = if playing {
        ui.play_layout.set(Some(split.shows_sidebar()));
        None
    } else {
        ui.play_layout.take()
    };
    sync_graph(ui, &window);
    window.nav().set_visible(!playing);
    // The sidebar is forced shut for the whole game (see `install_actions`), and the graph
    // hidden, so their toggles would be dead controls.
    window.sidebar_toggle().set_sensitive(!playing);
    for name in ["toggle-sidebar", "toggle-graph"] {
        if let Some(action) = win_simple(ui, name) {
            action.set_enabled(!playing);
        }
    }
    if playing {
        split.set_show_sidebar(false);
    } else if let Some(shown) = sidebar_shown {
        split.set_show_sidebar(shown);
    }
}

/// The graph shows when the View menu asks for it and no game hides it.
fn sync_graph(ui: &Ui, window: &MiraiWindow) {
    let visible = ui.show_graph.get() && ui.play_layout.get().is_none();
    if let Some(action) = win_simple(ui, "toggle-graph") {
        action.set_state(&ui.show_graph.get().to_variant());
    }
    if visible == ui.winrate.is_visible() {
        return;
    }
    if visible {
        // The divider position was the board's height when the graph went away, and the
        // window may have been resized since; hand the graph its remembered height instead.
        ui.winrate.pin();
        window.content_paned().set_property("position-set", false);
    }
    ui.winrate.set_visible(visible);
}

fn install_actions(window: &MiraiWindow, ui: &Ui) {
    let group = ui.win_actions.clone();
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

    let split = window.split();
    let sidebar = toggle("toggle-sidebar", split.shows_sidebar(), {
        let split = split.clone();
        Box::new(move |_: &Ui| split.set_show_sidebar(!split.shows_sidebar()))
    });
    {
        let weak = weak.clone();
        split.connect_show_sidebar_notify(move |split| {
            // Uncollapsing, and a breakpoint unapplying, both show the sidebar again; during
            // a game it stays shut, since it would show the engine's hints.
            let in_game = weak
                .upgrade()
                .and_then(|window| window.with_ui(|ui| ui.play_layout.get().is_some()));
            if in_game == Some(true) && split.shows_sidebar() {
                split.set_show_sidebar(false);
                return;
            }
            sidebar.set_state(&split.shows_sidebar().to_variant());
        });
    }

    toggle(
        "toggle-graph",
        ui.show_graph.get(),
        Box::new(|ui| {
            ui.show_graph.set(!ui.show_graph.get());
            if let Some(window) = ui.window() {
                sync_graph(ui, &window);
            }
        }),
    );

    let editor = window.editor_revealer();
    let editor_action = toggle(
        "toggle-editor",
        editor.reveals_child(),
        Box::new(|ui| {
            if !ui.play.is_active()
                && let Some(window) = ui.window()
            {
                set_editor_visible(ui, !window.editor_revealer().reveals_child());
            }
        }),
    );
    let weak_editor = ui.weak_window();
    window.editor_toggle().connect_toggled(move |button| {
        with_window_ui(&weak_editor, |ui| {
            if let Some(window) = ui.window()
                && button.is_active() != window.editor_revealer().reveals_child()
                && !ui.play.is_active()
            {
                set_editor_visible(ui, button.is_active());
            }
        });
    });
    let editor_toggle = window.editor_toggle();
    editor.connect_reveal_child_notify(move |revealer| {
        let visible = revealer.reveals_child();
        if editor_toggle.is_active() != visible {
            editor_toggle.set_active(visible);
        }
        if editor_action.state().and_then(|state| state.get::<bool>()) != Some(visible) {
            editor_action.set_state(&visible.to_variant());
        }
    });

    // Menu check items request a state change directly; activation should use the same path.
    let details =
        gio::SimpleAction::new_stateful("toggle-candidate-details", None, &false.to_variant());
    let weak_details = ui.weak_window();
    details.connect_change_state(move |action, value| {
        let Some(value) = value else {
            return;
        };
        let Some(detailed) = value.get::<bool>() else {
            return;
        };
        with_window_ui(&weak_details, |ui| {
            ui.analysis.set_detailed_columns(detailed);
            if let Some(window) = ui.window() {
                set_candidate_sidebar_width(&window.split(), detailed);
            }
            action.set_state(value);
        });
    });
    group.add_action(&details);

    add(
        "pass",
        Box::new(|ui| {
            if ui.play.is_active() {
                ui.play.pass();
                return;
            }
            if let Err(error) = ui.state.play_move(Point::PASS) {
                ui.state.toast_illegal_move(error);
            }
        }),
    );
    add("undo", Box::new(do_undo));
    add("redo", Box::new(do_redo));
    add("delete-branch", Box::new(delete_branch));
    add("resign", Box::new(|ui| ui.play.resign()));
    add("retry-ai", Box::new(|ui| ui.play.retry()));
    add(
        "board-menu",
        Box::new(|ui| {
            let Some(window) = ui.window() else {
                return;
            };
            let Some(bounds) = window.board_menu_button().compute_bounds(&ui.board) else {
                return;
            };
            // Popover pointing rectangles use the parent BoardView's coordinates.
            let anchor = gdk::Rectangle::new(
                bounds.x().round() as i32,
                bounds.y().round() as i32,
                bounds.width().ceil() as i32,
                bounds.height().ceil() as i32,
            );
            ui.board.show_menu(None, Some(anchor), !ui.play.is_active());
        }),
    );

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
                        // Nothing on disk holds this record: it is unsaved from the start.
                        adopt(ui, tree, None, true);
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
            let Some(window) = ui.window() else { return };
            let weak = ui.weak_window();
            crate::new_game::present(&window, &ui.state, move |setup| {
                with_window_ui(&weak, |ui| {
                    supersede_open(ui);
                    ui.batch.cancel();
                    ui.comment_node.set(None);
                    *ui.file.borrow_mut() = None;
                    ui.play.start(setup);
                });
            });
        }),
    );
    add(
        "preferences",
        Box::new(|ui| {
            if let Some(window) = ui.window() {
                crate::prefs::present(&window, &ui.state);
            }
        }),
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

    let add_u32 = |name: &str, handle: PointAction| {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let weak = weak.clone();
        action.connect_activate(move |_, param| {
            let Some(value) = param.and_then(|p| p.get::<u32>()) else {
                return;
            };
            with_window_ui(&weak, |ui| handle(ui, value));
        });
        group.add_action(&action);
    };
    add_u32(
        "play-at",
        Box::new(|ui, raw| {
            if let Some(p) = point_from_u32(raw) {
                play_at_point(ui, p);
            }
        }),
    );
    add_u32(
        "promote-line-at",
        Box::new(|ui, raw| promote_line_id(ui, NodeId(raw))),
    );
    add_u32(
        "delete-branch-at",
        Box::new(|ui, raw| delete_branch_id(ui, NodeId(raw))),
    );

    let add_choice = |name: &str, initial: &str, handle: ChoiceAction| {
        let action = gio::SimpleAction::new_stateful(
            name,
            Some(glib::VariantTy::STRING),
            &initial.to_variant(),
        );
        let weak = weak.clone();
        action.connect_activate(move |_, param| {
            let Some(value) = param.and_then(|p| p.str().map(str::to_owned)) else {
                return;
            };
            with_window_ui(&weak, |ui| handle(ui, &value));
        });
        group.add_action(&action);
    };
    add_choice(
        "to-play",
        color_name(ui.state.to_play()),
        Box::new(|ui, value| {
            if ui.play.is_active() {
                return;
            }
            let Some(color) = parse_color(value) else {
                return;
            };
            ui.state
                .with_edit_session(|session| session.set_to_play(color));
        }),
    );
    add_choice(
        "edit-tool",
        ui.state.editor_tool().as_str(),
        Box::new(|ui, value| {
            if ui.play.is_active() {
                return;
            }
            let Some(tool) = EditorTool::from_str(value) else {
                return;
            };
            ui.state.set_editor_tool(tool);
        }),
    );

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
            ("win.redo", &["<Control><Shift>z"]),
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
            ("win.toggle-graph", &["g"]),
            ("win.analyse-game", &["<Control>a"]),
            ("win.new-game", &["<Control>n"]),
            ("win.score", &["<Control>e"]),
        ] {
            app.set_accels_for_action(action, accels);
        }
    }
    update_editor_actions(ui);
}

#[cfg(test)]
mod tests {

    use super::{blank_tree, cmp_autosave_names, line_extent, line_node, record_label};
    use mirai_core::{Color, GameInfo, GameTree, RuleSet, Size};

    fn empty_tree() -> GameTree {
        GameTree::new(GameInfo::new(Size::square(19), RuleSet::Chinese))
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
        assert!(!blank.has_content());
    }

    /// The move slider's contract: its range is the whole line through the cursor — the moves
    /// behind it plus the main-line continuation ahead of it — and dragging to an index lands
    /// on that node. Both used to be read off a materialised `Vec` of the line, so the index
    /// arithmetic that replaced it is worth pinning.
    #[test]
    fn the_move_slider_spans_the_line_through_the_cursor() {
        let size = Size::square(19);
        let mut tree = empty_tree();
        let mut line = vec![tree.root()];
        for i in 0..6u8 {
            let colour = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            let at = *line.last().expect("the line starts at the root");
            line.push(tree.play(at, colour, size.point(i, 0)).expect("legal"));
        }
        // A variation must not lengthen the line: only `children[0]` continues it.
        tree.play(line[2], Color::Black, size.point(10, 10))
            .expect("legal");

        let cursor = line[3];
        assert_eq!(
            line_extent(&tree, cursor),
            (6, 3),
            "three moves behind the cursor, three ahead"
        );
        for (i, &id) in line.iter().enumerate() {
            assert_eq!(line_node(&tree, cursor, i), Some(id), "index {i}");
        }
        assert_eq!(line_node(&tree, cursor, line.len()), None, "past the end");
    }

    /// A larger pid must not sort as newer. The stamp is `(start, n)`; the legacy
    /// file is older than any stamp, and a name that does not parse sorts first.
    #[test]
    fn autosaves_sort_by_start_time_not_by_pid() {
        let mut names = vec![
            "autosave-100-2000-1.sgf",
            "autosave-99999-1000-1.sgf",
            "autosave.sgf",
            "autosave-not-a-stamp.sgf",
            "autosave-100-2000-2.sgf",
        ];
        names.sort_by(|a, b| cmp_autosave_names(a, b));
        assert_eq!(
            names,
            [
                "autosave-not-a-stamp.sgf",
                "autosave.sgf",
                "autosave-99999-1000-1.sgf",
                "autosave-100-2000-1.sgf",
                "autosave-100-2000-2.sgf",
            ]
        );
    }
}
