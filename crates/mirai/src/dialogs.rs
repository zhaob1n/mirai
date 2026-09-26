// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Dynamic scoring and remote-profile confirmation dialogs. Steps 11 and 13.

use adw::prelude::*;

use crate::app::AppState;

/// Score result dialog. Close keeps counting; Review Game stops play; Analyse Game
/// stops then starts whole-game analysis (wired by the caller).
pub fn show_score_with(
    parent: &impl IsA<gtk::Widget>,
    state: &AppState,
    summary: &str,
    on_analyse: impl Fn() + 'static,
    on_review: impl Fn() + 'static,
) {
    let (rules, komi, size) = {
        let tree = state.tree();
        (tree.info.rules, tree.info.komi, tree.info.size)
    };
    let body = format!(
        "{summary}\n\n{}×{} · {} · komi {komi}",
        size.w,
        size.h,
        rules.label()
    );
    let dialog = adw::AlertDialog::new(Some("Result"), Some(&body));
    dialog.add_responses(&[
        ("close", "Close"),
        ("review", "Review Game"),
        ("analyse", "Analyse Game"),
    ]);
    dialog.set_response_appearance("analyse", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.connect_response(None, move |_, response| match response {
        "analyse" => on_analyse(),
        "review" => on_review(),
        _ => {}
    });
    dialog.present(Some(parent));
}

/// Asks the user to compare a server's certificate fingerprint before pinning it.
///
/// The fingerprint is a selectable label under the body, grouped the same way
/// `mirai-server` prints it at startup. Cancel is the default and the close
/// response, so Enter and Escape dismiss the dialog and never pin the server.
/// Trust is not styled as destructive. Returns the dialog so a superseded
/// activation can dismiss it.
pub fn confirm_fingerprint(
    parent: &impl IsA<gtk::Widget>,
    url: &str,
    fingerprint: &str,
    on_trust: impl Fn() + 'static,
    on_cancel: impl Fn() + 'static,
) -> adw::AlertDialog {
    let host = mirai_proto::parse_url(url)
        .map(|(host, _)| host)
        .unwrap_or_else(|_| url.to_string());
    let pretty = mirai_proto::sha256::format_fingerprint(fingerprint);
    let dialog = adw::AlertDialog::new(
        Some("Trust This Server?"),
        Some(&format!(
            "{host} presented this certificate. Compare it with the fingerprint \
             mirai-server printed at startup. The token is sent only after you \
             trust it, and a later change is refused."
        )),
    );
    // Colon groups are one token, so word wrap would not break them. WordChar
    // keeps the whole pin visible inside the dialog instead of ellipsizing it.
    let pin = gtk::Label::builder()
        .label(&pretty)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .ellipsize(gtk::pango::EllipsizeMode::None)
        .justify(gtk::Justification::Center)
        .xalign(0.5)
        .css_classes(["monospace"])
        .build();
    dialog.set_extra_child(Some(&pin));
    dialog.add_responses(&[("cancel", "Cancel"), ("trust", "Trust")]);
    dialog.set_response_appearance("trust", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "trust" {
            on_trust();
        } else {
            on_cancel();
        }
    });
    dialog.present(Some(parent));
    dialog
}

/// Dismisses a trust dialog a newer activation has replaced.
pub fn dismiss_trust_dialog(widget: &gtk::Widget) {
    if let Some(dialog) = widget.downcast_ref::<adw::AlertDialog>() {
        dialog.close();
    }
}
