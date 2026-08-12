// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `AppState` — the single source of truth every widget observes.
//!
//! It is a `glib::Object` subclass so display toggles can be bound with `notify::`, and it
//! emits a handful of custom signals for structural changes. Widgets never reach into each
//! other; they read `AppState` and listen to its signals.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use mirai_core::{Color, GameInfo, GameTree, IllegalMove, NodeId, Point, Position, RuleSet, Size};
use mirai_engine::{AnalyzeReq, Engine, EngineDesc, EngineError, Report, SubEvent, Want};

use crate::config::Config;
use crate::engines::EnginePool;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeEpoch(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub epoch: TreeEpoch,
    pub id: NodeId,
}

pub enum Change {
    Tree,
    Cursor,
    Report,
    Engine,
    Toast(String),
    Play,
    BatchProgress(u32, u32),
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

        pub tree: RefCell<GameTree>,
        pub cursor: Cell<NodeId>,
        pub tree_epoch: Cell<TreeEpoch>,
        pub config: RefCell<Config>,
        /// The configuration as this window loaded it, so a save can tell which keys this
        /// window actually changed and leave another window's edits alone.
        pub config_base: RefCell<Config>,
        pub config_path: RefCell<PathBuf>,
        pub engine: RefCell<Option<Arc<dyn Engine>>>,
        /// The application-wide engines, shared with every other window.
        pub pool: RefCell<Rc<EnginePool>>,
        pub engine_state: RefCell<EngineState>,
        pub runtime: RefCell<Option<tokio::runtime::Handle>>,
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
        /// Visits-per-second meter for the search that is running now; `None` when none is.
        pub change_hook: RefCell<Option<ChangeHook>>,
        pub speed: Cell<Option<SpeedMeter>>,
    }

    impl Default for AppState {
        fn default() -> Self {
            let info = GameInfo::new(Size::square(19), RuleSet::Chinese);
            let tree = GameTree::new(info);
            let root = tree.root();
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
                tree: RefCell::new(tree),
                cursor: Cell::new(root),
                tree_epoch: Cell::new(TreeEpoch(0)),
                engine_state: RefCell::new(EngineState::None),
                config: RefCell::new(Config::default()),
                config_base: RefCell::new(Config::default()),
                config_path: RefCell::new(PathBuf::new()),
                engine: RefCell::new(None),
                pool: RefCell::new(Rc::new(EnginePool::default())),
                runtime: RefCell::new(None),
                report: RefCell::new(None),
                pump: RefCell::new(None),
                activation: Cell::new(0),
                activation_task: RefCell::new(None),
                generation: Cell::new(0),
                change_hook: RefCell::new(None),
                speed: Cell::new(None),
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
        *imp.config_path.borrow_mut() = config_path;
        *imp.runtime.borrow_mut() = Some(runtime);
        *imp.pool.borrow_mut() = pool;
        this
    }
    pub fn set_change_hook(&self, hook: impl Fn(Change) + 'static) {
        let old = self.imp().change_hook.borrow_mut().replace(Box::new(hook));
        assert!(old.is_none(), "AppState change dispatcher installed twice");
    }

    pub fn changed(&self, change: Change) {
        if let Some(hook) = self.imp().change_hook.borrow().as_ref() {
            hook(change);
        }
    }

    pub fn pool(&self) -> Rc<EnginePool> {
        self.imp().pool.borrow().clone()
    }

    // -- configuration ------------------------------------------------------------------

    pub fn config(&self) -> Ref<'_, Config> {
        self.imp().config.borrow()
    }

    pub fn config_mut(&self) -> RefMut<'_, Config> {
        self.imp().config.borrow_mut()
    }

    pub fn config_path(&self) -> PathBuf {
        self.imp().config_path.borrow().clone()
    }

    /// Mirrors the current display toggles into the config and writes it out.
    ///
    /// Only the keys this window changed are written: every window holds its own `Config`,
    /// loaded when it opened, so a plain overwrite would revert whatever another window has
    /// changed since. See [`Config::save_merged`].
    pub fn save_config(&self) {
        {
            let mut cfg = self.imp().config.borrow_mut();
            cfg.ui.show_coordinates = self.show_coordinates();
            cfg.ui.show_move_numbers = self.show_move_numbers();
            cfg.ui.ownership_overlay = self.ownership_overlay();
            cfg.ui.policy_overlay = self.policy_overlay();
        }
        let path = self.config_path();
        let result = {
            let cfg = self.imp().config.borrow();
            cfg.save_merged(&self.imp().config_base.borrow(), &path)
        };
        match result {
            // What this window holds is now the baseline for its next save.
            Ok(()) => *self.imp().config_base.borrow_mut() = self.imp().config.borrow().clone(),
            Err(e) => {
                tracing::warn!(%e, "could not save the configuration");
                self.toast(format!("Could not save settings: {e}"));
            }
        }
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.imp()
            .runtime
            .borrow()
            .clone()
            .expect("AppState was built without a tokio runtime")
    }

    // -- tree and cursor ----------------------------------------------------------------

    pub fn tree(&self) -> Ref<'_, GameTree> {
        self.imp().tree.borrow()
    }

    /// Mutable access for a single edit; emits `tree-changed` afterwards.
    pub fn with_tree_mut<R>(&self, f: impl FnOnce(&mut GameTree) -> R) -> R {
        let out = f(&mut self.imp().tree.borrow_mut());
        self.set_modified(true);
        self.changed(Change::Tree);
        out
    }

    /// Read-only access that still needs `&mut GameTree` (e.g. `position`, which caches).
    pub fn with_tree_cached<R>(&self, f: impl FnOnce(&mut GameTree) -> R) -> R {
        f(&mut self.imp().tree.borrow_mut())
    }

    pub fn set_tree(&self, tree: GameTree, cursor: Option<NodeId>) {
        let root = tree.root();
        {
            let imp = self.imp();
            *imp.tree.borrow_mut() = tree;
            imp.tree_epoch
                .set(TreeEpoch(imp.tree_epoch.get().0.wrapping_add(1)));
            imp.cursor.set(cursor.unwrap_or(root));
            // A report belongs to one exact position; after replacing the game it may not
            // even have the same board size.
            imp.report.replace(None);
        }
        self.set_modified(false);
        self.changed(Change::Tree);
        self.changed(Change::Cursor);
        self.restart_analysis();
    }

    pub fn cursor(&self) -> NodeId {
        self.imp().cursor.get()
    }

    #[inline]
    pub fn tree_epoch(&self) -> TreeEpoch {
        self.imp().tree_epoch.get()
    }

    #[inline]
    pub fn node_ref(&self, id: NodeId) -> NodeRef {
        NodeRef {
            epoch: self.imp().tree_epoch.get(),
            id,
        }
    }

    #[inline]
    pub fn cursor_ref(&self) -> NodeRef {
        self.node_ref(self.cursor())
    }

    #[inline]
    pub fn resolve_node(&self, node: NodeRef) -> Option<NodeId> {
        (node.epoch == self.imp().tree_epoch.get() && self.tree().contains(node.id))
            .then_some(node.id)
    }

    pub fn set_cursor(&self, id: NodeId) {
        if self.imp().cursor.get() == id {
            return;
        }
        self.imp().cursor.set(id);
        self.imp().report.replace(None);
        self.changed(Change::Cursor);
        self.changed(Change::Report);
        self.restart_analysis();
    }

    /// The board position at the cursor.
    pub fn position(&self) -> Position {
        let cursor = self.cursor();
        self.with_tree_cached(|t| t.position(cursor).clone())
    }

    pub fn to_play(&self) -> Color {
        let cursor = self.cursor();
        self.with_tree_cached(|t| t.position(cursor).to_play)
    }

    /// Plays a move at the cursor and moves the cursor onto it.
    pub fn play_move(&self, color: Color, p: Point) -> Result<NodeId, IllegalMove> {
        let cursor = self.cursor();
        let id = self.with_tree_mut(|t| t.play(cursor, color, p))?;
        self.imp().cursor.set(id);
        self.imp().report.replace(None);
        self.changed(Change::Cursor);
        self.changed(Change::Report);
        self.restart_analysis();
        Ok(id)
    }

    // -- navigation ---------------------------------------------------------------------

    pub fn go_first(&self) {
        let root = self.tree().root();
        self.set_cursor(root);
    }

    pub fn go_last(&self) {
        let mut id = self.cursor();
        let tree = self.tree();
        while let Some(&next) = tree.children(id).first() {
            id = next;
        }
        drop(tree);
        self.set_cursor(id);
    }

    pub fn go_prev(&self) {
        self.go_back(1);
    }

    pub fn go_next(&self) {
        self.go_forward(1);
    }

    pub fn go_back(&self, n: usize) {
        let mut id = self.cursor();
        {
            let tree = self.tree();
            for _ in 0..n {
                match tree.parent(id) {
                    Some(p) => id = p,
                    None => break,
                }
            }
        }
        self.set_cursor(id);
    }

    pub fn go_forward(&self, n: usize) {
        let mut id = self.cursor();
        {
            let tree = self.tree();
            for _ in 0..n {
                match tree.children(id).first() {
                    Some(&c) => id = c,
                    None => break,
                }
            }
        }
        self.set_cursor(id);
    }

    /// Moves to the previous/next sibling of the current node.
    pub fn go_sibling(&self, delta: i32) {
        let id = self.cursor();
        let target = {
            let tree = self.tree();
            let Some(parent) = tree.parent(id) else {
                return;
            };
            let sibs = tree.children(parent);
            let Some(pos) = sibs.iter().position(|&c| c == id) else {
                return;
            };
            let next = pos as i32 + delta;
            if next < 0 || next as usize >= sibs.len() {
                return;
            }
            sibs[next as usize]
        };
        self.set_cursor(target);
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
    /// same profile adopts the running KataGo instead of starting its own.
    pub fn activate_profile(&self, name: &str) {
        let Some(profile) = self.config().profile(name).cloned() else {
            self.toast(format!("No engine profile named “{name}”"));
            return;
        };
        if let Some(task) = self.imp().activation_task.borrow_mut().take() {
            task.abort();
        }
        // Starting an engine is slow. A later selection supersedes this one.
        let activation = self.imp().activation.get().wrapping_add(1);
        self.imp().activation.set(activation);

        if let Some(engine) = self.pool().running(&profile) {
            self.set_busy(false);
            self.set_status(String::new());
            self.remember_active(name, None);
            self.set_engine(Some(engine));
            return;
        }

        *self.imp().engine.borrow_mut() = None;
        self.set_engine_state(EngineState::Starting {
            profile: profile.name.clone(),
        });
        self.set_busy(true);
        self.set_status(String::new());

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
                Ok((engine, fingerprint)) => {
                    this.remember_active(&profile_name, fingerprint.as_deref());
                    this.set_engine(Some(engine));
                }
                Err(e) => {
                    let message = e.to_string();
                    this.toast(format!("{profile_name}: {message}"));
                    *this.imp().engine.borrow_mut() = None;
                    this.set_engine_state(EngineState::Failed {
                        profile: profile_name,
                        message,
                    });
                    this.restart_analysis();
                }
            }
        });
        *self.imp().activation_task.borrow_mut() = Some(handle);
    }

    /// Records the profile now in use, and any certificate fingerprint worth pinning.
    fn remember_active(&self, name: &str, fingerprint: Option<&str>) {
        let changed = {
            let mut cfg = self.config_mut();
            // A fingerprint only ever arrives from a first connect, so it is always news.
            let mut changed = fingerprint.is_some();
            if let Some(fp) = fingerprint {
                cfg.set_pin(name, fp);
            }
            if cfg.active_engine.as_deref() != Some(name) {
                cfg.active_engine = Some(name.to_string());
                changed = true;
            }
            changed
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
    /// This is the single place that turns a node into an `AnalyzeReq`; live analysis and
    /// whole-game analysis both go through it, so they cannot drift apart.
    pub fn request_for_node(&self, id: NodeId, max_visits: Option<u32>, want: Want) -> AnalyzeReq {
        let (size, rules, komi) = {
            let t = self.tree();
            (t.info.size, t.info.rules, t.info.komi)
        };
        let mut req = AnalyzeReq::new(size, rules, komi);

        self.with_tree_cached(|t| {
            // Setup stones become `initialStones` only when they precede every move, which
            // is what real SGFs do. Anything later cannot be expressed as initial stones
            // plus a move list, so that case falls back to the replayed board.
            let mut setup: Vec<(Color, Point)> = Vec::new();
            let mut moves: Vec<(Color, Point)> = Vec::new();
            let mut late_setup = false;
            for nid in t.path_to(id) {
                let node = t.node(nid);
                if !node.setup.is_empty() {
                    if moves.is_empty() {
                        for &p in &node.setup.add_black {
                            setup.push((Color::Black, p));
                        }
                        for &p in &node.setup.add_white {
                            setup.push((Color::White, p));
                        }
                        setup.retain(|(_, p)| !node.setup.add_empty.contains(p));
                    } else {
                        late_setup = true;
                    }
                }
                if let Some((c, p)) = node.mv {
                    moves.push((c, p));
                }
            }

            if late_setup {
                // Correct, just without move history.
                let pos = t.position(id);
                req.initial_player = Some(pos.to_play);
                req.initial_stones = pos
                    .board
                    .stones()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| s.map(|c| (c, Point(i as u16))))
                    .collect();
            } else {
                req.initial_stones = setup;
                if moves.is_empty() {
                    req.initial_player = Some(t.position(id).to_play);
                }
                req.moves = moves;
            }
        });

        req.want = want;
        req.max_visits = max_visits.or_else(|| Some(self.config().analysis.live_max_visits));
        req.pv_len = Some(15);
        req
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
        imp.speed.set(None);

        if !self.live_analysis() {
            return;
        }
        let Some(engine) = self.engine() else {
            return;
        };

        let (max_visits, report_every, want) = {
            let cfg = self.config();
            let mut want = Want::OWNERSHIP | Want::PV_VISITS;
            if self.policy_overlay() {
                want |= Want::POLICY;
            }
            (
                cfg.analysis.live_max_visits,
                cfg.analysis.report_interval_ms,
                want,
            )
        };

        let mut req = self.request_for_cursor(Some(max_visits), want);
        req.report_every_ms = Some(report_every);
        req.priority = 4;

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

    /// Stores a report and notifies observers. Also caches it into the node so the winrate
    /// graph and the move tree have something to draw after navigating away.
    pub fn set_report(&self, report: Arc<Report>) {
        let cursor = self.cursor();
        let max = self.config().analysis.max_suggestions as usize;
        self.imp().report.replace(Some(report.clone()));
        if let Some(mut meter) = self.imp().speed.get() {
            meter.sample(report.root.visits, Instant::now());
            self.imp().speed.set(Some(meter));
        }
        {
            let mut tree = self.imp().tree.borrow_mut();
            tree.set_analysis(cursor, Some(crate::util::analysis_of(&report, max)));
        }
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

fn should_toast_illegal_move(error: IllegalMove) -> bool {
    !matches!(error, IllegalMove::Occupied)
}

/// Measures how fast a search is running, in visits per second.
///
/// KataGo reports no timing of its own — a report carries a visit count and nothing else —
/// so the rate is measured here from the visit delta between consecutive reports over the
/// wall time between them. Reports arrive at ~10 Hz and a single interval is noisy, so the
/// samples are exponentially smoothed; the readout would otherwise be unreadable.
#[derive(Clone, Copy)]
pub struct SpeedMeter {
    mark: Instant,
    visits: u32,
    rate: f32,
}

/// Weight of the newest sample. At the default 100 ms report interval this settles within
/// about a second and still tracks a real slowdown.
const SPEED_SMOOTHING: f32 = 0.35;

impl SpeedMeter {
    /// A meter for a search dispatched at `now`, which has reported nothing yet.
    pub fn started(now: Instant) -> SpeedMeter {
        SpeedMeter {
            mark: now,
            visits: 0,
            rate: 0.0,
        }
    }

    /// The smoothed rate, or `None` before any report showed progress.
    pub fn rate(&self) -> Option<f32> {
        (self.rate > 0.0).then_some(self.rate)
    }

    /// Folds in a report of `visits` total visits observed at `now`.
    ///
    /// A report with no new visits is ignored rather than counted as a zero-rate sample:
    /// the final report is echoed as `Done`, and a search that has hit its visit cap would
    /// otherwise decay its own last reading towards zero.
    pub fn sample(&mut self, visits: u32, now: Instant) {
        let dt = now.saturating_duration_since(self.mark).as_secs_f32();
        if visits <= self.visits || dt < 0.001 {
            return;
        }
        let sample = (visits - self.visits) as f32 / dt;
        self.rate = if self.rate > 0.0 {
            self.rate + SPEED_SMOOTHING * (sample - self.rate)
        } else {
            sample
        };
        self.mark = now;
        self.visits = visits;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn occupied_points_need_no_redundant_toast() {
        assert!(!should_toast_illegal_move(IllegalMove::Occupied));
        assert!(should_toast_illegal_move(IllegalMove::Suicide));
        assert!(should_toast_illegal_move(IllegalMove::Ko));
        assert!(should_toast_illegal_move(IllegalMove::OffBoard));
    }
    #[test]
    fn speed_meter_measures_visits_per_second() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        assert_eq!(m.rate(), None, "nothing has been reported yet");

        // 600 visits in 0.5 s, counting from dispatch.
        m.sample(600, t0 + Duration::from_millis(500));
        assert!((m.rate().unwrap() - 1200.0).abs() < 1.0, "{:?}", m.rate());

        // A steady 1200/s must stay at 1200/s however it is smoothed.
        for i in 1..=10 {
            m.sample(
                600 + i * 120,
                t0 + Duration::from_millis(500 + i as u64 * 100),
            );
        }
        assert!((m.rate().unwrap() - 1200.0).abs() < 1.0, "{:?}", m.rate());
    }

    #[test]
    fn a_repeated_final_report_does_not_zero_the_rate() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        m.sample(1000, t0 + Duration::from_millis(500));
        let running = m.rate().unwrap();

        // The engine echoes its last report as `Done`: same visit count, later arrival.
        m.sample(1000, t0 + Duration::from_millis(900));
        assert_eq!(m.rate(), Some(running));
    }

    #[test]
    fn the_meter_tracks_a_slowdown() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::started(t0);
        let mut visits = 0;
        let mut at = t0;
        for _ in 0..10 {
            visits += 200;
            at += Duration::from_millis(100);
            m.sample(visits, at);
        }
        assert!((m.rate().unwrap() - 2000.0).abs() < 1.0, "{:?}", m.rate());

        for _ in 0..20 {
            visits += 20;
            at += Duration::from_millis(100);
            m.sample(visits, at);
        }
        assert!((m.rate().unwrap() - 200.0).abs() < 20.0, "{:?}", m.rate());
    }
}
