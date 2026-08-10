// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! mirai — a KataGo analysis and playing GUI.

mod app;
mod batch;
mod config;
mod dialogs;
#[cfg(debug_assertions)]
mod harness;
mod panels;
mod play;
mod prefs;
mod util;
mod widgets;
mod window;

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

    // One shared multi-thread runtime for every engine driver. The GTK main loop never
    // blocks on it; results come back through `glib::spawn_future_local`.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("mirai-rt")
        .build()
        .expect("could not start the tokio runtime");
    let handle = runtime.handle().clone();

    gio::resources_register_include!("mirai.gresource")
        .expect("the compiled resource bundle should be embedded");

    let application = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    application.connect_startup(|_| {
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

    {
        let handle = handle.clone();
        application.connect_activate(move |app| {
            window::present(app, handle.clone(), None);
            #[cfg(debug_assertions)]
            harness::install(app);
        });
    }
    {
        let handle = handle.clone();
        application.connect_open(move |app, files, _hint| {
            let first = files.first().and_then(|f| f.path());
            window::present(app, handle.clone(), first);
            #[cfg(debug_assertions)]
            harness::install(app);
        });
    }

    let code = application.run();
    // Engines are dropped with the windows; give their processes a moment to exit.
    runtime.shutdown_timeout(std::time::Duration::from_millis(500));
    code
}
