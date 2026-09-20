// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, gdk, glib};

use mirai_core::Point;

use crate::app::NodeRef;
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
        glib::Object::new()
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
    intercept_entry_history(&entry);

    dialog.imp().cancel_button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    let weak = window.downgrade();
    dialog.imp().apply_button.connect_clicked({
        let weak = weak.clone();
        glib::clone!(
            #[weak]
            dialog,
            move |_| apply_label(&dialog, &weak, node, point)
        )
    });
    entry.connect_activate({
        let weak = weak.clone();
        glib::clone!(
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
                ui.state
                    .toast("The position changed; open the label editor again");
            } else {
                ui.state
                    .with_edit_session(|session| session.set_label(point, text.as_str()));
            }
        });
    }
    dialog.close();
}

fn intercept_entry_history(entry: &gtk::Entry) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        entry,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, keyval, _, modifiers| intercept_undo_keys(&entry, keyval, modifiers)
    ));
    entry.add_controller(keys);
}

fn intercept_undo_keys(
    entry: &gtk::Entry,
    keyval: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
        return glib::Propagation::Proceed;
    }
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    let z = keyval == gdk::Key::z || keyval == gdk::Key::Z;
    let y = keyval == gdk::Key::y || keyval == gdk::Key::Y;
    let action = if !shift && z {
        "text.undo"
    } else if (shift && z) || (!shift && y) {
        "text.redo"
    } else {
        return glib::Propagation::Proceed;
    };
    if let Some(text) = entry
        .delegate()
        .and_then(|d| d.downcast::<gtk::Text>().ok())
    {
        let _ = text.activate_action(action, None);
    }
    glib::Propagation::Stop
}
