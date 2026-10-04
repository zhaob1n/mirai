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

    pub fn widgets(&self) -> LocalFormWidgets {
        let imp = self.imp();
        LocalFormWidgets {
            name_row: imp.name_row.get(),
            katago_row: imp.katago_row.get(),
            katago_button: imp.katago_button.get(),
            model_row: imp.model_row.get(),
            model_slot: imp.model_slot.get(),
            model_button: imp.model_button.get(),
            mode_row: imp.mode_row.get(),
            config_row: imp.config_row.get(),
            config_slot: imp.config_slot.get(),
            config_button: imp.config_button.get(),
            analysis_row: imp.analysis_row.get(),
            search_row: imp.search_row.get(),
            memory_group: imp.memory_group.get(),
            batch_row: imp.batch_row.get(),
            cache_row: imp.cache_row.get(),
            tuning_group: imp.tuning_group.get(),
            tune_row: imp.tune_row.get(),
            tune_button: imp.tune_button.get(),
        }
    }
}

/// The children of `local_profile_form.blp` that the editor fills in and wires.
pub struct LocalFormWidgets {
    pub name_row: adw::EntryRow,
    pub katago_row: adw::ActionRow,
    pub katago_button: gtk::Button,
    pub model_row: adw::ActionRow,
    pub model_slot: gtk::Box,
    pub model_button: gtk::Button,
    pub mode_row: adw::ComboRow,
    pub config_row: adw::ActionRow,
    pub config_slot: gtk::Box,
    pub config_button: gtk::Button,
    pub analysis_row: adw::SpinRow,
    pub search_row: adw::SpinRow,
    pub memory_group: adw::PreferencesGroup,
    pub batch_row: adw::SpinRow,
    pub cache_row: adw::SpinRow,
    pub tuning_group: adw::PreferencesGroup,
    pub tune_row: adw::ActionRow,
    pub tune_button: gtk::Button,
}

impl RemoteProfileForm {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn widgets(&self) -> RemoteFormWidgets {
        let imp = self.imp();
        RemoteFormWidgets {
            name_row: imp.name_row.get(),
            url_row: imp.url_row.get(),
            token_row: imp.token_row.get(),
            engine_row: imp.engine_row.get(),
            trust_row: imp.trust_row.get(),
            test_button: imp.test_button.get(),
        }
    }
}

/// The children of `remote_profile_form.blp` that the editor fills in and wires.
pub struct RemoteFormWidgets {
    pub name_row: adw::EntryRow,
    pub url_row: adw::EntryRow,
    pub token_row: adw::PasswordEntryRow,
    pub engine_row: adw::EntryRow,
    pub trust_row: adw::ActionRow,
    pub test_button: gtk::Button,
}
