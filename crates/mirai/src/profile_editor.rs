// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/profile_editor.blp")]
    pub struct ProfileEditorPage {
        #[template_child]
        pub content: TemplateChild<adw::PreferencesPage>,
        #[template_child]
        pub save_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProfileEditorPage {
        const NAME: &'static str = "MiraiProfileEditorPage";

        type Type = super::ProfileEditorPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ProfileEditorPage {}
    impl WidgetImpl for ProfileEditorPage {}
    impl NavigationPageImpl for ProfileEditorPage {}
}

glib::wrapper! {
    pub struct ProfileEditorPage(ObjectSubclass<imp::ProfileEditorPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ProfileEditorPage {
    pub fn new(title: &str) -> Self {
        glib::Object::builder().property("title", title).build()
    }

    pub fn content(&self) -> adw::PreferencesPage {
        self.imp().content.get()
    }

    pub fn save_button(&self) -> gtk::Button {
        self.imp().save_button.get()
    }

    pub fn banner(&self) -> adw::Banner {
        self.imp().banner.get()
    }
}
