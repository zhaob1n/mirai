// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::prelude::*;
use glib::clone;

use mirai_core::Point;

use crate::app::NodeRef;
use crate::i18n::{gettext, pgettext};
use crate::window_shell::MiraiWindow;

struct Editor {
    dialog: adw::Dialog,
    entry: gtk::Entry,
    cancel: gtk::Button,
    apply: gtk::Button,
}

fn editor() -> Editor {
    // Translators: a text mark on an intersection.
    let title = pgettext("mark", "Edit Label");
    let cancel = gtk::Button::builder()
        .name("cancel_button")
        .label(gettext("Cancel"))
        .build();
    // Translators: a text mark on an intersection.
    let apply = gtk::Button::builder()
        .name("apply_button")
        .label(pgettext("mark", "Apply Label"))
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&apply);

    // Translators: a text mark on an intersection.
    let entry = gtk::Entry::builder()
        .name("entry")
        .placeholder_text(pgettext("mark", "Label text"))
        .hexpand(true)
        .enable_undo(true)
        .build();
    // Translators: a text mark on an intersection.
    let hint = gtk::Label::builder()
        .label(gettext("Leave empty to remove the label"))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["caption"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(16)
        .margin_end(16)
        .margin_top(12)
        .margin_bottom(16)
        .build();
    content.append(&entry);
    content.append(&hint);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(&title)
        .content_width(360)
        .child(&toolbar)
        .build();
    crate::widgets::sheet_texture::install(&dialog);
    Editor {
        dialog,
        entry,
        cancel,
        apply,
    }
}

pub(crate) fn present(window: &MiraiWindow, point: Point) {
    let Some((node, prefill)) = window.with_ui(|ui| {
        let node = ui.state.cursor_ref();
        let prefill = {
            let tree = ui.state.tree();
            tree.node(node.id)
                .marks
                .labels
                .iter()
                .find(|(p, _)| *p == point)
                .map(|(_, text)| text.clone())
                .unwrap_or_default()
        };
        (node, prefill)
    }) else {
        return;
    };

    let Editor {
        dialog,
        entry,
        cancel,
        apply,
    } = editor();
    entry.set_text(&prefill);

    cancel.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    let weak = window.downgrade();
    apply.connect_clicked({
        let weak = weak.clone();
        clone!(
            #[weak]
            dialog,
            #[weak]
            entry,
            move |_| apply_label(&dialog, &entry, &weak, node, point)
        )
    });
    entry.connect_activate({
        let weak = weak.clone();
        clone!(
            #[weak]
            dialog,
            move |entry| apply_label(&dialog, entry, &weak, node, point)
        )
    });

    dialog.present(Some(window));
    entry.grab_focus();
}

fn apply_label(
    dialog: &adw::Dialog,
    entry: &gtk::Entry,
    window: &glib::WeakRef<MiraiWindow>,
    node: NodeRef,
    point: Point,
) {
    let text = entry.text();
    if let Some(window) = window.upgrade() {
        window.with_ui(|ui| {
            let cursor = ui.state.cursor();
            if ui.play.is_active() || ui.state.resolve_node(node) != Some(cursor) {
                ui.state
                    .toast(gettext("The position changed; open the label editor again"));
            } else {
                ui.state
                    .with_edit_session(|session| session.set_label(point, text.as_str()));
            }
        });
    }
    dialog.close();
}
