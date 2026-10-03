// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, glib};

use mirai_core::Point;

use crate::app::NodeRef;
use crate::i18n;
use crate::window_shell::MiraiWindow;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/label_editor.blp")]
    pub struct LabelEditorDialog {
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub apply_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub entry: TemplateChild<gtk::Entry>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LabelEditorDialog {
        const NAME: &'static str = "MiraiLabelEditorDialog";

        type Type = super::LabelEditorDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LabelEditorDialog {}
    impl WidgetImpl for LabelEditorDialog {}
    impl AdwDialogImpl for LabelEditorDialog {}
}

glib::wrapper! {
    pub struct LabelEditorDialog(ObjectSubclass<imp::LabelEditorDialog>)
        @extends gtk::Widget, adw::Dialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::ShortcutManager;
}

impl LabelEditorDialog {
    fn new() -> Self {
        let dialog = glib::Object::new();
        crate::widgets::sheet_texture::install(&dialog);
        dialog
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

    let dialog = LabelEditorDialog::new();
    let entry = dialog.imp().entry.get();
    entry.set_text(&prefill);

    dialog.imp().cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    let weak = window.downgrade();
    dialog.imp().apply_button.connect_clicked({
        let weak = weak.clone();
        clone!(
            #[weak]
            dialog,
            move |_| apply_label(&dialog, &weak, node, point)
        )
    });
    entry.connect_activate({
        let weak = weak.clone();
        clone!(
            #[weak]
            dialog,
            move |_| apply_label(&dialog, &weak, node, point)
        )
    });

    dialog.present(Some(window));
    entry.grab_focus();
}

fn apply_label(
    dialog: &LabelEditorDialog,
    window: &glib::WeakRef<MiraiWindow>,
    node: NodeRef,
    point: Point,
) {
    let text = dialog.imp().entry.text();
    if let Some(window) = window.upgrade() {
        window.with_ui(|ui| {
            let cursor = ui.state.cursor();
            if ui.play.is_active() || ui.state.resolve_node(node) != Some(cursor) {
                ui.state.toast(i18n::gettext(
                    "The position changed; open the label editor again",
                ));
            } else {
                ui.state
                    .with_edit_session(|session| session.set_label(point, text.as_str()));
            }
        });
    }
    dialog.close();
}
