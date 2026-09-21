// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The preferences dialog, the engine-profile editors and the header-bar engine menu.
//! Step 13.
//!
//! Everything here writes straight through to [`crate::config::Config`] and calls
//! [`AppState::save_config`], so the on-disk `config.toml` is always what the dialog shows.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use mirai_core::RuleSet;

use crate::app::{AppState, Change};
use crate::config::{
    AnalysisSettings, EngineProfile, PlaySettings, ProfileKind, StrengthSetting, UiSettings,
};
use crate::preferences_shell::{PreferencesDialog, PreferencesWidgets};
use crate::profile_editor::ProfileEditorPage;
use mirai_engine::{CalibrationConfig, CalibrationProgress, CalibrationResult, EngineTuning};
type WindowWatch = (gtk::Application, glib::SignalHandlerId);

struct CalibrationRun {
    state: AppState,
    previous: Option<String>,
    save: gtk::Button,
    tune: gtk::Button,
    task: tokio::task::JoinHandle<()>,
    window_watch: Option<WindowWatch>,
}

impl CalibrationRun {
    fn disconnect_window_watch(&mut self) {
        if let Some((app, id)) = self.window_watch.take() {
            app.disconnect(id);
        }
    }

    fn finish(&mut self) {
        self.disconnect_window_watch();
        self.save.set_sensitive(true);
        self.tune.set_sensitive(true);
        restore_engine(&self.state, self.previous.as_deref());
    }

    fn cancel(mut self) {
        self.disconnect_window_watch();
        self.task.abort();
        let state = self.state.clone();
        let previous = self.previous.clone();
        let save = self.save.clone();
        let tune = self.tune.clone();
        // Strong transient capture: this ends after the shutdown grace.
        glib::spawn_future_local(async move {
            glib::timeout_future(
                mirai_engine::LOCAL_ENGINE_SHUTDOWN_GRACE + std::time::Duration::from_millis(250),
            )
            .await;
            restore_engine(&state, previous.as_deref());
            save.set_sensitive(true);
            tune.set_sensitive(true);
        });
    }
}

/// Builds, wires and presents the preferences dialog.
pub fn present(parent: &impl IsA<gtk::Widget>, state: &AppState) {
    let dialog = PreferencesDialog::new();
    let widgets = dialog.widgets();

    connect_engines(&dialog, &widgets, state);
    connect_analysis(&dialog, &widgets, state);
    connect_play(&dialog, &widgets, state);
    connect_appearance(&dialog, &widgets, state);

    dialog.present(Some(parent));
}

/// The empty-state page shown on first run when no engine profile exists.
pub fn no_engine_status_page() -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name("application-x-executable-symbolic")
        .title("No Engine Configured")
        .description("Add a local KataGo or a remote mirai-server in Preferences.")
        .build();
    let button = gtk::Button::builder()
        .label("Preferences")
        .action_name("win.preferences")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    page.set_child(Some(&button));
    page
}

/// The engine drop-down menu model for the header bar: one radio item per profile plus a
/// "Preferences…" item. Rebuild it whenever `engine-changed` fires.
///
/// Profile items activate `win.set-engine` with the profile name as the string parameter;
/// the trailing item activates `win.preferences`. Both actions belong to the window.
pub fn engine_menu_model(state: &AppState) -> gio::Menu {
    let menu = gio::Menu::new();

    let profiles = gio::Menu::new();
    {
        let cfg = state.config();
        for profile in &cfg.engine_profiles {
            let item = gio::MenuItem::new(Some(&profile.name), None);
            item.set_action_and_target_value(
                Some("win.set-engine"),
                Some(&profile.name.to_variant()),
            );
            profiles.append_item(&item);
        }
        if cfg.engine_profiles.is_empty() {
            // No action, so GTK renders it insensitive — a hint, not a choice.
            profiles.append_item(&gio::MenuItem::new(Some("No engine profiles"), None));
        }
    }
    menu.append_section(None, &profiles);

    let tail = gio::Menu::new();
    tail.append(Some("Preferences…"), Some("win.preferences"));
    menu.append_section(None, &tail);

    menu
}

// -- Engines ----------------------------------------------------------------------------

fn connect_engines(dialog: &PreferencesDialog, widgets: &PreferencesWidgets, state: &AppState) {
    let local_state = state.clone();
    widgets.add_local_button.connect_activated(glib::clone!(
        #[weak]
        dialog,
        #[weak(rename_to = group)]
        widgets.profiles_group,
        move |_| {
            open_editor(dialog.upcast_ref(), &group, &local_state, None, false);
        }
    ));

    let remote_state = state.clone();
    widgets.add_remote_button.connect_activated(glib::clone!(
        #[weak]
        dialog,
        #[weak(rename_to = group)]
        widgets.profiles_group,
        move |_| {
            open_editor(dialog.upcast_ref(), &group, &remote_state, None, true);
        }
    ));

    refresh_profiles(&widgets.profiles_group, dialog.upcast_ref(), state);
}

/// Rebuilds the profile list in place. Called after every mutation.
fn refresh_profiles(
    group: &adw::PreferencesGroup,
    dialog: &adw::PreferencesDialog,
    state: &AppState,
) {
    let mut old = Vec::new();
    let mut index = 0;
    while let Some(row) = group.row(index) {
        old.push(row);
        index += 1;
    }
    for row in old {
        group.remove(&row);
    }

    let (profiles, active) = {
        let cfg = state.config();
        (cfg.engine_profiles.clone(), cfg.active_engine.clone())
    };

    if profiles.is_empty() {
        let empty = adw::ActionRow::builder()
            .title("No Engine Profiles Yet")
            .subtitle("Add a local KataGo installation or a remote mirai-server.")
            .build();
        empty.set_activatable(false);
        group.add(&empty);
        return;
    }

    // One radio group across every row: the active profile is the selected one.
    let mut leader: Option<gtk::CheckButton> = None;

    for profile in profiles {
        let name = profile.name.clone();

        let row = adw::ActionRow::builder()
            .title(&name)
            .subtitle(profile.subtitle())
            .build();
        row.set_use_markup(false);
        row.set_subtitle_lines(2);

        let radio = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .tooltip_text("Use this engine")
            .build();
        match &leader {
            Some(first) => radio.set_group(Some(first)),
            None => leader = Some(radio.clone()),
        }
        radio.set_active(active.as_deref() == Some(name.as_str()));

        let toggle_state = state.clone();
        let toggle_name = name.clone();
        radio.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            let already =
                toggle_state.config().active_engine.as_deref() == Some(toggle_name.as_str());
            if !already {
                toggle_state.activate_profile(&toggle_name);
            }
        });
        row.add_prefix(&radio);
        row.set_activatable_widget(Some(&radio));

        let edit = gtk::Button::from_icon_name("document-edit-symbolic");
        edit.set_valign(gtk::Align::Center);
        edit.set_tooltip_text(Some("Edit this profile"));
        edit.add_css_class("flat");
        let edit_state = state.clone();
        let edit_profile = profile.clone();
        edit.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            #[weak]
            group,
            move |_| {
                let remote = !edit_profile.is_local();
                open_editor(
                    &dialog,
                    &group,
                    &edit_state,
                    Some(edit_profile.clone()),
                    remote,
                );
            }
        ));
        row.add_suffix(&edit);

        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.set_valign(gtk::Align::Center);
        delete.set_tooltip_text(Some("Delete this profile"));
        delete.add_css_class("flat");
        let delete_state = state.clone();
        let delete_name = name.clone();
        delete.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            #[weak]
            group,
            move |_| {
                confirm_delete(&dialog, &group, &delete_state, delete_name.clone());
            }
        ));
        row.add_suffix(&delete);

        group.add(&row);
    }
}

fn confirm_delete(
    dialog: &adw::PreferencesDialog,
    group: &adw::PreferencesGroup,
    state: &AppState,
    name: String,
) {
    let alert = adw::AlertDialog::new(
        Some("Delete This Profile?"),
        Some(&format!(
            "“{name}” will be removed from the configuration. Nothing on disk is deleted."
        )),
    );
    alert.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");

    let state = state.clone();
    alert.connect_response(
        None,
        glib::clone!(
            #[weak]
            dialog,
            #[weak]
            group,
            move |_, response| {
                if response != "delete" {
                    return;
                }
                let was_active = {
                    let mut cfg = state.config_mut();
                    cfg.engine_profiles.retain(|p| p.name != name);
                    let was_active = cfg.active_engine.as_deref() == Some(name.as_str());
                    if was_active {
                        cfg.active_engine = None;
                    }
                    was_active
                };
                state.save_config();
                if was_active {
                    // The engine it points at is gone; stop using it.
                    state.set_engine(None);
                } else {
                    state.changed(Change::Engine);
                }
                state.toast(format!("Deleted “{name}”"));
                refresh_profiles(&group, &dialog, &state);
            }
        ),
    );
    alert.present(Some(dialog));
}

// -- Profile editors --------------------------------------------------------------------

/// The chrome shared by the local and remote editors: a navigation subpage with a header
/// bar, a Save button and an inline error banner.
struct Editor {
    page: adw::NavigationPage,
    content: adw::PreferencesPage,
    save: gtk::Button,
    banner: adw::Banner,
}

fn editor_shell(title: &str) -> Editor {
    let page = ProfileEditorPage::new(title);
    Editor {
        content: page.content(),
        save: page.save_button(),
        banner: page.banner(),
        page: page.upcast(),
    }
}

fn complain(banner: &adw::Banner, message: impl AsRef<str>) {
    banner.set_title(message.as_ref());
    banner.set_revealed(true);
}

fn open_editor(
    dialog: &adw::PreferencesDialog,
    group: &adw::PreferencesGroup,
    state: &AppState,
    existing: Option<EngineProfile>,
    remote: bool,
) {
    let page = if remote {
        remote_editor(dialog, group, state, existing)
    } else {
        local_editor(dialog, group, state, existing)
    };
    dialog.push_subpage(&page);
}

/// A read-only row showing a chosen path, with a button that opens a `gtk::FileDialog` and,
/// when discovery found any, a chooser over those candidates.
fn file_row(
    title: &str,
    initial: PathBuf,
    candidates: Vec<PathBuf>,
    dialog: &adw::PreferencesDialog,
) -> (adw::ActionRow, Rc<RefCell<PathBuf>>) {
    let cell = Rc::new(RefCell::new(initial));

    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(path_subtitle(&cell.borrow()))
        .build();
    row.set_use_markup(false);
    row.set_subtitle_lines(3);

    // Before the file button, so the discovered list is the first thing reached.
    if let Some(chooser) = discovered_button(candidates, &row, &cell) {
        row.add_suffix(&chooser);
    }

    let button = gtk::Button::from_icon_name("document-open-symbolic");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some("Choose a file"));
    button.add_css_class("flat");
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));

    let prompt = format!("Select the {}", title.to_lowercase());
    let cell_for_click = cell.clone();
    button.connect_clicked(glib::clone!(
        #[weak]
        row,
        #[weak]
        dialog,
        move |_| {
            let chooser = gtk::FileDialog::builder()
                .title(prompt.clone())
                .modal(true)
                .build();
            let start = cell_for_click.borrow().clone();
            if let Some(parent) = start.parent()
                && parent.is_dir()
            {
                chooser.set_initial_folder(Some(&gio::File::for_path(parent)));
            }
            let window = dialog.root().and_downcast::<gtk::Window>();
            let cell = cell_for_click.clone();
            glib::spawn_future_local(async move {
                match chooser.open_future(window.as_ref()).await {
                    Ok(file) => {
                        if let Some(path) = file.path() {
                            row.set_subtitle(&path_subtitle(&path));
                            *cell.borrow_mut() = path;
                        }
                    }
                    // Dismissing the chooser is not an error worth reporting.
                    Err(e) => tracing::debug!(%e, "file chooser dismissed"),
                }
            });
        }
    ));

    (row, cell)
}

fn path_subtitle(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        "Not chosen".to_string()
    } else {
        path.display().to_string()
    }
}

/// A chooser over every path discovered in the configured XDG directories, living in the row
/// it fills rather than beside it.
///
/// Labels are file names, which is what actually distinguishes candidates; a name two
/// directories share carries its directory underneath. Both ellipsize in the middle, keeping
/// the ends that identify a file, and the whole path is in the tooltip either way. The row's
/// own file button still accepts anything discovery never saw.
fn discovered_button(
    candidates: Vec<PathBuf>,
    row: &adw::ActionRow,
    path: &Rc<RefCell<PathBuf>>,
) -> Option<gtk::MenuButton> {
    if candidates.is_empty() {
        return None;
    }
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let popover = gtk::Popover::builder().build();
    for (candidate, (name, dir)) in candidates.iter().zip(candidate_labels(&candidates)) {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&candidate_label(&name, false));
        if let Some(dir) = dir {
            content.append(&candidate_label(&dir, true));
        }
        let button = gtk::Button::builder()
            .child(&content)
            .tooltip_text(candidate.display().to_string())
            .build();
        button.add_css_class("flat");
        let candidate = candidate.clone();
        let path = path.clone();
        button.connect_clicked(glib::clone!(
            #[weak]
            row,
            #[weak]
            popover,
            move |_| {
                row.set_subtitle(&path_subtitle(&candidate));
                path.replace(candidate.clone());
                popover.popdown();
            }
        ));
        list.append(&button);
    }
    // A discovery directory can hold a dozen networks; the popover scrolls rather than
    // growing past the dialog.
    popover.set_child(Some(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .max_content_height(320)
            .build(),
    ));
    let button = gtk::MenuButton::builder()
        .icon_name("view-list-symbolic")
        .popover(&popover)
        .valign(gtk::Align::Center)
        .tooltip_text(format!(
            "Choose one of {} discovered files",
            candidates.len()
        ))
        .build();
    button.add_css_class("flat");
    Some(button)
}

fn candidate_label(text: &str, dim: bool) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(44)
        .build();
    if dim {
        label.add_css_class("dim-label");
        label.add_css_class("caption");
    }
    label
}

/// What each candidate is called in the chooser: its file name, plus the directory holding
/// it only when another candidate shares that name. Discovery deliberately spans several
/// directories that conventionally hold identically named files - `analysis.cfg` in two XDG
/// roots, the same network staged twice - so a bare name is not always enough.
fn candidate_labels(candidates: &[PathBuf]) -> Vec<(String, Option<String>)> {
    candidates
        .iter()
        .map(|path| {
            let name = match path.file_name() {
                Some(name) => name.to_string_lossy().into_owned(),
                None => path.display().to_string(),
            };
            let shared = candidates
                .iter()
                .filter(|other| other.file_name() == path.file_name())
                .count()
                > 1;
            let dir = shared
                .then(|| path.parent().map(|dir| dir.display().to_string()))
                .flatten();
            (name, dir)
        })
        .collect()
}

/// `0` means "use the default": mirai's own when it generates the analysis config, or
/// whatever the file says when the user supplies one.
fn tuned_row(title: &str, subtitle: &str, value: u32, max: f64) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(0.0, max, 1.0);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(value as f64);
    hide_steppers(&row);
    row
}

fn spin_value_u16(row: &adw::SpinRow) -> Option<u16> {
    let v = row.value() as u16;
    (v > 0).then_some(v)
}

fn spin_value_u8(row: &adw::SpinRow) -> Option<u8> {
    let v = row.value() as u8;
    (v > 0).then_some(v)
}

/// KataGo's own estimate is about 3 KiB per cached evaluation once ownership is included,
/// and mirai asks for ownership on nearly every query.
fn cache_subtitle(power: u8) -> String {
    if power == 0 {
        return format!(
            "0 uses mirai's default ({})",
            EngineTuning::default().nn_cache_size_power_of_two
        );
    }
    format!(
        "2^{power} evaluations, roughly {} MiB once warm",
        (3u64 << power) / 1024
    )
}

/// Both halves of a thread setting, in the words the two rows above the tuning group use.
fn candidate_summary(analysis: u16, search: u16) -> String {
    format!(
        "{analysis} position{} in parallel, {search} threads each",
        if analysis == 1 { "" } else { "s" }
    )
}

/// What the run settled on, with the throughput it actually measured there — the numbers
/// in the rows above are worth trusting only because this one was timed.
fn measured_subtitle(result: &CalibrationResult) -> String {
    let tuning = result.tuning;
    let speed = result
        .samples
        .iter()
        .filter(|s| s.at == tuning.analysis_threads && s.st == tuning.search_threads)
        .map(|s| s.visits_per_second())
        .fold(0.0_f64, f64::max);
    let best = candidate_summary(tuning.analysis_threads, tuning.search_threads);
    if speed > 0.0 {
        format!("Fastest here: {best} — about {speed:.0} visits/s. Press Save Profile to apply.")
    } else {
        format!("Fastest here: {best}. Press Save Profile to apply.")
    }
}

/// The modal a calibration runs behind: one bar for the eight candidates, one line naming
/// the setting being timed, and a Cancel that stops the run.
fn tuning_progress() -> (adw::AlertDialog, gtk::ProgressBar, gtk::Label) {
    let bar = gtk::ProgressBar::builder()
        .show_text(true)
        .text("Starting…")
        .build();
    let caption = gtk::Label::builder()
        .label("Waiting for the current engine to let go of the GPU…")
        .wrap(true)
        .build();
    caption.add_css_class("dim-label");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(&bar);
    body.append(&caption);

    let dialog = adw::AlertDialog::new(
        Some("Tuning KataGo"),
        Some(
            "Every setting is timed in its own KataGo, so this takes a few minutes. \
             Your engine stays stopped until the run ends.",
        ),
    );
    dialog.set_extra_child(Some(&body));
    dialog.add_response("cancel", "Stop Tuning");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    (dialog, bar, caption)
}

/// True when a second mirai window is open.
///
/// Engines are shared application-wide (see [`crate::engines::EnginePool`]), so another
/// window keeps the running KataGo alive whatever this one drops, and the run would be
/// timing a GPU it does not have to itself.
fn other_windows_open(widget: &impl IsA<gtk::Widget>) -> bool {
    let Some(app) = widget
        .root()
        .and_downcast::<gtk::Window>()
        .and_then(|window| window.application())
    else {
        return false;
    };
    app.windows()
        .iter()
        .filter(|w| w.is::<adw::ApplicationWindow>())
        .count()
        > 1
}

/// Puts the engine back after a tuning run, however the run ended.
///
/// The profile restored is the saved one: unsaved edits in the editor are not a decision
/// the user has made yet, and tuning must not turn them into one.
fn restore_engine(state: &AppState, previous: Option<&str>) {
    if let Some(name) = previous {
        state.activate_profile(name);
    }
}

fn local_editor(
    dialog: &adw::PreferencesDialog,
    group: &adw::PreferencesGroup,
    state: &AppState,
    existing: Option<EngineProfile>,
) -> adw::NavigationPage {
    let editing = existing.as_ref().map(|p| p.name.clone());
    let defaults = EngineTuning::default();
    let (katago, model, config, analysis_threads, search_threads, batch, cache) =
        match existing.map(|p| p.kind) {
            Some(ProfileKind::Local {
                katago,
                model,
                config,
                analysis_threads,
                search_threads,
                nn_max_batch_size,
                nn_cache_size_power_of_two,
            }) => (
                katago,
                model,
                config,
                analysis_threads,
                search_threads,
                nn_max_batch_size,
                nn_cache_size_power_of_two,
            ),
            _ => (PathBuf::new(), PathBuf::new(), None, None, None, None, None),
        };
    let custom = config.is_some();
    let model_candidates = crate::config::discover_models();
    let config_candidates = crate::config::discover_analysis_configs();
    // Discovery only supplies chooser entries. Managed mode remains selected until the user
    // explicitly switches to Custom file.
    let suggested_config = config
        .clone()
        .or_else(|| config_candidates.first().cloned())
        .unwrap_or_default();

    let editor = editor_shell(if editing.is_some() {
        "Edit Local Engine"
    } else {
        "Add Local Engine"
    });

    let identity = adw::PreferencesGroup::new();
    let name_row = adw::EntryRow::builder().title("Name").build();
    name_row.set_text(editing.as_deref().unwrap_or(""));
    identity.add(&name_row);
    editor.content.add(&identity);

    let paths = adw::PreferencesGroup::builder()
        .title("KataGo")
        .description("Both must exist before the profile can be saved.")
        .build();
    let (katago_row, katago_path) = file_row("KataGo binary", katago, Vec::new(), dialog);
    let (model_row, model_path) = file_row("Neural network model", model, model_candidates, dialog);
    paths.add(&katago_row);
    paths.add(&model_row);
    editor.content.add(&paths);

    // KataGo will not start without a `-config`, but mirai can write that file itself —
    // it needs three keys the user has no reason to care about. A custom file stays
    // available for anyone who does.
    let source = adw::PreferencesGroup::builder()
        .title("Configuration")
        .build();
    let mode = adw::ComboRow::builder()
        .title("Analysis config")
        .model(&gtk::StringList::new(&["Managed by mirai", "Custom file"]))
        .selected(u32::from(custom))
        .build();
    let (config_row, config_path) = file_row(
        "Custom analysis config",
        suggested_config,
        config_candidates,
        dialog,
    );
    source.add(&mode);
    source.add(&config_row);
    editor.content.add(&source);

    let threads = adw::PreferencesGroup::builder().title("Search").build();
    let analysis_row = tuned_row(
        "Positions in parallel",
        "",
        u32::from(analysis_threads.unwrap_or(0)),
        f64::from(EngineTuning::MAX_ANALYSIS_THREADS),
    );
    let search_row = tuned_row(
        "Threads per position",
        "",
        u32::from(search_threads.unwrap_or(0)),
        f64::from(EngineTuning::MAX_SEARCH_THREADS),
    );
    threads.add(&analysis_row);
    threads.add(&search_row);
    editor.content.add(&threads);

    // A custom file has to carry these two itself — KataGo will not start without
    // `nnMaxBatchSize` — so there is nothing for mirai to override.
    let memory = adw::PreferencesGroup::builder()
        .title("Batching and Memory")
        .build();
    let batch_row = tuned_row(
        "GPU batch size",
        &format!(
            "nnMaxBatchSize — 0 uses mirai's default ({}). Wants to be at least positions × threads.",
            defaults.nn_max_batch_size
        ),
        u32::from(batch.unwrap_or(0)),
        f64::from(EngineTuning::MAX_BATCH_SIZE),
    );
    let cache_row = tuned_row(
        "Neural-net cache",
        &cache_subtitle(cache.unwrap_or(0)),
        u32::from(cache.unwrap_or(0)),
        f64::from(EngineTuning::MAX_CACHE_POWER),
    );
    cache_row.connect_value_notify(|row| {
        row.set_subtitle(&cache_subtitle(row.value() as u8));
    });
    memory.add(&batch_row);
    memory.add(&cache_row);
    editor.content.add(&memory);

    // -- automatic tuning ---------------------------------------------------------------
    //
    // Threads and batch size are worth measuring rather than guessing: what a particular
    // GPU does with them is not something mirai can predict from the model file or the
    // driver version. The run needs the machine to itself, hence the engine shutdown and
    // the refusal to start with a second window open.
    let auto = adw::PreferencesGroup::builder()
        .title("Automatic Tuning")
        .description(
            "Times KataGo on this machine at a series of thread settings and fills in the \
             values above. It starts KataGo once per setting, so allow a few minutes.",
        )
        .build();
    let tune_row = adw::ActionRow::builder()
        .title("Measure This Machine")
        .subtitle(
            "Uses the binary and model chosen above, and stops the running engine while \
             it works.",
        )
        .build();
    tune_row.set_use_markup(false);
    tune_row.set_subtitle_lines(3);
    let tune = gtk::Button::with_label("Tune…");
    tune.set_valign(gtk::Align::Center);
    tune_row.add_suffix(&tune);
    tune_row.set_activatable_widget(Some(&tune));
    auto.add(&tune_row);
    editor.content.add(&auto);

    let tune_state = state.clone();
    let tune_banner = editor.banner.clone();
    let tune_save = editor.save.clone();
    let tune_katago = katago_path.clone();
    let tune_model = model_path.clone();
    let tune_name = name_row.clone();
    let tune_analysis = analysis_row.clone();
    let tune_search = search_row.clone();
    let tune_batch = batch_row.clone();
    let tune_cache = cache_row.clone();
    tune.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        tune_row,
        move |button| {
            let katago = tune_katago.borrow().clone();
            let model = tune_model.borrow().clone();
            for (label, path) in [("KataGo binary", &katago), ("neural network model", &model)] {
                if path.as_os_str().is_empty() {
                    complain(&tune_banner, format!("Choose the {label} before tuning."));
                    return;
                }
                if !path.exists() {
                    complain(&tune_banner, format!("{} does not exist.", path.display()));
                    return;
                }
            }
            if other_windows_open(&dialog) {
                complain(
                    &tune_banner,
                    "Close the other mirai windows before tuning: they keep the current \
                     KataGo on the GPU, which would skew every measurement.",
                );
                return;
            }
            if tune_state.busy() {
                complain(
                    &tune_banner,
                    "Wait for engine startup, or finish or cancel the running whole-game \
                     analysis before tuning.",
                );
                return;
            }
            tune_banner.set_revealed(false);

            // Measure from where the user is now: whatever the rows say, with `0` meaning
            // mirai's default, exactly as a real start would read them.
            let base = EngineTuning::default();
            let tuning = EngineTuning {
                analysis_threads: spin_value_u16(&tune_analysis).unwrap_or(base.analysis_threads),
                search_threads: spin_value_u16(&tune_search).unwrap_or(base.search_threads),
                nn_max_batch_size: spin_value_u16(&tune_batch).unwrap_or(base.nn_max_batch_size),
                nn_cache_size_power_of_two: spin_value_u8(&tune_cache)
                    .unwrap_or(base.nn_cache_size_power_of_two),
            };
            let name = match tune_name.text().trim() {
                "" => "tuning".to_string(),
                named => named.to_string(),
            };
            let log_dir = crate::config::Config::data_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join("katago-logs");
            let config = CalibrationConfig::new(name, katago, model, log_dir, tuning);

            // Hand the GPU over: dropping this window's engine leaves nobody holding the
            // pool's weak entry, so KataGo exits. The profile that was active comes back
            // when the run ends, whichever way it ends.
            let previous = tune_state.config().active_engine.clone();
            if tune_state.engine().is_some() {
                tune_state.set_engine(None);
            }

            button.set_sensitive(false);
            tune_save.set_sensitive(false);

            let (progress_tx, mut progress_rx) =
                tokio::sync::mpsc::unbounded_channel::<CalibrationProgress>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let task = tune_state.runtime().spawn(async move {
                // Wait beyond LocalEngine's shutdown deadline before putting the next KataGo
                // on the GPU.
                tokio::time::sleep(
                    mirai_engine::LOCAL_ENGINE_SHUTDOWN_GRACE
                        + std::time::Duration::from_millis(250),
                )
                .await;
                let outcome = mirai_engine::calibrate(config, move |step| {
                    let _ = progress_tx.send(step);
                })
                .await;
                let _ = done_tx.send(outcome);
            });

            let (progress, bar, caption) = tuning_progress();
            progress.present(Some(&dialog));

            // Opening a game during the run creates another AppState and may start another
            // KataGo. Abort instead of presenting measurements taken against a contended GPU.
            let window_watch = if let Some(app) = dialog
                .root()
                .and_downcast::<gtk::Window>()
                .and_then(|window| window.application())
            {
                let watch_banner = tune_banner.clone();
                let watch_progress = progress.clone();
                let id = app.connect_window_added(move |_, window| {
                    if window.is::<adw::ApplicationWindow>() {
                        complain(
                            &watch_banner,
                            "Tuning stopped because another mirai window opened.",
                        );
                        watch_progress.close();
                    }
                });
                Some((app, id))
            } else {
                None
            };

            // Exactly one owner takes and tears down the run. Completion and cancellation
            // therefore cannot both restore the engine or leave a signal handler connected.
            let run = Rc::new(RefCell::new(Some(CalibrationRun {
                state: tune_state.clone(),
                previous,
                save: tune_save.clone(),
                tune: button.clone(),
                task,
                window_watch,
            })));
            {
                let run = run.clone();
                progress.connect_response(None, move |_, _| {
                    let Some(run) = run.borrow_mut().take() else {
                        return;
                    };
                    run.cancel();
                });
            }

            // Progress arrives from a runtime thread; the channel is what carries it back
            // onto this one, and it closes when the task ends or is aborted.
            glib::spawn_future_local(async move {
                while let Some(step) = progress_rx.recv().await {
                    bar.set_fraction(f64::from(step.completed) / f64::from(step.total.max(1)));
                    bar.set_text(Some(&format!(
                        "Step {} of {}",
                        step.completed + 1,
                        step.total
                    )));
                    caption.set_label(&candidate_summary(
                        step.analysis_threads,
                        step.search_threads,
                    ));
                }
            });

            let banner = tune_banner.clone();
            let analysis_row = tune_analysis.clone();
            let search_row = tune_search.clone();
            let batch_row = tune_batch.clone();
            glib::spawn_future_local(async move {
                let outcome = done_rx.await;
                let Some(mut run) = run.borrow_mut().take() else {
                    // Cancelled: the response handler owns teardown.
                    return;
                };
                // Taking ownership first makes the response emitted by close a no-op.
                progress.close();
                match outcome {
                    Ok(Ok(result)) => {
                        // Only the three values the run measured. The cache is a memory
                        // decision, not a speed one, so it stays the user's.
                        analysis_row.set_value(f64::from(result.tuning.analysis_threads));
                        search_row.set_value(f64::from(result.tuning.search_threads));
                        batch_row.set_value(f64::from(result.tuning.nn_max_batch_size));
                        tune_row.set_subtitle(&measured_subtitle(&result));
                    }
                    Ok(Err(e)) => complain(&banner, format!("Tuning failed: {e}")),
                    // The result channel only goes away with the task behind it.
                    Err(_) => complain(&banner, "The tuning run stopped without a result."),
                }
                run.finish();
            });
        }
    ));

    // What `0` falls back to depends on who owns the config file, so say which.
    let apply_mode = {
        let config_row = config_row.clone();
        let memory = memory.clone();
        let auto = auto.clone();
        let analysis_row = analysis_row.clone();
        let search_row = search_row.clone();
        move |custom: bool| {
            config_row.set_visible(custom);
            memory.set_visible(!custom);
            // Tuning writes the managed values; with a custom file there is nothing for
            // it to write.
            auto.set_visible(!custom);
            let (analysis, search) = if custom {
                (
                    "numAnalysisThreads — 0 keeps the value from your analysis config".to_string(),
                    "numSearchThreadsPerAnalysisThread — 0 keeps the value from your analysis config"
                        .to_string(),
                )
            } else {
                (
                    format!(
                        "numAnalysisThreads — 0 uses mirai's default ({})",
                        defaults.analysis_threads
                    ),
                    format!(
                        "numSearchThreadsPerAnalysisThread — 0 uses mirai's default ({})",
                        defaults.search_threads
                    ),
                )
            };
            analysis_row.set_subtitle(&analysis);
            search_row.set_subtitle(&search);
        }
    };
    apply_mode(custom);
    mode.connect_selected_notify(move |row| apply_mode(row.selected() == 1));

    let save_state = state.clone();
    let banner = editor.banner.clone();
    editor.save.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        group,
        move |_| {
            let name = name_row.text().trim().to_string();
            let katago = katago_path.borrow().clone();
            let model = model_path.borrow().clone();
            let custom = mode.selected() == 1;
            let config = custom.then(|| config_path.borrow().clone());

            if let Err(message) = check_name(&save_state, &name, editing.as_deref()) {
                complain(&banner, message);
                return;
            }
            let required = [
                ("KataGo binary", Some(&katago)),
                ("neural network model", Some(&model)),
                ("analysis config", config.as_ref()),
            ];
            for (label, path) in required.into_iter().filter_map(|(l, p)| p.map(|p| (l, p))) {
                if path.as_os_str().is_empty() {
                    complain(&banner, format!("Choose the {label}."));
                    return;
                }
                if !path.exists() {
                    complain(&banner, format!("{} does not exist.", path.display()));
                    return;
                }
            }

            let profile = EngineProfile {
                name: name.clone(),
                kind: ProfileKind::Local {
                    katago,
                    model,
                    config,
                    analysis_threads: spin_value_u16(&analysis_row),
                    search_threads: spin_value_u16(&search_row),
                    nn_max_batch_size: (!custom).then(|| spin_value_u16(&batch_row)).flatten(),
                    nn_cache_size_power_of_two: (!custom)
                        .then(|| spin_value_u8(&cache_row))
                        .flatten(),
                },
            };
            commit_profile(&save_state, profile, editing.as_deref());
            dialog.pop_subpage();
            refresh_profiles(&group, &dialog, &save_state);
        }
    ));

    editor.page
}

fn remote_editor(
    dialog: &adw::PreferencesDialog,
    group: &adw::PreferencesGroup,
    state: &AppState,
    existing: Option<EngineProfile>,
) -> adw::NavigationPage {
    let editing = existing.as_ref().map(|p| p.name.clone());
    let (url, token, engine, cert) = match existing.map(|p| p.kind) {
        Some(ProfileKind::Remote {
            url,
            token,
            engine,
            cert_sha256,
        }) => (url, token, engine.unwrap_or_default(), cert_sha256),
        _ => (String::new(), String::new(), String::new(), None),
    };

    // The pin only belongs to the URL it was obtained from; changing the URL invalidates it.
    let pin: Rc<RefCell<Option<(String, String)>>> =
        Rc::new(RefCell::new(cert.map(|c| (url.clone(), c))));

    let editor = editor_shell(if editing.is_some() {
        "Edit Remote Engine"
    } else {
        "Add Remote Engine"
    });

    let identity = adw::PreferencesGroup::new();
    let name_row = adw::EntryRow::builder().title("Name").build();
    name_row.set_text(editing.as_deref().unwrap_or(""));
    identity.add(&name_row);
    editor.content.add(&identity);

    let server = adw::PreferencesGroup::builder()
        .title("Server")
        .description("For example mirai://192.168.1.10:9678")
        .build();
    let url_row = adw::EntryRow::builder().title("Server URL").build();
    url_row.set_text(&url);
    url_row.set_tooltip_text(Some("mirai://host:9678"));
    let token_row = adw::PasswordEntryRow::builder().title("Token").build();
    token_row.set_text(&token);
    let engine_row = adw::EntryRow::builder()
        .title("Engine name (optional)")
        .build();
    engine_row.set_text(&engine);
    server.add(&url_row);
    server.add(&token_row);
    server.add(&engine_row);
    editor.content.add(&server);

    let trust = adw::PreferencesGroup::builder()
        .title("Certificate")
        .description(
            "mirai pins the server certificate the first time it connects. \
             Test the connection now to see the fingerprint before saving.",
        )
        .build();
    let trust_row = adw::ActionRow::builder()
        .title("Pinned fingerprint")
        .subtitle(fingerprint_subtitle(&pin.borrow()))
        .build();
    trust_row.set_use_markup(false);
    trust_row.set_subtitle_lines(3);
    let test = gtk::Button::with_label("Test Connection");
    test.set_valign(gtk::Align::Center);
    trust_row.add_suffix(&test);
    trust.add(&trust_row);
    editor.content.add(&trust);

    // -- test connection ------------------------------------------------------------
    let test_state = state.clone();
    let test_pin = pin.clone();
    let test_banner = editor.banner.clone();
    let test_url = url_row.clone();
    let test_token = token_row.clone();
    let test_engine = engine_row.clone();
    test.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        trust_row,
        move |button| {
            let url = test_url.text().trim().to_string();
            if url.is_empty() {
                complain(&test_banner, "Enter the server URL first.");
                return;
            }
            let token = test_token.text().to_string();
            let engine = {
                let e = test_engine.text().trim().to_string();
                (!e.is_empty()).then_some(e)
            };

            test_banner.set_revealed(false);
            button.set_sensitive(false);
            button.set_label("Connecting…");

            let (tx, rx) = tokio::sync::oneshot::channel();
            let connect_url = url.clone();
            test_state.runtime().spawn(async move {
                let outcome =
                    mirai_engine::RemoteEngine::connect(&connect_url, &token, engine, None)
                        .await
                        .map(|remote| remote.fingerprint().to_string());
                let _ = tx.send(outcome);
            });

            let button = button.clone();
            let banner = test_banner.clone();
            let pin = test_pin.clone();
            glib::spawn_future_local(async move {
                let outcome = rx.await;
                button.set_sensitive(true);
                button.set_label("Test Connection");
                match outcome {
                    Ok(Ok(fingerprint)) => {
                        let pinned = (url.clone(), fingerprint.clone());
                        crate::dialogs::confirm_fingerprint(
                            &dialog,
                            &url,
                            &fingerprint,
                            move || {
                                *pin.borrow_mut() = Some(pinned.clone());
                                trust_row.set_subtitle(&fingerprint_subtitle(&pin.borrow()));
                            },
                        );
                    }
                    Ok(Err(e)) => complain(&banner, format!("Could not connect: {e}")),
                    Err(_) => complain(&banner, "The connection attempt was cancelled."),
                }
            });
        }
    ));

    // -- save -------------------------------------------------------------------------
    let save_state = state.clone();
    let banner = editor.banner.clone();
    editor.save.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        group,
        move |_| {
            let name = name_row.text().trim().to_string();
            let url = url_row.text().trim().to_string();

            if let Err(message) = check_name(&save_state, &name, editing.as_deref()) {
                complain(&banner, message);
                return;
            }
            if url.is_empty() {
                complain(&banner, "Enter the server URL.");
                return;
            }
            if !url.starts_with("mirai://") {
                complain(&banner, "The URL must start with mirai://.");
                return;
            }

            let engine = {
                let e = engine_row.text().trim().to_string();
                (!e.is_empty()).then_some(e)
            };
            // Keep the pin only if it was obtained from this very URL; otherwise the first
            // connection performs TOFU again and `activate_profile` records the new one.
            let cert_sha256 = pin
                .borrow()
                .as_ref()
                .filter(|(pinned, _)| *pinned == url)
                .map(|(_, fingerprint)| fingerprint.clone());

            let profile = EngineProfile {
                name: name.clone(),
                kind: ProfileKind::Remote {
                    url,
                    token: token_row.text().to_string(),
                    engine,
                    cert_sha256,
                },
            };
            commit_profile(&save_state, profile, editing.as_deref());
            dialog.pop_subpage();
            refresh_profiles(&group, &dialog, &save_state);
        }
    ));

    editor.page
}

fn fingerprint_subtitle(pin: &Option<(String, String)>) -> String {
    match pin {
        Some((_, fingerprint)) => fingerprint
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(":"),
        None => "Not pinned yet".to_string(),
    }
}

fn check_name(state: &AppState, name: &str, editing: Option<&str>) -> Result<(), String> {
    if name.is_empty() {
        return Err("The profile needs a name.".to_string());
    }
    let taken = state
        .config()
        .engine_profiles
        .iter()
        .any(|p| p.name == name && Some(p.name.as_str()) != editing);
    if taken {
        return Err(format!("There is already a profile called “{name}”."));
    }
    Ok(())
}

/// Inserts or replaces a profile, persists the config, and restarts the engine when the
/// profile that changed is the one in use.
fn commit_profile(state: &AppState, profile: EngineProfile, editing: Option<&str>) {
    let name = profile.name.clone();
    let index = editing.and_then(|old| {
        state
            .config()
            .engine_profiles
            .iter()
            .position(|p| p.name == old)
    });

    {
        let mut cfg = state.config_mut();
        match index {
            Some(i) => cfg.engine_profiles[i] = profile,
            None => cfg.engine_profiles.push(profile),
        }
        // A rename carries the active pointer with it, and the very first profile becomes
        // the active one.
        if cfg.active_engine.as_deref() == editing || cfg.active_engine.is_none() {
            cfg.active_engine = Some(name.clone());
        }
    }
    state.save_config();

    let is_active = state.config().active_engine.as_deref() == Some(name.as_str());
    if is_active {
        // Re-reads the profile, so edited paths and thread counts take effect immediately.
        state.activate_profile(&name);
    } else {
        state.changed(Change::Engine);
    }
}

// -- Analysis ---------------------------------------------------------------------------

fn connect_analysis(dialog: &PreferencesDialog, widgets: &PreferencesWidgets, state: &AppState) {
    // Loading a whole page back into its rows must not be mistaken for the user editing
    // them: the value handlers persist, and a half-loaded page would be persisted too.
    let syncing = Rc::new(Cell::new(false));

    configure_spin(
        &widgets.analysis_visits_row,
        1000.0,
        10_000_000.0,
        1000.0,
        0,
    );
    configure_spin(&widgets.analysis_interval_row, 20.0, 1000.0, 10.0, 0);
    configure_spin(&widgets.analysis_suggestions_row, 0.0, 50.0, 1.0, 0);
    configure_spin(
        &widgets.analysis_batch_visits_row,
        100.0,
        100_000.0,
        100.0,
        0,
    );

    // On this row `0` is not a count but "no limit", so the row says the word instead of
    // the digit — and takes it back when typed.
    let suggestions = &widgets.analysis_suggestions_row;
    suggestions.connect_output(|row| {
        if row.value() < 0.5 {
            row.set_text("All");
            return true;
        }
        false
    });
    suggestions.connect_input(|row| {
        row.text()
            .trim()
            .eq_ignore_ascii_case("all")
            .then_some(Ok(0.0))
    });

    load_analysis(widgets, state, &syncing);

    let visits_state = state.clone();
    let visits_syncing = syncing.clone();
    widgets
        .analysis_visits_row
        .connect_value_notify(move |row| {
            if visits_syncing.get() {
                return;
            }
            visits_state.config_mut().analysis.live_max_visits = row.value() as u32;
            visits_state.save_config();
        });

    let interval_state = state.clone();
    let interval_syncing = syncing.clone();
    widgets
        .analysis_interval_row
        .connect_value_notify(move |row| {
            if interval_syncing.get() {
                return;
            }
            interval_state.config_mut().analysis.report_interval_ms = row.value() as u16;
            interval_state.save_config();
        });

    let suggestions_state = state.clone();
    let suggestions_syncing = syncing.clone();
    widgets
        .analysis_suggestions_row
        .connect_value_notify(move |row| {
            if suggestions_syncing.get() {
                return;
            }
            suggestions_state.config_mut().analysis.max_suggestions = row.value() as u8;
            suggestions_state.save_config();
            // The board and the candidate list truncate to this, so redraw them now.
            suggestions_state.changed(Change::Report);
        });

    let batch_state = state.clone();
    let batch_syncing = syncing.clone();
    widgets
        .analysis_batch_visits_row
        .connect_value_notify(move |row| {
            if batch_syncing.get() {
                return;
            }
            batch_state.config_mut().analysis.batch_visits = row.value() as u32;
            batch_state.save_config();
        });

    let auto_state = state.clone();
    let auto_syncing = syncing.clone();
    widgets
        .analysis_auto_open_row
        .connect_active_notify(move |row| {
            if auto_syncing.get() {
                return;
            }
            auto_state.config_mut().analysis.auto_analyse_on_open = row.is_active();
            auto_state.save_config();
        });

    let reset_state = state.clone();
    let reset_syncing = syncing.clone();
    widgets.analysis_reset_button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| reset_analysis(&dialog, &reset_state, &reset_syncing)
    ));
}

/// Pushes `config.analysis` into the preference rows.
///
/// The settings are copied out first: the value handlers take `config_mut`, and a `Ref` held
/// across them would panic.
fn load_analysis(widgets: &PreferencesWidgets, state: &AppState, syncing: &Cell<bool>) {
    let settings = state.config().analysis.clone();
    syncing.set(true);
    widgets
        .analysis_visits_row
        .set_value(settings.live_max_visits as f64);
    widgets
        .analysis_interval_row
        .set_value(settings.report_interval_ms as f64);
    widgets
        .analysis_suggestions_row
        .set_value(settings.max_suggestions as f64);
    widgets
        .analysis_batch_visits_row
        .set_value(settings.batch_visits as f64);
    widgets
        .analysis_auto_open_row
        .set_active(settings.auto_analyse_on_open);
    syncing.set(false);
}

fn apply_analysis(
    dialog: &PreferencesDialog,
    state: &AppState,
    syncing: &Cell<bool>,
    settings: AnalysisSettings,
) {
    state.config_mut().analysis = settings;
    load_analysis(&dialog.widgets(), state, syncing);
    state.save_config();
    // A live search is holding the old visit cap and report interval.
    state.restart_analysis();
    state.changed(Change::Report);
}

/// Restores the analysis defaults, offering the previous values back for as long as the
/// toast is up. Engine profiles are user data and are never part of a reset.
fn reset_analysis(dialog: &PreferencesDialog, state: &AppState, syncing: &Rc<Cell<bool>>) {
    let previous = state.config().analysis.clone();
    apply_analysis(dialog, state, syncing, AnalysisSettings::default());

    let undo_state = state.clone();
    let undo_syncing = syncing.clone();
    let toast = undo_toast("Analysis settings restored");
    toast.connect_button_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| apply_analysis(&dialog, &undo_state, &undo_syncing, previous.clone())
    ));
    dialog.add_toast(toast);
}

fn undo_toast(title: &str) -> adw::Toast {
    adw::Toast::builder()
        .title(title)
        .button_label("Undo")
        .build()
}

// -- Play -------------------------------------------------------------------------------

const STRENGTH_KINDS: [&str; 3] = ["Fixed visits", "Fixed time", "Human-like"];

fn connect_play(dialog: &PreferencesDialog, widgets: &PreferencesWidgets, state: &AppState) {
    let syncing = Rc::new(Cell::new(false));

    widgets
        .play_strength_kind_row
        .set_model(Some(&gtk::StringList::new(&STRENGTH_KINDS)));
    configure_spin(&widgets.play_visits_row, 1.0, 1_000_000.0, 100.0, 0);
    configure_spin(&widgets.play_seconds_row, 0.1, 300.0, 0.5, 1);
    configure_spin(&widgets.play_temperature_row, 0.0, 2.0, 0.05, 2);
    configure_spin(&widgets.play_threshold_row, 0.0, 0.5, 0.01, 2);
    configure_spin(&widgets.play_streak_row, 1.0, 10.0, 1.0, 0);
    let labels: Vec<&str> = RuleSet::ALL.iter().map(|rules| rules.label()).collect();
    widgets
        .play_rules_row
        .set_model(Some(&gtk::StringList::new(&labels)));

    load_play(widgets, state, &syncing);

    // The mode and its three value rows all describe one setting, so they share a handler.
    let on_strength: Rc<dyn Fn()> = {
        let state = state.clone();
        let syncing = syncing.clone();
        let dialog = dialog.downgrade();
        Rc::new(move || {
            if syncing.get() {
                return;
            }
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let widgets = dialog.widgets();
            sync_strength_rows(&widgets);
            store_strength(&widgets, &state);
        })
    };
    let on_kind = on_strength.clone();
    widgets
        .play_strength_kind_row
        .connect_selected_notify(move |_| on_kind());
    let on_visits = on_strength.clone();
    widgets
        .play_visits_row
        .connect_value_notify(move |_| on_visits());
    let on_seconds = on_strength.clone();
    widgets
        .play_seconds_row
        .connect_value_notify(move |_| on_seconds());
    widgets
        .play_human_row
        .connect_changed(move |_| on_strength());

    let temperature_state = state.clone();
    let temperature_syncing = syncing.clone();
    widgets
        .play_temperature_row
        .connect_value_notify(move |row| {
            if temperature_syncing.get() {
                return;
            }
            temperature_state.config_mut().play.temperature = row.value() as f32;
            temperature_state.save_config();
        });

    let threshold_state = state.clone();
    let threshold_syncing = syncing.clone();
    widgets.play_threshold_row.connect_value_notify(move |row| {
        if threshold_syncing.get() {
            return;
        }
        threshold_state.config_mut().play.resign_threshold = row.value() as f32;
        threshold_state.save_config();
    });

    let streak_state = state.clone();
    let streak_syncing = syncing.clone();
    widgets.play_streak_row.connect_value_notify(move |row| {
        if streak_syncing.get() {
            return;
        }
        streak_state.config_mut().play.resign_streak = row.value() as u8;
        streak_state.save_config();
    });

    let rules_state = state.clone();
    let rules_syncing = syncing.clone();
    widgets.play_rules_row.connect_selected_notify(move |row| {
        if rules_syncing.get() {
            return;
        }
        let Some(&chosen) = RuleSet::ALL.get(row.selected() as usize) else {
            return;
        };
        rules_state.config_mut().play.rules = chosen;
        rules_state.save_config();
    });

    let reset_state = state.clone();
    let reset_syncing = syncing.clone();
    widgets.play_reset_button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| reset_play(&dialog, &reset_state, &reset_syncing)
    ));
}

/// Only the row the selected strength mode uses is shown.
fn sync_strength_rows(widgets: &PreferencesWidgets) {
    let selected = widgets.play_strength_kind_row.selected();
    widgets.play_visits_row.set_visible(selected == 0);
    widgets.play_seconds_row.set_visible(selected == 1);
    widgets.play_human_row.set_visible(selected == 2);
}

fn store_strength(widgets: &PreferencesWidgets, state: &AppState) {
    let strength = match widgets.play_strength_kind_row.selected() {
        0 => StrengthSetting::Visits {
            visits: widgets.play_visits_row.value() as u32,
        },
        1 => StrengthSetting::Time {
            time_ms: (widgets.play_seconds_row.value() * 1000.0) as u32,
        },
        _ => StrengthSetting::Human {
            profile: widgets.play_human_row.text().trim().to_string(),
        },
    };
    state.config_mut().play.strength = strength;
    state.save_config();
}

/// Pushes `config.play` into every row on the page, including the two strength rows the
/// current mode does not use — they keep their own defaults so switching mode lands on a
/// sensible value.
fn load_play(widgets: &PreferencesWidgets, state: &AppState, syncing: &Cell<bool>) {
    let play = state.config().play.clone();
    syncing.set(true);
    widgets.play_visits_row.set_value(800.0);
    widgets.play_seconds_row.set_value(5.0);
    widgets
        .play_human_row
        .set_text(crate::play::DEFAULT_HUMAN_PROFILE);
    let selected = match &play.strength {
        StrengthSetting::Visits { visits } => {
            widgets.play_visits_row.set_value(*visits as f64);
            0
        }
        StrengthSetting::Time { time_ms } => {
            widgets.play_seconds_row.set_value(*time_ms as f64 / 1000.0);
            1
        }
        StrengthSetting::Human { profile } => {
            widgets.play_human_row.set_text(profile);
            2
        }
    };
    widgets.play_strength_kind_row.set_selected(selected);
    widgets
        .play_temperature_row
        .set_value(play.temperature as f64);
    widgets
        .play_threshold_row
        .set_value(play.resign_threshold as f64);
    widgets.play_streak_row.set_value(play.resign_streak as f64);
    widgets.play_rules_row.set_selected(
        RuleSet::ALL
            .iter()
            .position(|rules| *rules == play.rules)
            .unwrap_or(0) as u32,
    );
    sync_strength_rows(widgets);
    syncing.set(false);
}

fn apply_play(
    dialog: &PreferencesDialog,
    state: &AppState,
    syncing: &Cell<bool>,
    settings: PlaySettings,
) {
    state.config_mut().play = settings;
    load_play(&dialog.widgets(), state, syncing);
    state.save_config();
}

fn reset_play(dialog: &PreferencesDialog, state: &AppState, syncing: &Rc<Cell<bool>>) {
    let previous = state.config().play.clone();
    apply_play(dialog, state, syncing, PlaySettings::default());

    let undo_state = state.clone();
    let undo_syncing = syncing.clone();
    let toast = undo_toast("Play settings restored");
    toast.connect_button_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| apply_play(&dialog, &undo_state, &undo_syncing, previous.clone())
    ));
    dialog.add_toast(toast);
}

// -- Appearance -------------------------------------------------------------------------

fn connect_appearance(dialog: &PreferencesDialog, widgets: &PreferencesWidgets, state: &AppState) {
    bind_switch(&widgets.show_coordinates_row, state, "show-coordinates");
    bind_switch(&widgets.show_move_numbers_row, state, "show-move-numbers");

    let overlay_syncing = Rc::new(Cell::new(false));
    widgets
        .overlay_row
        .set_model(Some(&gtk::StringList::new(&OVERLAY_CHOICES)));
    overlay_syncing.set(true);
    widgets.overlay_row.set_selected(overlay_index(
        state.ownership_overlay(),
        state.policy_overlay(),
    ));
    overlay_syncing.set(false);

    let overlay_state = state.clone();
    let overlay_syncing_row = overlay_syncing.clone();
    widgets.overlay_row.connect_selected_notify(move |row| {
        if overlay_syncing_row.get() {
            return;
        }
        apply_overlay_index(&overlay_state, row.selected());
        overlay_state.save_config();
    });

    state.connect_ownership_overlay_notify(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        overlay_syncing,
        move |state| sync_overlay_row(&dialog, state, &overlay_syncing)
    ));
    state.connect_policy_overlay_notify(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        overlay_syncing,
        move |state| sync_overlay_row(&dialog, state, &overlay_syncing)
    ));

    widgets
        .save_analysis_row
        .set_active(state.config().ui.save_analysis_in_sgf);
    let sgf_state = state.clone();
    widgets.save_analysis_row.connect_active_notify(move |row| {
        sgf_state.config_mut().ui.save_analysis_in_sgf = row.is_active();
        sgf_state.save_config();
    });

    let reset_state = state.clone();
    widgets
        .appearance_reset_button
        .connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| reset_ui(&dialog, &reset_state)
        ));
}

const OVERLAY_CHOICES: [&str; 3] = ["None", "Ownership", "Policy"];

fn overlay_index(ownership: bool, policy: bool) -> u32 {
    if ownership {
        1
    } else if policy {
        2
    } else {
        0
    }
}

/// Maps the combo onto the existing mutually exclusive setters. Turning one
/// overlay on clears the other inside `AppState`, so Ownership↔Policy is one
/// call and the combo never passes through None.
fn apply_overlay_index(state: &AppState, index: u32) {
    match index {
        1 => state.set_ownership_overlay(true),
        2 => state.set_policy_overlay(true),
        _ => {
            state.set_ownership_overlay(false);
            state.set_policy_overlay(false);
        }
    }
}

fn sync_overlay_row(dialog: &PreferencesDialog, state: &AppState, syncing: &Cell<bool>) {
    let index = overlay_index(state.ownership_overlay(), state.policy_overlay());
    let row = &dialog.widgets().overlay_row;
    if row.selected() == index {
        return;
    }
    syncing.set(true);
    row.set_selected(index);
    syncing.set(false);
}

/// Display switches bind to `AppState` properties; the overlay combo maps onto
/// the same two booleans. Only the SGF switch owns its own key.
fn apply_ui(dialog: &PreferencesDialog, state: &AppState, ui: UiSettings) {
    state.set_show_coordinates(ui.show_coordinates);
    state.set_show_move_numbers(ui.show_move_numbers);
    apply_overlay_index(
        state,
        overlay_index(ui.ownership_overlay, ui.policy_overlay),
    );
    dialog
        .widgets()
        .save_analysis_row
        .set_active(ui.save_analysis_in_sgf);
    state.save_config();
}

fn reset_ui(dialog: &PreferencesDialog, state: &AppState) {
    let previous = state.config().ui.clone();
    apply_ui(dialog, state, UiSettings::default());

    let undo_state = state.clone();
    let toast = undo_toast("Appearance settings restored");
    toast.connect_button_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| apply_ui(&dialog, &undo_state, previous.clone())
    ));
    dialog.add_toast(toast);
}

fn bind_switch(row: &adw::SwitchRow, state: &AppState, property: &'static str) {
    row.bind_property("active", state, property)
        .bidirectional()
        .sync_create()
        .build();
    // Connected after the binding so the property is already up to date when we persist.
    let state = state.clone();
    row.connect_active_notify(move |_| state.save_config());
}

/// A preference spin row: fixed range, and no `+`/`−` — see [`hide_steppers`]. The value
/// itself arrives later, from the page's `load_*`.
fn configure_spin(row: &adw::SpinRow, min: f64, max: f64, step: f64, digits: u32) {
    row.configure(
        Some(&gtk::Adjustment::new(min, min, max, step, step * 10.0, 0.0)),
        0.0,
        digits,
    );
    hide_steppers(row);
}

/// Drops the steppers from a spin row: the value is typed, scrolled or arrowed instead.
///
/// Stepping a visit budget by 1 000 up to ten million was never a real gesture, and the two
/// buttons crowd every settings row. libadwaita has no property for this, so the row's only
/// buttons — the pair inside its internal `GtkSpinButton` — are hidden directly. Hiding,
/// rather than shrinking them with CSS, is also what keeps them out of the accessibility
/// tree.
fn hide_steppers(row: &adw::SpinRow) {
    fn walk(w: &gtk::Widget) {
        if let Some(button) = w.downcast_ref::<gtk::Button>() {
            button.set_visible(false);
            return;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            walk(&c);
            child = next;
        }
    }
    walk(row.upcast_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chooser is useless if two candidates read the same, and noisy if every unique one
    /// drags its directory along.
    #[test]
    fn only_candidates_sharing_a_name_carry_their_directory() {
        let candidates = [
            PathBuf::from("/home/u/.config/katago/cfg/analysis/analysis.cfg"),
            PathBuf::from("/home/u/.config/mirai/cfg/analysis/analysis.cfg"),
            PathBuf::from("/etc/xdg/mirai/cfg/analysis/analysis-fast.cfg"),
        ];
        assert_eq!(
            candidate_labels(&candidates),
            vec![
                (
                    "analysis.cfg".to_string(),
                    Some("/home/u/.config/katago/cfg/analysis".to_string())
                ),
                (
                    "analysis.cfg".to_string(),
                    Some("/home/u/.config/mirai/cfg/analysis".to_string())
                ),
                ("analysis-fast.cfg".to_string(), None),
            ]
        );
    }
}
