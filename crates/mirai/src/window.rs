// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The application window: layout, actions, accelerators, SGF I/O and autosave. Step 8.
//!
//! Everything the user can trigger lives here as a `win.*` action, so the header menu, the
//! bottom bar buttons, the keyboard accelerators and the shortcuts dialog all name the same
//! thing. Widgets never talk to each other; they observe [`AppState`].

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use mirai_core::{DeadSet, GameTree, NodeId, Point, sgf};
use mirai_engine::{Report, SubEvent, Want};

use crate::app::{AppState, signal};
use crate::batch::BatchAnalysis;
use crate::config::Config;
use crate::panels::AnalysisPanel;
use crate::play::PlayController;
use crate::util;
use crate::widgets::{BoardView, MoveTreeView, WinrateGraph};

/// Autosave cadence, in seconds.
const AUTOSAVE_SECS: u32 = 30;
/// Visit cap for the one-off score-estimate query.
const SCORE_VISITS: u32 = 400;
/// Ownership magnitude above which a stone counts as dead, matching KataGo's own default.
const DEAD_THRESHOLD: f32 = 0.4;

/// Everything the window owns, kept together so callbacks can clone one `Rc`.
pub struct Ui {
    pub window: adw::ApplicationWindow,
    pub state: AppState,
    pub toasts: adw::ToastOverlay,
    pub comment: gtk::TextView,
    pub play: Rc<PlayController>,
    pub batch: Rc<BatchAnalysis>,
    pub readout: gtk::Label,
    pub move_scale: gtk::Scale,
    pub engine_menu: gtk::MenuButton,
    pub file: RefCell<Option<PathBuf>>,

    /// Header title widget, so the file name and status can be refreshed in place.
    title: adw::WindowTitle,
    /// Holds the two clock labels; hidden entirely when the game is untimed.
    clock_box: gtk::Box,
    clock_black: gtk::Label,
    clock_white: gtk::Label,
    /// Switches the sidebar's Analysis page between the panel and the no-engine page.
    analysis_stack: gtk::Stack,
    /// The node the comment buffer is currently editing, so it can be flushed on the way out.
    comment_node: Cell<Option<NodeId>>,
    /// Set while the move scale is being driven programmatically.
    scale_guard: Cell<bool>,
    /// The one-off score-estimate pump, aborted if a second estimate is asked for.
    score_task: RefCell<Option<glib::JoinHandle<()>>>,
}

/// Builds and presents a window for `app`, optionally opening `path`.
pub fn present(app: &adw::Application, runtime: tokio::runtime::Handle, path: Option<PathBuf>) {
    let config_path = Config::default_path().unwrap_or_else(|_| PathBuf::from("mirai.toml"));
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(%e, "falling back to a seeded configuration");
            Config::seeded()
        }
    };
    let state = AppState::new(config, config_path, runtime);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("mirai")
        .default_width(1280)
        .default_height(860)
        .build();

    let toasts = adw::ToastOverlay::new();
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
    let play = PlayController::new(&state);
    let batch = BatchAnalysis::new(&state);

    // Content: board on top, winrate graph as a strip under it. The graph keeps its
    // requested height when the window grows — all extra space belongs to the board —
    // but the Paned still lets the user drag it taller.
    board.set_hexpand(true);
    board.set_vexpand(true);
    winrate.set_hexpand(true);
    winrate.set_size_request(-1, 170);
    let content = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .resize_start_child(true)
        .resize_end_child(false)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .start_child(&board)
        .end_child(&winrate)
        .build();

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(batch.banner());
    content_box.append(&content);
    content_box.set_hexpand(true);
    content_box.set_vexpand(true);

    // Sidebar: Analysis / Moves / Comment. The Analysis page swaps to an empty state when
    // no engine profile is configured at all.
    let analysis_stack = gtk::Stack::new();
    analysis_stack.add_named(&analysis, Some("panel"));
    analysis_stack.add_named(&crate::prefs::no_engine_status_page(), Some("empty"));
    analysis_stack.set_vexpand(true);

    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(
        &analysis_stack,
        Some("analysis"),
        "Analysis",
        "view-list-symbolic",
    );
    let tree_scroller = move_tree.in_scroller();
    tree_scroller.set_hexpand(true);
    tree_scroller.set_vexpand(true);
    stack.add_titled_with_icon(&tree_scroller, Some("moves"), "Moves", "view-grid-symbolic");
    let comment_scroll = gtk::ScrolledWindow::builder().child(&comment).build();
    comment_scroll.set_vexpand(true);
    stack.add_titled_with_icon(
        &comment_scroll,
        Some("comment"),
        "Comment",
        "text-editor-symbolic",
    );
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let sidebar_view = adw::ToolbarView::new();
    // `show_title` false would hide the title widget too, and the title widget IS the
    // Analysis/Moves/Comment switcher — without it those pages are unreachable.
    let sidebar_bar = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    sidebar_bar.set_title_widget(Some(&switcher));
    sidebar_view.add_top_bar(&sidebar_bar);
    sidebar_view.set_content(Some(&stack));
    sidebar_view.set_size_request(340, -1);

    let split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::End)
        .sidebar(&sidebar_view)
        .content(&content_box)
        .show_sidebar(true)
        .min_sidebar_width(320.0)
        .max_sidebar_width(520.0)
        .build();

    // -- header bar ---------------------------------------------------------------------

    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("Untitled", "");
    header.set_title_widget(Some(&title));

    let open_button = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("Open an SGF game record (Ctrl+O)")
        .action_name("win.open")
        .build();
    let save_button = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Save the game record (Ctrl+S)")
        .action_name("win.save")
        .build();
    header.pack_start(&open_button);
    header.pack_start(&save_button);

    let live_toggle = gtk::ToggleButton::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Live analysis (Space)")
        .build();
    state
        .bind_property("live-analysis", &live_toggle, "active")
        .bidirectional()
        .sync_create()
        .build();
    header.pack_start(&live_toggle);

    let engine_menu = gtk::MenuButton::builder()
        .label("No engine")
        .tooltip_text("Analysis engine")
        .build();
    state
        .bind_property("engine-label", &engine_menu, "label")
        .sync_create()
        .build();
    header.pack_start(&engine_menu);

    let primary = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .menu_model(&primary_menu())
        .build();
    header.pack_end(&primary);

    let new_game_button = gtk::Button::builder()
        .label("New game")
        .tooltip_text("Start a game against the engine (Ctrl+N)")
        .action_name("win.new-game")
        .build();
    header.pack_end(&new_game_button);

    let clock_black = gtk::Label::new(Some("0:00"));
    let clock_white = gtk::Label::new(Some("0:00"));
    clock_black.add_css_class("mirai-clock");
    clock_white.add_css_class("mirai-clock");
    let clock_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    clock_box.append(&clock_black);
    clock_box.append(&clock_white);
    clock_box.set_visible(false);
    header.pack_end(&clock_box);

    // -- bottom navigation bar ----------------------------------------------------------

    let readout = gtk::Label::builder()
        .label("—")
        .tooltip_text("Side to move, win rate, score lead, visits, search speed")
        .build();
    readout.add_css_class("mirai-readout");
    let move_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    move_scale.set_hexpand(true);
    move_scale.set_draw_value(false);
    move_scale.set_round_digits(0);
    move_scale.set_increments(1.0, 10.0);

    let nav = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_start(8)
        .margin_end(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    for (icon, action, tip) in [
        ("go-first-symbolic", "win.first", "First move (Home)"),
        ("go-previous-symbolic", "win.prev", "Previous move (Left)"),
        ("go-next-symbolic", "win.next", "Next move (Right)"),
        ("go-last-symbolic", "win.last", "Last move (End)"),
    ] {
        nav.append(
            &gtk::Button::builder()
                .icon_name(icon)
                .action_name(action)
                .tooltip_text(tip)
                .build(),
        );
    }
    let branches = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    branches.add_css_class("linked");
    for (icon, action, tip) in [
        ("go-up-symbolic", "win.branch-prev", "Previous variation (Up)"),
        ("go-down-symbolic", "win.branch-next", "Next variation (Down)"),
    ] {
        branches.append(
            &gtk::Button::builder()
                .icon_name(icon)
                .action_name(action)
                .tooltip_text(tip)
                .build(),
        );
    }
    nav.append(&branches);
    nav.append(&move_scale);
    nav.append(&readout);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_bottom_bar(&nav);
    toolbar.set_content(Some(&split));

    toasts.set_child(Some(&toolbar));
    window.set_content(Some(&toasts));

    // Collapse the sidebar on narrow windows.
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        900.0,
        adw::LengthUnit::Sp,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    let ui = Rc::new(Ui {
        window: window.clone(),
        state: state.clone(),
        toasts: toasts.clone(),
        comment,
        play: play.clone(),
        batch: batch.clone(),
        readout,
        move_scale,
        engine_menu,
        file: RefCell::new(None),
        title,
        clock_box,
        clock_black,
        clock_white,
        analysis_stack,
        comment_node: Cell::new(None),
        scale_guard: Cell::new(false),
        score_task: RefCell::new(None),
    });

    connect_toasts(&ui);
    install_actions(&ui);
    connect_state(&ui);
    connect_comment(&ui);
    connect_scale(&ui);

    // Cross-widget wiring the two panels cannot do for themselves.
    play.attach_board(&board);
    {
        let board = board.clone();
        analysis.connect_pv_preview(move |index| board.set_pv_preview(index));
    }
    {
        let analysis = analysis.clone();
        batch.connect_finished(move |rows| analysis.set_blunders(rows));
    }
    {
        let batch = batch.clone();
        play.set_analyse_hook(move || batch.start());
    }

    refresh_engine_menu(&ui);
    update_analysis_page(&ui);
    update_scale(&ui);
    update_readout(&ui);
    update_title(&ui);
    update_subtitle(&ui);
    load_comment(&ui);

    // A crash leaves the clean-exit flag missing; that is how the restore offer is armed.
    // An autosave with nothing in it is never worth offering, and a corrupt one is deleted
    // rather than shown.
    let stale_autosave = match autosave_paths() {
        Some((autosave, flag)) => {
            let stale = !flag.exists() && autosave.exists();
            let _ = std::fs::remove_file(&flag);
            stale.then_some(autosave).filter(|path| {
                match std::fs::read(path)
                    .ok()
                    .and_then(|bytes| mirai_core::sgf::parse(&bytes).ok())
                {
                    Some(games) => games.iter().any(tree_has_content),
                    None => {
                        let _ = std::fs::remove_file(path);
                        false
                    }
                }
            })
        }
        None => None,
    };

    install_autosave(&ui);
    connect_close(&ui);

    // Start the configured engine, if any.
    let active = state.config().active_profile().map(|p| p.name.clone());
    if let Some(name) = active {
        state.activate_profile(&name);
    }

    window.present();

    if let Some(path) = path {
        load_sgf(&ui, &path, true);
    } else if let Some(autosave) = stale_autosave {
        offer_restore(&ui, autosave);
    }

    if state.config().engine_profiles.is_empty() {
        crate::prefs::present(&ui.window, &state);
    }
}

// -- menus ------------------------------------------------------------------------------

fn primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let file = gio::Menu::new();
    file.append(Some("_New Game…"), Some("win.new-game"));
    // Passing and undo have accelerators; resigning deliberately does not, but it still
    // needs a way in — otherwise only the engine can ever concede a game.
    file.append(Some("_Resign"), Some("win.resign"));
    file.append(Some("_Open…"), Some("win.open"));
    file.append(Some("_Save"), Some("win.save"));
    file.append(Some("Save _As…"), Some("win.save-as"));
    menu.append_section(None, &file);

    let clip = gio::Menu::new();
    clip.append(Some("_Copy SGF"), Some("win.copy-sgf"));
    clip.append(Some("_Paste SGF"), Some("win.paste-sgf"));
    menu.append_section(None, &clip);

    let analyse = gio::Menu::new();
    analyse.append(Some("Analyse _Game"), Some("win.analyse-game"));
    analyse.append(Some("_Estimate Score"), Some("win.score"));
    menu.append_section(None, &analyse);

    let view = gio::Menu::new();
    view.append(Some("Coordinates"), Some("win.toggle-coords"));
    view.append(Some("Move Numbers"), Some("win.toggle-move-numbers"));
    view.append(Some("Ownership Overlay"), Some("win.toggle-ownership"));
    view.append(Some("Policy Overlay"), Some("win.toggle-policy"));
    menu.append_section(None, &view);

    let app = gio::Menu::new();
    app.append(Some("_Preferences"), Some("win.preferences"));
    app.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
    app.append(Some("_About mirai"), Some("win.about"));
    menu.append_section(None, &app);

    menu
}

fn refresh_engine_menu(ui: &Rc<Ui>) {
    let model = crate::prefs::engine_menu_model(&ui.state);
    ui.engine_menu.set_menu_model(Some(&model));
}

// -- signal wiring ----------------------------------------------------------------------

/// Runs `f` with the live `Ui`, or does nothing once the window is gone.
///
/// Signal handlers outlive what they act on: they hang off widgets, and off the
/// [`AppState`], that the `Ui` itself owns. A strong `Rc<Ui>` inside one is therefore a
/// cycle nothing ever breaks — `Ui -> AppState -> handler -> Ui` — and it would keep the
/// window, its engine, the KataGo process behind that engine and the runtime handle alive
/// for the rest of the session. Handlers capture a `Weak<Ui>` and come through here
/// instead. The one deliberate owning capture is in [`connect_close`], which drops the
/// `Ui` as the window closes.
///
/// Generic over the pointee only so the discipline is testable without a display.
fn with_ui<T>(weak: &Weak<T>, f: impl FnOnce(&Rc<T>)) {
    if let Some(ui) = weak.upgrade() {
        f(&ui);
    }
}

fn connect_toasts(ui: &Rc<Ui>) {
    let toasts = ui.toasts.clone();
    ui.state.connect_closure(
        signal::TOAST,
        false,
        glib::closure_local!(move |_: AppState, text: String| {
            toasts.add_toast(adw::Toast::new(&text));
        }),
    );
}

fn connect_state(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    {
        let weak = weak.clone();
        ui.state.connect_closure(
            signal::CURSOR_CHANGED,
            false,
            glib::closure_local!(move |_: AppState| {
                with_ui(&weak, |ui| {
                    flush_comment(ui);
                    load_comment(ui);
                    update_scale(ui);
                    update_readout(ui);
                    update_clocks(ui);
                });
            }),
        );
    }
    {
        let weak = weak.clone();
        ui.state.connect_closure(
            signal::TREE_CHANGED,
            false,
            glib::closure_local!(move |_: AppState| {
                with_ui(&weak, |ui| {
                    update_scale(ui);
                    update_title(ui);
                });
            }),
        );
    }
    {
        let weak = weak.clone();
        ui.state.connect_closure(
            signal::REPORT,
            false,
            glib::closure_local!(move |_: AppState| {
                with_ui(&weak, update_readout);
            }),
        );
    }
    {
        let weak = weak.clone();
        ui.state.connect_closure(
            signal::ENGINE_CHANGED,
            false,
            glib::closure_local!(move |_: AppState| {
                with_ui(&weak, |ui| {
                    refresh_engine_menu(ui);
                    update_analysis_page(ui);
                    update_subtitle(ui);
                });
            }),
        );
    }
    {
        let weak = weak.clone();
        ui.state.connect_closure(
            signal::PLAY_CHANGED,
            false,
            glib::closure_local!(move |_: AppState| {
                with_ui(&weak, update_clocks);
            }),
        );
    }
    let state = ui.state.clone();
    {
        let weak = weak.clone();
        state.connect_modified_notify(move |_| with_ui(&weak, update_title));
    }
    {
        let weak = weak.clone();
        state.connect_status_notify(move |_| {
            with_ui(&weak, |ui| {
                update_subtitle(ui);
                update_readout(ui);
            });
        });
    }
    state.connect_engine_label_notify(move |_| with_ui(&weak, update_subtitle));
}

fn connect_scale(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    ui.move_scale.connect_value_changed(move |scale| {
        with_ui(&weak, |ui| {
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

fn connect_comment(ui: &Rc<Ui>) {
    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(ui);
    focus.connect_leave(move |_| with_ui(&weak, flush_comment));
    ui.comment.add_controller(focus);
}

/// Connects the window's teardown handler, which is also the sole owner of the [`Ui`].
///
/// Every other handler holds a `Weak<Ui>` (see [`with_ui`]), so this closure's strong
/// reference is the only thing keeping the window's state alive while it is open. That is
/// a cycle — `Ui -> window -> this handler -> Ui` — but a cycle with exactly one owner and
/// one release point: the `Ui` is *taken* out of the cell here rather than borrowed, so it
/// is dropped as the window closes, and with it the `AppState`, the engine (terminating
/// KataGo) and the runtime handle. The `Ui` is unquestionably alive when `close-request`
/// fires, so the final autosave and the clean-exit flag are written as before.
fn connect_close(ui: &Rc<Ui>) {
    let owner = Cell::new(Some(ui.clone()));
    ui.window.connect_close_request(move |_| {
        // A second close-request finds the cell empty; there is nothing left to tear down.
        let Some(ui) = owner.take() else {
            return glib::Propagation::Proceed;
        };
        flush_comment(&ui);
        write_autosave(&ui);
        if let Some((_, flag)) = autosave_paths() {
            if let Some(dir) = flag.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&flag, b"clean\n");
        }
        ui.batch.cancel();
        ui.play.stop();
        if let Some(task) = ui.score_task.borrow_mut().take() {
            task.abort();
        }
        // Dropping the engine terminates KataGo; live analysis has to stop first so the
        // pump releases its subscription.
        ui.state.set_live_analysis(false);
        ui.state.save_config();
        ui.state.set_engine(None);
        // Last strong reference: the window's state is released here, not at process exit.
        drop(ui);
        glib::Propagation::Proceed
    });
}

fn install_autosave(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    glib::timeout_add_seconds_local(AUTOSAVE_SECS, move || match weak.upgrade() {
        Some(ui) => {
            write_autosave(&ui);
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
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

fn update_scale(ui: &Rc<Ui>) {
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

fn update_readout(ui: &Rc<Ui>) {
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

fn update_clocks(ui: &Rc<Ui>) {
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

fn update_title(ui: &Rc<Ui>) {
    let name = ui
        .file
        .borrow()
        .as_ref()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Untitled".to_string());
    let shown = if ui.state.modified() {
        format!("{name} •")
    } else {
        name
    };
    ui.title.set_title(&shown);
    ui.window.set_title(Some(&format!("{shown} — mirai")));
}

fn update_subtitle(ui: &Rc<Ui>) {
    let status = ui.state.status();
    let subtitle = if status.is_empty() {
        ui.state.engine_label()
    } else {
        status
    };
    ui.title.set_subtitle(&subtitle);
}

fn update_analysis_page(ui: &Rc<Ui>) {
    let empty = ui.state.config().engine_profiles.is_empty();
    ui.analysis_stack
        .set_visible_child_name(if empty { "empty" } else { "panel" });
}

// -- comment pane -----------------------------------------------------------------------

fn flush_comment(ui: &Rc<Ui>) {
    let Some(id) = ui.comment_node.get() else {
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
    ui.comment_node.set(Some(id));
}

fn load_comment(ui: &Rc<Ui>) {
    let id = ui.state.cursor();
    let text = {
        let tree = ui.state.tree();
        tree.node(id).comment.clone()
    };
    ui.comment_node.set(None);
    ui.comment.buffer().set_text(&text);
    ui.comment_node.set(Some(id));
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

fn sgf_text(ui: &Rc<Ui>) -> String {
    let include = ui.state.config().ui.save_analysis_in_sgf;
    let tree = ui.state.tree();
    sgf::write(&tree, include)
}

/// Installs `tree` as the current game. `path` is remembered for plain Save.
fn adopt(ui: &Rc<Ui>, tree: GameTree, path: Option<PathBuf>) {
    ui.play.stop();
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
}

fn load_sgf(ui: &Rc<Ui>, path: &Path, remember: bool) {
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
    let mut bits = vec![format!("{}×{}", info.size.w, info.size.h), format!("{moves} moves")];
    if !info.result.is_empty() {
        bits.push(info.result.clone());
    }
    if !info.date.is_empty() {
        bits.push(info.date.clone());
    }
    adw::ActionRow::builder()
        .title(format!("{} vs {}", who(&info.players[0]), who(&info.players[1])))
        .subtitle(bits.join(" · "))
        .build()
}

fn choose_game(ui: &Rc<Ui>, trees: Vec<GameTree>, path: Option<PathBuf>, label: String) {
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

    let pool = Rc::new(RefCell::new(trees));
    // Strong on purpose: the response arrives after this call returns, and the dialog —
    // which owns the handler — is dismissed either way, releasing the clone with it.
    let ui2 = ui.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "open" {
            return;
        }
        let index = list.selected_row().map(|r| r.index() as usize).unwrap_or(0);
        let mut pool = pool.borrow_mut();
        if index < pool.len() {
            let tree = pool.remove(index);
            drop(pool);
            adopt(&ui2, tree, path.clone());
        }
    });
    dialog.present(Some(&ui.window));
}

fn do_open(ui: &Rc<Ui>) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title("Open SGF");
    let (filters, default) = sgf_filters();
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&default));
    // Strong on purpose: this future must hold the window open until the user answers the
    // file dialog. It completes and drops the clone; a failed upgrade would eat the Open.
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match dialog.open_future(Some(&ui.window)).await {
            Ok(file) => match file.path() {
                Some(path) => load_sgf(&ui, &path, true),
                None => ui.state.toast("That location is not a local file"),
            },
            Err(e) => {
                if !e.matches(gtk::DialogError::Dismissed) {
                    ui.state.toast(format!("Could not open: {e}"));
                }
            }
        }
    });
}

fn write_to(ui: &Rc<Ui>, path: &Path) {
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

fn do_save(ui: &Rc<Ui>) {
    flush_comment(ui);
    let existing = ui.file.borrow().clone();
    match existing {
        Some(path) => write_to(ui, &path),
        None => do_save_as(ui),
    }
}

fn do_save_as(ui: &Rc<Ui>) {
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
    // Strong on purpose: a weak handle that failed to upgrade here would silently drop the
    // user's Save. The future finishes with the dialog and releases the clone.
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match dialog.save_future(Some(&ui.window)).await {
            Ok(file) => match file.path() {
                Some(path) => write_to(&ui, &path),
                None => ui.state.toast("That location is not a local file"),
            },
            Err(e) => {
                if !e.matches(gtk::DialogError::Dismissed) {
                    ui.state.toast(format!("Could not save: {e}"));
                }
            }
        }
    });
}

// -- autosave ---------------------------------------------------------------------------

/// `(autosave.sgf, clean-exit)` in the data directory.
fn autosave_paths() -> Option<(PathBuf, PathBuf)> {
    let dir = Config::data_dir().ok()?;
    Some((dir.join("autosave.sgf"), dir.join("clean-exit")))
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

fn write_autosave(ui: &Rc<Ui>) {
    let Some((autosave, _)) = autosave_paths() else {
        return;
    };
    if !tree_has_content(&ui.state.tree()) {
        // Leave no bait for the restore prompt.
        let _ = std::fs::remove_file(&autosave);
        return;
    }
    if let Some(dir) = autosave.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!(%e, "could not create the data directory");
        return;
    }
    let text = sgf_text(ui);
    if let Err(e) = std::fs::write(&autosave, text) {
        tracing::warn!(%e, "could not write the autosave");
    }
}

fn offer_restore(ui: &Rc<Ui>, autosave: PathBuf) {
    let dialog = adw::AlertDialog::new(
        Some("Restore the last game?"),
        Some(
            "mirai did not shut down cleanly. An autosaved copy of the game record you were \
             looking at is available.",
        ),
    );
    dialog.add_responses(&[("discard", "Discard"), ("restore", "Restore")]);
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("restore"));
    dialog.set_close_response("discard");
    // Strong on purpose: the dialog owns this handler and outlives the call; it is dropped
    // once the user answers, and the restore must not be skipped while it is up.
    let ui2 = ui.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "restore" {
            // Deliberately not remembered as the save target: it is not the user's file.
            load_sgf(&ui2, &autosave, false);
        } else {
            let _ = std::fs::remove_file(&autosave);
        }
    });
    dialog.present(Some(&ui.window));
}

// -- score estimate ---------------------------------------------------------------------

fn do_score(ui: &Rc<Ui>) {
    let Some(engine) = ui.state.engine() else {
        ui.state.toast("No engine to estimate the score with");
        return;
    };
    if let Some(task) = ui.score_task.borrow_mut().take() {
        task.abort();
    }
    let mut req = ui.state.request_for_cursor(Some(SCORE_VISITS), Want::OWNERSHIP);
    req.report_every_ms = None;
    req.priority = 8;
    let mut sub = engine.subscribe(req);

    ui.state.set_status("Estimating the score…".to_string());
    // Strong on purpose: the pump must outlive this call to deliver the estimate. It ends
    // at Done/Failed, and `connect_close` aborts it, so the clone is always released.
    let ui2 = ui.clone();
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
                    ui2.state.set_status(String::new());
                    ui2.state.on_engine_error(e);
                    return;
                }
            }
        }
        ui2.state.set_status(String::new());
        let Some(report) = last else {
            ui2.state.toast("The engine returned no estimate");
            return;
        };
        show_estimate(&ui2, &report);
    });
    *ui.score_task.borrow_mut() = Some(handle);
}

fn show_estimate(ui: &Rc<Ui>, report: &Report) {
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
        if result.approximate { " (estimated)" } else { "" },
        result.black,
        result.white,
        util::si_visits(report.root.visits),
        util::signed1(lead),
    );
    let dialog = adw::AlertDialog::new(Some("Score estimate"), Some(&body));
    dialog.add_responses(&[("close", "Close")]);
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(&ui.window));
}

// -- dialogs ----------------------------------------------------------------------------

fn show_shortcuts(ui: &Rc<Ui>) {
    let dialog = adw::ShortcutsDialog::new();
    for (title, items) in [
        (
            "Navigation",
            &[
                ("First move", "win.first"),
                ("Last move", "win.last"),
                ("Previous move", "win.prev"),
                ("Next move", "win.next"),
                ("Back ten moves", "win.prev10"),
                ("Forward ten moves", "win.next10"),
                ("Previous variation", "win.branch-prev"),
                ("Next variation", "win.branch-next"),
            ][..],
        ),
        (
            "Analysis",
            &[
                ("Live analysis", "win.toggle-analysis"),
                ("Analyse whole game", "win.analyse-game"),
                ("Estimate score", "win.score"),
                ("Ownership overlay", "win.toggle-ownership"),
                ("Policy overlay", "win.toggle-policy"),
                ("Coordinates", "win.toggle-coords"),
                ("Move numbers", "win.toggle-move-numbers"),
            ][..],
        ),
        (
            "Game",
            &[
                ("New game", "win.new-game"),
                ("Pass", "win.pass"),
                ("Undo", "win.undo"),
                ("Delete branch", "win.delete-branch"),
            ][..],
        ),
        (
            "File",
            &[
                ("Open", "win.open"),
                ("Save", "win.save"),
                ("Save as", "win.save-as"),
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
    dialog.present(Some(&ui.window));
}

fn show_about(ui: &Rc<Ui>) {
    let about = adw::AboutDialog::builder()
        .application_name("mirai")
        .application_icon("io.github.mirai.Mirai")
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("mirai")
        .comments("A KataGo analysis and playing board for GNOME.")
        .build();
    about.present(Some(&ui.window));
}

// -- actions ----------------------------------------------------------------------------

/// Deletes the branch starting at the cursor and steps back to its parent.
fn delete_branch(ui: &Rc<Ui>) {
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

/// The body of a window action: a boxed closure over the built UI.
type UiAction = Box<dyn Fn(&Rc<Ui>)>;

fn install_actions(ui: &Rc<Ui>) {
    let group = gio::SimpleActionGroup::new();
    // The group is inserted on the window, so every action outlives the `Ui` unless it
    // holds only a weak handle. Downgrading once here fixes all of them.
    let weak = Rc::downgrade(ui);
    let add = |name: &str, f: UiAction| {
        let action = gio::SimpleAction::new(name, None);
        let weak = weak.clone();
        action.connect_activate(move |_, _| with_ui(&weak, |ui| f(ui)));
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
    add(
        "toggle-ownership",
        Box::new(|ui| ui.state.set_ownership_overlay(!ui.state.ownership_overlay())),
    );
    add(
        "toggle-policy",
        Box::new(|ui| ui.state.set_policy_overlay(!ui.state.policy_overlay())),
    );
    add(
        "toggle-coords",
        Box::new(|ui| ui.state.set_show_coordinates(!ui.state.show_coordinates())),
    );
    add(
        "toggle-move-numbers",
        Box::new(|ui| ui.state.set_show_move_numbers(!ui.state.show_move_numbers())),
    );

    add(
        "pass",
        Box::new(|ui| {
            if ui.play.is_active() {
                ui.play.pass();
                return;
            }
            let to_play = ui.state.to_play();
            if let Err(e) = ui.state.play_move(to_play, Point::PASS) {
                ui.state.toast(e.to_string());
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
            // Strong on purpose: the clipboard read resolves after this action returns,
            // and the future drops the clone as soon as the paste is done.
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                let text = match clipboard.read_text_future().await {
                    Ok(Some(t)) => t,
                    Ok(None) => {
                        ui.state.toast("The clipboard holds no text");
                        return;
                    }
                    Err(e) => {
                        ui.state.toast(format!("Clipboard: {e}"));
                        return;
                    }
                };
                match sgf::parse_str(&text) {
                    Ok(mut trees) if !trees.is_empty() => {
                        let tree = trees.remove(0);
                        let moves = tree.main_line().len().saturating_sub(1);
                        adopt(&ui, tree, None);
                        ui.state.toast(format!("Pasted a game of {moves} moves"));
                    }
                    Ok(_) => ui.state.toast("The clipboard holds no game record"),
                    Err(e) => ui.state.toast(format!("Clipboard: {e}")),
                }
            });
        }),
    );

    add("analyse-game", Box::new(|ui| ui.batch.start()));
    add("score", Box::new(do_score));
    add(
        "new-game",
        Box::new(|ui| {
            let play = ui.play.clone();
            crate::dialogs::new_game(&ui.window, &ui.state, move |setup| play.start(setup));
        }),
    );
    add(
        "preferences",
        Box::new(|ui| crate::prefs::present(&ui.window, &ui.state)),
    );
    add("shortcuts", Box::new(show_shortcuts));
    add("about", Box::new(show_about));

    // Stateful so the engine menu can render a radio dot next to the live profile.
    let initial = ui
        .state
        .config()
        .active_engine
        .clone()
        .unwrap_or_default();
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
            with_ui(&weak, |ui| ui.state.activate_profile(name));
        });
    }
    group.add_action(&set_engine);

    ui.window.insert_action_group("win", Some(&group));

    if let Some(app) = ui.window.application() {
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
            ("win.save", &["<Control>s"]),
            ("win.save-as", &["<Control><Shift>s"]),
            ("win.copy-sgf", &["<Control>c"]),
            ("win.paste-sgf", &["<Control>v"]),
            ("win.toggle-ownership", &["o"]),
            ("win.toggle-policy", &["y"]),
            ("win.toggle-coords", &["c"]),
            ("win.toggle-move-numbers", &["n"]),
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
    use std::cell::Cell;
    use std::rc::Rc;

    use super::{tree_has_content, with_ui};
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

    /// A stand-in for `Ui`: the real thing is full of GTK objects, which cannot be built
    /// without `gtk::init` and a display. `with_ui` is generic exactly so the capture
    /// discipline it enforces can be checked here.
    struct Window {
        torn_down: Cell<bool>,
    }

    #[test]
    fn handlers_do_not_keep_the_window_alive() {
        let window = Rc::new(Window {
            torn_down: Cell::new(false),
        });
        // What every signal handler on the window now captures.
        let handler = Rc::downgrade(&window);
        assert_eq!(
            Rc::strong_count(&window),
            1,
            "a handler's handle must not own the window"
        );

        let fired = Cell::new(0u32);
        with_ui(&handler, |w| {
            fired.set(fired.get() + 1);
            w.torn_down.set(true);
        });
        assert_eq!(fired.get(), 1, "a live window runs the handler body once");
        assert!(window.torn_down.get(), "the body saw the real window");

        // Closing the window drops the sole owner (in the real thing, the cell inside
        // `connect_close`). Nothing else may hold it back.
        drop(window);
        assert!(
            handler.upgrade().is_none(),
            "the window leaked: a handler is still holding it alive"
        );

        with_ui(&handler, |_| fired.set(fired.get() + 1));
        assert_eq!(
            fired.get(),
            1,
            "a handler firing after the window is gone must do nothing"
        );
    }
}
