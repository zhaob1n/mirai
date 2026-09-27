// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Dynamic scoring and remote-profile confirmation dialogs. Steps 11 and 13.

use adw::prelude::*;

use crate::app::AppState;
use crate::i18n;

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
    let rules = i18n::rules_label(rules);
    let width = size.w.to_string();
    let height = size.h.to_string();
    let komi = komi.to_string();
    // Translators: {width} and {height} are the board size; {rules} is the rule set; {komi} is White's compensation.
    let meta = i18n::gettext_f(
        "{width}×{height} · {rules} · komi {komi}",
        &[
            ("width", width.as_str()),
            ("height", height.as_str()),
            ("rules", rules.as_str()),
            ("komi", komi.as_str()),
        ],
    );
    let body = format!("{summary}\n\n{meta}");
    let title = i18n::gettext("Result");
    let dialog = adw::AlertDialog::new(Some(&title), Some(&body));
    let close = i18n::gettext("Close");
    let review = i18n::gettext("Review Game");
    let analyse = i18n::gettext("Analyse Game");
    dialog.add_responses(&[
        ("close", close.as_str()),
        ("review", review.as_str()),
        ("analyse", analyse.as_str()),
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
    let title = i18n::gettext("Trust This Server?");
    // Translators: {host} is a server address. mirai-server is the program name.
    let body = i18n::gettext_f(
        "{host} presented this certificate. Compare it with the fingerprint mirai-server printed at startup. The token is sent only after you trust it, and a later change is refused.",
        &[("host", host.as_str())],
    );
    let dialog = adw::AlertDialog::new(Some(&title), Some(&body));
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
    let cancel = i18n::gettext("Cancel");
    let trust = i18n::pgettext("verb", "Trust");
    dialog.add_responses(&[("cancel", cancel.as_str()), ("trust", trust.as_str())]);
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
