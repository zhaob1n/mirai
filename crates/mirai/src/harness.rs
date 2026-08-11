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
//! Steps run in order: `wait:<ms>`, `action:<prefix.name>`, `action:<prefix.name>=<string arg>`,
//! `shot:<path.png>`, `quit`.

use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

#[derive(Debug)]
enum Step {
    Wait(u64),
    Action(String, Option<String>),
    /// Activate the first button whose label contains this text, anywhere in the window's
    /// widget tree — including inside a presented `adw::Dialog`.
    Press(String),
    Shot(String),
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
                "shot" => Some(Step::Shot(rest.to_string())),
                "quit" => Some(Step::Quit),
                "press" => Some(Step::Press(rest.to_string())),
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
                Step::Shot(path) => {
                    // `WidgetPaintable::snapshot` yields nothing if the window has not
                    // drawn since the last change, so nudge it and retry a few frames.
                    let mut result = Err("not attempted".to_string());
                    for _ in 0..12 {
                        if let Some(w) = app.active_window() {
                            w.queue_draw();
                        }
                        glib::timeout_future(Duration::from_millis(120)).await;
                        result = shot(&app, &path);
                        if result.is_ok() {
                            break;
                        }
                    }
                    match result {
                        Ok(()) => eprintln!("harness: wrote {path}"),
                        Err(e) => eprintln!("harness: screenshot failed: {e}"),
                    }
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
/// window, so this reaches dialog buttons too.
fn press(app: &adw::Application, needle: &str) -> bool {
    fn walk(w: &gtk::Widget, needle: &str) -> bool {
        if w.is_visible()
            && let Some(button) = w.downcast_ref::<gtk::Button>()
            && button_text(button).is_some_and(|l| l.replace('_', "").contains(needle))
        {
            button.emit_clicked();
            return true;
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

/// Renders the active window through its live `gsk` renderer and writes a PNG.
fn shot(app: &adw::Application, path: &str) -> Result<(), String> {
    let window = app.active_window().ok_or("no active window")?;
    let (w, h) = (window.width(), window.height());
    if w <= 0 || h <= 0 {
        return Err(format!("window is not mapped yet ({w}x{h})"));
    }

    let paintable = gtk::WidgetPaintable::new(Some(&window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or("nothing was drawn")?;

    let renderer = window
        .native()
        .and_then(|n| n.renderer())
        .ok_or("the window has no renderer")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| format!("{path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_parsing_covers_every_step_kind() {
        let steps = parse(
            "wait:250, action:win.toggle-analysis, action:win.set-engine=local, shot:/tmp/x.png, quit",
        );
        assert_eq!(steps.len(), 5);
        assert!(matches!(steps[0], Step::Wait(250)));
        match &steps[1] {
            Step::Action(n, None) => assert_eq!(n, "win.toggle-analysis"),
            other => panic!("{other:?}"),
        }
        match &steps[2] {
            Step::Action(n, Some(a)) => {
                assert_eq!(n, "win.set-engine");
                assert_eq!(a, "local");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&steps[3], Step::Shot(p) if p == "/tmp/x.png"));
        assert!(matches!(steps[4], Step::Quit));
    }

    #[test]
    fn unknown_and_empty_steps_are_dropped_not_fatal() {
        assert!(parse("").is_empty());
        assert!(parse("  ,  ").is_empty());
        assert_eq!(parse("frobnicate:3,wait:10").len(), 1);
        // A malformed wait is dropped rather than silently becoming zero.
        assert!(parse("wait:soon").is_empty());
    }
}
