// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::Cell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};

use mirai_core::{Color, RuleSet, Size, TimeControl};

use crate::app::AppState;
use crate::config::{PlaySettings, StrengthSetting};
use crate::play::{GameSetup, Strength};

const HUMAN_LIKE: &str = "Human-like";
const SIZE_CHOICES: [u8; 3] = [9, 13, 19];

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/new_game.blp")]
    pub struct NewGameDialog {
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub start_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub size_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub custom_size_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub handicap_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub komi_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub rules_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub players_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub colour_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub time_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub main_time_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub periods_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub period_seconds_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub increment_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub strength_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub strength_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub visits_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub seconds_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub profile_row: TemplateChild<adw::EntryRow>,
        pub syncing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NewGameDialog {
        const NAME: &'static str = "MiraiNewGameDialog";

        type Type = super::NewGameDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for NewGameDialog {}
    impl WidgetImpl for NewGameDialog {}
    impl AdwDialogImpl for NewGameDialog {}
}

glib::wrapper! {
    pub struct NewGameDialog(ObjectSubclass<imp::NewGameDialog>)
        @extends gtk::Widget, adw::Dialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::ShortcutManager;
}

impl NewGameDialog {
    fn new(play: &PlaySettings, has_human_model: bool) -> Self {
        let dialog: Self = glib::Object::new();
        dialog.configure(play, has_human_model);
        dialog.connect_dynamic_rows();
        dialog
    }

    fn configure(&self, play: &PlaySettings, has_human_model: bool) {
        let imp = self.imp();

        set_items(&imp.size_row, &["9 × 9", "13 × 13", "19 × 19", "Custom"]);
        imp.size_row.set_selected(2);
        configure_spin(&imp.custom_size_row, 2.0, 19.0, 1.0, 0, 19.0);

        set_items(
            &imp.handicap_row,
            &[
                "None", "2 stones", "3 stones", "4 stones", "5 stones", "6 stones", "7 stones",
                "8 stones", "9 stones",
            ],
        );
        configure_spin(
            &imp.komi_row,
            -150.0,
            150.0,
            0.5,
            1,
            play.rules.default_komi() as f64,
        );

        let labels: Vec<&str> = RuleSet::ALL.iter().map(|rules| rules.label()).collect();
        set_items(&imp.rules_row, &labels);
        imp.rules_row.set_selected(
            RuleSet::ALL
                .iter()
                .position(|rules| *rules == play.rules)
                .unwrap_or(1) as u32,
        );

        set_items(&imp.colour_row, &["Black", "White", "Both (no engine)"]);
        set_items(
            &imp.time_row,
            &["None", "Absolute", "Byo-yomi", "Fischer increment"],
        );
        configure_spin(&imp.main_time_row, 0.0, 600.0, 1.0, 0, 20.0);
        configure_spin(&imp.periods_row, 1.0, 25.0, 1.0, 0, 5.0);
        configure_spin(&imp.period_seconds_row, 1.0, 600.0, 5.0, 0, 30.0);
        configure_spin(&imp.increment_row, 0.0, 600.0, 1.0, 0, 10.0);

        imp.strength_group.set_description(Some(if has_human_model {
            "The human-like model imitates a rank instead of searching"
        } else {
            "This engine has no human-like model loaded"
        }));
        set_items(&imp.strength_row, &["Visits", "Time per move", HUMAN_LIKE]);
        grey_out_human(&imp.strength_row, has_human_model);
        configure_spin(&imp.visits_row, 1.0, 1_000_000.0, 100.0, 0, 800.0);
        configure_spin(&imp.seconds_row, 0.1, 600.0, 0.5, 1, 5.0);
        imp.profile_row.set_text(crate::play::DEFAULT_HUMAN_PROFILE);

        let saved_mode = match &play.strength {
            StrengthSetting::Visits { visits } => {
                imp.visits_row.set_value(*visits as f64);
                0
            }
            StrengthSetting::Time { time_ms } => {
                imp.seconds_row.set_value(*time_ms as f64 / 1000.0);
                1
            }
            StrengthSetting::Human { profile } => {
                imp.profile_row.set_text(profile);
                u32::from(has_human_model) * 2
            }
        };
        imp.strength_row.set_selected(saved_mode);
    }

    fn connect_dynamic_rows(&self) {
        let imp = self.imp();
        imp.size_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| dialog
                .imp()
                .custom_size_row
                .set_visible(row.selected() == SIZE_CHOICES.len() as u32)
        ));
        imp.rules_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.sync_komi()
        ));
        imp.handicap_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.sync_komi()
        ));
        self.refresh_time(imp.time_row.selected());
        imp.time_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| dialog.refresh_time(row.selected())
        ));
        self.refresh_strength(imp.strength_row.selected());
        imp.strength_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| dialog.refresh_strength(row.selected())
        ));
        self.refresh_players();
        imp.colour_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.refresh_players()
        ));
    }

    fn sync_komi(&self) {
        let imp = self.imp();
        if imp.syncing.replace(true) {
            return;
        }
        let value = if imp.handicap_row.selected() > 0 {
            0.5
        } else {
            rules_at(imp.rules_row.selected()).default_komi() as f64
        };
        imp.komi_row.set_value(value);
        imp.syncing.set(false);
    }

    fn refresh_time(&self, kind: u32) {
        let imp = self.imp();
        imp.main_time_row.set_visible(kind != 0);
        imp.periods_row.set_visible(kind == 2);
        imp.period_seconds_row.set_visible(kind == 2);
        imp.increment_row.set_visible(kind == 3);
    }

    fn refresh_strength(&self, kind: u32) {
        let imp = self.imp();
        imp.visits_row.set_visible(kind == 0);
        imp.seconds_row.set_visible(kind == 1);
        imp.profile_row.set_visible(kind == 2);
    }

    fn refresh_players(&self) {
        let imp = self.imp();
        let both = imp.colour_row.selected() == 2;
        imp.strength_group.set_visible(!both);
        imp.players_group.set_description(Some(if both {
            "Play both sides on this device"
        } else {
            "The engine takes the other colour"
        }));
    }

    fn setup(&self) -> GameSetup {
        let imp = self.imp();
        let size = match imp.size_row.selected() {
            index if (index as usize) < SIZE_CHOICES.len() => {
                Size::square(SIZE_CHOICES[index as usize])
            }
            _ => Size::new(
                imp.custom_size_row.value() as u8,
                imp.custom_size_row.value() as u8,
            )
            .unwrap_or(Size::square(19)),
        };
        let handicap = match imp.handicap_row.selected() {
            0 => 0,
            count => (count + 1) as u8,
        };
        let human = match imp.colour_row.selected() {
            0 => Some(Color::Black),
            1 => Some(Color::White),
            _ => None,
        };
        let tc = match imp.time_row.selected() {
            1 => TimeControl {
                main_s: (imp.main_time_row.value() * 60.0) as u32,
                ..TimeControl::UNLIMITED
            },
            2 => TimeControl {
                main_s: (imp.main_time_row.value() * 60.0) as u32,
                byo_periods: imp.periods_row.value() as u8,
                byo_period_s: imp.period_seconds_row.value() as u32,
                increment_s: 0,
            },
            3 => TimeControl {
                main_s: (imp.main_time_row.value() * 60.0) as u32,
                byo_periods: 0,
                byo_period_s: 0,
                increment_s: imp.increment_row.value() as u32,
            },
            _ => TimeControl::UNLIMITED,
        };
        let strength = match imp.strength_row.selected() {
            1 => Strength::TimeMs((imp.seconds_row.value() * 1000.0) as u32),
            2 => Strength::Human {
                profile: imp.profile_row.text().to_string(),
            },
            _ => Strength::Visits(imp.visits_row.value() as u32),
        };

        GameSetup {
            size,
            rules: rules_at(imp.rules_row.selected()),
            komi: imp.komi_row.value() as f32,
            handicap,
            human,
            tc,
            strength,
        }
    }
}

pub fn present(
    parent: &impl IsA<gtk::Widget>,
    state: &AppState,
    on_start: impl Fn(GameSetup) + 'static,
) {
    let play = state.config().play.clone();
    let has_human_model = state.engine_desc().is_some_and(|desc| desc.has_human_model);
    let dialog = NewGameDialog::new(&play, has_human_model);

    let close_dialog = dialog.clone();
    dialog.imp().cancel_button.connect_clicked(move |_| {
        close_dialog.close();
    });

    let start_dialog = dialog.clone();
    let start_state = state.clone();
    dialog.imp().start_button.connect_clicked(move |_| {
        let setup = start_dialog.setup();
        {
            let mut config = start_state.config_mut();
            config.play.rules = setup.rules;
            config.play.strength = match &setup.strength {
                Strength::Visits(visits) => StrengthSetting::Visits { visits: *visits },
                Strength::TimeMs(time_ms) => StrengthSetting::Time { time_ms: *time_ms },
                Strength::Human { profile } => StrengthSetting::Human {
                    profile: profile.clone(),
                },
            };
        }
        start_state.save_config();
        start_dialog.close();
        on_start(setup);
    });

    dialog.present(Some(parent));
}

fn set_items(row: &adw::ComboRow, items: &[&str]) {
    row.set_model(Some(&gtk::StringList::new(items)));
}

fn configure_spin(row: &adw::SpinRow, min: f64, max: f64, step: f64, digits: u32, value: f64) {
    row.configure(
        Some(&gtk::Adjustment::new(
            value,
            min,
            max,
            step,
            step * 10.0,
            0.0,
        )),
        0.0,
        digits,
    );
}

fn rules_at(index: u32) -> RuleSet {
    RuleSet::ALL
        .get(index as usize)
        .copied()
        .unwrap_or(RuleSet::Chinese)
}

fn grey_out_human(row: &adw::ComboRow, enabled: bool) {
    if enabled {
        return;
    }
    let make_factory = |list: bool| {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
        });
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };
            let text = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|item| item.string().to_string())
                .unwrap_or_default();
            label.set_label(&text);
            let usable = text != HUMAN_LIKE;
            label.set_sensitive(usable);
            if list {
                item.set_selectable(usable);
                item.set_activatable(usable);
            }
        });
        factory
    };
    row.set_factory(Some(&make_factory(false)));
    row.set_list_factory(Some(&make_factory(true)));
}
