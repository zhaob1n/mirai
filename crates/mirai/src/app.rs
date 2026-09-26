// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `AppState` — the single source of truth every widget observes.
//!
//! It is a `glib::Object` subclass so display toggles can be bound with `notify::`, and it
//! emits a handful of custom signals for structural changes. Widgets never reach into each
//! other; they read `AppState` and listen to its signals.

use std::cell::{Cell, OnceCell, Ref, RefCell, RefMut};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use mirai_client::{GameSession, SpeedMeter};
use mirai_core::{Color, GameTree, IllegalMove, NodeAnalysis, NodeId, Point, Position};
use mirai_engine::{AnalyzeReq, Engine, EngineDesc, EngineError, Report, SubEvent, Want};

use crate::config::{Config, ConfigError, ProfileKind};
use crate::engines::EnginePool;

/// How long settings may keep changing before they are written. A spin row saves on every
/// step and a drag is dozens of steps; one write when it settles is enough.
const CONFIG_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Bookkeeping for [`AppState::save_config`]. Main-thread only.
#[derive(Default)]
pub struct ConfigSave {
    /// The debounce timer; `None` when nothing is pending.
    timer: Cell<Option<glib::SourceId>>,
    /// A write is on the blocking pool.
    in_flight: Cell<bool>,
    /// The config changed again while a write was in flight.
    again: Cell<bool>,
    /// The number given to the latest save started.
    started: Cell<u64>,
    /// The latest save whose result became the base.
    applied: Cell<u64>,
}

impl ConfigSave {
    fn next_seq(&self) -> u64 {
        let seq = self.started.get() + 1;
        self.started.set(seq);
        seq
    }
}

impl Drop for ConfigSave {
    fn drop(&mut self) {
        if let Some(id) = self.timer.take() {
            id.remove();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeEpoch(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub epoch: TreeEpoch,
    pub id: NodeId,
}

pub enum Change {
    BeforeEdit,
    Edit {
        positions_changed: bool,
        structure_changed: bool,
    },
    Editor,
    Tree,
    Cursor,
    Report,
    Engine,
    Toast(String),
    Play,
    BatchProgress(u32, u32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EditorTool {
    #[default]
    Play,
    Setup(Color),
    Triangle,
    Square,
    Circle,
    Cross,
    Label,
    EraseMark,
}

impl EditorTool {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Setup(Color::Black) => "setup-black",
            Self::Setup(Color::White) => "setup-white",
            Self::Triangle => "triangle",
            Self::Square => "square",
            Self::Circle => "circle",
            Self::Cross => "cross",
            Self::Label => "label",
            Self::EraseMark => "erase-mark",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "play" => Self::Play,
            "setup-black" => Self::Setup(Color::Black),
            "setup-white" => Self::Setup(Color::White),
            "triangle" => Self::Triangle,
            "square" => Self::Square,
            "circle" => Self::Circle,
            "cross" => Self::Cross,
            "label" => Self::Label,
            "erase-mark" => Self::EraseMark,
            _ => return None,
        })
    }
}
#[derive(Clone, Debug)]
pub enum EngineState {
    None,
    Starting {
        profile: String,
    },
    Ready {
        profile: String,
        description: String,
    },
    Failed {
        profile: String,
        message: String,
    },
}

impl EngineState {
    pub fn label(&self) -> String {
        match self {
            Self::None => "No engine".to_string(),
            Self::Starting { profile } => format!("Starting {profile}…"),
            Self::Ready { description, .. } => description.clone(),
            Self::Failed { profile, .. } => format!("{profile} unavailable"),
        }
    }
}

type ChangeHook = Box<dyn Fn(Change)>;

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::AppState)]
    pub struct AppState {
        // `explicit_notify` is a pspec flag that turns OFF GObject's automatic
        // `notify::` emission, so it belongs ONLY on properties with a hand-written
        // setter that emits the signal itself. On a derive-generated setter it would
        // silently break every `notify::` handler and `bind_property` target.
        /// Pondering the current node.
        #[property(get, set = Self::set_live_analysis, explicit_notify)]
        pub live_analysis: Cell<bool>,
        #[property(get, set)]
        pub show_coordinates: Cell<bool>,
        #[property(get, set)]
        pub show_move_numbers: Cell<bool>,
        #[property(get, set = Self::set_ownership_overlay, explicit_notify)]
        pub ownership_overlay: Cell<bool>,
        #[property(get, set = Self::set_policy_overlay, explicit_notify)]
        pub policy_overlay: Cell<bool>,
        /// Label for the engine drop-down / header.
        #[property(get, set)]
        pub engine_label: RefCell<String>,
        /// One-line status shown in the bottom bar.
        #[property(get, set)]
        pub status: RefCell<String>,
        /// An engine operation is in flight (starting, connecting, batch analysing).
        #[property(get, set)]
        pub busy: Cell<bool>,
        /// The file the current game came from, for plain Save.
        #[property(get, set)]
        pub file_path: RefCell<String>,
        /// Set when the tree has unsaved edits.
        #[property(get, set)]
        pub modified: Cell<bool>,

        /// The record the user is editing: tree, cursor, dirty flag and Save target.
        /// `AppState` adds the observability around it, not a second copy of it.
        pub session: RefCell<GameSession>,
        pub config: RefCell<Config>,
        /// The configuration as this window loaded it, so a save can tell which keys this
        /// window actually changed and leave another window's edits alone.
        pub config_base: RefCell<Config>,
        /// Pending, in-flight and applied config saves; see [`super::AppState::save_config`].
        pub config_save: super::ConfigSave,
        pub config_path: OnceCell<PathBuf>,
        pub engine: RefCell<Option<Arc<dyn Engine>>>,
        /// The application-wide engines, shared with every other window. Installed when
        /// the window is built and never replaced.
        pub pool: OnceCell<Rc<EnginePool>>,
        pub engine_state: RefCell<EngineState>,
        pub runtime: OnceCell<tokio::runtime::Handle>,
        pub report: RefCell<Option<Arc<Report>>>,
        /// The live-analysis pump. Aborting it drops the `Subscription`, which terminates
        /// the KataGo query — that is the whole cancellation mechanism.
        pub pump: RefCell<Option<glib::JoinHandle<()>>>,
        /// Bumped on every `activate_profile` so a slow engine start can tell it has been
        /// superseded by a newer selection.
        pub activation: Cell<u64>,
        /// The profile startup waiter. Replacing or closing the window aborts it.
        pub activation_task: RefCell<Option<glib::JoinHandle<()>>>,
        /// Bumped on every analysis restart so a stale pump can tell it has been superseded.
        pub generation: Cell<u64>,
        pub analysis_revision: Cell<u64>,
        pub(super) editor_tool: Cell<EditorTool>,
        /// This window's one `Change` dispatcher (INV-7). The cell *is* the invariant: a
        /// second installation cannot silently win, and `changed` borrows nothing while the
        /// dispatcher runs back through this state.
        pub change_hook: OnceCell<ChangeHook>,
        /// Visits-per-second meter for the search that is running now; `None` when none is.
        pub speed: Cell<Option<SpeedMeter>>,
        /// The window trust dialogs are presented on. Set once, when the window is built.
        pub dialog_parent: OnceCell<glib::WeakRef<gtk::Widget>>,
        /// The certificate dialog for an unpinned profile, so a newer selection can dismiss it.
        pub trust_dialog: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    }

    impl Default for AppState {
        fn default() -> Self {
            AppState {
                live_analysis: Cell::new(false),
                show_coordinates: Cell::new(true),
                show_move_numbers: Cell::new(false),
                ownership_overlay: Cell::new(false),
                policy_overlay: Cell::new(false),
                engine_label: RefCell::new(String::from("No engine")),
                status: RefCell::new(String::new()),
                busy: Cell::new(false),
                file_path: RefCell::new(String::new()),
                modified: Cell::new(false),
                session: RefCell::new(GameSession::blank()),
                engine_state: RefCell::new(EngineState::None),
                config: RefCell::new(Config::default()),
                config_base: RefCell::new(Config::default()),
                config_save: super::ConfigSave::default(),
                config_path: OnceCell::new(),
                engine: RefCell::new(None),
                pool: OnceCell::new(),
                runtime: OnceCell::new(),
                report: RefCell::new(None),
                pump: RefCell::new(None),
                activation: Cell::new(0),
                activation_task: RefCell::new(None),
                generation: Cell::new(0),
                analysis_revision: Cell::new(0),
                editor_tool: Cell::new(EditorTool::Play),
                change_hook: OnceCell::new(),
                speed: Cell::new(None),
                dialog_parent: OnceCell::new(),
                trust_dialog: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AppState {
        const NAME: &'static str = "MiraiAppState";
        type Type = super::AppState;
    }

    #[glib::derived_properties]
    impl ObjectImpl for AppState {}

    impl AppState {
        fn set_live_analysis(&self, on: bool) {
            if self.live_analysis.replace(on) == on {
                return;
            }
            self.obj().notify_live_analysis();
            self.obj().restart_analysis();
        }

        fn set_ownership_overlay(&self, on: bool) {
            if self.ownership_overlay.replace(on) == on {
                return;
            }
            if on && self.policy_overlay.replace(false) {
                self.obj().notify_policy_overlay();
            }
            self.obj().notify_ownership_overlay();
        }

        fn set_policy_overlay(&self, on: bool) {
            if self.policy_overlay.replace(on) == on {
                return;
            }
            if on && self.ownership_overlay.replace(false) {
                self.obj().notify_ownership_overlay();
            }
            self.obj().notify_policy_overlay();
            // The policy array is only requested when the overlay is on.
            self.obj().restart_analysis();
        }
    }
}

glib::wrapper! {
    pub struct AppState(ObjectSubclass<imp::AppState>);
}

impl Default for AppState {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AppState {
    pub fn new(
        config: Config,
        config_path: PathBuf,
        runtime: tokio::runtime::Handle,
        pool: Rc<EnginePool>,
    ) -> AppState {
        let this: AppState = glib::Object::new();
        let imp = this.imp();
        this.set_show_coordinates(config.ui.show_coordinates);
        this.set_show_move_numbers(config.ui.show_move_numbers);
        this.set_ownership_overlay(config.ui.ownership_overlay);
        this.set_policy_overlay(config.ui.policy_overlay);
        *imp.config_base.borrow_mut() = config.clone();
        *imp.config.borrow_mut() = config;
        // Write-once cells on an object nobody else has seen yet, so `set` cannot fail.
        let _ = imp.config_path.set(config_path);
        let _ = imp.runtime.set(runtime);
        let _ = imp.pool.set(pool);
        this
    }

    pub fn set_change_hook(&self, hook: impl Fn(Change) + 'static) {
        let installed = self.imp().change_hook.set(Box::new(hook)).is_ok();
        assert!(installed, "AppState change dispatcher installed twice");
    }

    /// Runs the window's dispatcher. Nothing is held across the call: handlers re-enter
    /// this state freely, and an open borrow here would be a panic waiting for the first
    /// one that also reaches for the hook.
    pub fn changed(&self, change: Change) {
        if let Some(hook) = self.imp().change_hook.get() {
            hook(change);
        }
    }

    pub fn pool(&self) -> Rc<EnginePool> {
        Rc::clone(
            self.imp()
                .pool
                .get()
                .expect("AppState was built without an engine pool"),
        )
    }

    // -- configuration ------------------------------------------------------------------

    pub fn config(&self) -> Ref<'_, Config> {
        self.imp().config.borrow()
    }

    pub fn config_mut(&self) -> RefMut<'_, Config> {
        self.imp().config.borrow_mut()
    }

    /// Borrowed: the path is fixed for the window's lifetime, so saving need not allocate
    /// a copy of it.
    pub fn config_path(&self) -> &Path {
        self.imp()
            .config_path
            .get()
            .map(PathBuf::as_path)
            .expect("AppState was built without a configuration path")
    }

    /// Mirrors the current display toggles into the config and schedules writing it out.
    ///
    /// Only the keys this window changed are written: every window holds its own `Config`,
    /// loaded when it opened, so a plain overwrite would revert whatever another window has
    /// changed since. See [`Config::save_merged`].
    ///
    /// The write is debounced and runs on the runtime's blocking pool. It syncs the file
    /// and its directory, 50–100 ms on an ordinary disk, and a spin row saves on every
    /// step: done here, each step was a dropped frame. [`Self::flush_config`] writes what
    /// is pending at once, for the paths that cannot wait.
    pub fn save_config(&self) {
        self.capture_display_settings();
        let save = &self.imp().config_save;
        if let Some(id) = save.timer.take() {
            id.remove();
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local(CONFIG_SAVE_DEBOUNCE, move || {
            if let Some(state) = weak.upgrade() {
                // The source is running; removing it again would target an id glib has freed.
                state.imp().config_save.timer.take();
                state.start_config_save();
            }
            glib::ControlFlow::Break
        });
        save.timer.set(Some(id));
    }

    /// Writes any pending change now, on this thread. For a closing window, and for a new
    /// window about to load the file this one has not written yet.
    pub fn flush_config(&self) {
        self.capture_display_settings();
        let imp = self.imp();
        if let Some(id) = imp.config_save.timer.take() {
            id.remove();
        }
        let (config, base) = (
            imp.config.borrow().clone(),
            imp.config_base.borrow().clone(),
        );
        if config == base {
            return;
        }
        let seq = imp.config_save.next_seq();
        let result = config.save_merged(&base, self.config_path());
        self.finish_config_save(seq, config, result);
    }

    fn capture_display_settings(&self) {
        let mut cfg = self.imp().config.borrow_mut();
        cfg.ui.show_coordinates = self.show_coordinates();
        cfg.ui.show_move_numbers = self.show_move_numbers();
        cfg.ui.ownership_overlay = self.ownership_overlay();
        cfg.ui.policy_overlay = self.policy_overlay();
    }

    /// Hands a snapshot of the config to the blocking pool. One save per window is in
    /// flight at a time; a change made meanwhile is written when it lands.
    fn start_config_save(&self) {
        let imp = self.imp();
        if imp.config_save.in_flight.get() {
            imp.config_save.again.set(true);
            return;
        }
        let (config, base) = (
            imp.config.borrow().clone(),
            imp.config_base.borrow().clone(),
        );
        if config == base {
            return;
        }
        imp.config_save.in_flight.set(true);
        let seq = imp.config_save.next_seq();
        let path = self.config_path().to_path_buf();
        let job = self.runtime().spawn_blocking(move || {
            let result = config.save_merged(&base, &path);
            (config, result)
        });
        let weak = self.downgrade();
        // Finite and transient: it holds only a weak reference, and a window that is gone
        // by the time the write lands has already flushed synchronously.
        glib::spawn_future_local(async move {
            let outcome = job.await;
            let Some(state) = weak.upgrade() else { return };
            let save = &state.imp().config_save;
            save.in_flight.set(false);
            match outcome {
                Ok((config, result)) => state.finish_config_save(seq, config, result),
                Err(e) => tracing::warn!(%e, "the configuration save task failed"),
            }
            if save.again.replace(false) {
                state.start_config_save();
            }
        });
    }

    /// Records a finished save. The config it wrote becomes the base for the next diff —
    /// unless a later save already finished: a flush on close can overtake a write still on
    /// the pool, and restoring the older base would re-apply keys another window has since
    /// changed.
    fn finish_config_save(&self, seq: u64, written: Config, result: Result<(), ConfigError>) {
        let imp = self.imp();
        match result {
            Ok(()) => {
                if seq > imp.config_save.applied.get() {
                    imp.config_save.applied.set(seq);
                    *imp.config_base.borrow_mut() = written;
                }
            }
            Err(e) => {
                tracing::warn!(%e, "could not save the configuration");
                self.toast(format!("Could not save settings: {e}"));
            }
        }
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.imp()
            .runtime
            .get()
            .expect("AppState was built without a tokio runtime")
            .clone()
    }

    // -- the record ---------------------------------------------------------------------
    //
    // `GameSession` owns the tree, the cursor and the dirty bookkeeping; everything here
    // adds is the observability a window needs. Every borrow is released before `changed`
    // runs, because the dispatcher borrows the tree again (INV-10).

    /// The session's document token. Cursor moves do not change it; edits do.
    pub fn document_token(&self) -> u64 {
        self.imp().session.borrow().document_token()
    }

    pub fn tree(&self) -> Ref<'_, GameTree> {
        Ref::map(self.imp().session.borrow(), GameSession::tree)
    }

    /// Read-only access that still needs `&mut GameTree` (e.g. `position`, which caches).
    pub fn with_tree_cached<R>(&self, f: impl FnOnce(&mut GameTree) -> R) -> R {
        f(self.imp().session.borrow_mut().tree_cached_mut())
    }

    /// Writes an engine evaluation onto a node. Pondering is not an edit, so this leaves
    /// the dirty flag alone; the caller emits [`Change::Tree`] when it wants a redraw.
    pub(crate) fn set_analysis_at(
        &self,
        id: NodeId,
        expected_position_revision: u64,
        analysis: Option<NodeAnalysis>,
    ) -> bool {
        self.imp()
            .session
            .borrow_mut()
            .set_analysis_at(id, expected_position_revision, analysis)
    }

    /// Mirrors the record's own dirty flag and Save target onto the GObject properties the
    /// title bar and the close handler are bound to.
    fn sync_record_flags(&self) {
        let (path, modified) = {
            let session = self.imp().session.borrow();
            (
                session.file_path().unwrap_or_default().to_string(),
                session.modified(),
            )
        };
        if self.file_path() != path {
            self.set_file_path(path);
        }
        if self.modified() != modified {
            self.set_modified(modified);
        }
    }

    /// Runs `f` against the record, then tells the window whatever changed.
    ///
    /// [`mirai_client::Play`] drives the record directly — turns, clocks, scoring and undo
    /// are its rules, not the window's — so this is the one door it goes through and a play
    /// action cannot forget to dispatch. The borrow is always released first (INV-10).
    pub fn with_session_mut<R>(&self, f: impl FnOnce(&mut GameSession) -> R) -> R {
        let (out, changed, positions_changed, structure_changed, cursor_changed) = {
            let mut session = self.imp().session.borrow_mut();
            let before = (
                session.revision(),
                session.cursor(),
                session.epoch(),
                session.position_revision(),
                session.tree().structure_revision(),
            );
            let out = f(&mut session);
            let replaced = before.2 != session.epoch();
            if replaced {
                self.imp().editor_tool.set(EditorTool::Play);
            }
            (
                out,
                before.0 != session.revision(),
                replaced || before.3 != session.position_revision(),
                replaced || before.4 != session.tree().structure_revision(),
                replaced || before.1 != session.cursor(),
            )
        };
        if changed {
            if positions_changed || cursor_changed {
                self.imp().report.replace(None);
            }
            self.sync_record_flags();
            self.changed(Change::Edit {
                positions_changed,
                structure_changed,
            });
            self.changed(Change::Tree);
            if positions_changed || cursor_changed {
                self.moved_cursor();
            }
        }
        out
    }

    pub(crate) fn with_edit_session<R>(&self, f: impl FnOnce(&mut GameSession) -> R) -> R {
        self.changed(Change::BeforeEdit);
        self.with_session_mut(f)
    }

    pub(crate) fn set_comment_at(&self, node: NodeRef, text: &str) {
        if let Some(id) = self.resolve_node(node) {
            self.with_session_mut(|session| session.set_comment_at(id, text));
        }
    }

    pub(crate) fn position_revision(&self) -> u64 {
        self.imp().session.borrow().position_revision()
    }

    pub(crate) fn can_undo(&self) -> bool {
        self.imp().session.borrow().can_undo()
    }

    pub(crate) fn can_redo(&self) -> bool {
        self.imp().session.borrow().can_redo()
    }

    pub(crate) fn editor_tool(&self) -> EditorTool {
        self.imp().editor_tool.get()
    }

    pub(crate) fn set_editor_tool(&self, tool: EditorTool) {
        if self.imp().editor_tool.replace(tool) != tool {
            self.changed(Change::Editor);
        }
    }

    /// Replaces the record. `path` is the file Save writes to, or `None` for a record that
    /// must go through Save As.
    pub fn adopt_record(&self, tree: GameTree, path: Option<String>) {
        self.changed(Change::BeforeEdit);
        self.imp().editor_tool.set(EditorTool::Play);
        self.with_session_mut(|session| session.adopt(tree, path));
        self.changed(Change::Editor);
    }

    /// Replaces the record with one nothing on disk holds — a paste, a download, or
    /// crash-recovery data. No Save target, and dirty from the start.
    pub fn adopt_unsaved(&self, tree: GameTree) {
        self.changed(Change::BeforeEdit);
        self.imp().editor_tool.set(EditorTool::Play);
        self.with_session_mut(|session| session.restore(tree));
        self.changed(Change::Editor);
    }

    /// Records that the tree was written to `path`.
    pub fn saved_to(&self, path: String) {
        self.imp().session.borrow_mut().saved_to(path);
        self.sync_record_flags();
    }

    pub fn cursor(&self) -> NodeId {
        self.imp().session.borrow().cursor()
    }

    /// The generation of the record the node ids in flight belong to. Bumped only when the
    /// whole record is replaced, which is exactly when an id stops meaning anything.
    #[inline]
    pub fn tree_epoch(&self) -> TreeEpoch {
        TreeEpoch(self.imp().session.borrow().epoch())
    }

    #[inline]
    pub fn node_ref(&self, id: NodeId) -> NodeRef {
        NodeRef {
            epoch: self.tree_epoch(),
            id,
        }
    }

    #[inline]
    pub fn cursor_ref(&self) -> NodeRef {
        self.node_ref(self.cursor())
    }

    #[inline]
    pub fn resolve_node(&self, node: NodeRef) -> Option<NodeId> {
        (node.epoch == self.tree_epoch() && self.tree().contains(node.id)).then_some(node.id)
    }

    pub fn set_cursor(&self, id: NodeId) {
        if self.cursor() == id || !self.tree().contains(id) {
            return;
        }
        self.changed(Change::BeforeEdit);
        self.imp().session.borrow_mut().go_to(id);
        self.imp().report.replace(None);
        self.moved_cursor();
    }

    /// Everything a cursor move has to tell the window, once the borrow is gone.
    fn moved_cursor(&self) {
        self.changed(Change::Cursor);
        self.changed(Change::Report);
        self.restart_analysis();
    }

    /// The board position at the cursor.
    pub fn position(&self) -> Position {
        self.imp().session.borrow_mut().position().clone()
    }

    pub fn to_play(&self) -> Color {
        self.imp().session.borrow_mut().to_play()
    }

    /// Plays a move for the side to move at the cursor, and moves the cursor onto it.
    pub fn play_move(&self, p: Point) -> Result<NodeId, IllegalMove> {
        self.with_edit_session(|session| session.play(p))
    }

    // -- navigation ---------------------------------------------------------------------
    //
    // Each of these walks the record through `GameSession` and then tells the window,
    // because the dispatcher borrows the tree again (INV-10).

    /// Runs `walk` over the record and dispatches if it actually moved the cursor.
    fn navigate(&self, walk: impl FnOnce(&mut GameSession)) {
        self.changed(Change::BeforeEdit);
        {
            let imp = self.imp();
            let mut session = imp.session.borrow_mut();
            let before = session.cursor();
            walk(&mut session);
            if session.cursor() == before {
                return;
            }
            imp.report.replace(None);
        }
        self.moved_cursor();
    }

    pub fn go_first(&self) {
        self.navigate(GameSession::go_first);
    }

    pub fn go_last(&self) {
        self.navigate(GameSession::go_last);
    }

    pub fn go_prev(&self) {
        self.go_back(1);
    }

    pub fn go_next(&self) {
        self.go_forward(1);
    }

    pub fn go_back(&self, n: usize) {
        self.navigate(|s| s.go_back(n));
    }

    pub fn go_forward(&self, n: usize) {
        self.navigate(|s| s.go_forward(n));
    }

    /// Moves to the previous/next sibling of the current node, wrapping around at the ends
    /// so one key cycles a node's variations.
    pub fn go_sibling(&self, delta: i32) {
        self.navigate(|s| s.go_sibling(delta));
    }

    // -- engine -------------------------------------------------------------------------

    pub fn engine(&self) -> Option<Arc<dyn Engine>> {
        self.imp().engine.borrow().clone()
    }

    pub fn engine_desc(&self) -> Option<EngineDesc> {
        self.engine().map(|e| e.describe())
    }
    pub fn engine_state(&self) -> EngineState {
        self.imp().engine_state.borrow().clone()
    }

    fn set_engine_state(&self, state: EngineState) {
        self.set_engine_label(state.label());
        *self.imp().engine_state.borrow_mut() = state;
        self.changed(Change::Engine);
    }

    pub fn set_engine(&self, engine: Option<Arc<dyn Engine>>) {
        let state = match &engine {
            Some(engine) => {
                let desc = engine.describe();
                let description = if desc.katago_version.is_empty() {
                    desc.name.clone()
                } else {
                    format!("{} ({})", desc.name, desc.katago_version)
                };
                EngineState::Ready {
                    profile: desc.name,
                    description,
                }
            }
            None => EngineState::None,
        };
        *self.imp().engine.borrow_mut() = engine;
        self.set_engine_state(state);
        self.restart_analysis();
    }

    /// Starts (or connects to) the named profile and installs it as the active engine.
    ///
    /// The engine comes from the application-wide [`EnginePool`], so a second window on the
    /// same profile adopts the running KataGo instead of starting its own. A remote profile
    /// with no pin is probed first: the token is sent only after the user trusts the
    /// fingerprint the dialog showed.
    pub fn activate_profile(&self, name: &str) {
        let Some(profile) = self.config().profile(name).cloned() else {
            self.toast(format!("No engine profile named “{name}”"));
            return;
        };
        if let Some(task) = self.imp().activation_task.borrow_mut().take() {
            task.abort();
        }
        self.dismiss_trust_dialog();
        // Starting an engine is slow. A later selection supersedes this one.
        let activation = self.imp().activation.get().wrapping_add(1);
        self.imp().activation.set(activation);

        if let Some(engine) = self.pool().running(&profile) {
            self.set_busy(false);
            self.set_status(String::new());
            self.remember_active(name);
            self.set_engine(Some(engine));
            return;
        }

        *self.imp().engine.borrow_mut() = None;
        self.set_engine_state(EngineState::Starting {
            profile: profile.name.clone(),
        });
        self.set_busy(true);
        self.set_status(String::new());

        if let ProfileKind::Remote {
            cert_sha256: None,
            ref url,
            ..
        } = profile.kind
        {
            self.probe_unpinned(name, url, activation);
            return;
        }

        self.acquire_profile(name, profile, activation);
    }

    /// The window trust dialogs attach to. Set from `window::present` before the first
    /// activation, which may need to ask about a certificate.
    pub fn set_dialog_parent(&self, parent: &impl IsA<gtk::Widget>) {
        let widget = parent.clone().upcast::<gtk::Widget>();
        let _ = self.imp().dialog_parent.set(widget.downgrade());
    }

    fn dialog_parent(&self) -> Option<gtk::Widget> {
        self.imp()
            .dialog_parent
            .get()
            .and_then(|parent| parent.upgrade())
    }

    fn dismiss_trust_dialog(&self) {
        let Some(weak) = self.imp().trust_dialog.borrow_mut().take() else {
            return;
        };
        if let Some(widget) = weak.upgrade() {
            crate::dialogs::dismiss_trust_dialog(&widget);
        }
    }

    /// TLS only. The token stays on this machine until [`Self::pin_and_connect`].
    ///
    /// The probe lives in the activation future. Aborting that future — a newer
    /// selection, or the window closing — aborts the network task with it.
    fn probe_unpinned(&self, name: &str, url: &str, activation: u64) {
        let probe_url = url.to_string();
        let probe =
            AbortOnDrop::new(self.runtime().spawn(async move {
                mirai_engine::RemoteEngine::probe_fingerprint(&probe_url).await
            }));
        let this = self.clone();
        let profile_name = name.to_string();
        let url = url.to_string();
        let handle = glib::spawn_future_local(async move {
            let outcome = probe.await;
            if this.imp().activation.get() != activation {
                return;
            }
            this.set_busy(false);
            match outcome {
                Ok(Ok(fingerprint)) => {
                    this.ask_trust(&profile_name, &url, &fingerprint, activation);
                }
                Ok(Err(e)) => this.fail_activation(&profile_name, e.to_string()),
                Err(_) => this
                    .fail_activation(&profile_name, "the certificate check was cancelled".into()),
            }
        });
        *self.imp().activation_task.borrow_mut() = Some(handle);
    }

    fn ask_trust(&self, name: &str, url: &str, fingerprint: &str, activation: u64) {
        let Some(parent) = self.dialog_parent() else {
            self.fail_activation(name, "no window to confirm the certificate".into());
            return;
        };
        let this = self.clone();
        let profile_name = name.to_string();
        let fp = fingerprint.to_string();
        let on_cancel_state = self.clone();
        let on_cancel_name = name.to_string();
        let dialog = crate::dialogs::confirm_fingerprint(
            &parent,
            url,
            fingerprint,
            move || {
                if this.imp().activation.get() != activation {
                    return;
                }
                this.pin_and_connect(&profile_name, &fp, activation);
            },
            move || {
                if on_cancel_state.imp().activation.get() != activation {
                    return;
                }
                on_cancel_state.set_busy(false);
                on_cancel_state
                    .set_status("Certificate was not trusted. The token was not sent.".to_string());
                *on_cancel_state.imp().engine.borrow_mut() = None;
                on_cancel_state.set_engine_state(EngineState::Failed {
                    profile: on_cancel_name.clone(),
                    message: "certificate was not trusted".into(),
                });
                on_cancel_state.restart_analysis();
            },
        );
        let widget: gtk::Widget = dialog.upcast();
        *self.imp().trust_dialog.borrow_mut() = Some(widget.downgrade());
    }

    /// Persists the fingerprint the user just accepted, then connects with that pin.
    fn pin_and_connect(&self, name: &str, fingerprint: &str, activation: u64) {
        {
            let mut cfg = self.config_mut();
            cfg.set_pin(name, fingerprint);
        }
        self.save_config();
        let Some(profile) = self.config().profile(name).cloned() else {
            self.fail_activation(name, format!("No engine profile named “{name}”"));
            return;
        };
        self.set_busy(true);
        self.set_status(String::new());
        self.set_engine_state(EngineState::Starting {
            profile: name.to_string(),
        });
        self.acquire_profile(name, profile, activation);
    }

    fn acquire_profile(&self, name: &str, profile: crate::config::EngineProfile, activation: u64) {
        let log_dir = crate::config::Config::data_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("katago-logs");
        let acquire = self.pool().acquire(profile, log_dir, self.runtime());
        let this = self.clone();
        let profile_name = name.to_string();
        let handle = glib::spawn_future_local(async move {
            let result = acquire.await;
            if this.imp().activation.get() != activation {
                tracing::debug!(
                    profile = %profile_name,
                    "discarding a superseded engine activation"
                );
                return;
            }
            this.set_busy(false);
            this.set_status(String::new());
            match result {
                Ok(engine) => {
                    this.remember_active(&profile_name);
                    this.set_engine(Some(engine));
                }
                Err(e) => this.fail_activation(&profile_name, e),
            }
        });
        *self.imp().activation_task.borrow_mut() = Some(handle);
    }

    fn fail_activation(&self, profile: &str, message: String) {
        self.set_busy(false);
        self.toast(format!("{profile}: {message}"));
        *self.imp().engine.borrow_mut() = None;
        self.set_engine_state(EngineState::Failed {
            profile: profile.to_string(),
            message,
        });
        self.restart_analysis();
    }

    /// Records the profile now in use. A certificate pin is written by
    /// [`Self::pin_and_connect`], never from a connection that has already sent the token.
    fn remember_active(&self, name: &str) {
        let changed = {
            let mut cfg = self.config_mut();
            if cfg.active_engine.as_deref() == Some(name) {
                false
            } else {
                cfg.active_engine = Some(name.to_string());
                true
            }
        };
        if changed {
            self.save_config();
        }
    }

    // -- analysis -----------------------------------------------------------------------

    pub fn last_report(&self) -> Option<Arc<Report>> {
        self.imp().report.borrow().clone()
    }

    /// How fast the running live analysis is searching, in visits per second. `None` when
    /// no search is running, or before it has made measurable progress.
    pub fn analysis_speed(&self) -> Option<f32> {
        self.imp().speed.get().and_then(|m| m.rate())
    }

    /// Builds a stateless analysis request for the position at `id`.
    ///
    /// The construction itself lives in [`mirai_client::analysis`], shared with the
    /// HarmonyOS client; all this adds is the configured default visit cap.
    pub fn request_for_node(&self, id: NodeId, max_visits: Option<u32>, want: Want) -> AnalyzeReq {
        let max_visits = max_visits.unwrap_or_else(|| self.config().analysis.live_max_visits);
        self.with_tree_cached(|t| mirai_client::request_for_node(t, id, want, max_visits))
    }

    /// [`Self::request_for_node`] for the node the user is looking at.
    pub fn request_for_cursor(&self, max_visits: Option<u32>, want: Want) -> AnalyzeReq {
        self.request_for_node(self.cursor(), max_visits, want)
    }

    /// Drops any live pump and, if live analysis is on, starts a new one for the cursor.
    pub fn restart_analysis(&self) {
        let imp = self.imp();
        if let Some(handle) = imp.pump.take() {
            handle.abort();
        }
        imp.generation.set(imp.generation.get().wrapping_add(1));
        imp.analysis_revision.set(self.position_revision());
        imp.speed.set(None);

        if !self.live_analysis() {
            return;
        }
        let Some(engine) = self.engine() else {
            return;
        };

        let (max_visits, report_every, max_candidates, want) = {
            let cfg = self.config();
            // Nothing reads pv_visits; asking for them only fattens every report.
            let mut want = Want::OWNERSHIP;
            if self.policy_overlay() {
                want |= Want::POLICY;
            }
            (
                cfg.analysis.live_max_visits,
                cfg.analysis.report_interval_ms,
                // Only what the board and the list show; "All" is `usize::MAX`, so `None`.
                u8::try_from(cfg.analysis.suggestion_limit()).ok(),
                want,
            )
        };

        let mut req = self.request_for_cursor(Some(max_visits), want);
        req.report_every_ms = Some(report_every);
        req.priority = 4;
        req.max_candidates = max_candidates;

        // The clock starts at dispatch, so the first sample charges the search for the
        // engine's queueing and warm-up too; later samples are pure deltas.
        imp.speed.set(Some(SpeedMeter::started(Instant::now())));
        let mut sub = engine.subscribe(req);
        let this = self.clone();
        let generation = imp.generation.get();
        let handle = glib::spawn_future_local(async move {
            loop {
                let event = match sub.next().await {
                    Some(e) => e,
                    None => break,
                };
                if this.imp().generation.get() != generation {
                    break;
                }
                match event {
                    SubEvent::Pending => {}
                    SubEvent::Report(r) | SubEvent::Done(r) => {
                        this.set_report(r);
                    }
                    SubEvent::Failed(e) => {
                        this.on_engine_error(e);
                        break;
                    }
                }
            }
        });
        *imp.pump.borrow_mut() = Some(handle);
    }

    /// Stores a report and notifies observers. The live [`Report`] always replaces the
    /// previous one; the node's cached [`mirai_core::NodeAnalysis`] is updated only when
    /// this report searched deeper, so a whole-game sweep is not clobbered by the first
    /// handful of live visits.
    pub fn set_report(&self, report: Arc<Report>) {
        let cursor = self.cursor();
        let max = self.config().analysis.stored_suggestion_limit();
        if let Some(mut meter) = self.imp().speed.get() {
            meter.sample(report.root.visits, Instant::now());
            self.imp().speed.set(Some(meter));
        }
        // Everything above only reads the report, so the analysis is built before the tree
        // is borrowed and the `Arc` moves into the cell last instead of being cloned into
        // it. The dispatcher below is the first thing that can observe either.
        let analysis = mirai_client::analysis_of(&report, max);
        {
            let mut session = self.imp().session.borrow_mut();
            let existing = session
                .tree()
                .node(cursor)
                .analysis
                .as_ref()
                .map(|a| a.visits);
            if crate::util::replaces_stored_analysis(existing, analysis.visits) {
                session.set_analysis_at(cursor, self.imp().analysis_revision.get(), Some(analysis));
            }
        }
        self.imp().report.replace(Some(report));
        self.changed(Change::Report);
    }

    /// Cancels every future owned by this window state.
    pub fn cancel_tasks(&self) {
        let imp = self.imp();
        if let Some(task) = imp.activation_task.borrow_mut().take() {
            task.abort();
        }
        if let Some(task) = imp.pump.borrow_mut().take() {
            task.abort();
        }
        imp.generation.set(imp.generation.get().wrapping_add(1));
    }

    pub fn on_engine_error(&self, e: EngineError) {
        tracing::warn!(%e, "engine error");
        self.toast(e.to_string());
        if matches!(e, EngineError::EngineExited(_) | EngineError::Startup(_)) {
            self.set_engine(None);
        }
    }

    pub fn toast(&self, msg: impl Into<String>) {
        self.changed(Change::Toast(msg.into()));
    }

    /// Reports an illegal move only when the board itself does not already explain it.
    pub fn toast_illegal_move(&self, error: IllegalMove) {
        if should_toast_illegal_move(error) {
            self.toast(error.to_string());
        }
    }

    pub fn notify_play_changed(&self) {
        self.changed(Change::Play);
    }

    pub fn notify_batch_progress(&self, done: u32, total: u32) {
        self.changed(Change::BatchProgress(done, total));
    }
}

/// Aborts a tokio task when dropped. A glib future holds one, so aborting that
/// future — the one teardown path — cancels the network work too.
pub(crate) struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let handle = self
            .get_mut()
            .handle
            .as_mut()
            .expect("an aborted probe was polled again");
        Pin::new(handle).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn should_toast_illegal_move(error: IllegalMove) -> bool {
    !matches!(error, IllegalMove::Occupied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_points_need_no_redundant_toast() {
        assert!(!should_toast_illegal_move(IllegalMove::Occupied));
        assert!(should_toast_illegal_move(IllegalMove::Suicide));
        assert!(should_toast_illegal_move(IllegalMove::Ko));
        assert!(should_toast_illegal_move(IllegalMove::OffBoard));
    }
}
