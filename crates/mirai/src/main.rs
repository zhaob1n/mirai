// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! mirai — a KataGo analysis and playing GUI.

mod app;
mod application_shell;
mod batch;
mod config;
mod dialogs;
mod engines;
mod fox;
mod fox_picker;
#[cfg(debug_assertions)]
mod harness;
mod new_game;
mod panels;
mod play;
mod preferences_shell;
mod prefs;
mod profile_editor;
mod render_probe;
mod util;
mod widgets;
mod window;
mod window_shell;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

const APP_ID: &str = "io.github.mirai.Mirai";
const RESOURCE_PREFIX: &str = "/io/github/mirai/Mirai";

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    gio::resources_register_include!("mirai.gresource")
        .expect("the compiled resource bundle should be embedded");

    // A harnessed run must never be adopted by an instance the developer is already using; see
    // `harness::application_flags`.
    #[cfg(debug_assertions)]
    let flags = gio::ApplicationFlags::HANDLES_OPEN | harness::application_flags();
    #[cfg(not(debug_assertions))]
    let flags = gio::ApplicationFlags::HANDLES_OPEN;

    let application = application_shell::MiraiApplication::new(APP_ID, flags);

    application.connect_startup(|_| {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display)
                .add_resource_path(&format!("{RESOURCE_PREFIX}/icons/hicolor"));
            gtk::Window::set_default_icon_name(APP_ID);
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_resource(&format!("{RESOURCE_PREFIX}/style.css"));
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });

    application.connect_activate(|app| {
        window::present(
            app.upcast_ref(),
            app.runtime_handle(),
            app.engine_pool(),
            None,
        );
        #[cfg(debug_assertions)]
        harness::install(app.upcast_ref());
    });
    application.connect_open(|app, files, _hint| {
        // One window per file. Dropping all but the first used to lose the rest without
        // a word when several records were opened at once.
        for file in files {
            window::present(
                app.upcast_ref(),
                app.runtime_handle(),
                app.engine_pool(),
                file.path(),
            );
        }
        #[cfg(debug_assertions)]
        harness::install(app.upcast_ref());
    });

    application.run()
}
