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

use crate::app::{AppState, signal};
use crate::config::{EngineProfile, ProfileKind, StrengthSetting};
use mirai_engine::{CalibrationConfig, CalibrationProgress, CalibrationResult, EngineTuning};

/// Builds and presents the preferences dialog.
pub fn present(parent: &impl IsA<gtk::Widget>, state: &AppState) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        // Wide enough for the four-page view switcher to spell "Appearance" out; below this
        // libadwaita ellipsises the last title rather than falling back to its narrow layout.
        .content_width(760)
        .content_height(720)
        .build();

    dialog.add(&engines_page(&dialog, state));
    dialog.add(&analysis_page(state));
    dialog.add(&play_page(state));
    dialog.add(&appearance_page(state));

    dialog.present(Some(parent));
}

/// The empty-state page shown on first run when no engine profile exists.
pub fn no_engine_status_page() -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name("application-x-executable-symbolic")
        .title("No engine configured")
        .description("Add a local KataGo or a remote mirai-server in Preferences.")
        .build()
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

fn engines_page(dialog: &adw::PreferencesDialog, state: &AppState) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Engines")
        .icon_name("application-x-executable-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Engine profiles")
        .description("The selected profile provides analysis and plays as the computer.")
        .build();
    page.add(&group);

    let buttons = adw::PreferencesGroup::new();
    let row = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();

    // `gtk::Button` carries either a label or an icon, never both — setting `icon_name`
    // drops the label, leaving two unexplained icons. `adw::ButtonContent` shows both.
    let add_local = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label("Add local…")
                .build(),
        )
        .build();
    add_local.add_css_class("pill");
    let add_remote = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("network-server-symbolic")
                .label("Add remote…")
                .build(),
        )
        .build();
    add_remote.add_css_class("pill");
    row.append(&add_local);
    row.append(&add_remote);
    buttons.add(&row);
    page.add(&buttons);

    let local_state = state.clone();
    add_local.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        group,
        move |_| {
            open_editor(&dialog, &group, &local_state, None, false);
        }
    ));

    let remote_state = state.clone();
    add_remote.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        group,
        move |_| {
            open_editor(&dialog, &group, &remote_state, None, true);
        }
    ));

    refresh_profiles(&group, dialog, state);
    page
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
            .title("No engine profiles yet")
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
        Some("Delete this profile?"),
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
                    state.emit_by_name::<()>(signal::ENGINE_CHANGED, &[]);
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
    let header = adw::HeaderBar::new();
    let save = gtk::Button::with_label("Save profile");
    save.add_css_class("suggested-action");
    header.pack_end(&save);

    let banner = adw::Banner::new("");

    let content = adw::PreferencesPage::new();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    toolbar.set_content(Some(&content));

    let page = adw::NavigationPage::new(&toolbar, title);
    Editor {
        page,
        content,
        save,
        banner,
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
        format!("Fastest here: {best} — about {speed:.0} visits/s. Press Save profile to apply.")
    } else {
        format!("Fastest here: {best}. Press Save profile to apply.")
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
    dialog.add_response("cancel", "Stop tuning");
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
        "Edit local engine"
    } else {
        "Add local engine"
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
        .title("Batching and memory")
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
        .title("Automatic tuning")
        .description(
            "Times KataGo on this machine at a series of thread settings and fills in the \
             values above. It starts KataGo once per setting, so allow a few minutes.",
        )
        .build();
    let tune_row = adw::ActionRow::builder()
        .title("Measure this machine")
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
                let outcome = mirai_engine::calibrate(config, move |progress| {
                    let _ = progress_tx.send(progress);
                })
                .await;
                let _ = done_tx.send(outcome);
            });

            let (progress, bar, caption) = tuning_progress();
            progress.present(Some(&dialog));

            // Opening a game during the run creates another AppState and may start another
            // KataGo. Abort instead of presenting measurements taken against a contended GPU.
            let window_watch = Rc::new(RefCell::new(None));
            if let Some(app) = dialog
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
                window_watch.replace(Some((app, id)));
            }

            // Whichever comes first — the run finishing or the user cancelling — owns the
            // teardown; the other side finds the flag set and leaves it alone.
            let finished = Rc::new(Cell::new(false));

            {
                let finished = finished.clone();
                let state = tune_state.clone();
                let previous = previous.clone();
                let save = tune_save.clone();
                let button = button.clone();
                let window_watch = window_watch.clone();
                progress.connect_response(None, move |_, _| {
                    if finished.replace(true) {
                        return;
                    }
                    if let Some((app, id)) = window_watch.borrow_mut().take() {
                        app.disconnect(id);
                    }
                    // Aborting drops the calibration's engine and its subscription inside
                    // the runtime, which is the whole cancellation mechanism. Unlike the
                    // normal completion path there is no shutdown acknowledgement to await,
                    // so do not restore the saved engine inside LocalEngine's shutdown grace.
                    task.abort();
                    let state = state.clone();
                    let previous = previous.clone();
                    let save = save.clone();
                    let button = button.clone();
                    // Strong transient capture: this ends after the shutdown grace.
                    glib::spawn_future_local(async move {
                        glib::timeout_future(
                            mirai_engine::LOCAL_ENGINE_SHUTDOWN_GRACE
                                + std::time::Duration::from_millis(250),
                        )
                        .await;
                        restore_engine(&state, previous.as_deref());
                        save.set_sensitive(true);
                        button.set_sensitive(true);
                    });
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

            let state = tune_state.clone();
            let banner = tune_banner.clone();
            let save = tune_save.clone();
            let button = button.clone();
            let analysis_row = tune_analysis.clone();
            let search_row = tune_search.clone();
            let batch_row = tune_batch.clone();
            let window_watch = window_watch.clone();
            glib::spawn_future_local(async move {
                let outcome = done_rx.await;
                if finished.replace(true) {
                    // Cancelled: the response handler has already put everything back.
                    return;
                }
                if let Some((app, id)) = window_watch.borrow_mut().take() {
                    app.disconnect(id);
                }
                progress.close();
                save.set_sensitive(true);
                button.set_sensitive(true);
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
                restore_engine(&state, previous.as_deref());
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
        "Edit remote engine"
    } else {
        "Add remote engine"
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
    let test = gtk::Button::with_label("Test connection");
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
                let outcome = mirai_engine::RemoteEngine::connect(&connect_url, &token, engine, None)
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
                button.set_label("Test connection");
                match outcome {
                    Ok(Ok(fingerprint)) => {
                        let pinned = (url.clone(), fingerprint.clone());
                        crate::dialogs::confirm_fingerprint(&dialog, &url, &fingerprint, move || {
                            *pin.borrow_mut() = Some(pinned.clone());
                            trust_row.set_subtitle(&fingerprint_subtitle(&pin.borrow()));
                        });
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
        state.emit_by_name::<()>(signal::ENGINE_CHANGED, &[]);
    }
}

// -- Analysis ---------------------------------------------------------------------------

fn analysis_page(state: &AppState) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Analysis")
        .icon_name("system-search-symbolic")
        .build();

    let live = adw::PreferencesGroup::builder()
        .title("Live analysis")
        .description("Used while the board follows the cursor.")
        .build();
    page.add(&live);

    let visits = adw::SpinRow::with_range(1000.0, 10_000_000.0, 1000.0);
    visits.set_title("Maximum visits");
    visits.set_subtitle("Where a live search stops thinking");
    visits.set_value(state.config().analysis.live_max_visits as f64);
    let visits_state = state.clone();
    visits.connect_value_notify(move |row| {
        visits_state.config_mut().analysis.live_max_visits = row.value() as u32;
        visits_state.save_config();
    });
    live.add(&visits);

    let interval = adw::SpinRow::with_range(20.0, 1000.0, 10.0);
    interval.set_title("Report interval");
    interval.set_subtitle("Milliseconds between updates from the engine");
    interval.set_value(state.config().analysis.report_interval_ms as f64);
    let interval_state = state.clone();
    interval.connect_value_notify(move |row| {
        interval_state.config_mut().analysis.report_interval_ms = row.value() as u16;
        interval_state.save_config();
    });
    live.add(&interval);

    let suggestions = adw::SpinRow::with_range(1.0, 50.0, 1.0);
    suggestions.set_title("Suggestions shown");
    suggestions.set_subtitle("Candidate moves kept on the board and in the panel");
    suggestions.set_value(state.config().analysis.max_suggestions as f64);
    let suggestions_state = state.clone();
    suggestions.connect_value_notify(move |row| {
        suggestions_state.config_mut().analysis.max_suggestions = row.value() as u8;
        suggestions_state.save_config();
    });
    live.add(&suggestions);

    let batch = adw::PreferencesGroup::builder()
        .title("Whole-game analysis")
        .build();
    page.add(&batch);

    let batch_visits = adw::SpinRow::with_range(100.0, 100_000.0, 100.0);
    batch_visits.set_title("Visits per move");
    batch_visits.set_subtitle("Budget for each position when analysing a whole game");
    batch_visits.set_value(state.config().analysis.batch_visits as f64);
    let batch_state = state.clone();
    batch_visits.connect_value_notify(move |row| {
        batch_state.config_mut().analysis.batch_visits = row.value() as u32;
        batch_state.save_config();
    });
    batch.add(&batch_visits);

    page
}

// -- Play -------------------------------------------------------------------------------

const STRENGTH_KINDS: [&str; 3] = ["Fixed visits", "Fixed time", "Human-like"];

fn play_page(state: &AppState) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Play")
        .icon_name("media-playback-start-symbolic")
        .build();

    let strength_group = adw::PreferencesGroup::builder()
        .title("Computer strength")
        .description("How much thinking the computer does for each of its moves.")
        .build();
    page.add(&strength_group);

    let kind = adw::ComboRow::builder()
        .title("Mode")
        .model(&gtk::StringList::new(&STRENGTH_KINDS))
        .build();
    strength_group.add(&kind);

    let visits = adw::SpinRow::with_range(1.0, 1_000_000.0, 100.0);
    visits.set_title("Visits per move");
    strength_group.add(&visits);

    let seconds = adw::SpinRow::with_range(0.1, 300.0, 0.5);
    seconds.set_digits(1);
    seconds.set_title("Seconds per move");
    strength_group.add(&seconds);

    let human = adw::EntryRow::builder().title("Human model profile").build();
    strength_group.add(&human);

    // Seed the rows from the stored setting; the two unused ones keep sensible defaults.
    let selected: u32 = {
        let cfg = state.config();
        visits.set_value(800.0);
        seconds.set_value(5.0);
        human.set_text(crate::play::DEFAULT_HUMAN_PROFILE);
        match &cfg.play.strength {
            StrengthSetting::Visits { visits: v } => {
                visits.set_value(*v as f64);
                0
            }
            StrengthSetting::Time { time_ms } => {
                seconds.set_value(*time_ms as f64 / 1000.0);
                1
            }
            StrengthSetting::Human { profile } => {
                human.set_text(profile);
                2
            }
        }
    };
    kind.set_selected(selected);

    let sync = {
        let visits = visits.clone();
        let seconds = seconds.clone();
        let human = human.clone();
        let kind = kind.clone();
        let state = state.clone();
        move |persist: bool| {
            let selected = kind.selected();
            visits.set_visible(selected == 0);
            seconds.set_visible(selected == 1);
            human.set_visible(selected == 2);
            if !persist {
                return;
            }
            let strength = match selected {
                0 => StrengthSetting::Visits {
                    visits: visits.value() as u32,
                },
                1 => StrengthSetting::Time {
                    time_ms: (seconds.value() * 1000.0) as u32,
                },
                _ => StrengthSetting::Human {
                    profile: human.text().trim().to_string(),
                },
            };
            state.config_mut().play.strength = strength;
            state.save_config();
        }
    };
    sync(false);

    let on_kind = sync.clone();
    kind.connect_selected_notify(move |_| on_kind(true));
    let on_visits = sync.clone();
    visits.connect_value_notify(move |_| on_visits(true));
    let on_seconds = sync.clone();
    seconds.connect_value_notify(move |_| on_seconds(true));
    let on_human = sync.clone();
    human.connect_changed(move |_| on_human(true));

    let behaviour = adw::PreferencesGroup::builder().title("Behaviour").build();
    page.add(&behaviour);

    let temperature = adw::SpinRow::with_range(0.0, 2.0, 0.05);
    temperature.set_digits(2);
    temperature.set_title("Temperature");
    temperature.set_subtitle("0 always plays the best move; higher values add variety");
    temperature.set_value(state.config().play.temperature as f64);
    let temperature_state = state.clone();
    temperature.connect_value_notify(move |row| {
        temperature_state.config_mut().play.temperature = row.value() as f32;
        temperature_state.save_config();
    });
    behaviour.add(&temperature);

    let threshold = adw::SpinRow::with_range(0.0, 0.5, 0.01);
    threshold.set_digits(2);
    threshold.set_title("Resign threshold");
    threshold.set_subtitle("Winrate below which the computer considers resigning");
    threshold.set_value(state.config().play.resign_threshold as f64);
    let threshold_state = state.clone();
    threshold.connect_value_notify(move |row| {
        threshold_state.config_mut().play.resign_threshold = row.value() as f32;
        threshold_state.save_config();
    });
    behaviour.add(&threshold);

    let streak = adw::SpinRow::with_range(1.0, 10.0, 1.0);
    streak.set_title("Resign streak");
    streak.set_subtitle("Consecutive hopeless moves before resigning");
    streak.set_value(state.config().play.resign_streak as f64);
    let streak_state = state.clone();
    streak.connect_value_notify(move |row| {
        streak_state.config_mut().play.resign_streak = row.value() as u8;
        streak_state.save_config();
    });
    behaviour.add(&streak);

    let labels: Vec<&str> = RuleSet::ALL.iter().map(|r| r.label()).collect();
    let rules = adw::ComboRow::builder()
        .title("Default ruleset")
        .subtitle("Used for new games")
        .model(&gtk::StringList::new(&labels))
        .build();
    let current = state.config().play.rules;
    let index = RuleSet::ALL
        .iter()
        .position(|r| *r == current)
        .unwrap_or(0) as u32;
    rules.set_selected(index);
    let rules_state = state.clone();
    rules.connect_selected_notify(move |row| {
        let Some(&chosen) = RuleSet::ALL.get(row.selected() as usize) else {
            return;
        };
        rules_state.config_mut().play.rules = chosen;
        rules_state.save_config();
    });
    behaviour.add(&rules);

    page
}

// -- Appearance -------------------------------------------------------------------------

fn appearance_page(state: &AppState) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name("preferences-desktop-appearance-symbolic")
        .build();

    let board = adw::PreferencesGroup::builder().title("Board").build();
    page.add(&board);
    board.add(&bound_switch(
        state,
        "Coordinates",
        "Letters and numbers around the board",
        "show-coordinates",
    ));
    board.add(&bound_switch(
        state,
        "Move numbers",
        "Number every stone; the last move's number is red",
        "show-move-numbers",
    ));

    let overlays = adw::PreferencesGroup::builder()
        .title("Overlays")
        .description("Only one overlay is drawn at a time.")
        .build();
    page.add(&overlays);
    overlays.add(&bound_switch(
        state,
        "Ownership",
        "Shade each point by who is predicted to own it",
        "ownership-overlay",
    ));
    overlays.add(&bound_switch(
        state,
        "Policy",
        "Shade each point by the raw network policy",
        "policy-overlay",
    ));

    let files = adw::PreferencesGroup::builder().title("Files").build();
    page.add(&files);

    let sgf = adw::SwitchRow::builder()
        .title("Save analysis in SGF")
        .subtitle("Write winrates and candidate moves alongside the moves")
        .active(state.config().ui.save_analysis_in_sgf)
        .build();
    let sgf_state = state.clone();
    sgf.connect_active_notify(move |row| {
        sgf_state.config_mut().ui.save_analysis_in_sgf = row.is_active();
        sgf_state.save_config();
    });
    files.add(&sgf);

    page
}

/// A switch row wired both ways to an `AppState` boolean property.
fn bound_switch(
    state: &AppState,
    title: &str,
    subtitle: &str,
    property: &'static str,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    row.bind_property("active", state, property)
        .bidirectional()
        .sync_create()
        .build();
    // Connected after the binding so the property is already up to date when we persist.
    let state = state.clone();
    row.connect_active_notify(move |_| state.save_config());
    row
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
