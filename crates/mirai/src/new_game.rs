// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, glib};

use mirai_core::{Color, RuleSet, Size, TimeControl, fixed_handicap};

use crate::app::AppState;
use crate::config::{
    DEFAULT_SECONDS_PER_MOVE, DEFAULT_VISITS_PER_MOVE, MAX_SECONDS_PER_MOVE, MAX_VISITS_PER_MOVE,
    MIN_SECONDS_PER_MOVE, PlaySettings, StrengthSetting,
};
use crate::i18n;
use crate::play::{GameSetup, Strength};

const SIZE_CHOICES: [u8; 3] = [9, 13, 19];
// Where the clock rows start: 20 minutes, then five 30-second periods or 10 seconds a move.
const MAIN_MINUTES: f64 = 20.0;
const PERIODS: f64 = 5.0;
const PERIOD_SECONDS: f64 = 30.0;
const INCREMENT_SECONDS: f64 = 10.0;

/// What Start Game hands the window: set again at each presentation.
type StartHandler = Rc<dyn Fn(GameSetup)>;

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
        pub page: TemplateChild<adw::PreferencesPage>,
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
        pub coerced_strength: Cell<bool>,
        /// Whether the strength row was built for an engine with a human-like model.
        pub human_model: Cell<bool>,
        /// Presented and not yet closing.
        pub shown: Cell<bool>,
        pub state: glib::WeakRef<AppState>,
        pub on_start: RefCell<Option<StartHandler>>,
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

    impl ObjectImpl for NewGameDialog {
        fn dispose(&self) {
            self.on_start.take();
        }
    }
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
    fn new(has_human_model: bool) -> Self {
        let dialog: Self = glib::Object::new();
        dialog.build(has_human_model);
        dialog.connect_dynamic_rows();
        dialog.connect_buttons();
        dialog
    }

    /// What a reopen keeps: the choices each row offers, the spin ranges and steps, and
    /// whether Human-like is on offer.
    fn build(&self, has_human_model: bool) {
        let imp = self.imp();
        imp.human_model.set(has_human_model);

        let custom = i18n::pgettext("size", "Custom");
        set_items(
            &imp.size_row,
            &["9 × 9", "13 × 13", "19 × 19", custom.as_str()],
        );
        configure_spin(&imp.custom_size_row, 2.0, 19.0, 1.0, 0, 19.0);

        let handicap = handicap_labels();
        set_owned(&imp.handicap_row, &handicap);

        configure_spin(&imp.komi_row, -150.0, 150.0, 0.5, 1, 0.0);
        // KataGo accepts only an integer or half-integer komi. Snap arrow clicks
        // and focus-out to that grid; `setup` still rounds a value that has not
        // been committed yet.
        imp.komi_row.set_numeric(true);
        imp.komi_row.set_snap_to_ticks(true);

        let labels: Vec<String> = RuleSet::ALL
            .iter()
            .copied()
            .map(i18n::rules_label)
            .collect();
        set_owned(&imp.rules_row, &labels);

        let colours = colour_labels();
        set_owned(&imp.colour_row, &colours);
        let times = time_labels();
        set_owned(&imp.time_row, &times);
        configure_spin(&imp.main_time_row, 0.0, 600.0, 1.0, 0, MAIN_MINUTES);
        configure_spin(&imp.periods_row, 1.0, 25.0, 1.0, 0, PERIODS);
        configure_spin(&imp.period_seconds_row, 1.0, 600.0, 5.0, 0, PERIOD_SECONDS);
        configure_spin(&imp.increment_row, 0.0, 600.0, 1.0, 0, INCREMENT_SECONDS);

        let strength_description = if has_human_model {
            i18n::gettext("The human-like model imitates a rank instead of searching")
        } else {
            i18n::gettext("This engine has no human-like model loaded")
        };
        imp.strength_group
            .set_description(Some(&strength_description));
        let strengths = strength_labels();
        set_owned(&imp.strength_row, &strengths);
        grey_out_human(&imp.strength_row, has_human_model);
        configure_spin(
            &imp.visits_row,
            1.0,
            MAX_VISITS_PER_MOVE,
            100.0,
            0,
            f64::from(DEFAULT_VISITS_PER_MOVE),
        );
        configure_spin(
            &imp.seconds_row,
            MIN_SECONDS_PER_MOVE,
            MAX_SECONDS_PER_MOVE,
            0.5,
            1,
            DEFAULT_SECONDS_PER_MOVE,
        );
    }

    /// Puts every row where a new game starts: board, handicap, colour and clock at their
    /// defaults, rules and strength as last saved. Rows already there are left alone, since
    /// setting a spin row's value again formats its text and relays it out.
    fn reset(&self, play: &PlaySettings) {
        let imp = self.imp();
        let has_human_model = imp.human_model.get();
        imp.size_row.set_selected(2);
        set_spin(&imp.custom_size_row, 19.0);
        imp.handicap_row.set_selected(0);
        imp.rules_row.set_selected(
            RuleSet::ALL
                .iter()
                .position(|rules| *rules == play.rules)
                .unwrap_or(1) as u32,
        );
        // Changed rules re-derive the komi through `sync_komi`; unchanged ones would leave
        // a komi edited last time.
        set_spin(&imp.komi_row, play.rules.default_komi() as f64);
        imp.colour_row.set_selected(0);
        imp.time_row.set_selected(0);
        set_spin(&imp.main_time_row, MAIN_MINUTES);
        set_spin(&imp.periods_row, PERIODS);
        set_spin(&imp.period_seconds_row, PERIOD_SECONDS);
        set_spin(&imp.increment_row, INCREMENT_SECONDS);

        let (visits, seconds, profile, saved_mode) = match &play.strength {
            StrengthSetting::Visits { visits } => (
                *visits as f64,
                DEFAULT_SECONDS_PER_MOVE,
                crate::play::DEFAULT_HUMAN_PROFILE,
                0,
            ),
            StrengthSetting::Time { time_ms } => (
                f64::from(DEFAULT_VISITS_PER_MOVE),
                *time_ms as f64 / 1000.0,
                crate::play::DEFAULT_HUMAN_PROFILE,
                1,
            ),
            StrengthSetting::Human { profile } => (
                f64::from(DEFAULT_VISITS_PER_MOVE),
                DEFAULT_SECONDS_PER_MOVE,
                profile.as_str(),
                u32::from(has_human_model) * 2,
            ),
        };
        set_spin(&imp.visits_row, visits);
        set_spin(&imp.seconds_row, seconds);
        if imp.profile_row.text() != profile {
            imp.profile_row.set_text(profile);
        }
        imp.strength_row.set_selected(saved_mode);
        // Last: the strength handlers above clear it.
        imp.coerced_strength
            .set(matches!(play.strength, StrengthSetting::Human { .. }) && !has_human_model);

        self.refresh_handicap();
        self.refresh_time(imp.time_row.selected());
        self.refresh_strength(imp.strength_row.selected());
        self.refresh_players();
        imp.page.scroll_to_top();
    }

    fn connect_buttons(&self) {
        let imp = self.imp();
        imp.cancel_button.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        imp.start_button.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let imp = dialog.imp();
                let Some(state) = imp.state.upgrade() else {
                    return;
                };
                let setup = dialog.setup();
                {
                    let mut config = state.config_mut();
                    config.play.rules = setup.rules;
                    config.play.strength = strength_to_save(
                        &config.play.strength,
                        &setup.strength,
                        imp.coerced_strength.get(),
                    );
                }
                state.save_config();
                dialog.close();
                let on_start = imp.on_start.borrow().clone();
                if let Some(on_start) = on_start {
                    on_start(setup);
                }
            }
        ));
        self.connect_closed(|dialog| dialog.imp().shown.set(false));
    }

    fn connect_dynamic_rows(&self) {
        let imp = self.imp();
        imp.size_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| {
                dialog
                    .imp()
                    .custom_size_row
                    .set_visible(row.selected() == SIZE_CHOICES.len() as u32);
                dialog.refresh_handicap();
            }
        ));
        imp.custom_size_row.connect_value_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.refresh_handicap()
        ));
        imp.rules_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.sync_komi()
        ));
        imp.handicap_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.sync_komi()
        ));
        self.refresh_time(imp.time_row.selected());
        imp.time_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| dialog.refresh_time(row.selected())
        ));
        self.refresh_strength(imp.strength_row.selected());
        imp.strength_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| {
                dialog.imp().coerced_strength.set(false);
                dialog.refresh_strength(row.selected());
            }
        ));
        imp.visits_row.connect_value_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.imp().coerced_strength.set(false)
        ));
        self.refresh_players();
        imp.colour_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.refresh_players()
        ));
    }

    fn selected_size(&self) -> Size {
        let imp = self.imp();
        match imp.size_row.selected() {
            index if (index as usize) < SIZE_CHOICES.len() => {
                Size::square(SIZE_CHOICES[index as usize])
            }
            _ => Size::square(imp.custom_size_row.value() as u8),
        }
    }

    /// Handicap stones exist only on odd square boards of 7 and up. Anywhere else the
    /// row is insensitive at None rather than accepting a count that places nothing.
    fn refresh_handicap(&self) {
        let imp = self.imp();
        let takes_handicap = takes_handicap(self.selected_size());
        imp.handicap_row.set_sensitive(takes_handicap);
        // Unselecting notifies the handicap row, which re-derives komi. A size change
        // that keeps the handicap leaves a komi override alone.
        if !takes_handicap {
            imp.handicap_row.set_selected(0);
        }
    }

    fn sync_komi(&self) {
        let imp = self.imp();
        if imp.syncing.replace(true) {
            return;
        }
        let value = if selected_handicap(self.selected_size(), imp.handicap_row.selected()) > 0 {
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
        let description = if both {
            i18n::gettext("Play both sides on this device")
        } else {
            i18n::gettext("The engine takes the other colour")
        };
        imp.players_group.set_description(Some(&description));
    }

    fn setup(&self) -> GameSetup {
        let imp = self.imp();
        // Commit typed text. Enter on Start does not always focus-out the row first,
        // and an uncommitted SpinRow still reports its previous value.
        for row in [
            &imp.komi_row,
            &imp.custom_size_row,
            &imp.main_time_row,
            &imp.periods_row,
            &imp.period_seconds_row,
            &imp.increment_row,
            &imp.visits_row,
            &imp.seconds_row,
        ] {
            row.update();
        }
        let size = self.selected_size();
        let handicap = selected_handicap(size, imp.handicap_row.selected());
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
            komi: komi_points(imp.komi_row.value()),
            handicap,
            human,
            tc,
            strength,
        }
    }
}

/// Presents `slot`'s New Game dialog over `parent`, building it on first use.
///
/// The dialog is kept for the window's life: building its template and presenting it
/// cost 20–55 ms of the GTK thread on every open, presenting a built one a few. Each
/// presentation resets it to where a new game starts, as a fresh one would be.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    slot: &RefCell<Option<NewGameDialog>>,
    state: &AppState,
    on_start: impl Fn(GameSetup) + 'static,
) {
    let play = state.config().play.clone();
    let has_human_model = state.engine_desc().is_some_and(|desc| desc.has_human_model);
    let dialog = {
        let mut slot = slot.borrow_mut();
        // Already up: resetting would throw away what the user is choosing.
        if slot.as_ref().is_some_and(|dialog| dialog.imp().shown.get()) {
            return;
        }
        // Greying out Human-like replaces the strength row's factories, which cannot be
        // handed back; a dialog built for the other engine is built again.
        if slot
            .as_ref()
            .is_some_and(|dialog| dialog.imp().human_model.get() != has_human_model)
        {
            *slot = None;
        }
        slot.get_or_insert_with(|| NewGameDialog::new(has_human_model))
            .clone()
    };
    let imp = dialog.imp();
    imp.state.set(Some(state));
    imp.on_start.replace(Some(Rc::new(on_start)));
    dialog.reset(&play);
    imp.shown.set(true);
    dialog.present(Some(parent));
}

fn strength_to_save(
    saved: &StrengthSetting,
    selected: &Strength,
    coerced: bool,
) -> StrengthSetting {
    if coerced {
        return saved.clone();
    }
    match selected {
        Strength::Visits(visits) => StrengthSetting::Visits { visits: *visits },
        Strength::TimeMs(time_ms) => StrengthSetting::Time { time_ms: *time_ms },
        Strength::Human { profile } => StrengthSetting::Human {
            profile: profile.clone(),
        },
    }
}

fn set_items(row: &adw::ComboRow, items: &[&str]) {
    row.set_model(Some(&gtk::StringList::new(items)));
}

fn set_owned(row: &adw::ComboRow, items: &[String]) {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    set_items(row, &refs);
}

fn handicap_labels() -> Vec<String> {
    let mut items = Vec::with_capacity(9);
    // Translators: no handicap stones.
    items.push(i18n::pgettext("handicap", "None"));
    for stones in 2u64..=9 {
        let n = stones.to_string();
        items.push(i18n::ngettext_f(
            "{n} stone",
            "{n} stones",
            stones,
            &[("n", n.as_str())],
        ));
    }
    items
}

fn colour_labels() -> Vec<String> {
    vec![
        i18n::color_name(Color::Black),
        i18n::color_name(Color::White),
        i18n::gettext("Both (no engine)"),
    ]
}

fn time_labels() -> Vec<String> {
    vec![
        // Translators: no time limit.
        i18n::pgettext("time-control", "None"),
        // Translators: one clock and no overtime.
        i18n::pgettext("time-control", "Absolute"),
        // Translators: Japanese overtime, a fixed number of periods.
        i18n::gettext("Byo-yomi"),
        // Translators: seconds added after every move.
        i18n::gettext("Fischer increment"),
    ]
}

fn strength_labels() -> Vec<String> {
    vec![
        i18n::pgettext("strength", "Visits"),
        i18n::gettext("Time per move"),
        i18n::gettext("Human-like"),
    ]
}

/// Nearest half-point. The wire stores `komi * 2` as an integer, so 7.3 would
/// otherwise be written into the record and silently become 7.5 in analysis.
fn komi_points(value: f64) -> f32 {
    ((value * 2.0).round() / 2.0) as f32
}

/// Sets a spin row unless it already shows `value`: setting an equal one formats the
/// row's text again and relays it out.
fn set_spin(row: &adw::SpinRow, value: f64) {
    if row.value() != value {
        row.set_value(value);
    }
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

fn takes_handicap(size: Size) -> bool {
    !fixed_handicap(size, 2).is_empty()
}

/// Stones for combo `selected` (`0` = None, then 2..=9) on `size`.
fn selected_handicap(size: Size, selected: u32) -> u8 {
    if selected == 0 || !takes_handicap(size) {
        return 0;
    }
    (selected + 1) as u8
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
            let usable = text != i18n::gettext("Human-like");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_human_model_does_not_replace_the_saved_profile() {
        let saved = StrengthSetting::Human {
            profile: "rank_3d".into(),
        };
        let fallback = Strength::Visits(DEFAULT_VISITS_PER_MOVE);
        assert_eq!(strength_to_save(&saved, &fallback, true), saved);
        assert_eq!(
            strength_to_save(&saved, &Strength::Visits(250), false),
            StrengthSetting::Visits { visits: 250 }
        );
    }

    #[test]
    fn a_board_without_handicap_points_takes_no_stones() {
        for side in [2, 5, 6, 8, 10, 18] {
            let size = Size::square(side);
            assert_eq!(selected_handicap(size, 1), 0, "{side}x{side}");
            assert_eq!(selected_handicap(size, 8), 0, "{side}x{side}");
        }
        for side in [7, 9, 13, 19] {
            let size = Size::square(side);
            for selected in 1..=8 {
                let stones = selected_handicap(size, selected);
                assert_eq!(fixed_handicap(size, stones).len(), usize::from(stones));
            }
            assert_eq!(selected_handicap(size, 0), 0);
        }
    }

    #[test]
    fn komi_is_stored_as_a_half_point() {
        assert_eq!(komi_points(7.5), 7.5);
        assert_eq!(komi_points(7.0), 7.0);
        assert_eq!(komi_points(7.3), 7.5);
        assert_eq!(komi_points(7.2), 7.0);
        assert_eq!(komi_points(7.25), 7.5);
        assert_eq!(komi_points(0.0), 0.0);
        assert_eq!(komi_points(-0.2), 0.0);
        assert_eq!(komi_points(-0.3), -0.5);
    }
}
