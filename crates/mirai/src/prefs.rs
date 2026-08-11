// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The preferences dialog, the engine-profile editors and the header-bar engine menu.
//! Step 13.
//!
//! Everything here writes straight through to [`crate::config::Config`] and calls
//! [`AppState::save_config`], so the on-disk `config.toml` is always what the dialog shows.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use mirai_core::RuleSet;

use crate::app::{AppState, signal};
use crate::config::{EngineProfile, ProfileKind, StrengthSetting};

/// Builds and presents the preferences dialog.
pub fn present(parent: &impl IsA<gtk::Widget>, state: &AppState) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .content_width(620)
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

    let add_local = gtk::Button::builder()
        .label("Add local…")
        .icon_name("list-add-symbolic")
        .build();
    add_local.add_css_class("pill");
    let add_remote = gtk::Button::builder()
        .label("Add remote…")
        .icon_name("network-server-symbolic")
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
    let save = gtk::Button::with_label("Save");
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

/// A read-only row showing a chosen path, with a button that opens a `gtk::FileDialog`.
fn file_row(
    title: &str,
    initial: PathBuf,
    dialog: &adw::PreferencesDialog,
) -> (adw::ActionRow, Rc<RefCell<PathBuf>>) {
    let cell = Rc::new(RefCell::new(initial));

    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(path_subtitle(&cell.borrow()))
        .build();
    row.set_use_markup(false);
    row.set_subtitle_lines(3);

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

/// `0` means "leave it to the analysis config file".
fn thread_row(title: &str, value: Option<u16>, max: f64) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(0.0, max, 1.0);
    row.set_title(title);
    row.set_subtitle("0 keeps the value from the analysis config");
    row.set_value(value.unwrap_or(0) as f64);
    row
}

fn spin_value_u16(row: &adw::SpinRow) -> Option<u16> {
    let v = row.value() as u16;
    (v > 0).then_some(v)
}

fn local_editor(
    dialog: &adw::PreferencesDialog,
    group: &adw::PreferencesGroup,
    state: &AppState,
    existing: Option<EngineProfile>,
) -> adw::NavigationPage {
    let editing = existing.as_ref().map(|p| p.name.clone());
    let (katago, model, config, analysis_threads, search_threads) = match existing.map(|p| p.kind) {
        Some(ProfileKind::Local {
            katago,
            model,
            config,
            analysis_threads,
            search_threads,
        }) => (katago, model, config, analysis_threads, search_threads),
        _ => (PathBuf::new(), PathBuf::new(), PathBuf::new(), None, None),
    };

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
        .description("All three must exist before the profile can be saved.")
        .build();
    let (katago_row, katago_path) = file_row("KataGo binary", katago, dialog);
    let (model_row, model_path) = file_row("Neural network model", model, dialog);
    let (config_row, config_path) = file_row("Analysis config", config, dialog);
    paths.add(&katago_row);
    paths.add(&model_row);
    paths.add(&config_row);
    editor.content.add(&paths);

    let tuning = adw::PreferencesGroup::builder().title("Tuning").build();
    let analysis_row = thread_row("Analysis threads", analysis_threads, 64.0);
    let search_row = thread_row("Search threads", search_threads, 256.0);
    tuning.add(&analysis_row);
    tuning.add(&search_row);
    editor.content.add(&tuning);

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
            let config = config_path.borrow().clone();

            if let Err(message) = check_name(&save_state, &name, editing.as_deref()) {
                complain(&banner, message);
                return;
            }
            for (label, path) in [
                ("KataGo binary", &katago),
                ("neural network model", &model),
                ("analysis config", &config),
            ] {
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
