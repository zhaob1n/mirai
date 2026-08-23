// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The GTK half of Fox Go lookup: a `gio` transport, the last-search cache, and the picker
//! dialog.
//!
//! The endpoints, the reply shapes and the SGF dialect live in [`mirai_client::fox`], which
//! the HarmonyOS client uses as well. This file does not reimplement any of them — it cannot
//! even use that module's `Fetch` trait, because GIO's futures are `!Send`, so it composes
//! the URL builders with the pure parsers instead.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use mirai_client::fox;
pub(crate) use mirai_client::fox::FoxGame;
use mirai_core::GameTree;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::fox_picker::{FoxPickerDialog, FoxRow};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const RETRIES: u32 = 3;
const RETRY_BASE: Duration = Duration::from_millis(350);
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum FoxError {
    #[error("{0}")]
    Service(String),
    #[error("Fox returned invalid data: {0}")]
    InvalidData(String),
    #[error("Could not reach Fox: {0}")]
    Request(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LastSearch {
    query: String,
    account: String,
    rows: Vec<FoxGame>,
}

struct Games {
    account: String,
    rows: Vec<FoxGame>,
}

pub(crate) struct DownloadedGame {
    pub(crate) tree: GameTree,
    pub(crate) label: String,
}

fn last_search_path() -> Option<PathBuf> {
    Config::data_dir()
        .ok()
        .map(|dir| dir.join("fox-last-search.json"))
}

static LAST_SEARCH: LazyLock<Mutex<Option<LastSearch>>> =
    LazyLock::new(|| Mutex::new(load_last_search()));

fn load_last_search() -> Option<LastSearch> {
    last_search_path().and_then(|path| read_last_search(&path))
}

fn read_last_search(path: &Path) -> Option<LastSearch> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_last_search(path: &Path, search: &LastSearch) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string(search).map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

fn cached_search() -> Option<LastSearch> {
    LAST_SEARCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn remember_search(search: LastSearch) {
    if let Some(path) = last_search_path()
        && let Err(error) = write_last_search(&path, &search)
    {
        tracing::warn!(%error, "could not cache the Fox search");
    }
    *LAST_SEARCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(search);
}

/// Fetches `url` as text, retrying a few times with a growing pause.
///
/// `gio` rather than a HTTP crate: it is already linked, it honours the desktop's proxy
/// settings, and it keeps the request on the GLib main context where the dialog lives.
async fn get_body(url: &str) -> Result<String, FoxError> {
    let mut last_error = "request failed".to_string();
    for attempt in 1..=RETRIES {
        let file = gio::File::for_uri(url);
        match glib::future_with_timeout(REQUEST_TIMEOUT, file.load_contents_future()).await {
            Ok(Ok((bytes, _))) if bytes.len() > MAX_RESPONSE_BYTES => {
                last_error = "response was unexpectedly large".to_string();
            }
            Ok(Ok((bytes, _))) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => return Ok(text),
                Err(error) => last_error = format!("response was not UTF-8 ({error})"),
            },
            Ok(Err(error)) => last_error = error.to_string(),
            Err(_) => last_error = "request timed out".to_string(),
        }
        if attempt < RETRIES {
            glib::timeout_future(RETRY_BASE * attempt).await;
        }
    }
    Err(FoxError::Request(last_error))
}

async fn search_games(query: &str) -> Result<Games, FoxError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(FoxError::Service(
            "Enter an exact Fox nickname or numeric UID".to_string(),
        ));
    }

    // A numeric query is already a UID; looking it up as a nickname would only fail.
    let (uid, account) = if query.bytes().all(|b| b.is_ascii_digit()) {
        (query.to_string(), format!("UID {query}"))
    } else {
        let body = get_body(&fox::user_url(query)).await?;
        let user = fox::parse_user(&body, query).map_err(FoxError::Service)?;
        if user.hidden {
            return Err(FoxError::Service(
                "This player has hidden their game records".to_string(),
            ));
        }
        if user.uid.is_empty() {
            return Err(FoxError::InvalidData(
                "the player UID is missing".to_string(),
            ));
        }
        (user.uid, user.name)
    };

    let body = get_body(&fox::games_url(&uid)).await?;
    let rows = fox::parse_games(&body).map_err(FoxError::Service)?;
    Ok(Games { account, rows })
}

async fn fetch_game(game: &FoxGame) -> Result<DownloadedGame, FoxError> {
    let body = get_body(&fox::sgf_url(&game.chess_id)).await?;
    let raw = fox::sgf_field(&body).map_err(FoxError::Service)?;
    let tree = fox::parse_record(&raw).map_err(FoxError::InvalidData)?;
    Ok(DownloadedGame {
        tree,
        label: game.matchup(),
    })
}

/// How one record reads in the list. Plain text: the labels bound to it do not parse markup,
/// so a nickname with `<` in it is not a problem.
fn list_row(game: &FoxGame) -> FoxRow {
    let mut details = Vec::with_capacity(5);
    if !game.date.is_empty() {
        details.push(fox::display_text(&game.date));
    }
    details.push(format!("{}×{}", game.board_size, game.board_size));
    details.push(format!("{} moves", game.moves));
    details.push(game.result());
    if !game.title.is_empty() {
        details.push(fox::display_text(&game.title));
    }
    FoxRow::new(&game.matchup(), &details.join(" · "))
}

impl FoxPickerDialog {
    fn wired() -> Self {
        let dialog = FoxPickerDialog::new();
        let widgets = dialog.widgets();
        widgets.stack.set_visible_child(&widgets.status_page);

        widgets.cancel_button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        widgets.entry.connect_search_changed(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.refresh_actions()
        ));
        widgets.entry.connect_activate(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.start_search()
        ));
        widgets.search_button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.start_search()
        ));
        widgets.open_button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.start_download()
        ));
        dialog.connect_selection_changed(glib::clone!(
            #[weak]
            dialog,
            move || dialog.refresh_actions()
        ));
        widgets.result_list.connect_activate(glib::clone!(
            #[weak]
            dialog,
            move |_, _| dialog.start_download()
        ));
        dialog.connect_closed(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                // The dialog outlives its own close, so an in-flight request has
                // to be dropped here rather than by disposal.
                dialog.abort_task();
                dialog.set_busy(false);
            }
        ));
        dialog
    }

    /// Puts the dialog back into a state fit to be shown again.
    fn prepare_to_show(&self) {
        self.abort_task();
        self.set_busy(false);
        let widgets = self.widgets();
        widgets.banner.set_revealed(false);
        if self.has_games() {
            widgets.stack.set_visible_child(&widgets.results_page);
        }
        self.refresh_actions();
    }

    fn refresh_actions(&self) {
        let widgets = self.widgets();
        let idle = !self.is_busy();
        widgets.entry.set_sensitive(idle);
        widgets
            .search_button
            .set_sensitive(idle && !widgets.entry.text().trim().is_empty());
        widgets.result_list.set_sensitive(idle);
        widgets
            .open_button
            .set_sensitive(idle && self.has_selection());
    }

    fn set_busy(&self, busy: bool) {
        self.set_busy_flag(busy);
        self.refresh_actions();
    }

    fn start_search(&self) {
        if self.is_busy() {
            return;
        }
        let widgets = self.widgets();
        let query = widgets.entry.text().trim().to_string();
        if query.is_empty() {
            return;
        }
        widgets.banner.set_revealed(false);
        widgets.loading_page.set_title("Searching Fox");
        widgets
            .loading_page
            .set_description(Some("Looking up the player and their recent games."));
        widgets.stack.set_visible_child(&widgets.loading_page);
        self.set_busy(true);

        let weak = self.downgrade();
        let task = glib::spawn_future_local(async move {
            let result = search_games(&query).await;
            if let Some(dialog) = weak.upgrade() {
                dialog.finish_search(query, result);
            }
        });
        self.replace_task(task);
    }

    fn finish_search(&self, query: String, result: Result<Games, FoxError>) {
        self.set_busy(false);
        match result {
            Ok(found) => {
                remember_search(LastSearch {
                    query,
                    account: found.account.clone(),
                    rows: found.rows.clone(),
                });
                self.show_search(found);
            }
            Err(error) => {
                self.clear_games();
                let widgets = self.widgets();
                widgets
                    .status_page
                    .set_icon_name(Some("dialog-warning-symbolic"));
                widgets.status_page.set_title("Couldn’t load games");
                widgets
                    .status_page
                    .set_description(Some(&error.to_string()));
                widgets.stack.set_visible_child(&widgets.status_page);
            }
        }
    }

    fn show_search(&self, found: Games) {
        let widgets = self.widgets();
        if found.rows.is_empty() {
            self.clear_games();
            widgets
                .status_page
                .set_icon_name(Some("edit-find-symbolic"));
            widgets.status_page.set_title("No public games");
            widgets.status_page.set_description(Some(
                "This account has no visible games in Fox's recent-history window.",
            ));
            widgets.stack.set_visible_child(&widgets.status_page);
            return;
        }

        let count = found.rows.len();
        widgets.result_label.set_label(&format!(
            "{} · {count} recent games",
            fox::display_text(&found.account)
        ));
        let rows: Vec<FoxRow> = found.rows.iter().map(list_row).collect();
        self.replace_games(found.rows, &rows);
        widgets.stack.set_visible_child(&widgets.results_page);
        self.refresh_actions();
    }

    fn restore_last_search(&self) {
        let Some(last) = cached_search() else {
            return;
        };
        self.widgets().entry.set_text(&last.query);
        self.show_search(Games {
            account: last.account,
            rows: last.rows,
        });
    }

    fn start_download(&self) {
        if self.is_busy() {
            return;
        }
        let widgets = self.widgets();
        let Some(game) = self.selected_game() else {
            return;
        };

        widgets.banner.set_revealed(false);
        widgets.loading_page.set_title("Downloading game");
        widgets.loading_page.set_description(Some(&game.matchup()));
        widgets.stack.set_visible_child(&widgets.loading_page);
        self.set_busy(true);

        let weak = self.downgrade();
        let task = glib::spawn_future_local(async move {
            let result = fetch_game(&game).await;
            if let Some(dialog) = weak.upgrade() {
                dialog.finish_download(result);
            }
        });
        self.replace_task(task);
    }

    fn finish_download(&self, result: Result<DownloadedGame, FoxError>) {
        match result {
            Ok(game) => {
                self.open_game(game);
                self.close();
            }
            Err(error) => {
                self.set_busy(false);
                let widgets = self.widgets();
                widgets.stack.set_visible_child(&widgets.results_page);
                widgets
                    .banner
                    .set_title(&format!("Could not download the game: {error}"));
                widgets.banner.set_revealed(true);
            }
        }
    }
}

/// Shows the picker for `parent`'s window, reusing the one kept in `slot`.
///
/// One picker per window: libadwaita refuses to present the same dialog in two
/// windows at once, and several mirai windows are normal. Keeping it also keeps
/// its record list, so opening it again costs nothing.
pub(crate) fn present(
    parent: &impl IsA<gtk::Widget>,
    slot: &std::cell::RefCell<Option<FoxPickerDialog>>,
    on_open: impl Fn(DownloadedGame) + 'static,
) {
    let dialog = slot
        .borrow_mut()
        .get_or_insert_with(FoxPickerDialog::wired)
        .clone();
    dialog.install_handler(on_open);
    dialog.prepare_to_show();
    dialog.present(Some(parent));
    // Populate only once the dialog has a viewport. A list view whose rows have
    // never been measured tracks its whole model, so filling it beforehand
    // builds every row widget inside the layout pass that shows the dialog.
    if !dialog.has_games() {
        dialog.restore_last_search();
    }
    dialog.widgets().entry.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> FoxGame {
        FoxGame {
            chess_id: "abc".into(),
            black_nick: "柯洁".into(),
            black_en_name: String::new(),
            white_nick: "申真谞".into(),
            white_en_name: String::new(),
            black_dan: 23,
            white_dan: 23,
            black_occ: 0,
            white_occ: 0,
            winner: 1,
            point: 75,
            moves: 241,
            board_size: 19,
            date: "2024-01-01 12:00:00".into(),
            title: String::new(),
        }
    }

    #[test]
    fn a_list_row_reads_as_one_line_of_plain_text() {
        let row = list_row(&row());
        assert_eq!(row.title(), "柯洁 (6d) vs 申真谞 (6d)");
        assert_eq!(
            row.subtitle(),
            "2024-01-01 12:00:00 · 19×19 · 241 moves · B+0.75"
        );
    }

    #[test]
    fn last_search_round_trips_through_json() {
        // Reopening the picker must not require another Fox lookup: the last successful
        // query and its rows are written to a cache file and read back as-is.
        let dir = std::env::temp_dir().join(format!("mirai-fox-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fox-last-search.json");
        let search = LastSearch {
            query: "柯洁".into(),
            account: "柯洁".into(),
            rows: vec![row()],
        };
        write_last_search(&path, &search).expect("write cache");
        assert_eq!(read_last_search(&path).as_ref(), Some(&search));

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            read_last_search(&path),
            None,
            "a corrupt cache must be ignored, not fatal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
