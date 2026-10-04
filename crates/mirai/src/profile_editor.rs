// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{CompositeTemplate, glib};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/profile_editor.blp")]
    pub struct ProfileEditorPage {
        #[template_child]
        pub toolbar: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub save_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
    }

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/local_profile_form.blp")]
    pub struct LocalProfileForm {
        #[template_child]
        pub name_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub katago_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub katago_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub katago_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub model_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub model_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub model_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub mode_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub config_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub config_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub config_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub analysis_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub search_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub memory_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub batch_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub cache_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub tuning_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub tune_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub tune_button: TemplateChild<gtk::Button>,
    }

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/remote_profile_form.blp")]
    pub struct RemoteProfileForm {
        #[template_child]
        pub name_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub url_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub token_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub engine_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub trust_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub test_button: TemplateChild<gtk::Button>,
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

    #[glib::object_subclass]
    impl ObjectSubclass for LocalProfileForm {
        const NAME: &'static str = "MiraiLocalProfileForm";

        type Type = super::LocalProfileForm;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LocalProfileForm {}
    impl WidgetImpl for LocalProfileForm {}
    impl PreferencesPageImpl for LocalProfileForm {}

    #[glib::object_subclass]
    impl ObjectSubclass for RemoteProfileForm {
        const NAME: &'static str = "MiraiRemoteProfileForm";

        type Type = super::RemoteProfileForm;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RemoteProfileForm {}
    impl WidgetImpl for RemoteProfileForm {}
    impl PreferencesPageImpl for RemoteProfileForm {}
}

glib::wrapper! {
    pub struct ProfileEditorPage(ObjectSubclass<imp::ProfileEditorPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

glib::wrapper! {
    pub struct LocalProfileForm(ObjectSubclass<imp::LocalProfileForm>)
        @extends gtk::Widget, adw::PreferencesPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

glib::wrapper! {
    pub struct RemoteProfileForm(ObjectSubclass<imp::RemoteProfileForm>)
        @extends gtk::Widget, adw::PreferencesPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ProfileEditorPage {
    pub fn new(title: &str, content: &impl IsA<gtk::Widget>) -> Self {
        let page: Self = glib::Object::builder().property("title", title).build();
        page.imp().toolbar.set_content(Some(content));
        page
    }

    pub fn save_button(&self) -> gtk::Button {
        self.imp().save_button.get()
    }

    pub fn banner(&self) -> adw::Banner {
        self.imp().banner.get()
    }
}

impl LocalProfileForm {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn name_row(&self) -> adw::EntryRow {
        self.imp().name_row.get()
    }

    pub fn katago_row(&self) -> adw::ActionRow {
        self.imp().katago_row.get()
    }

    pub fn katago_slot(&self) -> gtk::Box {
        self.imp().katago_slot.get()
    }

    pub fn katago_button(&self) -> gtk::Button {
        self.imp().katago_button.get()
    }

    pub fn model_row(&self) -> adw::ActionRow {
        self.imp().model_row.get()
    }

    pub fn model_slot(&self) -> gtk::Box {
        self.imp().model_slot.get()
    }

    pub fn model_button(&self) -> gtk::Button {
        self.imp().model_button.get()
    }

    pub fn mode_row(&self) -> adw::ComboRow {
        self.imp().mode_row.get()
    }

    pub fn config_row(&self) -> adw::ActionRow {
        self.imp().config_row.get()
    }

    pub fn config_slot(&self) -> gtk::Box {
        self.imp().config_slot.get()
    }

    pub fn config_button(&self) -> gtk::Button {
        self.imp().config_button.get()
    }

    pub fn analysis_row(&self) -> adw::SpinRow {
        self.imp().analysis_row.get()
    }

    pub fn search_row(&self) -> adw::SpinRow {
        self.imp().search_row.get()
    }

    pub fn memory_group(&self) -> adw::PreferencesGroup {
        self.imp().memory_group.get()
    }

    pub fn batch_row(&self) -> adw::SpinRow {
        self.imp().batch_row.get()
    }

    pub fn cache_row(&self) -> adw::SpinRow {
        self.imp().cache_row.get()
    }

    pub fn tuning_group(&self) -> adw::PreferencesGroup {
        self.imp().tuning_group.get()
    }

    pub fn tune_row(&self) -> adw::ActionRow {
        self.imp().tune_row.get()
    }

    pub fn tune_button(&self) -> gtk::Button {
        self.imp().tune_button.get()
    }
}

impl RemoteProfileForm {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn name_row(&self) -> adw::EntryRow {
        self.imp().name_row.get()
    }

    pub fn url_row(&self) -> adw::EntryRow {
        self.imp().url_row.get()
    }

    pub fn token_row(&self) -> adw::PasswordEntryRow {
        self.imp().token_row.get()
    }

    pub fn engine_row(&self) -> adw::EntryRow {
        self.imp().engine_row.get()
    }

    pub fn trust_row(&self) -> adw::ActionRow {
        self.imp().trust_row.get()
    }

    pub fn test_button(&self) -> gtk::Button {
        self.imp().test_button.get()
    }
}
