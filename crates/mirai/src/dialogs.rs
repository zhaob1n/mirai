// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Dialogs: new game, scoring, remote-profile confirmation. Steps 11 and 13.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::prelude::{ButtonExt, EditableExt, ListItemExt, WidgetExt};

use mirai_core::{Color, RuleSet, Size, TimeControl};

use crate::app::AppState;
use crate::config::StrengthSetting;
use crate::play::{GameSetup, Strength};

/// The strength row's third option, kept in one place because it is also the string the
/// list factory greys out when the engine has no human-like model.
const HUMAN_LIKE: &str = "Human-like";

const SIZE_CHOICES: [u8; 3] = [9, 13, 19];

/// Shows the New Game dialog; `on_start` runs with the chosen setup.
pub fn new_game(
    parent: &impl IsA<gtk::Widget>,
    state: &AppState,
    on_start: impl Fn(GameSetup) + 'static,
) {
    let play = state.config().play.clone();
    let has_human_model = state
        .engine_desc()
        .is_some_and(|d| d.has_human_model);

    let dialog = adw::Dialog::builder()
        .title("New game")
        .content_width(460)
        .content_height(620)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let start = gtk::Button::with_label("Start");
    start.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&start);
    toolbar.add_top_bar(&header);

    let page = adw::PreferencesPage::new();

    // -- board ---------------------------------------------------------------------
    let board_group = adw::PreferencesGroup::builder().title("Board").build();

    let size_row = combo_row("Size", &["9 × 9", "13 × 13", "19 × 19", "Custom"]);
    let custom_size = spin_row("Custom size", 2.0, 19.0, 1.0, 0);
    custom_size.set_value(19.0);
    custom_size.set_visible(false);

    let rules_row = combo_row("Rules", &RuleSet::ALL.map(|r| r.label()));
    rules_row.set_selected(
        RuleSet::ALL
            .iter()
            .position(|r| *r == play.rules)
            .unwrap_or(1) as u32,
    );

    let handicap_row = combo_row(
        "Handicap",
        &[
            "None", "2 stones", "3 stones", "4 stones", "5 stones", "6 stones", "7 stones",
            "8 stones", "9 stones",
        ],
    );

    let komi_row = spin_row("Komi", -150.0, 150.0, 0.5, 1);
    komi_row.set_value(play.rules.default_komi() as f64);

    board_group.add(&size_row);
    board_group.add(&custom_size);
    board_group.add(&handicap_row);
    board_group.add(&komi_row);
    board_group.add(&rules_row);
    page.add(&board_group);

    // 19x19 first, which is what `SIZE_CHOICES` index 2 is.
    size_row.set_selected(2);
    {
        let custom_size = custom_size.clone();
        size_row.connect_selected_notify(move |row| {
            custom_size.set_visible(row.selected() == SIZE_CHOICES.len() as u32);
        });
    }

    // Komi follows the ruleset, except that a handicap game is played at 0.5.
    let syncing = Rc::new(Cell::new(false));
    let sync_komi = {
        let komi_row = komi_row.clone();
        let rules_row = rules_row.clone();
        let handicap_row = handicap_row.clone();
        let syncing = syncing.clone();
        move || {
            if syncing.get() {
                return;
            }
            syncing.set(true);
            let rules = rules_at(rules_row.selected());
            let komi = if handicap_row.selected() > 0 {
                0.5
            } else {
                rules.default_komi() as f64
            };
            komi_row.set_value(komi);
            syncing.set(false);
        }
    };
    {
        let sync = sync_komi.clone();
        rules_row.connect_selected_notify(move |_| sync());
    }
    {
        let sync = sync_komi.clone();
        handicap_row.connect_selected_notify(move |_| sync());
    }

    // -- players -------------------------------------------------------------------
    let player_group = adw::PreferencesGroup::builder()
        .title("Players")
        .description("The engine takes the other colour")
        .build();
    let colour_row = combo_row("You play", &["Black", "White", "Both (no engine)"]);
    player_group.add(&colour_row);
    page.add(&player_group);

    // -- time control --------------------------------------------------------------
    let time_group = adw::PreferencesGroup::builder().title("Time control").build();
    let time_row = combo_row(
        "Type",
        &["None", "Absolute", "Byo-yomi", "Fischer increment"],
    );
    let main_row = spin_row("Main time (minutes)", 0.0, 600.0, 1.0, 0);
    main_row.set_value(20.0);
    let periods_row = spin_row("Byo-yomi periods", 1.0, 25.0, 1.0, 0);
    periods_row.set_value(5.0);
    let period_row = spin_row("Seconds per period", 1.0, 600.0, 5.0, 0);
    period_row.set_value(30.0);
    let increment_row = spin_row("Increment (seconds)", 0.0, 600.0, 1.0, 0);
    increment_row.set_value(10.0);

    time_group.add(&time_row);
    time_group.add(&main_row);
    time_group.add(&periods_row);
    time_group.add(&period_row);
    time_group.add(&increment_row);
    page.add(&time_group);

    let refresh_time = {
        let main_row = main_row.clone();
        let periods_row = periods_row.clone();
        let period_row = period_row.clone();
        let increment_row = increment_row.clone();
        move |kind: u32| {
            main_row.set_visible(kind != 0);
            periods_row.set_visible(kind == 2);
            period_row.set_visible(kind == 2);
            increment_row.set_visible(kind == 3);
        }
    };
    refresh_time(0);
    {
        let refresh = refresh_time.clone();
        time_row.connect_selected_notify(move |row| refresh(row.selected()));
    }

    // -- strength ------------------------------------------------------------------
    let strength_group = adw::PreferencesGroup::builder()
        .title("Engine strength")
        .description(if has_human_model {
            "The human-like model imitates a rank instead of searching"
        } else {
            "This engine has no human-like model loaded"
        })
        .build();
    let strength_row = combo_row("Mode", &["Visits", "Time per move", HUMAN_LIKE]);
    grey_out_human(&strength_row, has_human_model);

    let visits_row = spin_row("Visits per move", 1.0, 1_000_000.0, 100.0, 0);
    let seconds_row = spin_row("Seconds per move", 0.1, 600.0, 0.5, 1);
    let profile_row = adw::EntryRow::builder().title("Human-like profile").build();
    profile_row.set_text("rank_5k");

    strength_group.add(&strength_row);
    strength_group.add(&visits_row);
    strength_group.add(&seconds_row);
    strength_group.add(&profile_row);
    page.add(&strength_group);

    // Prefill the strength rows from the saved preference.
    let saved_mode = match &play.strength {
        StrengthSetting::Visits { visits } => {
            visits_row.set_value(*visits as f64);
            0
        }
        StrengthSetting::Time { time_ms } => {
            seconds_row.set_value(*time_ms as f64 / 1000.0);
            1
        }
        StrengthSetting::Human { profile } => {
            profile_row.set_text(profile);
            if has_human_model { 2 } else { 0 }
        }
    };
    if visits_row.value() <= 1.0 {
        visits_row.set_value(800.0);
    }
    if seconds_row.value() <= 0.1 {
        seconds_row.set_value(5.0);
    }

    let refresh_strength = {
        let visits_row = visits_row.clone();
        let seconds_row = seconds_row.clone();
        let profile_row = profile_row.clone();
        move |kind: u32| {
            visits_row.set_visible(kind == 0);
            seconds_row.set_visible(kind == 1);
            profile_row.set_visible(kind == 2);
        }
    };
    strength_row.set_selected(saved_mode);
    refresh_strength(saved_mode);
    {
        let refresh = refresh_strength.clone();
        strength_row.connect_selected_notify(move |row| refresh(row.selected()));
    }

    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        let state = state.clone();
        start.connect_clicked(move |_| {
            let size = match size_row.selected() {
                i if (i as usize) < SIZE_CHOICES.len() => Size::square(SIZE_CHOICES[i as usize]),
                _ => Size::new(custom_size.value() as u8, custom_size.value() as u8)
                    .unwrap_or(Size::square(19)),
            };
            let rules = rules_at(rules_row.selected());
            let handicap = match handicap_row.selected() {
                0 => 0,
                n => (n + 1) as u8,
            };
            let human = match colour_row.selected() {
                0 => Some(Color::Black),
                1 => Some(Color::White),
                _ => None,
            };
            let tc = match time_row.selected() {
                1 => TimeControl {
                    main_s: (main_row.value() * 60.0) as u32,
                    ..TimeControl::UNLIMITED
                },
                2 => TimeControl {
                    main_s: (main_row.value() * 60.0) as u32,
                    byo_periods: periods_row.value() as u8,
                    byo_period_s: period_row.value() as u32,
                    increment_s: 0,
                },
                3 => TimeControl {
                    main_s: (main_row.value() * 60.0) as u32,
                    byo_periods: 0,
                    byo_period_s: 0,
                    increment_s: increment_row.value() as u32,
                },
                _ => TimeControl::UNLIMITED,
            };
            let strength = match strength_row.selected() {
                1 => Strength::TimeMs((seconds_row.value() * 1000.0) as u32),
                2 => Strength::Human {
                    profile: profile_row.text().to_string(),
                },
                _ => Strength::Visits(visits_row.value() as u32),
            };

            {
                let mut cfg = state.config_mut();
                cfg.play.rules = rules;
                cfg.play.strength = match &strength {
                    Strength::Visits(v) => StrengthSetting::Visits { visits: *v },
                    Strength::TimeMs(t) => StrengthSetting::Time { time_ms: *t },
                    Strength::Human { profile } => StrengthSetting::Human {
                        profile: profile.clone(),
                    },
                };
            }
            state.save_config();

            dialog.close();
            on_start(GameSetup {
                size,
                rules,
                komi: komi_row.value() as f32,
                handicap,
                human,
                tc,
                strength,
            });
        });
    }

    dialog.present(Some(parent));
}

/// The same dialog, with a working "Analyse game" button.
pub fn show_score_with(
    parent: &impl IsA<gtk::Widget>,
    state: &AppState,
    summary: &str,
    on_analyse: impl Fn() + 'static,
) {
    let (rules, komi, size) = {
        let tree = state.tree();
        (tree.info.rules, tree.info.komi, tree.info.size)
    };
    let body = format!(
        "{summary}\n\n{}×{} · {} · komi {komi}",
        size.w,
        size.h,
        rules.label()
    );
    let dialog = adw::AlertDialog::new(Some("Result"), Some(&body));
    dialog.add_responses(&[("close", "Close"), ("analyse", "Analyse game")]);
    dialog.set_response_appearance("analyse", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.connect_response(None, move |_, response| {
        if response == "analyse" {
            on_analyse();
        }
    });
    dialog.present(Some(parent));
}

/// Asks the user to confirm a server's certificate fingerprint before pinning it.
pub fn confirm_fingerprint(
    parent: &impl IsA<gtk::Widget>,
    url: &str,
    fingerprint: &str,
    on_accept: impl Fn() + 'static,
) {
    let pretty: String = fingerprint
        .as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(":");
    let dialog = adw::AlertDialog::new(
        Some("Trust this server?"),
        Some(&format!(
            "{url} presented a certificate with SHA-256\n\n{pretty}\n\n\
             mirai will refuse to connect if it ever changes."
        )),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("trust", "Trust")]);
    dialog.set_response_appearance("trust", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "trust" {
            on_accept();
        }
    });
    dialog.present(Some(parent));
}

// -- row helpers --------------------------------------------------------------------

fn combo_row(title: &str, items: &[&str]) -> adw::ComboRow {
    let model = gtk::StringList::new(items);
    adw::ComboRow::builder().title(title).model(&model).build()
}

fn spin_row(title: &str, min: f64, max: f64, step: f64, digits: u32) -> adw::SpinRow {
    let adjustment = gtk::Adjustment::new(min, min, max, step, step * 10.0, 0.0);
    adw::SpinRow::builder()
        .title(title)
        .adjustment(&adjustment)
        .digits(digits)
        .build()
}

fn rules_at(index: u32) -> RuleSet {
    RuleSet::ALL
        .get(index as usize)
        .copied()
        .unwrap_or(RuleSet::Chinese)
}

/// Makes the human-like entry unselectable and dim when the engine cannot play it.
/// `GtkListItem::set_selectable(false)` is what actually stops the click; the dimmed
/// label is what makes that visible.
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
                .map(|s| s.string().to_string())
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
