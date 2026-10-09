// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::prelude::*;

use crate::i18n::{gettext, pgettext};

/// Rows [`crate::prefs`] binds. Built once with the dialog and kept for the window's life.
pub struct PreferencesWidgets {
    pub profiles_group: adw::PreferencesGroup,
    pub add_local_button: adw::ButtonRow,
    pub add_remote_button: adw::ButtonRow,
    pub analysis_visits_row: adw::SpinRow,
    pub analysis_interval_row: adw::SpinRow,
    pub analysis_suggestions_row: adw::SpinRow,
    pub analysis_batch_visits_row: adw::SpinRow,
    pub analysis_auto_open_row: adw::SwitchRow,
    pub analysis_reset_button: adw::ButtonRow,
    pub play_strength_kind_row: adw::ComboRow,
    pub play_visits_row: adw::SpinRow,
    pub play_seconds_row: adw::SpinRow,
    pub play_human_row: adw::EntryRow,
    pub play_temperature_row: adw::SpinRow,
    pub play_threshold_row: adw::SpinRow,
    pub play_streak_row: adw::SpinRow,
    pub play_rules_row: adw::ComboRow,
    pub play_reset_button: adw::ButtonRow,
    pub show_coordinates_row: adw::SwitchRow,
    pub show_move_numbers_row: adw::SwitchRow,
    pub overlay_row: adw::ComboRow,
    pub save_analysis_row: adw::SwitchRow,
    pub stone_volume_scale: gtk::Scale,
    pub general_reset_button: adw::ButtonRow,
}

/// The preferences dialog and the controls on its four pages.
pub fn dialog() -> (adw::PreferencesDialog, PreferencesWidgets) {
    let profiles_group = adw::PreferencesGroup::builder()
        .name("profiles_group")
        .title(gettext("Engine Profiles"))
        .description(gettext(
            "The selected profile provides analysis and plays as the computer.",
        ))
        .build();
    let add_local_button = add_button(
        "add_local_button",
        &gettext("Add Local Engine"),
        "list-add-symbolic",
    );
    let add_remote_button = add_button(
        "add_remote_button",
        &gettext("Add Remote Engine"),
        "network-server-symbolic",
    );
    let engines = page(
        &gettext("Engines"),
        "application-x-executable-symbolic",
        &[
            &profiles_group,
            &group(
                &gettext("Add Profile"),
                None,
                &[
                    add_local_button.upcast_ref(),
                    add_remote_button.upcast_ref(),
                ],
            ),
        ],
    );

    let analysis_visits_row = spin(
        "analysis_visits_row",
        &gettext("Maximum Visits"),
        Some(&gettext("Where a live search stops thinking")),
    );
    let analysis_interval_row = spin(
        "analysis_interval_row",
        &gettext("Report Interval"),
        Some(&gettext("Milliseconds between updates from the engine")),
    );
    // Translators: “All” is the word the suggestions row shows instead of 0, meaning every candidate.
    let suggestions = gettext(
        "Candidate moves kept on the board and in the panel — “All” keeps every move the engine searched",
    );
    let analysis_suggestions_row = spin(
        "analysis_suggestions_row",
        &gettext("Suggestions Shown"),
        Some(&suggestions),
    );
    let analysis_batch_visits_row = spin(
        "analysis_batch_visits_row",
        &gettext("Visits per Move"),
        Some(&gettext(
            "Budget for each position when analysing a whole game",
        )),
    );
    let analysis_auto_open_row = switch_row(
        "analysis_auto_open_row",
        &gettext("Analyse on Open"),
        &gettext("Sweep the main line when a record is opened or downloaded"),
    );
    let save_analysis_row = switch_row(
        "save_analysis_row",
        &gettext("Save Analysis in SGF"),
        &gettext("Write winrates and candidate moves alongside the moves"),
    );
    let analysis_reset_button = button_row("analysis_reset_button", &gettext("Restore Defaults"));
    let analysis = page(
        &gettext("Analysis"),
        "system-search-symbolic",
        &[
            &group(
                &gettext("Live Analysis"),
                Some(&gettext("Used while the board follows the cursor.")),
                &[
                    analysis_visits_row.upcast_ref(),
                    analysis_interval_row.upcast_ref(),
                    analysis_suggestions_row.upcast_ref(),
                ],
            ),
            &group(
                &gettext("Whole-Game Analysis"),
                None,
                &[
                    analysis_batch_visits_row.upcast_ref(),
                    analysis_auto_open_row.upcast_ref(),
                ],
            ),
            &group(&gettext("Files"), None, &[save_analysis_row.upcast_ref()]),
            &group(
                &gettext("Analysis Defaults"),
                None,
                &[analysis_reset_button.upcast_ref()],
            ),
        ],
    );

    let play_strength_kind_row = combo("play_strength_kind_row", &gettext("Mode"), None);
    let play_visits_row = spin("play_visits_row", &gettext("Visits per Move"), None);
    let play_seconds_row = spin("play_seconds_row", &gettext("Seconds per Move"), None);
    let play_human_row = adw::EntryRow::builder()
        .name("play_human_row")
        .title(gettext("Human Model Profile"))
        .build();
    let play_temperature_row = spin(
        "play_temperature_row",
        &gettext("Temperature"),
        Some(&gettext(
            "0 always plays the best move; higher values add variety",
        )),
    );
    let play_threshold_row = spin(
        "play_threshold_row",
        &gettext("Resign Threshold"),
        Some(&gettext(
            "Winrate below which the computer considers resigning",
        )),
    );
    let play_streak_row = spin(
        "play_streak_row",
        &gettext("Resign Streak"),
        Some(&gettext("Consecutive hopeless moves before resigning")),
    );
    let play_rules_row = combo(
        "play_rules_row",
        &gettext("Default Ruleset"),
        Some(&gettext(
            "Preselected in New Game, which remembers the last ruleset started",
        )),
    );
    let play_reset_button = button_row("play_reset_button", &gettext("Restore Defaults"));
    let play = page(
        &pgettext("noun", "Play"),
        "media-playback-start-symbolic",
        &[
            &group(
                &gettext("Computer Strength"),
                Some(&gettext(
                    "How much thinking the computer does for each of its moves.",
                )),
                &[
                    play_strength_kind_row.upcast_ref(),
                    play_visits_row.upcast_ref(),
                    play_seconds_row.upcast_ref(),
                    play_human_row.upcast_ref(),
                ],
            ),
            &group(
                &gettext("Behaviour"),
                None,
                &[
                    play_temperature_row.upcast_ref(),
                    play_threshold_row.upcast_ref(),
                    play_streak_row.upcast_ref(),
                ],
            ),
            &group(&gettext("New Games"), None, &[play_rules_row.upcast_ref()]),
            &group(
                &gettext("Play Defaults"),
                None,
                &[play_reset_button.upcast_ref()],
            ),
        ],
    );

    let show_coordinates_row = switch_row(
        "show_coordinates_row",
        &gettext("Coordinates"),
        &gettext("Letters and numbers around the board"),
    );
    let show_move_numbers_row = switch_row(
        "show_move_numbers_row",
        &gettext("Move Numbers"),
        &gettext("Number every stone; the last move’s number is red"),
    );
    let overlay_row = combo(
        "overlay_row",
        &gettext("Overlay"),
        Some(&gettext(
            "Shade the board by predicted ownership or raw network policy",
        )),
    );
    let stone_volume_row = adw::ActionRow::builder()
        .name("stone_volume_row")
        .title(gettext("Stone Sounds"))
        .subtitle(gettext(
            "A click for each move, and falling stones for a capture",
        ))
        .build();
    let stone_volume_scale = gtk::Scale::builder()
        .name("stone_volume_scale")
        .width_request(180)
        .valign(gtk::Align::Center)
        .draw_value(true)
        .value_pos(gtk::PositionType::Left)
        .digits(0)
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 100.0, 5.0, 10.0, 0.0))
        .build();
    stone_volume_row.add_suffix(&stone_volume_scale);
    stone_volume_scale.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        stone_volume_row.upcast_ref::<gtk::Widget>().upcast_ref(),
    ])]);
    let general_reset_button = button_row("general_reset_button", &gettext("Restore Defaults"));
    let general = page(
        &gettext("General"),
        "preferences-system-symbolic",
        &[
            &group(
                &gettext("Board"),
                None,
                &[
                    show_coordinates_row.upcast_ref(),
                    show_move_numbers_row.upcast_ref(),
                    overlay_row.upcast_ref(),
                ],
            ),
            &group(&gettext("Sound"), None, &[stone_volume_row.upcast_ref()]),
            &group(
                &gettext("General Defaults"),
                None,
                &[general_reset_button.upcast_ref()],
            ),
        ],
    );

    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Preferences"))
        .content_width(760)
        .content_height(720)
        .build();
    for page in [&engines, &analysis, &play, &general] {
        dialog.add(page);
    }
    crate::widgets::sheet_texture::install(&dialog);

    (
        dialog,
        PreferencesWidgets {
            profiles_group,
            add_local_button,
            add_remote_button,
            analysis_visits_row,
            analysis_interval_row,
            analysis_suggestions_row,
            analysis_batch_visits_row,
            analysis_auto_open_row,
            analysis_reset_button,
            play_strength_kind_row,
            play_visits_row,
            play_seconds_row,
            play_human_row,
            play_temperature_row,
            play_threshold_row,
            play_streak_row,
            play_rules_row,
            play_reset_button,
            show_coordinates_row,
            show_move_numbers_row,
            overlay_row,
            save_analysis_row,
            stone_volume_scale,
            general_reset_button,
        },
    )
}

fn page(title: &str, icon: &str, groups: &[&adw::PreferencesGroup]) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(title)
        .icon_name(icon)
        .build();
    for group in groups {
        page.add(*group);
    }
    page
}

fn group(
    title: &str,
    description: Option<&str>,
    rows: &[&adw::PreferencesRow],
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    if let Some(description) = description {
        group.set_description(Some(description));
    }
    for row in rows {
        group.add(*row);
    }
    group
}

fn spin(id: &str, title: &str, subtitle: Option<&str>) -> adw::SpinRow {
    let row = adw::SpinRow::builder().name(id).title(title);
    match subtitle {
        Some(subtitle) => row.subtitle(subtitle).build(),
        None => row.build(),
    }
}

fn switch_row(id: &str, title: &str, subtitle: &str) -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .name(id)
        .title(title)
        .subtitle(subtitle)
        .build()
}

fn combo(id: &str, title: &str, subtitle: Option<&str>) -> adw::ComboRow {
    let row = adw::ComboRow::builder().name(id).title(title);
    match subtitle {
        Some(subtitle) => row.subtitle(subtitle).build(),
        None => row.build(),
    }
}

fn button_row(id: &str, title: &str) -> adw::ButtonRow {
    adw::ButtonRow::builder().name(id).title(title).build()
}

fn add_button(id: &str, title: &str, icon: &str) -> adw::ButtonRow {
    adw::ButtonRow::builder()
        .name(id)
        .title(title)
        .start_icon_name(icon)
        .end_icon_name("go-next-symbolic")
        .build()
}
