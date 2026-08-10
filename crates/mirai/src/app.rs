// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `AppState` — the single source of truth every widget observes.
//!
//! It is a `glib::Object` subclass so display toggles can be bound with `notify::`, and it
//! emits a handful of custom signals for structural changes. Widgets never reach into each
//! other; they read `AppState` and listen to its signals.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::path::PathBuf;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use mirai_core::{Color, GameInfo, GameTree, IllegalMove, NodeId, Point, Position, RuleSet, Size};
use mirai_engine::{AnalyzeReq, Engine, EngineDesc, EngineError, Report, SubEvent, Want};

use crate::config::{Config, EngineProfile, ProfileKind};

/// Signal names, so typos are compile-time-ish rather than silent no-ops.
pub mod signal {
    /// The tree's shape or a node's content changed.
    pub const TREE_CHANGED: &str = "tree-changed";
    /// The cursor moved to a different node.
    pub const CURSOR_CHANGED: &str = "cursor-changed";
    /// A new live analysis report is available from [`super::AppState::last_report`].
    pub const REPORT: &str = "report";
    /// The active engine changed (or went away).
    pub const ENGINE_CHANGED: &str = "engine-changed";
    /// A user-visible message; the window turns it into an `adw::Toast`.
    pub const TOAST: &str = "toast";
    /// The play session changed state (turn, clock, game over).
    pub const PLAY_CHANGED: &str = "play-changed";
    /// Whole-game analysis progress changed; `(done, total)`.
    pub const BATCH_PROGRESS: &str = "batch-progress";
}

mod imp {
    use super::*;
    use glib::subclass::Signal;
    use std::sync::LazyLock;

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
        pub config: RefCell<Config>,
        pub config_path: RefCell<PathBuf>,
        pub engine: RefCell<Option<Arc<dyn Engine>>>,
        pub runtime: RefCell<Option<tokio::runtime::Handle>>,
        pub report: RefCell<Option<Arc<Report>>>,
        /// The live-analysis pump. Aborting it drops the `Subscription`, which terminates
        /// the KataGo query — that is the whole cancellation mechanism.
        pub pump: RefCell<Option<glib::JoinHandle<()>>>,
        /// Bumped on every `activate_profile` so a slow engine start can tell it has been
        /// superseded by a newer selection.
        pub activation: Cell<u64>,
        /// Bumped on every analysis restart so a stale pump can tell it has been superseded.
        pub generation: Cell<u64>,
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
                config: RefCell::new(Config::default()),
                config_path: RefCell::new(PathBuf::new()),
                engine: RefCell::new(None),
                runtime: RefCell::new(None),
                report: RefCell::new(None),
                pump: RefCell::new(None),
                activation: Cell::new(0),
                generation: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AppState {
        const NAME: &'static str = "MiraiAppState";
        type Type = super::AppState;
    }

    #[glib::derived_properties]
    impl ObjectImpl for AppState {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder(signal::TREE_CHANGED).build(),
                    Signal::builder(signal::CURSOR_CHANGED).build(),
                    Signal::builder(signal::REPORT).build(),
                    Signal::builder(signal::ENGINE_CHANGED).build(),
                    Signal::builder(signal::TOAST)
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder(signal::PLAY_CHANGED).build(),
                    Signal::builder(signal::BATCH_PROGRESS)
                        .param_types([u32::static_type(), u32::static_type()])
                        .build(),
                ]
            });
            &SIGNALS
        }
    }

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
    pub fn new(config: Config, config_path: PathBuf, runtime: tokio::runtime::Handle) -> AppState {
        let this: AppState = glib::Object::new();
        let imp = this.imp();
        this.set_show_coordinates(config.ui.show_coordinates);
        this.set_show_move_numbers(config.ui.show_move_numbers);
        this.set_ownership_overlay(config.ui.ownership_overlay);
        this.set_policy_overlay(config.ui.policy_overlay);
        *imp.config.borrow_mut() = config;
        *imp.config_path.borrow_mut() = config_path;
        *imp.runtime.borrow_mut() = Some(runtime);
        this
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
    pub fn save_config(&self) {
        {
            let mut cfg = self.imp().config.borrow_mut();
            cfg.ui.show_coordinates = self.show_coordinates();
            cfg.ui.show_move_numbers = self.show_move_numbers();
            cfg.ui.ownership_overlay = self.ownership_overlay();
            cfg.ui.policy_overlay = self.policy_overlay();
        }
        let path = self.config_path();
        let result = self.imp().config.borrow().save(&path);
        if let Err(e) = result {
            tracing::warn!(%e, "could not save the configuration");
            self.toast(format!("Could not save settings: {e}"));
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
        self.emit_by_name::<()>(signal::TREE_CHANGED, &[]);
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
            imp.cursor.set(cursor.unwrap_or(root));
        }
        self.set_modified(false);
        self.emit_by_name::<()>(signal::TREE_CHANGED, &[]);
        self.emit_by_name::<()>(signal::CURSOR_CHANGED, &[]);
        self.restart_analysis();
    }

    pub fn cursor(&self) -> NodeId {
        self.imp().cursor.get()
    }

    pub fn set_cursor(&self, id: NodeId) {
        if self.imp().cursor.get() == id {
            return;
        }
        self.imp().cursor.set(id);
        self.imp().report.replace(None);
        self.emit_by_name::<()>(signal::CURSOR_CHANGED, &[]);
        self.emit_by_name::<()>(signal::REPORT, &[]);
        self.restart_analysis();
    }

    /// The board position at the cursor.
    pub fn position(&self) -> Position {
        let cursor = self.cursor();
        self.with_tree_cached(|t| t.position(cursor).clone())
    }

    pub fn to_play(&self) -> Color {
        self.position().to_play
    }

    /// Plays a move at the cursor and moves the cursor onto it.
    pub fn play_move(&self, color: Color, p: Point) -> Result<NodeId, IllegalMove> {
        let cursor = self.cursor();
        let id = self.with_tree_mut(|t| t.play(cursor, color, p))?;
        self.imp().cursor.set(id);
        self.imp().report.replace(None);
        self.emit_by_name::<()>(signal::CURSOR_CHANGED, &[]);
        self.emit_by_name::<()>(signal::REPORT, &[]);
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

    pub fn set_engine(&self, engine: Option<Arc<dyn Engine>>) {
        let label = match &engine {
            Some(e) => {
                let d = e.describe();
                if d.katago_version.is_empty() {
                    d.name
                } else {
                    format!("{} ({})", d.name, d.katago_version)
                }
            }
            None => "No engine".to_string(),
        };
        *self.imp().engine.borrow_mut() = engine;
        self.set_engine_label(label);
        self.emit_by_name::<()>(signal::ENGINE_CHANGED, &[]);
        self.restart_analysis();
    }

    /// Starts (or connects to) the named profile and installs it as the active engine.
    ///
    /// Runs the blocking work on the tokio runtime and lands the result back on the GTK
    /// main context.
    pub fn activate_profile(&self, name: &str) {
        let Some(profile) = self.config().profile(name).cloned() else {
            self.toast(format!("No engine profile named “{name}”"));
            return;
        };
        // Starting an engine is slow (a local KataGo takes seconds to load its net). If the
        // user picks another engine meanwhile, the older start MUST NOT install itself over
        // the newer one — that used to silently drop a freshly connected remote engine.
        let activation = self
            .imp()
            .activation
            .get()
            .wrapping_add(1);
        self.imp().activation.set(activation);

        self.set_engine(None);
        self.set_busy(true);
        self.set_status(format!("Starting {}…", profile.name));

        let handle = self.runtime();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let log_dir = crate::config::Config::data_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("katago-logs");
        handle.spawn(async move {
            let _ = tx.send(build_engine(profile, log_dir).await);
        });

        let this = self.clone();
        let profile_name = name.to_string();
        glib::spawn_future_local(async move {
            let result = rx.await;
            if this.imp().activation.get() != activation {
                tracing::debug!(
                    profile = %profile_name,
                    "discarding a superseded engine activation"
                );
                return;
            }
            this.set_busy(false);
            match result {
                Ok(Ok((engine, fingerprint))) => {
                    {
                        let mut cfg = this.config_mut();
                        if let Some(fp) = fingerprint {
                            cfg.set_pin(&profile_name, &fp);
                        }
                        cfg.active_engine = Some(profile_name.clone());
                    }
                    this.save_config();
                    this.set_status(String::new());
                    this.set_engine(Some(engine));
                }
                Ok(Err(e)) => {
                    this.set_status(String::new());
                    this.toast(format!("{profile_name}: {e}"));
                    this.set_engine(None);
                }
                Err(_) => {
                    this.set_status(String::new());
                    this.toast("Engine startup was cancelled".to_string());
                }
            }
        });
    }

    // -- analysis -----------------------------------------------------------------------

    pub fn last_report(&self) -> Option<Arc<Report>> {
        self.imp().report.borrow().clone()
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
        {
            let mut tree = self.imp().tree.borrow_mut();
            tree.set_analysis(cursor, Some(crate::util::analysis_of(&report, max)));
        }
        self.emit_by_name::<()>(signal::REPORT, &[]);
    }

    pub fn on_engine_error(&self, e: EngineError) {
        tracing::warn!(%e, "engine error");
        self.toast(e.to_string());
        if matches!(e, EngineError::EngineExited(_) | EngineError::Startup(_)) {
            self.set_engine(None);
        }
    }

    pub fn toast(&self, msg: impl Into<String>) {
        self.emit_by_name::<()>(signal::TOAST, &[&msg.into()]);
    }

    pub fn notify_play_changed(&self) {
        self.emit_by_name::<()>(signal::PLAY_CHANGED, &[]);
    }

    pub fn notify_batch_progress(&self, done: u32, total: u32) {
        self.emit_by_name::<()>(signal::BATCH_PROGRESS, &[&done, &total]);
    }
}

/// Builds an engine from a profile. Returns the engine and, for remote profiles, the
/// certificate fingerprint that should be pinned.
async fn build_engine(
    profile: EngineProfile,
    log_dir: PathBuf,
) -> Result<(Arc<dyn Engine>, Option<String>), EngineError> {
    match profile.kind {
        ProfileKind::Local {
            katago,
            model,
            config,
            analysis_threads,
            search_threads,
        } => {
            let mut cfg =
                mirai_engine::LocalEngineConfig::new(profile.name.clone(), katago, model, config);
            cfg.log_dir = log_dir;
            cfg.analysis_threads = analysis_threads;
            cfg.search_threads = search_threads;
            let engine = mirai_engine::LocalEngine::spawn(cfg).await?;
            Ok((Arc::new(engine) as Arc<dyn Engine>, None))
        }
        ProfileKind::Remote {
            url,
            token,
            engine,
            cert_sha256,
        } => {
            let remote =
                mirai_engine::RemoteEngine::connect(&url, &token, engine, cert_sha256).await?;
            let fingerprint = remote.fingerprint().to_string();
            Ok((Arc::new(remote) as Arc<dyn Engine>, Some(fingerprint)))
        }
    }
}
