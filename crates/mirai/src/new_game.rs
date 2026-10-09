// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::clone;

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

/// Harness ids are `GtkWidget:name`.
struct Widgets {
    cancel_button: gtk::Button,
    start_button: gtk::Button,
    page: adw::PreferencesPage,
    size_row: adw::ComboRow,
    custom_size_row: adw::SpinRow,
    handicap_row: adw::ComboRow,
    komi_row: adw::SpinRow,
    rules_row: adw::ComboRow,
    players_group: adw::PreferencesGroup,
    colour_row: adw::ComboRow,
    time_row: adw::ComboRow,
    main_time_row: adw::SpinRow,
    periods_row: adw::SpinRow,
    period_seconds_row: adw::SpinRow,
    increment_row: adw::SpinRow,
    strength_group: adw::PreferencesGroup,
    strength_row: adw::ComboRow,
    visits_row: adw::SpinRow,
    seconds_row: adw::SpinRow,
    profile_row: adw::EntryRow,
}

mod imp {
    use super::*;

    pub struct NewGameDialog {
        toolbar: adw::ToolbarView,
        pub(super) widgets: super::Widgets,
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

        fn new() -> Self {
            let (toolbar, widgets) = super::Widgets::build();
            Self {
                toolbar,
                widgets,
                syncing: Cell::new(false),
                coerced_strength: Cell::new(false),
                human_model: Cell::new(false),
                shown: Cell::new(false),
                state: glib::WeakRef::default(),
                on_start: RefCell::new(None),
            }
        }
    }

    impl ObjectImpl for NewGameDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let dialog = self.obj();
            dialog.set_title(&i18n::gettext("New Game"));
            dialog.set_content_width(460);
            dialog.set_content_height(620);
            dialog.set_child(Some(&self.toolbar));
            dialog.set_default_widget(Some(&self.widgets.start_button));
        }

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
        let imp = dialog.imp();
        imp.human_model.set(has_human_model);
        imp.widgets.offer_human(has_human_model);
        imp.widgets.connect(&dialog);
        dialog.connect_closed(|dialog| dialog.imp().shown.set(false));
        crate::widgets::sheet_texture::install(&dialog);
        dialog
    }

    /// Puts every row where a new game starts: board, handicap, colour and clock at their
    /// defaults, rules and strength as last saved. Rows already there are left alone, since
    /// setting a spin row's value again formats its text and relays it out.
    fn reset(&self, play: &PlaySettings) {
        let imp = self.imp();
        // Strength handlers clear the flag; set it after they have run.
        imp.coerced_strength
            .set(imp.widgets.reset(play, imp.human_model.get()));
    }

    fn sync_komi(&self) {
        let imp = self.imp();
        if imp.syncing.replace(true) {
            return;
        }
        imp.widgets.apply_komi();
        imp.syncing.set(false);
    }

    fn start(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.upgrade() else {
            return;
        };
        let setup = imp.widgets.setup();
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
        self.close();
        let on_start = imp.on_start.borrow().clone();
        if let Some(on_start) = on_start {
            on_start(setup);
        }
    }
}

impl Widgets {
    /// Choices and ranges a reopen keeps. Human-like is applied in `offer_human`, once the
    /// engine for this presentation is known.
    fn build() -> (adw::ToolbarView, Self) {
        let cancel_button = gtk::Button::builder()
            .name("cancel_button")
            .label(i18n::gettext("Cancel"))
            .build();
        let start_button = gtk::Button::builder()
            .name("start_button")
            .label(i18n::gettext("Start Game"))
            .build();
        start_button.add_css_class("suggested-action");

        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel_button);
        header.pack_end(&start_button);

        let custom = i18n::pgettext("size", "Custom");
        let size_row = adw::ComboRow::builder()
            .name("size_row")
            .title(i18n::gettext("Size"))
            .model(&gtk::StringList::new(&[
                "9 × 9",
                "13 × 13",
                "19 × 19",
                custom.as_str(),
            ]))
            .build();
        let custom_size_row = adw::SpinRow::builder()
            .name("custom_size_row")
            .title(i18n::gettext("Custom Size"))
            .visible(false)
            .adjustment(&adjustment(19.0, 2.0, 19.0, 1.0))
            .digits(0)
            .climb_rate(0.0)
            .build();
        let handicap = handicap_labels();
        let handicap_row = adw::ComboRow::builder()
            .name("handicap_row")
            .title(i18n::gettext("Handicap"))
            .model(&string_list(&handicap))
            .build();
        let komi_row = adw::SpinRow::builder()
            .name("komi_row")
            .title(
                // Translators: compensation points given to White.
                i18n::gettext("Komi"),
            )
            .adjustment(&adjustment(0.0, -150.0, 150.0, 0.5))
            .digits(1)
            .climb_rate(0.0)
            // KataGo accepts only an integer or half-integer komi. Snap arrow clicks
            // and focus-out to that grid; `setup` still rounds a value that has not
            // been committed yet.
            .numeric(true)
            .snap_to_ticks(true)
            .build();
        let rules = RuleSet::ALL
            .iter()
            .copied()
            .map(i18n::rules_label)
            .collect::<Vec<_>>();
        let rules_row = adw::ComboRow::builder()
            .name("rules_row")
            .title(i18n::gettext("Rules"))
            .model(&string_list(&rules))
            .build();
        let board = group(
            adw::PreferencesGroup::builder()
                .title(i18n::gettext("Board"))
                .build(),
            &[
                size_row.upcast_ref(),
                custom_size_row.upcast_ref(),
                handicap_row.upcast_ref(),
                komi_row.upcast_ref(),
                rules_row.upcast_ref(),
            ],
        );

        let colours = colour_labels();
        let colour_row = adw::ComboRow::builder()
            .name("colour_row")
            .title(
                // Translators: which colour the human plays.
                i18n::gettext("You Play"),
            )
            .model(&string_list(&colours))
            .build();
        let players_group = group(
            adw::PreferencesGroup::builder()
                .name("players_group")
                .title(i18n::gettext("Players"))
                .description(i18n::gettext("The engine takes the other colour"))
                .build(),
            &[colour_row.upcast_ref()],
        );

        let times = time_labels();
        let time_row = adw::ComboRow::builder()
            .name("time_row")
            .title(i18n::pgettext("time-control", "Type"))
            .model(&string_list(&times))
            .build();
        let main_time_row = adw::SpinRow::builder()
            .name("main_time_row")
            .title(i18n::gettext("Main Time (Minutes)"))
            .adjustment(&adjustment(MAIN_MINUTES, 0.0, 600.0, 1.0))
            .digits(0)
            .climb_rate(0.0)
            .build();
        let periods_row = adw::SpinRow::builder()
            .name("periods_row")
            .title(
                // Translators: Japanese overtime, a fixed number of periods.
                i18n::gettext("Byo-yomi Periods"),
            )
            .adjustment(&adjustment(PERIODS, 1.0, 25.0, 1.0))
            .digits(0)
            .climb_rate(0.0)
            .build();
        let period_seconds_row = adw::SpinRow::builder()
            .name("period_seconds_row")
            .title(i18n::gettext("Seconds per Period"))
            .adjustment(&adjustment(PERIOD_SECONDS, 1.0, 600.0, 5.0))
            .digits(0)
            .climb_rate(0.0)
            .build();
        let increment_row = adw::SpinRow::builder()
            .name("increment_row")
            .title(i18n::gettext("Increment (Seconds)"))
            .adjustment(&adjustment(INCREMENT_SECONDS, 0.0, 600.0, 1.0))
            .digits(0)
            .climb_rate(0.0)
            .build();
        let time = group(
            adw::PreferencesGroup::builder()
                .title(i18n::gettext("Time Control"))
                .build(),
            &[
                time_row.upcast_ref(),
                main_time_row.upcast_ref(),
                periods_row.upcast_ref(),
                period_seconds_row.upcast_ref(),
                increment_row.upcast_ref(),
            ],
        );

        let strengths = strength_labels();
        let strength_row = adw::ComboRow::builder()
            .name("strength_row")
            .title(i18n::pgettext("strength", "Mode"))
            .model(&string_list(&strengths))
            .build();
        let visits_row = adw::SpinRow::builder()
            .name("visits_row")
            .title(i18n::gettext("Visits per Move"))
            .adjustment(&adjustment(
                f64::from(DEFAULT_VISITS_PER_MOVE),
                1.0,
                MAX_VISITS_PER_MOVE,
                100.0,
            ))
            .digits(0)
            .climb_rate(0.0)
            .build();
        let seconds_row = adw::SpinRow::builder()
            .name("seconds_row")
            .title(i18n::gettext("Seconds per Move"))
            .adjustment(&adjustment(
                DEFAULT_SECONDS_PER_MOVE,
                MIN_SECONDS_PER_MOVE,
                MAX_SECONDS_PER_MOVE,
                0.5,
            ))
            .digits(1)
            .climb_rate(0.0)
            .build();
        let profile_row = adw::EntryRow::builder()
            .name("profile_row")
            .title(
                // Translators: a KataGo human-SL profile name, such as rank_5k.
                i18n::gettext("Human Model Profile"),
            )
            .build();
        let strength_group = group(
            adw::PreferencesGroup::builder()
                .name("strength_group")
                .title(i18n::gettext("Engine Strength"))
                .build(),
            &[
                strength_row.upcast_ref(),
                visits_row.upcast_ref(),
                seconds_row.upcast_ref(),
                profile_row.upcast_ref(),
            ],
        );

        let page = adw::PreferencesPage::builder()
            .title(i18n::gettext("New Game"))
            .build();
        page.set_widget_name("page");
        for group in [&board, &players_group, &time, &strength_group] {
            page.add(group);
        }
        let toolbar = adw::ToolbarView::builder().content(&page).build();
        toolbar.add_top_bar(&header);

        (
            toolbar,
            Self {
                cancel_button,
                start_button,
                page,
                size_row,
                custom_size_row,
                handicap_row,
                komi_row,
                rules_row,
                players_group,
                colour_row,
                time_row,
                main_time_row,
                periods_row,
                period_seconds_row,
                increment_row,
                strength_group,
                strength_row,
                visits_row,
                seconds_row,
                profile_row,
            },
        )
    }

    fn offer_human(&self, enabled: bool) {
        let description = if enabled {
            i18n::gettext("The human-like model imitates a rank instead of searching")
        } else {
            i18n::gettext("This engine has no human-like model loaded")
        };
        self.strength_group.set_description(Some(&description));
        grey_out_human(&self.strength_row, enabled);
    }

    fn connect(&self, dialog: &NewGameDialog) {
        self.cancel_button.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        self.start_button.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| dialog.start()
        ));
        self.size_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |row| {
                let widgets = &dialog.imp().widgets;
                widgets
                    .custom_size_row
                    .set_visible(row.selected() == SIZE_CHOICES.len() as u32);
                widgets.refresh_handicap();
            }
        ));
        self.custom_size_row.connect_value_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| dialog.imp().widgets.refresh_handicap()
        ));
        self.rules_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| dialog.sync_komi()
        ));
        self.handicap_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| dialog.sync_komi()
        ));
        self.refresh_time(self.time_row.selected());
        self.time_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |row| dialog.imp().widgets.refresh_time(row.selected())
        ));
        self.refresh_strength(self.strength_row.selected());
        self.strength_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |row| {
                dialog.imp().coerced_strength.set(false);
                dialog.imp().widgets.refresh_strength(row.selected());
            }
        ));
        self.visits_row.connect_value_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| dialog.imp().coerced_strength.set(false)
        ));
        self.refresh_players();
        self.colour_row.connect_selected_notify(clone!(
            #[weak(rename_to = dialog)]
            dialog,
            move |_| dialog.imp().widgets.refresh_players()
        ));
    }

    /// The bool is the coerced-strength flag, set by the caller after this returns so the
    /// strength handler above does not clear a value this call just stored.
    fn reset(&self, play: &PlaySettings, has_human_model: bool) -> bool {
        self.size_row.set_selected(2);
        set_spin(&self.custom_size_row, 19.0);
        self.handicap_row.set_selected(0);
        self.rules_row.set_selected(
            RuleSet::ALL
                .iter()
                .position(|rules| *rules == play.rules)
                .unwrap_or(1) as u32,
        );
        // Changed rules re-derive the komi through `sync_komi`; unchanged ones would leave
        // a komi edited last time.
        set_spin(&self.komi_row, play.rules.default_komi() as f64);
        self.colour_row.set_selected(0);
        self.time_row.set_selected(0);
        set_spin(&self.main_time_row, MAIN_MINUTES);
        set_spin(&self.periods_row, PERIODS);
        set_spin(&self.period_seconds_row, PERIOD_SECONDS);
        set_spin(&self.increment_row, INCREMENT_SECONDS);

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
        set_spin(&self.visits_row, visits);
        set_spin(&self.seconds_row, seconds);
        if self.profile_row.text() != profile {
            self.profile_row.set_text(profile);
        }
        self.strength_row.set_selected(saved_mode);
        self.refresh_handicap();
        self.refresh_time(self.time_row.selected());
        self.refresh_strength(self.strength_row.selected());
        self.refresh_players();
        self.page.scroll_to_top();
        matches!(play.strength, StrengthSetting::Human { .. }) && !has_human_model
    }

    fn selected_size(&self) -> Size {
        match self.size_row.selected() {
            index if (index as usize) < SIZE_CHOICES.len() => {
                Size::square(SIZE_CHOICES[index as usize])
            }
            _ => Size::square(self.custom_size_row.value() as u8),
        }
    }

    /// Handicap stones exist only on odd square boards of 7 and up. Anywhere else the
    /// row is insensitive at None rather than accepting a count that places nothing.
    fn refresh_handicap(&self) {
        let takes = takes_handicap(self.selected_size());
        self.handicap_row.set_sensitive(takes);
        // Unselecting notifies the handicap row, which re-derives komi. A size change
        // that keeps the handicap leaves a komi override alone.
        if !takes {
            self.handicap_row.set_selected(0);
        }
    }

    fn apply_komi(&self) {
        let value = if selected_handicap(self.selected_size(), self.handicap_row.selected()) > 0 {
            0.5
        } else {
            rules_at(self.rules_row.selected()).default_komi() as f64
        };
        self.komi_row.set_value(value);
    }

    fn refresh_time(&self, kind: u32) {
        self.main_time_row.set_visible(kind != 0);
        self.periods_row.set_visible(kind == 2);
        self.period_seconds_row.set_visible(kind == 2);
        self.increment_row.set_visible(kind == 3);
    }

    fn refresh_strength(&self, kind: u32) {
        self.visits_row.set_visible(kind == 0);
        self.seconds_row.set_visible(kind == 1);
        self.profile_row.set_visible(kind == 2);
    }

    fn refresh_players(&self) {
        let both = self.colour_row.selected() == 2;
        self.strength_group.set_visible(!both);
        let description = if both {
            i18n::gettext("Play both sides on this device")
        } else {
            i18n::gettext("The engine takes the other colour")
        };
        self.players_group.set_description(Some(&description));
    }

    fn setup(&self) -> GameSetup {
        // Commit typed text. Enter on Start does not always focus-out the row first,
        // and an uncommitted SpinRow still reports its previous value.
        for row in [
            &self.komi_row,
            &self.custom_size_row,
            &self.main_time_row,
            &self.periods_row,
            &self.period_seconds_row,
            &self.increment_row,
            &self.visits_row,
            &self.seconds_row,
        ] {
            row.update();
        }
        let size = self.selected_size();
        let handicap = selected_handicap(size, self.handicap_row.selected());
        let human = match self.colour_row.selected() {
            0 => Some(Color::Black),
            1 => Some(Color::White),
            _ => None,
        };
        let tc = match self.time_row.selected() {
            1 => TimeControl {
                main_s: (self.main_time_row.value() * 60.0) as u32,
                ..TimeControl::UNLIMITED
            },
            2 => TimeControl {
                main_s: (self.main_time_row.value() * 60.0) as u32,
                byo_periods: self.periods_row.value() as u8,
                byo_period_s: self.period_seconds_row.value() as u32,
                increment_s: 0,
            },
            3 => TimeControl {
                main_s: (self.main_time_row.value() * 60.0) as u32,
                byo_periods: 0,
                byo_period_s: 0,
                increment_s: self.increment_row.value() as u32,
            },
            _ => TimeControl::UNLIMITED,
        };
        let strength = match self.strength_row.selected() {
            1 => Strength::TimeMs((self.seconds_row.value() * 1000.0) as u32),
            2 => Strength::Human {
                profile: self.profile_row.text().to_string(),
            },
            _ => Strength::Visits(self.visits_row.value() as u32),
        };
        GameSetup {
            size,
            rules: rules_at(self.rules_row.selected()),
            komi: komi_points(self.komi_row.value()),
            handicap,
            human,
            tc,
            strength,
        }
    }
}

fn group(group: adw::PreferencesGroup, rows: &[&adw::PreferencesRow]) -> adw::PreferencesGroup {
    for row in rows {
        group.add(*row);
    }
    group
}

fn string_list(items: &[String]) -> gtk::StringList {
    gtk::StringList::new(&items.iter().map(String::as_str).collect::<Vec<_>>())
}

fn adjustment(value: f64, min: f64, max: f64, step: f64) -> gtk::Adjustment {
    gtk::Adjustment::new(value, min, max, step, step * 10.0, 0.0)
}

/// Presents `slot`'s New Game dialog over `parent`, building it on first use.
///
/// The dialog is kept for the window's life: building it and presenting it cost 20–55 ms
/// of the GTK thread on every open, presenting a built one a few. Each presentation resets
/// it to where a new game starts, as a fresh one would be.
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
