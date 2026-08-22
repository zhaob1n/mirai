// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! A tiny scripted-UI harness, compiled only in debug builds and inert unless
//! `MIRAI_HARNESS` is set.
//!
//! It exists because this desktop is Wayland: an external screen grab of the XWayland root
//! window is black, so the only honest picture of what mirai draws comes from asking the
//! live `gsk` renderer for it. That works on the native Wayland backend, with no
//! `GDK_BACKEND` override. Driving the real `win.*` actions — the same ones the keyboard
//! accelerators activate — also keeps the smoke test on production code paths.
//!
//! ```text
//! MIRAI_HARNESS="wait:1500,action:win.toggle-analysis,wait:6000,shot:/tmp/a.png,quit"
//! ```
//!
//! Steps run in order: `wait:<ms>`, `wait-status:<text>`, `action:<prefix.name>`,
//! `action:<prefix.name>=<string arg>`, `press:<button text>`, `page:<preferences page>`,
//! `select:<row title>=<index>`, `set:<row title>=<number>`,
//! `fill:<entry placeholder>=<text>`, `shot:<path.png>`, `shot:<path.png>=<widget id>`,
//! `close-window`, `quit`.

use std::time::Duration;

use adw::prelude::{ComboRowExt, PreferencesDialogExt, PreferencesPageExt, PreferencesRowExt};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

#[derive(Debug)]
enum Step {
    Wait(u64),
    WaitStatus(String),
    Action(String, Option<String>),
    /// Activate the first button whose label contains this text, anywhere in the window's
    /// widget tree — including inside a presented `adw::Dialog`.
    Press(String),
    /// Open a PreferencesDialog page by title.
    Page(String),
    /// Set a ComboRow by title substring and raw model index.
    Select(String, u32),
    /// Set a SpinRow by title substring and value.
    Set(String, f64),
    /// Fill the first visible SearchEntry whose placeholder contains this text.
    Fill(String, String),
    /// Write a PNG of the whole window, or of the one widget whose Blueprint id is given —
    /// a 300x100 strip of the list you changed instead of a 1500x1600 window.
    Shot(String, Option<String>),
    CloseWindow,
    Quit,
}

fn parse(script: &str) -> Vec<Step> {
    script
        .split(',')
        .filter_map(|raw| {
            let step = raw.trim();
            if step.is_empty() {
                return None;
            }
            let (kind, rest) = step.split_once(':').unwrap_or((step, ""));
            match kind {
                "wait" => rest.parse().ok().map(Step::Wait),
                "wait-status" => Some(Step::WaitStatus(rest.to_string())),
                "shot" => Some(match rest.rsplit_once('=') {
                    Some((path, region)) => Step::Shot(path.to_string(), Some(region.to_string())),
                    None => Step::Shot(rest.to_string(), None),
                }),
                "close-window" => Some(Step::CloseWindow),
                "quit" => Some(Step::Quit),
                "press" => Some(Step::Press(rest.to_string())),
                "page" => Some(Step::Page(rest.to_string())),
                "select" => rest.rsplit_once('=').and_then(|(title, index)| {
                    index
                        .parse()
                        .ok()
                        .map(|index| Step::Select(title.to_string(), index))
                }),
                "set" => rest.rsplit_once('=').and_then(|(title, value)| {
                    value
                        .parse()
                        .ok()
                        .map(|value| Step::Set(title.to_string(), value))
                }),
                "fill" => rest
                    .split_once('=')
                    .map(|(field, text)| Step::Fill(field.to_string(), text.to_string())),
                "action" => Some(match rest.split_once('=') {
                    Some((name, arg)) => Step::Action(name.to_string(), Some(arg.to_string())),
                    None => Step::Action(rest.to_string(), None),
                }),
                other => {
                    eprintln!("harness: ignoring unknown step {other:?}");
                    None
                }
            }
        })
        .collect()
}

/// The application flags a harnessed run needs on top of the production ones.
///
/// A scripted run is `NON_UNIQUE`, because a unique `GApplication` hands its arguments to
/// whichever instance already owns the bus name and exits: the script would run nowhere, and
/// the developer's own window would be handed the script's SGF. Going non-unique also removes
/// the only reason to wrap a run in `dbus-run-session` — that private bus activates
/// `org.a11y.Bus`, whose launcher rewrites `$XDG_RUNTIME_DIR/at-spi/bus_0` and, when the
/// session ends, leaves every other GTK client in the login session unable to reach the
/// accessibility bus.
///
/// This still leaves the configuration, the autosave and the KataGo log directory shared with
/// the developer's instance; point `XDG_CONFIG_HOME` and `XDG_DATA_HOME` at a scratch
/// directory to isolate those. See `docs/dev/TESTING.md`.
pub fn application_flags() -> gio::ApplicationFlags {
    match std::env::var_os("MIRAI_HARNESS") {
        Some(_) => gio::ApplicationFlags::NON_UNIQUE,
        None => gio::ApplicationFlags::empty(),
    }
}

/// Starts the script against `app`, if `MIRAI_HARNESS` is set.
///
/// Called from both `activate` and `open`, and `open` may fire again later, so the script
/// runs exactly once: a second copy would drive the same actions against a second window.
pub fn install(app: &adw::Application) {
    thread_local! {
        static STARTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if STARTED.with(|started| started.replace(true)) {
        return;
    }
    let Ok(script) = std::env::var("MIRAI_HARNESS") else {
        return;
    };
    let steps = parse(&script);
    if steps.is_empty() {
        return;
    }
    eprintln!("harness: {} steps", steps.len());
    let app = app.clone();
    glib::spawn_future_local(async move {
        for step in steps {
            match step {
                Step::Wait(ms) => {
                    glib::timeout_future(Duration::from_millis(ms)).await;
                }
                Step::WaitStatus(text) => {
                    let found = wait_status(&app, &text).await;
                    eprintln!(
                        "harness: wait-status {text:?} -> {}",
                        if found { "ok" } else { "TIMEOUT" }
                    );
                }
                Step::Action(name, arg) => {
                    let done = activate(&app, &name, arg.as_deref());
                    eprintln!(
                        "harness: action {name} -> {}",
                        if done { "ok" } else { "MISSING" }
                    );
                    // Let the action's effects reach the frame clock.
                    glib::timeout_future(Duration::from_millis(120)).await;
                }
                Step::Press(label) => {
                    let done = press(&app, &label);
                    eprintln!(
                        "harness: press {label:?} -> {}",
                        if done { "ok" } else { "NOT FOUND" }
                    );
                    glib::timeout_future(Duration::from_millis(250)).await;
                }
                Step::Page(title) => {
                    let done = show_page(&app, &title);
                    eprintln!(
                        "harness: page {title:?} -> {}",
                        if done { "ok" } else { "NOT FOUND" }
                    );
                    glib::timeout_future(Duration::from_millis(250)).await;
                }
                Step::Select(title, index) => {
                    let done = select(&app, &title, index);
                    eprintln!(
                        "harness: select {title:?}={index} -> {}",
                        if done { "ok" } else { "NOT FOUND" }
                    );
                    glib::timeout_future(Duration::from_millis(250)).await;
                }
                Step::Set(title, value) => {
                    let done = set_spin(&app, &title, value);
                    eprintln!(
                        "harness: set {title:?}={value} -> {}",
                        if done { "ok" } else { "NOT FOUND" }
                    );
                    glib::timeout_future(Duration::from_millis(250)).await;
                }
                Step::Fill(field, text) => {
                    let done = fill(&app, &field, &text);
                    eprintln!(
                        "harness: fill {field:?} -> {}",
                        if done { "ok" } else { "NOT FOUND" }
                    );
                    glib::timeout_future(Duration::from_millis(120)).await;
                }
                Step::Shot(path, region) => {
                    // `WidgetPaintable::snapshot` yields nothing if the window has not
                    // drawn since the last change, so nudge it and retry a few frames.
                    let mut result = Err("not attempted".to_string());
                    for _ in 0..12 {
                        if let Some(w) = app.active_window() {
                            w.queue_draw();
                        }
                        glib::timeout_future(Duration::from_millis(120)).await;
                        result = shot(&app, &path, region.as_deref());
                        if result.is_ok() {
                            break;
                        }
                    }
                    match result {
                        Ok(()) => eprintln!("harness: wrote {path}"),
                        Err(e) => eprintln!("harness: screenshot failed: {e}"),
                    }
                }
                Step::CloseWindow => {
                    let done = app.active_window().is_some_and(|window| {
                        window.close();
                        true
                    });
                    eprintln!(
                        "harness: close-window -> {}",
                        if done { "ok" } else { "NO WINDOW" }
                    );
                    glib::timeout_future(Duration::from_millis(250)).await;
                }
                Step::Quit => {
                    eprintln!("harness: quitting");
                    app.quit();
                    return;
                }
            }
        }
    });
}

async fn wait_status(app: &adw::Application, needle: &str) -> bool {
    for _ in 0..100 {
        let found = app
            .active_window()
            .is_some_and(|window| find_label(window.upcast_ref(), needle));
        if found {
            return true;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    false
}

fn find_label(widget: &gtk::Widget, needle: &str) -> bool {
    if widget.is_visible()
        && let Some(label) = widget.downcast_ref::<gtk::Label>()
        && label.text().contains(needle)
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if find_label(&current, needle) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}

/// Activates `prefix.name` on the active window (or on the application for `app.*`).
///
/// `WidgetExt::activate_action` resolves the prefix through the widget's action muxer, so
/// this reaches exactly the same handler the keyboard accelerator would.
fn activate(app: &adw::Application, full: &str, arg: Option<&str>) -> bool {
    let param = arg.map(|a| a.to_variant());
    if let Some(name) = full.strip_prefix("app.") {
        if app.has_action(name) {
            app.activate_action(name, param.as_ref());
            return true;
        }
        return false;
    }
    match app.active_window() {
        Some(w) => w.activate_action(full, param.as_ref()).is_ok(),
        None => false,
    }
}

/// Activates the first `gtk::Button` whose text contains `needle`, searching the whole
/// widget tree under the active window. A presented `adw::Dialog` is a descendant of the
/// window, so this reaches dialog buttons too. A matching `gtk::MenuButton` is popped up
/// instead of clicked, which is how the discovered-file choosers are opened.
fn press(app: &adw::Application, needle: &str) -> bool {
    fn walk(w: &gtk::Widget, needle: &str) -> bool {
        if w.is_visible() {
            if let Some(menu) = w.downcast_ref::<gtk::MenuButton>()
                && menu_text(menu).is_some_and(|l| l.replace('_', "").contains(needle))
            {
                menu.popup();
                return true;
            }
            if let Some(button) = w.downcast_ref::<gtk::Button>()
                && button_text(button).is_some_and(|l| l.replace('_', "").contains(needle))
            {
                button.emit_clicked();
                return true;
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, needle) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    match app.active_window() {
        Some(w) => walk(w.upcast_ref::<gtk::Widget>(), needle),
        None => false,
    }
}

/// Opens the first `adw::PreferencesPage` whose title contains `needle`.
///
/// The libadwaita 1.9 view switcher is not a plain button, so emitting `clicked` on a child
/// does not select the page reliably. This enters through the dialog's public API instead.
fn show_page(app: &adw::Application, needle: &str) -> bool {
    fn walk(w: &gtk::Widget, needle: &str) -> Option<adw::PreferencesPage> {
        if let Some(page) = w.downcast_ref::<adw::PreferencesPage>()
            && page.title().contains(needle)
        {
            return Some(page.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(page) = walk(&c, needle) {
                return Some(page);
            }
            child = c.next_sibling();
        }
        None
    }

    let Some(window) = app.active_window() else {
        return false;
    };
    let Some(page) = walk(window.upcast_ref(), needle) else {
        return false;
    };
    let Some(dialog) = page
        .ancestor(adw::PreferencesDialog::static_type())
        .and_downcast::<adw::PreferencesDialog>()
    else {
        return false;
    };
    dialog.set_visible_page(&page);
    true
}

/// Selects the first visible `adw::ComboRow` whose title contains `needle`.
fn select(app: &adw::Application, needle: &str, index: u32) -> bool {
    fn walk(w: &gtk::Widget, needle: &str, index: u32) -> bool {
        if w.is_visible()
            && let Some(row) = w.downcast_ref::<adw::ComboRow>()
            && row.title().contains(needle)
        {
            row.set_selected(index);
            return true;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, needle, index) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    match app.active_window() {
        Some(w) => walk(w.upcast_ref::<gtk::Widget>(), needle, index),
        None => false,
    }
}

/// Sets the first visible `adw::SpinRow` whose title contains `needle`.
///
/// The preference rows have no stepper buttons, so `press:` cannot reach a number; this is
/// how a script changes one.
fn set_spin(app: &adw::Application, needle: &str, value: f64) -> bool {
    fn walk(w: &gtk::Widget, needle: &str, value: f64) -> bool {
        if w.is_visible()
            && let Some(row) = w.downcast_ref::<adw::SpinRow>()
            && row.title().contains(needle)
        {
            row.set_value(value);
            return true;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, needle, value) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    match app.active_window() {
        Some(w) => walk(w.upcast_ref::<gtk::Widget>(), needle, value),
        None => false,
    }
}

/// Fills the first visible `gtk::SearchEntry` whose placeholder contains `needle`.
fn fill(app: &adw::Application, needle: &str, text: &str) -> bool {
    fn walk(w: &gtk::Widget, needle: &str, text: &str) -> bool {
        if w.is_visible()
            && let Some(entry) = w.downcast_ref::<gtk::SearchEntry>()
            && entry
                .placeholder_text()
                .is_some_and(|placeholder| placeholder.contains(needle))
        {
            entry.set_text(text);
            return true;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, needle, text) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    match app.active_window() {
        Some(w) => walk(w.upcast_ref::<gtk::Widget>(), needle, text),
        None => false,
    }
}

/// A button's user-visible text. `GtkButton:label` is empty whenever the button holds a
/// child widget instead — an `adw::ButtonContent`, for one — so fall back to the first
/// label in its subtree, and then to the tooltip, which is the only text an icon-only
/// button ever shows.
fn button_text(button: &gtk::Button) -> Option<String> {
    fn first_label(w: &gtk::Widget) -> Option<String> {
        if let Some(label) = w.downcast_ref::<gtk::Label>() {
            return Some(label.text().to_string());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(text) = first_label(&c) {
                return Some(text);
            }
            child = c.next_sibling();
        }
        None
    }
    button
        .label()
        .map(|l| l.to_string())
        .or_else(|| button.child().and_then(|c| first_label(&c)))
        .or_else(|| button.tooltip_text().map(|t| t.to_string()))
}

/// A menu button's user-visible text: its label, else the tooltip an icon-only one carries.
fn menu_text(button: &gtk::MenuButton) -> Option<String> {
    button
        .label()
        .map(|l| l.to_string())
        .or_else(|| button.tooltip_text().map(|t| t.to_string()))
}

/// Renders the active window through its live `gsk` renderer and writes a PNG, optionally
/// cropped to one widget.
///
/// `region` is a widget id — Blueprint's, or `GtkWidget:name`. The window is always the node
/// that gets rendered and the region only narrows the *viewport*: a `WidgetPaintable` of the
/// widget alone draws no ancestor background, so a list came out as dark text on transparent
/// black. Cropping to one widget is not a nicety — reviewing a list row otherwise means
/// cutting it out of a 1486x1634 window PNG by hand every time.
fn shot(app: &adw::Application, path: &str, region: Option<&str>) -> Result<(), String> {
    let window = app.active_window().ok_or("no active window")?;
    let (w, h) = (window.width(), window.height());
    if w <= 0 || h <= 0 {
        return Err(format!("window is not mapped yet ({w}x{h})"));
    }
    let viewport = match region {
        Some(name) => {
            let target =
                find_named(window.upcast_ref(), name).ok_or(format!("no widget id {name:?}"))?;
            let bounds = target
                .compute_bounds(&window)
                .ok_or(format!("{name:?} has no bounds in the window"))?;
            if bounds.width() < 1.0 || bounds.height() < 1.0 {
                return Err(format!("{name:?} is not mapped yet"));
            }
            Some(bounds)
        }
        None => None,
    };

    let paintable = gtk::WidgetPaintable::new(Some(&window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or("nothing was drawn")?;

    let renderer = window
        .native()
        .and_then(|n| n.renderer())
        .ok_or("the window has no renderer")?;
    let texture = renderer.render_texture(&node, viewport.as_ref());
    texture
        .save_to_png(path)
        .map_err(|e| format!("{path}: {e}"))
}

/// The first widget whose Blueprint id — or `GtkWidget:name`, which is what CSS `#id` matches
/// — is exactly `name`.
fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name || widget.buildable_id().is_some_and(|id| id == name) {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_parsing_covers_every_step_kind() {
        let steps = parse(
            "wait:250, wait-status:Ready, action:win.toggle-analysis, action:win.set-engine=local, page:Analysis, select:Model=2, set:Suggestions Shown=0, fill:Exact nickname=柯洁, shot:/tmp/x.png, shot:/tmp/y.png=blunder_expander, close-window, quit",
        );
        assert_eq!(steps.len(), 12);
        assert!(matches!(steps[0], Step::Wait(250)));
        assert!(matches!(&steps[1], Step::WaitStatus(text) if text == "Ready"));
        match &steps[2] {
            Step::Action(n, None) => assert_eq!(n, "win.toggle-analysis"),
            other => panic!("{other:?}"),
        }
        match &steps[3] {
            Step::Action(n, Some(a)) => {
                assert_eq!(n, "win.set-engine");
                assert_eq!(a, "local");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&steps[4], Step::Page(title) if title == "Analysis"));
        assert!(matches!(&steps[5], Step::Select(title, 2) if title == "Model"));
        assert!(
            matches!(&steps[6], Step::Set(title, value) if title == "Suggestions Shown" && *value == 0.0)
        );
        assert!(
            matches!(&steps[7], Step::Fill(field, text) if field == "Exact nickname" && text == "柯洁")
        );
        assert!(matches!(&steps[8], Step::Shot(p, None) if p == "/tmp/x.png"));
        assert!(
            matches!(&steps[9], Step::Shot(p, Some(region)) if p == "/tmp/y.png" && region == "blunder_expander")
        );
        assert!(matches!(steps[10], Step::CloseWindow));
        assert!(matches!(steps[11], Step::Quit));
    }

    #[test]
    fn unknown_and_empty_steps_are_dropped_not_fatal() {
        assert!(parse("").is_empty());
        assert!(parse("  ,  ").is_empty());
        assert_eq!(parse("frobnicate:3,wait:10").len(), 1);
        // A malformed wait is dropped rather than silently becoming zero.
        assert!(parse("wait:soon").is_empty());
        assert!(parse("select:Model=not-an-index").is_empty());
        assert!(parse("fill:Nickname").is_empty());
    }
}
