// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/preferences.blp")]
    pub struct PreferencesDialog {
        #[template_child]
        pub profiles_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub add_local_button: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub add_remote_button: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub analysis_visits_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub analysis_interval_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub analysis_suggestions_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub analysis_batch_visits_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub analysis_auto_open_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub analysis_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_strength_kind_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub play_visits_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub play_seconds_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub play_human_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub play_temperature_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub play_threshold_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub play_streak_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub play_rules_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub play_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub show_coordinates_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub show_move_numbers_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub overlay_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub save_analysis_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub appearance_reset_button: TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "MiraiPreferencesDialog";

        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreferencesDialog {}
    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends gtk::Widget, adw::Dialog, adw::PreferencesDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::ShortcutManager;
}

impl PreferencesDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn widgets(&self) -> PreferencesWidgets {
        let imp = self.imp();
        PreferencesWidgets {
            profiles_group: imp.profiles_group.get(),
            add_local_button: imp.add_local_button.get(),
            add_remote_button: imp.add_remote_button.get(),
            analysis_visits_row: imp.analysis_visits_row.get(),
            analysis_interval_row: imp.analysis_interval_row.get(),
            analysis_suggestions_row: imp.analysis_suggestions_row.get(),
            analysis_batch_visits_row: imp.analysis_batch_visits_row.get(),
            analysis_auto_open_row: imp.analysis_auto_open_row.get(),
            analysis_reset_button: imp.analysis_reset_button.get(),
            play_strength_kind_row: imp.play_strength_kind_row.get(),
            play_visits_row: imp.play_visits_row.get(),
            play_seconds_row: imp.play_seconds_row.get(),
            play_human_row: imp.play_human_row.get(),
            play_temperature_row: imp.play_temperature_row.get(),
            play_threshold_row: imp.play_threshold_row.get(),
            play_streak_row: imp.play_streak_row.get(),
            play_rules_row: imp.play_rules_row.get(),
            play_reset_button: imp.play_reset_button.get(),
            show_coordinates_row: imp.show_coordinates_row.get(),
            show_move_numbers_row: imp.show_move_numbers_row.get(),
            overlay_row: imp.overlay_row.get(),
            save_analysis_row: imp.save_analysis_row.get(),
            appearance_reset_button: imp.appearance_reset_button.get(),
        }
    }
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PreferencesWidgets {
    pub profiles_group: adw::PreferencesGroup,
    pub add_local_button: adw::ButtonRow,
    pub add_remote_button: adw::ButtonRow,
    pub analysis_visits_row: adw::SpinRow,
    pub analysis_interval_row: adw::SpinRow,
    pub analysis_suggestions_row: adw::SpinRow,
    pub analysis_batch_visits_row: adw::SpinRow,
    pub analysis_auto_open_row: adw::SwitchRow,
    pub analysis_reset_button: gtk::Button,
    pub play_strength_kind_row: adw::ComboRow,
    pub play_visits_row: adw::SpinRow,
    pub play_seconds_row: adw::SpinRow,
    pub play_human_row: adw::EntryRow,
    pub play_temperature_row: adw::SpinRow,
    pub play_threshold_row: adw::SpinRow,
    pub play_streak_row: adw::SpinRow,
    pub play_rules_row: adw::ComboRow,
    pub play_reset_button: gtk::Button,
    pub show_coordinates_row: adw::SwitchRow,
    pub show_move_numbers_row: adw::SwitchRow,
    pub overlay_row: adw::ComboRow,
    pub save_analysis_row: adw::SwitchRow,
    pub appearance_reset_button: gtk::Button,
}
