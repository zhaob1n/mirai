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
use std::time::Duration;

use adw::prelude::*;
use glib::clone;
use gtk::{gio, glib};

use mirai_core::RuleSet;

use crate::app::{AppState, Change};
use crate::config::{
    AnalysisSettings, DEFAULT_SECONDS_PER_MOVE, DEFAULT_VISITS_PER_MOVE, EngineProfile,
    MAX_SECONDS_PER_MOVE, MAX_VISITS_PER_MOVE, MIN_SECONDS_PER_MOVE, PlaySettings, ProfileKind,
    StrengthSetting, UiSettings,
};
use crate::i18n::{self, gettext, gettext_f, ngettext_f, pgettext};
use crate::preferences_shell::PreferencesWidgets;
use crate::profile_editor::{self, LocalFormWidgets, RemoteFormWidgets};
use mirai_engine::{
    CalibrationConfig, CalibrationProgress, CalibrationResult, EngineTuning, TuningOverrides,
};
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

/// Puts a page's settings back into its rows. Each runs before every presentation, since
/// the config can change while the dialog is closed: the header's engine menu switches the
/// active profile, and New Game saves the rules and strength it started with.
type Reload = Box<dyn Fn(&PreferencesWidgets)>;

/// One window's preferences dialog, built the first time it is asked for and kept for the
/// window's life.
///
/// Building it is nearly all that opening it costs: the dialog's few hundred widgets,
/// then the measure `adw_dialog_present` makes of all four pages, took 38–130 ms of the GTK
/// thread on every open. Presenting the built dialog again takes 2–9 ms.
pub struct Preferences {
    dialog: adw::PreferencesDialog,
    /// Signal handlers weak-capture this set rather than owning their emitter.
    widgets: Rc<PreferencesWidgets>,
    reload: [Reload; 4],
    /// Presented and not yet closing.
    shown: Rc<Cell<bool>>,
}

/// Presents `slot`'s preferences dialog over `parent`, building it on first use.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    slot: &RefCell<Option<Preferences>>,
    state: &AppState,
) {
    let mut slot = slot.borrow_mut();
    let prefs = slot.get_or_insert_with(|| build(state));
    // Already up: presenting again would be a no-op, but the reload would rewrite the rows
    // under the user and the pop below would close the editor they are in.
    if prefs.shown.get() {
        return;
    }
    // A profile editor left open is not where Preferences opens.
    while prefs.dialog.pop_subpage() {}
    for reload in &prefs.reload {
        reload(&prefs.widgets);
    }
    prefs.shown.set(true);
    prefs.dialog.present(Some(parent));
}

fn build(state: &AppState) -> Preferences {
    let (dialog, widgets) = crate::preferences_shell::dialog();
    let widgets = Rc::new(widgets);
    let reload = [
        connect_engines(&dialog, &widgets, state),
        connect_analysis(&dialog, &widgets, state),
        connect_play(&dialog, &widgets, state),
        connect_general(&dialog, &widgets, state),
    ];
    let shown = Rc::new(Cell::new(false));
    dialog.connect_closed({
        let shown = shown.clone();
        move |_| shown.set(false)
    });
    Preferences {
        dialog,
        widgets,
        reload,
        shown,
    }
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
            profiles.append_item(&gio::MenuItem::new(
                Some(&gettext("No Engine Profiles")),
                None,
            ));
        }
    }
    menu.append_section(None, &profiles);

    let tail = gio::Menu::new();
    tail.append(Some(&gettext("Preferences…")), Some("win.preferences"));
    menu.append_section(None, &tail);

    menu
}

// -- Engines ----------------------------------------------------------------------------

fn connect_engines(
    dialog: &adw::PreferencesDialog,
    widgets: &PreferencesWidgets,
    state: &AppState,
) -> Reload {
    let local_state = state.clone();
    widgets.add_local_button.connect_activated(clone!(
        #[weak]
        dialog,
        #[weak(rename_to = group)]
        widgets.profiles_group,
        move |_| {
            open_editor(&dialog, &group, &local_state, None, false);
        }
    ));

    let remote_state = state.clone();
    widgets.add_remote_button.connect_activated(clone!(
        #[weak]
        dialog,
        #[weak(rename_to = group)]
        widgets.profiles_group,
        move |_| {
            open_editor(&dialog, &group, &remote_state, None, true);
        }
    ));

    let dialog = dialog.clone();
    let state = state.clone();
    // What the rows were last built from: a reopen rebuilds them only if it moved.
    let shown: RefCell<Option<(Vec<EngineProfile>, Option<String>)>> = RefCell::default();
    Box::new(move |widgets| {
        let now = {
            let cfg = state.config();
            (cfg.engine_profiles.clone(), cfg.active_engine.clone())
        };
        if shown.borrow().as_ref() != Some(&now) {
            refresh_profiles(&widgets.profiles_group, &dialog, &state);
            *shown.borrow_mut() = Some(now);
        }
    })
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
            .title(gettext("No Engine Profiles Yet"))
            .subtitle(gettext(
                "Add a local KataGo installation or a remote mirai-server.",
            ))
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
            .tooltip_text(gettext("Use This Engine"))
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
        edit.set_tooltip_text(Some(&gettext("Edit This Profile")));
        edit.add_css_class("flat");
        let edit_state = state.clone();
        let edit_name = name.clone();
        edit.connect_clicked(clone!(
            #[weak]
            dialog,
            #[weak]
            group,
            move |_| {
                // Read now, not when the row was built: selecting a remote profile here
                // pins its certificate without rebuilding the rows, and an editor opened on
                // the older copy would save the pin away again.
                let Some(profile) = edit_state.config().profile(&edit_name).cloned() else {
                    return;
                };
                let remote = !profile.is_local();
                open_editor(&dialog, &group, &edit_state, Some(profile), remote);
            }
        ));
        row.add_suffix(&edit);

        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.set_valign(gtk::Align::Center);
        delete.set_tooltip_text(Some(&gettext("Delete This Profile")));
        delete.add_css_class("flat");
        let delete_state = state.clone();
        let delete_name = name.clone();
        delete.connect_clicked(clone!(
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
    let heading = gettext("Delete This Profile?");
    let body = gettext_f(
        "“{name}” will be removed from the configuration. Nothing on disk is deleted.",
        &[("name", &name)],
    );
    let alert = adw::AlertDialog::new(Some(&heading), Some(&body));
    let cancel = gettext("Cancel");
    let delete_label = gettext("Delete");
    alert.add_responses(&[("cancel", &cancel), ("delete", &delete_label)]);
    alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");

    let state = state.clone();
    alert.connect_response(
        None,
        clone!(
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
                state.toast(gettext_f("Deleted “{name}”", &[("name", &name)]));
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
    save: gtk::Button,
    banner: adw::Banner,
}

fn editor_shell(title: &str, content: &impl IsA<gtk::Widget>) -> Editor {
    let save = gtk::Button::builder()
        .label(gettext("Save Profile"))
        .css_classes(["suggested-action"])
        .build();
    let banner = adw::Banner::builder()
        .title(gettext("The profile could not be saved"))
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::builder().content(content).build();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    let page = adw::NavigationPage::builder()
        .title(title)
        .child(&toolbar)
        .build();
    Editor { page, save, banner }
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

/// Wires a declared file row: the subtitle shows `initial`, and the button opens a
/// `gtk::FileDialog`. Where discovery runs, [`show_discovered`] later adds a chooser over
/// what it found.
fn wire_file_row(
    row: &adw::ActionRow,
    button: &gtk::Button,
    initial: PathBuf,
    chooser_title: &str,
    dialog: &adw::PreferencesDialog,
) -> Rc<RefCell<PathBuf>> {
    let cell = Rc::new(RefCell::new(initial));
    row.set_subtitle(&path_subtitle(&cell.borrow()));

    let row = row.clone();
    let prompt = chooser_title.to_string();
    let cell_for_click = cell.clone();
    button.connect_clicked(clone!(
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

    cell
}

fn path_subtitle(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        gettext("Not chosen")
    } else {
        path.display().to_string()
    }
}

fn missing_path(path: &Path) -> String {
    // Translators: {path} is a filesystem path the user chose.
    gettext_f(
        "{path} does not exist.",
        &[("path", &path.display().to_string())],
    )
}

/// Puts discovered paths into `slot`. When `suggest` is set and the row is still empty,
/// the first candidate becomes the displayed path — the suggestion a synchronous walk
/// used to make before the editor opened.
fn show_discovered(
    slot: &gtk::Box,
    row: &adw::ActionRow,
    path: &Rc<RefCell<PathBuf>>,
    candidates: Vec<PathBuf>,
    suggest: bool,
) {
    let Some(first) = candidates.first() else {
        return;
    };
    if suggest && path.borrow().as_os_str().is_empty() {
        row.set_subtitle(&path_subtitle(first));
        *path.borrow_mut() = first.clone();
    }
    slot.append(&discovered_button(&candidates, row, path));
}

/// A chooser over every path discovered in the configured XDG directories, living in the row
/// it fills rather than beside it.
///
/// Labels are file names, which is what actually distinguishes candidates; a name two
/// directories share carries its directory underneath. Both ellipsize in the middle, keeping
/// the ends that identify a file, and the whole path is in the tooltip either way. The row's
/// own file button still accepts anything discovery never saw.
fn discovered_button(
    candidates: &[PathBuf],
    row: &adw::ActionRow,
    path: &Rc<RefCell<PathBuf>>,
) -> gtk::MenuButton {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let popover = gtk::Popover::builder().build();
    for (candidate, (name, dir)) in candidates.iter().zip(candidate_labels(candidates)) {
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
        button.connect_clicked(clone!(
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
    let n = candidates.len() as u64;
    let count = n.to_string();
    // Translators: {n} is how many files were found on disk; there is always more than one
    // when the plural is used.
    let tip = ngettext_f(
        "Choose the Discovered File",
        "Choose One of {n} Discovered Files",
        n,
        &[("n", &count)],
    );
    let button = gtk::MenuButton::builder()
        .icon_name("view-list-symbolic")
        .popover(&popover)
        .valign(gtk::Align::Center)
        .tooltip_text(tip)
        .build();
    button.add_css_class("flat");
    button
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

/// Configures a declared spin row. `0` means "use the default": mirai's own when it
/// generates the analysis config, or whatever the file says when the user supplies one.
///
/// The maximum is an [`EngineTuning`] constant and the value is the setting this
/// row is showing, so the range is applied when the editor opens.
fn configure_tuned(row: &adw::SpinRow, value: u32, max: f64) {
    row.configure(
        Some(&gtk::Adjustment::new(
            f64::from(value),
            0.0,
            max,
            1.0,
            10.0,
            0.0,
        )),
        1.0,
        0,
    );
    hide_steppers(row);
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
/// and mirai asks for ownership on nearly every query. The row never shows a power between
/// 0 and [`EngineTuning::MIN_CACHE_POWER`], so the figure is always whole MiB.
fn cache_subtitle(power: u8) -> String {
    if power == 0 {
        let power = EngineTuning::default()
            .nn_cache_size_power_of_two
            .to_string();
        // Translators: {power} is the cache-size exponent used when the row is left at 0.
        return gettext_f("0 uses mirai's default ({power})", &[("power", &power)]);
    }
    let bytes = EngineTuning {
        nn_cache_size_power_of_two: power,
        ..EngineTuning::default()
    }
    .cache_bytes();
    let power_s = power.to_string();
    let size = (bytes / (1024 * 1024)).to_string();
    // Translators: {power} is an exponent; {size} is a number of mebibytes.
    ngettext_f(
        "2^{power} evaluation, roughly {size} MiB once warm",
        "2^{power} evaluations, roughly {size} MiB once warm",
        1u64 << power,
        &[("power", &power_s), ("size", &size)],
    )
}

/// `0` is the sentinel for mirai's default; anything else lies in
/// [`EngineTuning::MIN_CACHE_POWER`]`..=`[`EngineTuning::MAX_CACHE_POWER`]. The spinner's
/// next step after `0` is the minimum, and the step down from the minimum is `0`: a value
/// in the gap follows the direction of the change, and one with no previous (`0`) is raised.
fn snap_cache_power(previous: u8, value: u8) -> u8 {
    if value == 0
        || (EngineTuning::MIN_CACHE_POWER..=EngineTuning::MAX_CACHE_POWER).contains(&value)
    {
        return value;
    }
    if value > EngineTuning::MAX_CACHE_POWER {
        return EngineTuning::MAX_CACHE_POWER;
    }
    if value < previous {
        0
    } else {
        EngineTuning::MIN_CACHE_POWER
    }
}

/// `None` keeps the default. A value the spinner should not have offered is clamped
/// rather than written through.
fn stored_cache_power(power: u8) -> Option<u8> {
    match snap_cache_power(0, power) {
        0 => None,
        power => Some(power),
    }
}

/// Both halves of a thread setting, in the words the two rows above the tuning group use.
fn candidate_summary(analysis: u16, search: u16) -> String {
    let positions = analysis.to_string();
    let threads = search.to_string();
    // Translators: {positions} is how many positions are searched at once; {threads} is the
    // thread count on each of them and stays in both forms.
    ngettext_f(
        "{positions} position in parallel, {threads} threads each",
        "{positions} positions in parallel, {threads} threads each",
        u64::from(analysis),
        &[("positions", &positions), ("threads", &threads)],
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
    let positions = tuning.analysis_threads.to_string();
    let threads = tuning.search_threads.to_string();
    let n = u64::from(tuning.analysis_threads);
    if speed > 0.0 {
        let speed = format!("{speed:.0}");
        // Translators: {positions} and {threads} are the winning thread setting; {speed} is
        // visits per second, already rounded.
        ngettext_f(
            "Fastest here: {positions} position in parallel, {threads} threads each — about {speed} visits/s. Press Save Profile to apply.",
            "Fastest here: {positions} positions in parallel, {threads} threads each — about {speed} visits/s. Press Save Profile to apply.",
            n,
            &[
                ("positions", &positions),
                ("threads", &threads),
                ("speed", &speed),
            ],
        )
    } else {
        // Translators: {positions} and {threads} are the winning thread setting.
        ngettext_f(
            "Fastest here: {positions} position in parallel, {threads} threads each. Press Save Profile to apply.",
            "Fastest here: {positions} positions in parallel, {threads} threads each. Press Save Profile to apply.",
            n,
            &[("positions", &positions), ("threads", &threads)],
        )
    }
}

/// The modal a calibration runs behind, with its bar and caption.
fn tuning_progress() -> (adw::AlertDialog, gtk::ProgressBar, gtk::Label) {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Tuning KataGo"))
        .body(gettext("Every setting is timed in its own KataGo, so this takes a few minutes. Your engine stays stopped until the run ends."))
        .default_response("cancel")
        .close_response("cancel")
        .build();
    let bar = gtk::ProgressBar::builder()
        .show_text(true)
        .text(gettext("Starting…"))
        .build();
    let caption = gtk::Label::builder()
        .label(gettext(
            "Waiting for the current engine to let go of the GPU…",
        ))
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&bar);
    content.append(&caption);
    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", &gettext("Stop Tuning"));
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
    // Discovery only supplies chooser entries. It walks directories, so the editor opens
    // first and the list buttons appear when the walk finishes. Managed mode remains
    // selected until the user explicitly switches to Custom file.
    let suggested_config = config.clone().unwrap_or_default();

    let title = if editing.is_some() {
        gettext("Edit Local Engine")
    } else {
        gettext("Add Local Engine")
    };
    let (form, widgets) = profile_editor::local_form();
    let editor = editor_shell(&title, &form);

    let LocalFormWidgets {
        name_row,
        katago_row,
        katago_button,
        model_row,
        model_slot,
        model_button,
        mode_row: mode,
        config_row,
        config_slot,
        config_button,
        analysis_row,
        search_row,
        memory_group: memory,
        batch_row,
        cache_row,
        tuning_group: auto,
        tune_row,
        tune_button: tune,
    } = widgets;
    name_row.set_text(editing.as_deref().unwrap_or(""));

    let katago_path = wire_file_row(
        &katago_row,
        &katago_button,
        katago,
        &gettext("Select the KataGo Binary"),
        dialog,
    );
    let model_path = wire_file_row(
        &model_row,
        &model_button,
        model,
        &gettext("Select the Neural Network Model"),
        dialog,
    );

    // KataGo will not start without a `-config`, but mirai can write that file itself —
    // it needs three keys the user has no reason to care about. A custom file stays
    // available for anyone who does.
    mode.set_selected(u32::from(custom));
    let config_path = wire_file_row(
        &config_row,
        &config_button,
        suggested_config,
        &gettext("Select the Custom Analysis Config"),
        dialog,
    );

    configure_tuned(
        &analysis_row,
        u32::from(analysis_threads.unwrap_or(0)),
        f64::from(EngineTuning::MAX_ANALYSIS_THREADS),
    );
    configure_tuned(
        &search_row,
        u32::from(search_threads.unwrap_or(0)),
        f64::from(EngineTuning::MAX_SEARCH_THREADS),
    );

    // A custom file has to carry these two itself — KataGo will not start without
    // `nnMaxBatchSize` — so there is nothing for mirai to override.
    let batch_default = defaults.nn_max_batch_size.to_string();
    // Translators: nnMaxBatchSize is a KataGo setting name; leave it untranslated. {size} is that default.
    let batch_subtitle = gettext_f(
        "nnMaxBatchSize — 0 uses mirai's default ({size}). Wants to be at least positions × threads.",
        &[("size", &batch_default)],
    );
    batch_row.set_subtitle(&batch_subtitle);
    configure_tuned(
        &batch_row,
        u32::from(batch.unwrap_or(0)),
        f64::from(EngineTuning::MAX_BATCH_SIZE),
    );
    let cache_shown = snap_cache_power(0, cache.unwrap_or(0));
    cache_row.set_subtitle(&cache_subtitle(cache_shown));
    configure_tuned(
        &cache_row,
        u32::from(cache_shown),
        f64::from(EngineTuning::MAX_CACHE_POWER),
    );
    let cache_previous = Rc::new(Cell::new(cache_shown));
    cache_row.connect_value_notify(move |row| {
        let value = row.value() as u8;
        let snapped = snap_cache_power(cache_previous.get(), value);
        if snapped != value {
            cache_previous.set(snapped);
            row.set_value(f64::from(snapped));
            return;
        }
        cache_previous.set(snapped);
        row.set_subtitle(&cache_subtitle(snapped));
    });

    // -- automatic tuning ---------------------------------------------------------------
    //
    // Threads and batch size are worth measuring rather than guessing: what a particular
    // GPU does with them is not something mirai can predict from the model file or the
    // driver version. The run needs the machine to itself, hence the engine shutdown and
    // the refusal to start with a second window open.
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
    tune.connect_clicked(clone!(
        #[weak]
        dialog,
        #[weak]
        tune_row,
        move |button| {
            let katago = tune_katago.borrow().clone();
            let model = tune_model.borrow().clone();
            if katago.as_os_str().is_empty() {
                complain(
                    &tune_banner,
                    gettext("Choose the KataGo binary before tuning."),
                );
                return;
            }
            if !katago.exists() {
                complain(&tune_banner, missing_path(&katago));
                return;
            }
            if model.as_os_str().is_empty() {
                complain(
                    &tune_banner,
                    gettext("Choose the neural network model before tuning."),
                );
                return;
            }
            if !model.exists() {
                complain(&tune_banner, missing_path(&model));
                return;
            }
            if other_windows_open(&dialog) {
                complain(
                    &tune_banner,
                    gettext(
                        "Close the other mirai windows before tuning: they keep the current \
                         KataGo on the GPU, which would skew every measurement.",
                    ),
                );
                return;
            }
            if tune_state.busy() {
                complain(
                    &tune_banner,
                    gettext(
                        "Wait for engine startup, or finish or cancel the running whole-game \
                         analysis before tuning.",
                    ),
                );
                return;
            }
            tune_banner.set_revealed(false);

            // Measure from where the user is now: whatever the rows say, with `0` meaning
            // mirai's default, exactly as a real start would read them.
            let tuning = EngineTuning::with_overrides(TuningOverrides {
                analysis_threads: spin_value_u16(&tune_analysis),
                search_threads: spin_value_u16(&tune_search),
                nn_max_batch_size: spin_value_u16(&tune_batch),
                nn_cache_size_power_of_two: spin_value_u8(&tune_cache),
            });
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
                            gettext("Tuning stopped because another mirai window opened."),
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
                    let step_n = (step.completed + 1).to_string();
                    let total = step.total.to_string();
                    bar.set_text(Some(&gettext_f(
                        "Step {step} of {total}",
                        &[("step", &step_n), ("total", &total)],
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
                    Ok(Err(e)) => complain(
                        &banner,
                        // Translators: {error} is the engine's own message.
                        gettext_f(
                            "Tuning failed: {error}",
                            &[("error", &crate::i18n::engine_error(&e))],
                        ),
                    ),
                    // The result channel only goes away with the task behind it.
                    Err(_) => {
                        complain(&banner, gettext("The tuning run stopped without a result."))
                    }
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
                    // Translators: numAnalysisThreads is a KataGo setting name; leave it untranslated.
                    gettext("numAnalysisThreads — 0 keeps the value from your analysis config"),
                    // Translators: numSearchThreadsPerAnalysisThread is a KataGo setting name; leave it untranslated.
                    gettext(
                        "numSearchThreadsPerAnalysisThread — 0 keeps the value from your analysis config",
                    ),
                )
            } else {
                let analysis_n = defaults.analysis_threads.to_string();
                let search_n = defaults.search_threads.to_string();
                (
                    // Translators: numAnalysisThreads is a KataGo setting name; leave it untranslated. {threads} is mirai's default.
                    gettext_f(
                        "numAnalysisThreads — 0 uses mirai's default ({threads})",
                        &[("threads", &analysis_n)],
                    ),
                    // Translators: numSearchThreadsPerAnalysisThread is a KataGo setting name; leave it untranslated. {threads} is mirai's default.
                    gettext_f(
                        "numSearchThreadsPerAnalysisThread — 0 uses mirai's default ({threads})",
                        &[("threads", &search_n)],
                    ),
                )
            };
            analysis_row.set_subtitle(&analysis);
            search_row.set_subtitle(&search);
        }
    };
    apply_mode(custom);
    mode.connect_selected_notify(move |row| apply_mode(row.selected() == 1));

    let discover = state.runtime().spawn_blocking(|| {
        (
            crate::config::discover_models(),
            crate::config::discover_analysis_configs(),
        )
    });
    let found_model_slot = model_slot.clone();
    let found_model_row = model_row.clone();
    let found_model_path = model_path.clone();
    let found_config_slot = config_slot.clone();
    let found_config_row = config_row.clone();
    let found_config_path = config_path.clone();
    glib::spawn_future_local(async move {
        let Ok((models, configs)) = discover.await else {
            return;
        };
        show_discovered(
            &found_model_slot,
            &found_model_row,
            &found_model_path,
            models,
            false,
        );
        show_discovered(
            &found_config_slot,
            &found_config_row,
            &found_config_path,
            configs,
            true,
        );
    });

    let save_state = state.clone();
    let banner = editor.banner.clone();
    editor.save.connect_clicked(clone!(
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
            if katago.as_os_str().is_empty() {
                complain(&banner, gettext("Choose the KataGo binary."));
                return;
            }
            if !katago.exists() {
                complain(&banner, missing_path(&katago));
                return;
            }
            if model.as_os_str().is_empty() {
                complain(&banner, gettext("Choose the neural network model."));
                return;
            }
            if !model.exists() {
                complain(&banner, missing_path(&model));
                return;
            }
            if let Some(path) = config.as_ref() {
                if path.as_os_str().is_empty() {
                    complain(&banner, gettext("Choose the analysis config."));
                    return;
                }
                if !path.exists() {
                    complain(&banner, missing_path(path));
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
                        .then(|| stored_cache_power(cache_row.value() as u8))
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

    let title = if editing.is_some() {
        gettext("Edit Remote Engine")
    } else {
        gettext("Add Remote Engine")
    };
    let (form, widgets) = profile_editor::remote_form();
    let editor = editor_shell(&title, &form);

    let RemoteFormWidgets {
        name_row,
        url_row,
        token_row,
        engine_row,
        trust_row,
        test_button: test,
    } = widgets;
    name_row.set_text(editing.as_deref().unwrap_or(""));

    // Only the address is shown and typed. mirai speaks one protocol, so its scheme says
    // nothing to the user; `entered_url` puts it back.
    url_row.set_text(url.strip_prefix(mirai_proto::URL_SCHEME).unwrap_or(&url));
    token_row.set_text(&token);
    engine_row.set_text(&engine);

    trust_row.set_subtitle(&fingerprint_subtitle(&pin.borrow()));

    // -- test connection ------------------------------------------------------------
    // The check in flight: re-testing replaces it, and leaving the editor drops it —
    // Back and Save pop the subpage without closing the dialog, so its `hidden` counts as
    // well as the dialog's `closed`. Either aborts the glib future, and with it the network
    // task through `AbortOnDrop`, so no Trust prompt outlives the editor that asked. The
    // dialog outlives its editors, so the `closed` handler goes when the editor does.
    let testing = Rc::new(TestSlot::default());
    let closed = Cell::new(Some(dialog.connect_closed({
        let testing = testing.clone();
        move |_| testing.abort()
    })));
    editor.page.connect_hidden({
        let testing = testing.clone();
        let dialog = dialog.downgrade();
        move |_| {
            testing.abort();
            if let (Some(dialog), Some(id)) = (dialog.upgrade(), closed.take()) {
                dialog.disconnect(id);
            }
        }
    });
    let test_state = state.clone();
    let test_pin = pin.clone();
    let test_banner = editor.banner.clone();
    let test_url = url_row.clone();
    let test_token = token_row.clone();
    let test_engine = engine_row.clone();
    test.connect_clicked(clone!(
        #[weak]
        dialog,
        #[weak]
        trust_row,
        move |button| {
            let Some(url) = entered_url(&test_url) else {
                complain(&test_banner, gettext("Enter the server address first."));
                return;
            };
            let token = test_token.text().to_string();
            let engine = {
                let e = test_engine.text().trim().to_string();
                (!e.is_empty()).then_some(e)
            };

            test_banner.set_revealed(false);
            button.set_sensitive(false);
            button.set_label(&gettext("Checking…"));

            let connect_url = url.clone();
            let probe = crate::app::AbortOnDrop::new(test_state.runtime().spawn(async move {
                mirai_engine::RemoteEngine::probe_fingerprint(&connect_url).await
            }));

            let button = button.clone();
            let banner = test_banner.clone();
            let pin = test_pin.clone();
            let runtime = test_state.runtime();
            let token = token.clone();
            let engine = engine.clone();
            let slot = testing.clone();
            let handle = glib::spawn_future_local(async move {
                let outcome = probe.await;
                button.set_sensitive(true);
                button.set_label(&gettext("Test Connection"));
                match outcome {
                    Ok(Ok(fingerprint)) => {
                        let pinned = (url.clone(), fingerprint.clone());
                        let trust_pin = pin.clone();
                        let trust_row = trust_row.clone();
                        let trust_url = url.clone();
                        let trust_fp = fingerprint.clone();
                        let trust_banner = banner.clone();
                        crate::dialogs::confirm_fingerprint(
                            &dialog,
                            &url,
                            &fingerprint,
                            move || {
                                *trust_pin.borrow_mut() = Some(pinned.clone());
                                trust_row.set_subtitle(&fingerprint_subtitle(&trust_pin.borrow()));
                                let connect_url = trust_url.clone();
                                let token = token.clone();
                                let engine = engine.clone();
                                let fp = trust_fp.clone();
                                let attempt = crate::app::AbortOnDrop::new(runtime.spawn(async move {
                                    mirai_engine::RemoteEngine::connect(
                                        &connect_url,
                                        &token,
                                        engine,
                                        fp,
                                    )
                                    .await
                                    .map(|_| ())
                                }));
                                let banner = trust_banner.clone();
                                slot.replace(glib::spawn_future_local(async move {
                                    match attempt.await {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => complain(
                                            &banner,
                                            // Translators: {error} is the engine's own message.
                                            gettext_f(
                                                "Certificate trusted, but the connection failed: {error}",
                                                &[("error", &crate::i18n::engine_error(&e))],
                                            ),
                                        ),
                                        Err(_) => complain(
                                            &banner,
                                            gettext("The connection attempt was cancelled."),
                                        ),
                                    }
                                }));
                            },
                            || {},
                        );
                    }
                    Ok(Err(e)) => complain(
                        &banner,
                        // Translators: {error} is the engine's own message.
                        gettext_f(
                            "Could not check the certificate: {error}",
                            &[("error", &crate::i18n::engine_error(&e))],
                        ),
                    ),
                    Err(_) => complain(&banner, gettext("The certificate check was cancelled.")),
                }
            });
            testing.replace(handle);
        }
    ));

    // -- save -------------------------------------------------------------------------
    let save_state = state.clone();
    let banner = editor.banner.clone();
    editor.save.connect_clicked(clone!(
        #[weak]
        dialog,
        #[weak]
        group,
        move |_| {
            let name = name_row.text().trim().to_string();
            let url = entered_url(&url_row);

            if let Err(message) = check_name(&save_state, &name, editing.as_deref()) {
                complain(&banner, message);
                return;
            }
            let Some(url) = url else {
                complain(&banner, gettext("Enter the server address."));
                return;
            };
            if let Err(e) = mirai_proto::parse_url(&url) {
                complain(
                    &banner,
                    // Translators: {error} says what is wrong with the address, in English.
                    gettext_f(
                        "The server address is not valid: {error}",
                        &[("error", &e.reason)],
                    ),
                );
                return;
            }

            let engine = {
                let e = engine_row.text().trim().to_string();
                (!e.is_empty()).then_some(e)
            };
            // Keep the pin only if it was obtained from this server. A different one is
            // unpinned, and selecting the profile probes again before sending the token.
            let cert_sha256 = pin_for(&pin.borrow(), &url);

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

/// The server URL for the address in `row`. A pasted `mirai://host` is taken as it is
/// rather than doubled.
fn entered_url(row: &adw::EntryRow) -> Option<String> {
    let text = row.text();
    let text = text.trim();
    let address = text.strip_prefix(mirai_proto::URL_SCHEME).unwrap_or(text);
    (!address.is_empty()).then(|| format!("{}{address}", mirai_proto::URL_SCHEME))
}

/// The fingerprint in `pin` if it was obtained from the server `url` names.
///
/// Endpoints are compared, not strings: the port defaults and a hand-edited config may
/// omit the scheme, so `box`, `mirai://box:9678` and `mirai://box/` are one server, as are
/// two spellings of one IP address. Comparing the text dropped a pin taken on `box` once
/// the user added `mirai://`, and the profile asked to trust the same certificate again
/// the moment it was selected. An IPv6 zone names an interface and keeps its case.
fn pin_for(pin: &Option<(String, String)>, url: &str) -> Option<String> {
    let (pinned_url, fingerprint) = pin.as_ref()?;
    let endpoint = |url: &str| {
        let (host, port) = mirai_proto::parse_url(url).ok()?;
        let (addr, zone) = host.split_once('%').unwrap_or((&host, ""));
        let addr = match addr.parse::<std::net::IpAddr>() {
            Ok(ip) => ip.to_string(),
            Err(_) => addr.to_ascii_lowercase(),
        };
        Some((addr, zone.to_string(), port))
    };
    (endpoint(pinned_url)? == endpoint(url)?).then(|| fingerprint.clone())
}

fn fingerprint_subtitle(pin: &Option<(String, String)>) -> String {
    match pin {
        Some((_, fingerprint)) => mirai_proto::sha256::format_fingerprint(fingerprint),
        None => gettext("Not pinned yet"),
    }
}

fn check_name(state: &AppState, name: &str, editing: Option<&str>) -> Result<(), String> {
    if name.is_empty() {
        return Err(gettext("The profile needs a name."));
    }
    let taken = state
        .config()
        .engine_profiles
        .iter()
        .any(|p| p.name == name && Some(p.name.as_str()) != editing);
    if taken {
        return Err(gettext_f(
            "There is already a profile called “{name}”.",
            &[("name", name)],
        ));
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

/// How long a visit-cap or report-interval edit must sit still before the live search
/// restarts. A spin row notifies on every step of a drag or key repeat; restarting KataGo
/// for each of those would throw away a search that was about to be replaced again.
const ANALYSIS_RESTART_DEBOUNCE: Duration = Duration::from_millis(300);

/// The one connection check an engine editor has in flight.
#[derive(Default)]
struct TestSlot(RefCell<Option<glib::JoinHandle<()>>>);

impl TestSlot {
    fn replace(&self, handle: glib::JoinHandle<()>) {
        if let Some(old) = self.0.borrow_mut().replace(handle) {
            old.abort();
        }
    }

    fn abort(&self) {
        if let Some(old) = self.0.borrow_mut().take() {
            old.abort();
        }
    }
}

/// One pending restart for the two rows that change a running search's limits.
///
/// The timeout holds only a [`glib::WeakRef`] to [`AppState`]. The dialog's handlers own
/// this value; when they drop, [`Drop`] removes a restart that has not fired yet. A timeout
/// that is already queued keeps the `Rc` alive until it fires, so closing Preferences in
/// that window still applies the value just saved.
struct AnalysisRestart {
    source: Cell<Option<glib::SourceId>>,
}

impl AnalysisRestart {
    fn schedule(self: &Rc<Self>, state: &AppState) {
        if let Some(id) = self.source.take() {
            id.remove();
        }
        let weak = state.downgrade();
        let this = Rc::clone(self);
        let id = glib::timeout_add_local(ANALYSIS_RESTART_DEBOUNCE, move || {
            // The source is running; removing it again would target an id glib has freed.
            this.source.take();
            if let Some(state) = weak.upgrade() {
                state.restart_analysis();
            }
            glib::ControlFlow::Break
        });
        self.source.set(Some(id));
    }
}

impl Drop for AnalysisRestart {
    fn drop(&mut self) {
        if let Some(id) = self.source.take() {
            id.remove();
        }
    }
}

fn connect_analysis(
    dialog: &adw::PreferencesDialog,
    widgets: &Rc<PreferencesWidgets>,
    state: &AppState,
) -> Reload {
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
            // Translators: the suggestions row, meaning every candidate rather than a number.
            row.set_text(&pgettext("limit", "All"));
            return true;
        }
        false
    });
    suggestions.connect_input(|row| {
        let text = row.text();
        let text = text.trim();
        (text.eq_ignore_ascii_case("all") || text.eq_ignore_ascii_case(&pgettext("limit", "All")))
            .then_some(Ok(0.0))
    });

    let restart = Rc::new(AnalysisRestart {
        source: Cell::new(None),
    });

    let visits_state = state.clone();
    let visits_syncing = syncing.clone();
    let visits_restart = Rc::clone(&restart);
    widgets
        .analysis_visits_row
        .connect_value_notify(move |row| {
            if visits_syncing.get() {
                return;
            }
            visits_state.config_mut().analysis.live_max_visits = row.value() as u32;
            visits_state.save_config();
            // The running search is still holding the previous cap.
            visits_restart.schedule(&visits_state);
        });

    let interval_state = state.clone();
    let interval_syncing = syncing.clone();
    let interval_restart = Rc::clone(&restart);
    let suggestions_restart = restart;
    widgets
        .analysis_interval_row
        .connect_value_notify(move |row| {
            if interval_syncing.get() {
                return;
            }
            interval_state.config_mut().analysis.report_interval_ms = row.value() as u16;
            interval_state.save_config();
            // The running search is still reporting at the previous interval.
            interval_restart.schedule(&interval_state);
        });

    let suggestions_state = state.clone();
    let suggestions_syncing = syncing.clone();
    widgets
        .analysis_suggestions_row
        .connect_value_notify(move |row| {
            if suggestions_syncing.get() {
                return;
            }
            let grew = {
                let mut cfg = suggestions_state.config_mut();
                let before = cfg.analysis.suggestion_limit();
                cfg.analysis.max_suggestions = row.value() as u8;
                cfg.analysis.suggestion_limit() > before
            };
            suggestions_state.save_config();
            // The live request carries the old cap: the engine never sends the extra moves.
            // Same pause as the visit cap — a key repeat must not restart on every step.
            if grew {
                suggestions_restart.schedule(&suggestions_state);
            }
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

    let sgf_state = state.clone();
    let sgf_syncing = syncing.clone();
    widgets.save_analysis_row.connect_active_notify(move |row| {
        if sgf_syncing.get() {
            return;
        }
        sgf_state.config_mut().analysis.save_in_sgf = row.is_active();
        sgf_state.save_config();
    });

    widgets.analysis_reset_button.connect_activated(clone!(
        #[weak]
        dialog,
        #[weak]
        widgets,
        #[strong]
        state,
        #[strong]
        syncing,
        move |_| reset_analysis(&dialog, &widgets, &state, &syncing)
    ));

    let state = state.clone();
    Box::new(move |widgets| load_analysis(widgets, &state, &syncing))
}

/// Pushes `config.analysis` into the preference rows.
///
/// The settings are copied out first: the value handlers take `config_mut`, and a `Ref` held
/// across them would panic.
fn load_analysis(widgets: &PreferencesWidgets, state: &AppState, syncing: &Cell<bool>) {
    let settings = state.config().analysis.clone();
    syncing.set(true);
    set_spin(
        &widgets.analysis_visits_row,
        settings.live_max_visits as f64,
    );
    set_spin(
        &widgets.analysis_interval_row,
        settings.report_interval_ms as f64,
    );
    set_spin(
        &widgets.analysis_suggestions_row,
        settings.max_suggestions as f64,
    );
    set_spin(
        &widgets.analysis_batch_visits_row,
        settings.batch_visits as f64,
    );
    widgets
        .analysis_auto_open_row
        .set_active(settings.auto_analyse_on_open);
    widgets.save_analysis_row.set_active(settings.save_in_sgf);
    syncing.set(false);
}

/// Sets a spin row unless it already shows `value`. Setting an equal value is not free:
/// the row formats its text again and queues a relayout, on every reopen.
fn set_spin(row: &adw::SpinRow, value: f64) {
    if row.value() != value {
        row.set_value(value);
    }
}

fn apply_analysis(
    widgets: &PreferencesWidgets,
    state: &AppState,
    syncing: &Cell<bool>,
    settings: AnalysisSettings,
) {
    state.config_mut().analysis = settings;
    load_analysis(widgets, state, syncing);
    state.save_config();
    // A live search is holding the old visit cap and report interval.
    state.restart_analysis();
    state.changed(Change::Report);
}

/// Restores the analysis defaults, offering the previous values back for as long as the
/// toast is up. Engine profiles are user data and are never part of a reset.
fn reset_analysis(
    dialog: &adw::PreferencesDialog,
    widgets: &Rc<PreferencesWidgets>,
    state: &AppState,
    syncing: &Rc<Cell<bool>>,
) {
    let previous = state.config().analysis.clone();
    apply_analysis(widgets, state, syncing, AnalysisSettings::default());

    let toast = undo_toast(&gettext("Analysis settings restored"));
    toast.connect_button_clicked(clone!(
        #[weak]
        widgets,
        #[strong]
        state,
        #[strong]
        syncing,
        move |_| apply_analysis(&widgets, &state, &syncing, previous.clone())
    ));
    dialog.add_toast(toast);
}

fn undo_toast(title: &str) -> adw::Toast {
    adw::Toast::builder()
        .title(title)
        .button_label(gettext("Undo"))
        .build()
}

// -- Play -------------------------------------------------------------------------------

fn connect_play(
    dialog: &adw::PreferencesDialog,
    widgets: &Rc<PreferencesWidgets>,
    state: &AppState,
) -> Reload {
    let syncing = Rc::new(Cell::new(false));

    let visits = pgettext("strength", "Visits");
    let time = gettext("Time per move");
    let human = gettext("Human-like");
    widgets
        .play_strength_kind_row
        .set_model(Some(&gtk::StringList::new(&[&visits, &time, &human])));
    configure_spin(&widgets.play_visits_row, 1.0, MAX_VISITS_PER_MOVE, 100.0, 0);
    configure_spin(
        &widgets.play_seconds_row,
        MIN_SECONDS_PER_MOVE,
        MAX_SECONDS_PER_MOVE,
        0.5,
        1,
    );
    configure_spin(&widgets.play_temperature_row, 0.0, 2.0, 0.05, 2);
    configure_spin(&widgets.play_threshold_row, 0.0, 0.5, 0.01, 2);
    configure_spin(&widgets.play_streak_row, 1.0, 10.0, 1.0, 0);
    let labels: Vec<String> = RuleSet::ALL
        .iter()
        .copied()
        .map(i18n::rules_label)
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    widgets
        .play_rules_row
        .set_model(Some(&gtk::StringList::new(&label_refs)));

    // The mode and its three value rows all describe one setting, so they share a handler.
    let on_strength: Rc<dyn Fn()> = Rc::new(clone!(
        #[weak(rename_to = _dialog)]
        dialog,
        #[weak]
        widgets,
        #[strong]
        state,
        #[strong]
        syncing,
        move || {
            if !syncing.get() {
                sync_strength_rows(&widgets);
                store_strength(&widgets, &state);
            }
        }
    ));
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

    widgets.play_reset_button.connect_activated(clone!(
        #[weak]
        dialog,
        #[weak]
        widgets,
        #[strong]
        state,
        #[strong]
        syncing,
        move |_| reset_play(&dialog, &widgets, &state, &syncing)
    ));

    let state = state.clone();
    Box::new(move |widgets| load_play(widgets, &state, &syncing))
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
    let (visits, seconds, human, selected) = match &play.strength {
        StrengthSetting::Visits { visits } => (
            *visits as f64,
            DEFAULT_SECONDS_PER_MOVE,
            crate::play::DEFAULT_HUMAN_PROFILE,
            0,
        ),
        StrengthSetting::Time { time_ms } => (
            f64::from(DEFAULT_VISITS_PER_MOVE),
            *time_ms as f64 / 1000.0,
            crate::play::DEFAULT_HUMAN_PROFILE,
            1,
        ),
        StrengthSetting::Human { profile } => (
            f64::from(DEFAULT_VISITS_PER_MOVE),
            DEFAULT_SECONDS_PER_MOVE,
            profile.as_str(),
            2,
        ),
    };
    syncing.set(true);
    set_spin(&widgets.play_visits_row, visits);
    set_spin(&widgets.play_seconds_row, seconds);
    if widgets.play_human_row.text() != human {
        widgets.play_human_row.set_text(human);
    }
    widgets.play_strength_kind_row.set_selected(selected);
    set_spin(&widgets.play_temperature_row, play.temperature as f64);
    set_spin(&widgets.play_threshold_row, play.resign_threshold as f64);
    set_spin(&widgets.play_streak_row, play.resign_streak as f64);
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
    widgets: &PreferencesWidgets,
    state: &AppState,
    syncing: &Cell<bool>,
    settings: PlaySettings,
) {
    state.config_mut().play = settings;
    load_play(widgets, state, syncing);
    state.save_config();
}

fn reset_play(
    dialog: &adw::PreferencesDialog,
    widgets: &Rc<PreferencesWidgets>,
    state: &AppState,
    syncing: &Rc<Cell<bool>>,
) {
    let previous = state.config().play.clone();
    apply_play(widgets, state, syncing, PlaySettings::default());

    let toast = undo_toast(&gettext("Play settings restored"));
    toast.connect_button_clicked(clone!(
        #[weak]
        widgets,
        #[strong]
        state,
        #[strong]
        syncing,
        move |_| apply_play(&widgets, &state, &syncing, previous.clone())
    ));
    dialog.add_toast(toast);
}

// -- General ----------------------------------------------------------------------------

fn connect_general(
    dialog: &adw::PreferencesDialog,
    widgets: &PreferencesWidgets,
    state: &AppState,
) -> Reload {
    bind_switch(&widgets.show_coordinates_row, state, "show-coordinates");
    bind_switch(&widgets.show_move_numbers_row, state, "show-move-numbers");

    let overlay_syncing = Rc::new(Cell::new(false));
    let none = pgettext("overlay", "None");
    let ownership = pgettext("overlay", "Ownership");
    let policy = pgettext("overlay", "Policy");
    widgets
        .overlay_row
        .set_model(Some(&gtk::StringList::new(&[&none, &ownership, &policy])));
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

    // These live on AppState; they are taken back when the dialog goes, with its window.
    let row = &widgets.overlay_row;
    let ownership_id = state.connect_ownership_overlay_notify(clone!(
        #[weak]
        row,
        #[strong]
        overlay_syncing,
        move |state| sync_overlay_row(&row, state, &overlay_syncing)
    ));
    let policy_id = state.connect_policy_overlay_notify(clone!(
        #[weak]
        row,
        #[strong]
        overlay_syncing,
        move |state| sync_overlay_row(&row, state, &overlay_syncing)
    ));
    let watched = state.clone();
    let ids = RefCell::new(Some((ownership_id, policy_id)));
    dialog.connect_destroy(move |_| {
        if let Some((ownership, policy)) = ids.borrow_mut().take() {
            watched.disconnect(ownership);
            watched.disconnect(policy);
        }
    });

    let scale = &widgets.stone_volume_scale;
    scale.set_format_value_func(|_, value| match value.round() as u8 {
        0 => gettext("Muted"),
        volume => format!("{volume}%"),
    });
    let sound_state = state.clone();
    scale.connect_value_changed(move |scale| {
        set_stone_volume(&sound_state, scale.value().round() as u8);
    });

    let reset_state = state.clone();
    let reset_scale = scale.clone();
    widgets.general_reset_button.connect_activated(clone!(
        #[weak]
        dialog,
        move |_| reset_ui(&dialog, &reset_scale, &reset_state)
    ));

    // The switches are bound and the overlay row follows AppState; the volume is only
    // ever written, so it is the one value this page has to reload.
    let state = state.clone();
    Box::new(move |widgets| {
        widgets
            .stone_volume_scale
            .set_value(f64::from(state.config().ui.stone_volume))
    })
}

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

fn sync_overlay_row(row: &adw::ComboRow, state: &AppState, syncing: &Cell<bool>) {
    let index = overlay_index(state.ownership_overlay(), state.policy_overlay());
    if row.selected() == index {
        return;
    }
    syncing.set(true);
    row.set_selected(index);
    syncing.set(false);
}

/// Display switches bind to `AppState` properties; the overlay combo maps onto
/// the same two booleans. The sound slider owns its own key.
fn apply_ui(scale: &gtk::Scale, state: &AppState, ui: UiSettings) {
    state.set_show_coordinates(ui.show_coordinates);
    state.set_show_move_numbers(ui.show_move_numbers);
    apply_overlay_index(
        state,
        overlay_index(ui.ownership_overlay, ui.policy_overlay),
    );
    // Written directly: a hand-edited value above 100 sits at the scale's top already, so
    // moving the scale to 100 would not fire and would leave it in the file.
    set_stone_volume(state, ui.stone_volume);
    scale.set_value(f64::from(ui.stone_volume));
    state.save_config();
}

fn set_stone_volume(state: &AppState, volume: u8) {
    if state.config().ui.stone_volume == volume {
        return;
    }
    state.config_mut().ui.stone_volume = volume;
    state.save_config();
    state.changed(crate::app::Change::StoneVolume);
}

fn reset_ui(dialog: &adw::PreferencesDialog, scale: &gtk::Scale, state: &AppState) {
    let previous = state.config().ui.clone();
    apply_ui(scale, state, UiSettings::default());

    let undo_state = state.clone();
    let undo_scale = scale.clone();
    let toast = undo_toast(&gettext("General settings restored"));
    toast.connect_button_clicked(move |_| apply_ui(&undo_scale, &undo_state, previous.clone()));
    dialog.add_toast(toast);
}

/// `AppState` is the source: `sync_create` copies source to target, and binding from the
/// row copied its unset `active = false` over the setting whenever Preferences
/// opened, turning coordinates off.
fn bind_switch(row: &adw::SwitchRow, state: &AppState, property: &'static str) {
    state
        .bind_property(property, row, "active")
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

    /// A pin trusted through Test Connection survives Save however the address is spelled,
    /// and never carries over to another server.
    #[test]
    fn a_pin_follows_the_server_not_the_spelling() {
        let pin = Some(("192.168.1.10".to_string(), "ab".repeat(32)));
        for same in [
            "192.168.1.10",
            "192.168.1.10:9678",
            "mirai://192.168.1.10",
            "mirai://192.168.1.10:9678/",
        ] {
            assert_eq!(pin_for(&pin, same), Some("ab".repeat(32)), "{same}");
        }
        let named = Some(("mirai://Box.local".to_string(), "cd".repeat(32)));
        assert_eq!(pin_for(&named, "box.local:9678"), Some("cd".repeat(32)));
        let v6 = Some(("mirai://[FE80:0::1%eth0]".to_string(), "ef".repeat(32)));
        assert_eq!(pin_for(&v6, "[fe80::1%eth0]:9678"), Some("ef".repeat(32)));
        assert_eq!(
            pin_for(&v6, "[fe80::1%ETH0]"),
            None,
            "a zone is case-sensitive"
        );
        assert_eq!(pin_for(&v6, "[fe80::1%eth1]"), None);
        for other in [
            "192.168.1.11",
            "192.168.1.10:9679",
            "mirai://192.168.1.10:1",
            "192.168.1.10/x",
        ] {
            assert_eq!(pin_for(&pin, other), None, "{other}");
        }
        assert_eq!(pin_for(&None, "192.168.1.10"), None);
    }

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

    #[test]
    fn cache_power_steps_from_zero_to_the_minimum_and_clamps_on_save() {
        assert_eq!(snap_cache_power(0, 1), EngineTuning::MIN_CACHE_POWER);
        assert_eq!(
            snap_cache_power(
                EngineTuning::MIN_CACHE_POWER,
                EngineTuning::MIN_CACHE_POWER - 1
            ),
            0
        );
        assert_eq!(snap_cache_power(0, 0), 0);
        assert_eq!(snap_cache_power(20, 20), 20);
        assert_eq!(snap_cache_power(0, 10), EngineTuning::MIN_CACHE_POWER);
        assert_eq!(snap_cache_power(20, 10), 0);
        assert_eq!(stored_cache_power(0), None);
        assert_eq!(stored_cache_power(1), Some(EngineTuning::MIN_CACHE_POWER));
        assert_eq!(stored_cache_power(20), Some(20));
        assert_eq!(
            stored_cache_power(EngineTuning::MAX_CACHE_POWER + 1),
            Some(EngineTuning::MAX_CACHE_POWER)
        );
        assert!(cache_subtitle(20).contains("3072 MiB"));
    }
}
