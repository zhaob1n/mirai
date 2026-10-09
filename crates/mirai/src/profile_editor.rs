// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use adw::prelude::*;

use crate::i18n::{gettext, pgettext};

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

pub struct RemoteFormWidgets {
    pub name_row: adw::EntryRow,
    pub url_row: adw::EntryRow,
    pub token_row: adw::PasswordEntryRow,
    pub engine_row: adw::EntryRow,
    pub trust_row: adw::ActionRow,
    pub test_button: gtk::Button,
}

pub fn local_form() -> (adw::PreferencesPage, LocalFormWidgets) {
    let name_row = adw::EntryRow::builder().title(gettext("Name")).build();
    let identity = group("", &[name_row.upcast_ref()]);

    let (katago_row, katago_button) = file_row(&gettext("KataGo Binary"), None);
    let model_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let (model_row, model_button) = file_row(&gettext("Neural Network Model"), Some(&model_slot));
    let katago = group(
        &gettext("KataGo"),
        &[katago_row.upcast_ref(), model_row.upcast_ref()],
    );
    katago.set_description(Some(&gettext(
        "Both must exist before the profile can be saved.",
    )));

    let mode_row = adw::ComboRow::builder()
        .title(gettext("Analysis Config"))
        .model(&gtk::StringList::new(&[
            &gettext("Managed by mirai"),
            &gettext("Custom file"),
        ]))
        .build();
    let config_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let (config_row, config_button) =
        file_row(&gettext("Custom Analysis Config"), Some(&config_slot));
    let configuration = group(
        &gettext("Configuration"),
        &[mode_row.upcast_ref(), config_row.upcast_ref()],
    );

    let analysis_row = spin_row(&gettext("Positions in Parallel"));
    let search_row = spin_row(&gettext("Threads per Position"));
    // Translators: noun, the search-thread settings, not the verb.
    let search = group(
        &pgettext("noun", "Search"),
        &[analysis_row.upcast_ref(), search_row.upcast_ref()],
    );

    let batch_row = spin_row(&gettext("GPU Batch Size"));
    let cache_row = spin_row(&gettext("Neural-Net Cache"));
    let memory_group = group(
        &gettext("Batching and Memory"),
        &[batch_row.upcast_ref(), cache_row.upcast_ref()],
    );

    let tune_button = gtk::Button::builder()
        .label(gettext("Tune…"))
        .valign(gtk::Align::Center)
        .build();
    let tune_row = adw::ActionRow::builder()
        .title(gettext("Measure This Machine"))
        .subtitle(gettext(
            "Uses the binary and model chosen above, and stops the running engine while it works.",
        ))
        .use_markup(false)
        .subtitle_lines(3)
        .activatable_widget(&tune_button)
        .build();
    tune_row.add_suffix(&tune_button);
    let tuning_group = group(&gettext("Automatic Tuning"), &[tune_row.upcast_ref()]);
    tuning_group.set_description(Some(&gettext("Times KataGo on this machine at a series of thread settings and fills in the values above. It starts KataGo once per setting, so allow a few minutes.")));

    let page = adw::PreferencesPage::builder()
        .title(gettext("Engine Profile"))
        .build();
    for group in [
        &identity,
        &katago,
        &configuration,
        &search,
        &memory_group,
        &tuning_group,
    ] {
        page.add(group);
    }
    (
        page,
        LocalFormWidgets {
            name_row,
            katago_row,
            katago_button,
            model_row,
            model_slot,
            model_button,
            mode_row,
            config_row,
            config_slot,
            config_button,
            analysis_row,
            search_row,
            memory_group,
            batch_row,
            cache_row,
            tuning_group,
            tune_row,
            tune_button,
        },
    )
}

pub fn remote_form() -> (adw::PreferencesPage, RemoteFormWidgets) {
    let name_row = adw::EntryRow::builder().title(gettext("Name")).build();
    let identity = group("", &[name_row.upcast_ref()]);

    let url_row = adw::EntryRow::builder()
        .title(gettext("Server Address"))
        .build();
    let token_row = adw::PasswordEntryRow::builder()
        .title(gettext("Token"))
        .build();
    let engine_row = adw::EntryRow::builder()
        .title(gettext("Engine Name (Optional)"))
        .build();
    let server = group(
        &gettext("Server"),
        &[
            url_row.upcast_ref(),
            token_row.upcast_ref(),
            engine_row.upcast_ref(),
        ],
    );
    server.set_description(Some(&gettext(
        "For example 192.168.1.10, or 192.168.1.10:9678. The port defaults to 9678.",
    )));

    let test_button = gtk::Button::builder()
        .label(gettext("Test Connection"))
        .valign(gtk::Align::Center)
        .build();
    let trust_row = adw::ActionRow::builder()
        .title(gettext("Pinned Fingerprint"))
        .use_markup(false)
        .subtitle_lines(3)
        .build();
    trust_row.add_suffix(&test_button);
    let certificate = group(&gettext("Certificate"), &[trust_row.upcast_ref()]);
    certificate.set_description(Some(&gettext("mirai checks the certificate before sending the token. Test the connection and compare the fingerprint with the one mirai-server printed at startup.")));

    let page = adw::PreferencesPage::builder()
        .title(gettext("Engine Profile"))
        .build();
    for group in [&identity, &server, &certificate] {
        page.add(group);
    }
    (
        page,
        RemoteFormWidgets {
            name_row,
            url_row,
            token_row,
            engine_row,
            trust_row,
            test_button,
        },
    )
}

fn group(title: &str, rows: &[&adw::PreferencesRow]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    for row in rows {
        group.add(*row);
    }
    group
}

fn file_row(title: &str, slot: Option<&gtk::Box>) -> (adw::ActionRow, gtk::Button) {
    let button = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Choose a File"))
        .css_classes(["flat"])
        .build();
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .subtitle_lines(3)
        .activatable_widget(&button)
        .build();
    if let Some(slot) = slot {
        row.add_suffix(slot);
    }
    row.add_suffix(&button);
    (row, button)
}

fn spin_row(title: &str) -> adw::SpinRow {
    adw::SpinRow::builder().title(title).numeric(true).build()
}
