// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The GTK half of Fox Go lookup: a libsoup transport, the saved searches, and the picker
//! dialog.
//!
//! The endpoints, the reply shapes and the SGF dialect live in [`mirai_client::fox`], which
//! the HarmonyOS client uses as well. This file does not reimplement any of them — it cannot
//! even use that module's `Fetch` trait, because libsoup's futures are `!Send`, so it
//! composes the URL builders with the pure parsers instead.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use adw::prelude::*;
use glib::clone;
use glib::translate::IntoGlib;
use gtk::{gio, glib};
use mirai_client::fox;
pub(crate) use mirai_client::fox::FoxGame;
use mirai_client::play::{Outcome, outcome};
use mirai_core::GameTree;
use serde::{Deserialize, Serialize};
use soup::prelude::*;

use crate::config::Config;
use crate::fox_picker::{FoxPickerDialog, RecentText, RecordText};
use crate::i18n;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const RETRIES: u32 = 3;
const RETRY_BASE: Duration = Duration::from_millis(350);
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum FoxError {
    #[error("Enter an exact Fox nickname or numeric UID")]
    EmptyQuery,
    #[error("This player has hidden their game records")]
    HiddenRecords,
    /// A message from Fox, or from the Fox parsers in `mirai-client`.
    #[error("{0}")]
    Service(String),
    #[error("Fox returned invalid data: the player UID is missing")]
    MissingUid,
    #[error("Fox returned invalid data: {0}")]
    InvalidData(String),
    #[error("Could not reach Fox: request failed")]
    RequestFailed,
    #[error("Could not reach Fox: response was not UTF-8 ({0})")]
    NotUtf8(String),
    #[error("Could not reach Fox: response was unexpectedly large")]
    ResponseTooLarge,
    #[error("Could not reach Fox: request timed out")]
    TimedOut,
    #[error("Could not reach Fox: {0}")]
    Io(String),
}

/// The sentence shown for `error`. Server and I/O detail stays inside `{error}`.
fn fox_error_message(error: &FoxError) -> String {
    match error {
        FoxError::EmptyQuery => i18n::gettext("Enter an exact Fox nickname or numeric UID"),
        FoxError::HiddenRecords => i18n::gettext("This player has hidden their game records"),
        // Parser and server text from mirai-client. The status-page title is the frame.
        FoxError::Service(detail) => detail.clone(),
        FoxError::MissingUid => {
            i18n::gettext("Fox returned invalid data: the player UID is missing")
        }
        FoxError::InvalidData(detail) => {
            i18n::gettext_f("Fox returned invalid data: {error}", &[("error", detail)])
        }
        FoxError::RequestFailed => i18n::gettext("Could not reach Fox: request failed"),
        FoxError::NotUtf8(detail) => i18n::gettext_f(
            "Could not reach Fox: response was not UTF-8 ({error})",
            &[("error", detail)],
        ),
        FoxError::ResponseTooLarge => {
            i18n::gettext("Could not reach Fox: response was unexpectedly large")
        }
        FoxError::TimedOut => i18n::gettext("Could not reach Fox: request timed out"),
        FoxError::Io(detail) => {
            i18n::gettext_f("Could not reach Fox: {error}", &[("error", detail)])
        }
    }
}

/// Searches kept, most recent first: room for the players someone follows.
const SAVED_SEARCHES: usize = 20;

/// One search Fox answered, kept so that asking it again needs no lookup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SavedSearch {
    /// What was typed, trimmed: the key the search is found again by.
    query: String,
    /// The player as Fox names them, or "UID" and the number.
    account: String,
    /// When Fox answered, in seconds since the Unix epoch.
    saved: i64,
    rows: Vec<FoxGame>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct SearchHistory {
    /// Most recent first, at most [`SAVED_SEARCHES`], one per query.
    searches: Vec<SavedSearch>,
}

/// A change to the history, kept as a value so one made before the file has been read can
/// be applied again on top of what it holds.
#[derive(Clone, Debug)]
enum HistoryOp {
    /// Puts a search first, replacing an older one for the same query.
    Remember(SavedSearch),
    /// Moves the search for a query first.
    Touch(String),
    Forget(String),
}

impl SearchHistory {
    fn find(&self, query: &str) -> Option<&SavedSearch> {
        self.searches.iter().find(|search| search.query == query)
    }

    fn position(&self, query: &str) -> Option<usize> {
        self.searches
            .iter()
            .position(|search| search.query == query)
    }

    /// Applies `op`; false if it changed nothing.
    fn apply(&mut self, op: &HistoryOp) -> bool {
        match op {
            HistoryOp::Remember(search) => {
                if let Some(i) = self.position(&search.query) {
                    self.searches.remove(i);
                }
                self.searches.insert(0, search.clone());
                self.searches.truncate(SAVED_SEARCHES);
                true
            }
            HistoryOp::Touch(query) => match self.position(query) {
                Some(0) | None => false,
                Some(i) => {
                    let search = self.searches.remove(i);
                    self.searches.insert(0, search);
                    true
                }
            },
            HistoryOp::Forget(query) => match self.position(query) {
                Some(i) => {
                    self.searches.remove(i);
                    true
                }
                None => false,
            },
        }
    }
}

struct Games {
    account: String,
    rows: Vec<FoxGame>,
}

pub(crate) struct DownloadedGame {
    pub(crate) tree: GameTree,
    pub(crate) label: String,
}

fn history_path() -> Option<PathBuf> {
    Config::data_dir()
        .ok()
        .map(|dir| dir.join("fox-searches.json"))
}

/// The history every window's picker shares.
struct HistoryCache {
    /// False until the file has been read. The static starts cold so that no picker reads
    /// it on the GTK thread; it is never held across file I/O, so the GTK thread never
    /// waits on a disk either (INV-11).
    loaded: bool,
    /// Bumped on every change, so a picker knows when its rows are stale.
    revision: u64,
    value: Arc<SearchHistory>,
    /// Changes made before the file was read, to apply on top of it: what one changes is
    /// not known until then. `value` holds nothing meanwhile.
    pending: Vec<HistoryOp>,
}

static HISTORY: LazyLock<Mutex<HistoryCache>> = LazyLock::new(|| {
    Mutex::new(HistoryCache {
        loaded: false,
        revision: 0,
        value: Arc::default(),
        pending: Vec::new(),
    })
});

/// Serialises history writes. A later change must not be overwritten by an earlier write
/// that finishes second; the writer takes the history under this lock.
static WRITE_GATE: Mutex<()> = Mutex::new(());

fn history_lock() -> std::sync::MutexGuard<'static, HistoryCache> {
    HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read_history(path: &Path) -> Option<SearchHistory> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_history(path: &Path, history: &SearchHistory) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string(history).map_err(|error| error.to_string())?;
    mirai_proto::atomic::write_atomic(path, text.as_bytes()).map_err(|error| error.to_string())
}

/// The history as it stands, and its revision; `None` until the file has been read.
fn history() -> Option<(u64, Arc<SearchHistory>)> {
    let cache = history_lock();
    cache
        .loaded
        .then(|| (cache.revision, Arc::clone(&cache.value)))
}

/// Reads the file once, on the blocking pool, and applies any change made meanwhile.
fn load_history() {
    if history_lock().loaded {
        return;
    }
    let mut read = history_path()
        .and_then(|path| read_history(&path))
        .unwrap_or_default();
    let changed = {
        let mut cache = history_lock();
        if cache.loaded {
            return;
        }
        let changed = std::mem::take(&mut cache.pending)
            .iter()
            .fold(false, |changed, op| read.apply(op) | changed);
        cache.value = Arc::new(read);
        cache.loaded = true;
        cache.revision += 1;
        changed
    };
    if changed {
        write_history_now();
    }
}

/// Writes the history as it now stands. Blocking: syncs the file and its directory.
fn write_history_now() {
    let _gate = WRITE_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some((_, history)) = history() else {
        return;
    };
    let Some(path) = history_path() else {
        return;
    };
    if let Err(error) = write_history(&path, &history) {
        tracing::warn!(%error, "could not save the Fox searches");
    }
}

/// Changes the history in memory at once and writes it on the runtime's blocking pool,
/// where syncing the file costs no frame. Before the file has been read the change waits
/// in `pending`, and [`load_history`] applies and writes it.
fn change_history(op: HistoryOp, runtime: &tokio::runtime::Handle) {
    {
        let mut cache = history_lock();
        if !cache.loaded {
            cache.pending.push(op);
            return;
        }
        if !Arc::make_mut(&mut cache.value).apply(&op) {
            return;
        }
        cache.revision += 1;
    }
    runtime.spawn_blocking(write_history_now);
}

/// The search saved for `query`, if the history is loaded and holds one.
fn saved_search(query: &str) -> Option<SavedSearch> {
    history()?.1.find(query).cloned()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// When a search was saved, in local time, as Fox writes its own dates.
fn saved_time(saved: i64) -> String {
    glib::DateTime::from_unix_local(saved)
        .and_then(|time| time.format("%Y-%m-%d %H:%M"))
        .map(String::from)
        .unwrap_or_default()
}

/// Fetches `url` as text, retrying a few times with a growing pause.
///
/// libsoup rather than a Rust HTTP crate: its futures run on the GLib main context where
/// the dialog lives, and it takes the desktop's proxy settings from GIO. Not
/// `gio::File::for_uri`, which reaches `https://` only through GVfs's daemon — libsoup
/// underneath, and absent on desktops that do not install it.
async fn get_body(url: &str) -> Result<String, FoxError> {
    // One session per lookup, so a retry reuses its connection.
    let session = soup::Session::builder()
        .user_agent(fox::user_agent())
        .build();
    let mut last_error = FoxError::RequestFailed;
    for attempt in 1..=RETRIES {
        match glib::future_with_timeout(REQUEST_TIMEOUT, fetch(&session, url)).await {
            Ok(Ok(body)) => match String::from_utf8(body) {
                Ok(text) => return Ok(text),
                Err(error) => last_error = FoxError::NotUtf8(error.to_string()),
            },
            Ok(Err(BodyError::TooLarge)) => {
                last_error = FoxError::ResponseTooLarge;
            }
            Ok(Err(BodyError::Status(status))) => {
                last_error = FoxError::Io(format!("HTTP {status}"));
            }
            Ok(Err(BodyError::Io(error))) => last_error = FoxError::Io(error),
            Err(_) => last_error = FoxError::TimedOut,
        }
        if attempt < RETRIES {
            glib::timeout_future(RETRY_BASE * attempt).await;
        }
    }
    Err(last_error)
}

#[derive(Debug)]
enum BodyError {
    TooLarge,
    /// The server answered with something other than a 2xx.
    Status(i32),
    Io(String),
}

/// GETs `url` and reads its body, capped at [`MAX_RESPONSE_BYTES`].
async fn fetch(session: &soup::Session, url: &str) -> Result<Vec<u8>, BodyError> {
    let message =
        soup::Message::new("GET", url).map_err(|error| BodyError::Io(error.to_string()))?;
    let body = session
        .send_future(&message, glib::Priority::DEFAULT)
        .await
        .map_err(|error| BodyError::Io(error.to_string()))?;
    // libsoup hands over the body whatever the status; an error page is not Fox's reply.
    let status = message.status().into_glib();
    if !(200..300).contains(&status) {
        return Err(BodyError::Status(status));
    }
    read_capped(&body, MAX_RESPONSE_BYTES).await
}

/// Reads `stream` whole, giving up as soon as it has seen more than `cap` bytes.
///
/// Streamed rather than read in one call (`send_and_read`), which buffers the entire body
/// before its size can be checked: an endless or hostile response must cost at most `cap`
/// bytes of memory.
async fn read_capped(stream: &gio::InputStream, cap: usize) -> Result<Vec<u8>, BodyError> {
    const CHUNK: usize = 64 * 1024;
    let mut body = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(CHUNK, glib::Priority::DEFAULT)
            .await
            .map_err(|error| BodyError::Io(error.to_string()))?;
        if chunk.is_empty() {
            return Ok(body);
        }
        if chunk.len() > cap - body.len() {
            return Err(BodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
}

async fn search_games(query: &str) -> Result<Games, FoxError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(FoxError::EmptyQuery);
    }

    // A numeric query is already a UID; looking it up as a nickname would only fail.
    let (uid, account) = if query.bytes().all(|b| b.is_ascii_digit()) {
        (
            query.to_string(),
            i18n::gettext_f("UID {uid}", &[("uid", query)]),
        )
    } else {
        let body = get_body(&fox::user_url(query)).await?;
        let user = fox::parse_user(&body, query).map_err(FoxError::Service)?;
        if user.hidden {
            return Err(FoxError::HiddenRecords);
        }
        if user.uid.is_empty() {
            return Err(FoxError::MissingUid);
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

/// How one record reads in the list. Plain text: the rows do not parse markup, so a nickname
/// with `<` in it is not a problem.
fn record_text(game: &FoxGame) -> RecordText {
    let mut details = Vec::with_capacity(5);
    if !game.date.is_empty() {
        details.push(fox::display_text(&game.date));
    }
    details.push(format!("{}×{}", game.board_size, game.board_size));
    let moves = game.moves.to_string();
    details.push(i18n::ngettext_f(
        "{moves} move",
        "{moves} moves",
        game.moves as u64,
        &[("moves", &moves)],
    ));
    // Compact SGF notation (B+3.5, W+R) where it says everything; words where it cannot.
    let result = game.result();
    details.push(match outcome(&result) {
        Outcome::None | Outcome::Win(_) => i18n::result_phrase(&result),
        _ => result,
    });
    if !game.title.is_empty() {
        details.push(fox::display_text(&game.title));
    }
    RecordText {
        title: game.matchup(),
        subtitle: details.join(" · "),
    }
}

/// How a saved search reads in the recent list.
fn recent_text(search: &SavedSearch) -> RecentText {
    let count = search.rows.len();
    RecentText {
        query: search.query.clone(),
        title: fox::display_text(&search.account),
        // Translators: {time} is when Fox sent the games, such as 2026-07-29 22:57.
        subtitle: i18n::ngettext_f(
            "{count} game · saved {time}",
            "{count} games · saved {time}",
            count as u64,
            &[
                ("count", &count.to_string()),
                ("time", &saved_time(search.saved)),
            ],
        ),
    }
}

impl FoxPickerDialog {
    fn wired() -> Self {
        let dialog = FoxPickerDialog::new();
        let widgets = dialog.widgets();
        widgets.stack.set_visible_child(&widgets.status_page);

        widgets.entry.connect_search_changed(clone!(
            #[weak]
            dialog,
            move |_| dialog.show_idle_page()
        ));
        widgets.entry.connect_activate(clone!(
            #[weak]
            dialog,
            move |_| dialog.start_search()
        ));
        widgets.search_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| dialog.start_search()
        ));
        widgets.newer_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| dialog.turn_page(false)
        ));
        widgets.older_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| dialog.turn_page(true)
        ));
        widgets.refresh_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| dialog.fetch_search(dialog.shown_query())
        ));
        // Rows and their buttons name what they act on as their action's target — the row of
        // the page, the saved search — so a row refilled with another record or search needs
        // no handler of its own.
        let open_record = gio::SimpleAction::new("open-record", Some(glib::VariantTy::UINT32));
        open_record.connect_activate(clone!(
            #[weak]
            dialog,
            move |_, slot| {
                if let Some(game) = slot
                    .and_then(|slot| slot.get::<u32>())
                    .and_then(|slot| dialog.game_at(slot as usize))
                {
                    dialog.start_download(game);
                }
            }
        ));
        let open_saved = gio::SimpleAction::new("open-saved", Some(glib::VariantTy::STRING));
        open_saved.connect_activate(clone!(
            #[weak]
            dialog,
            move |_, query| {
                if let Some(query) = query.and_then(|query| query.str()) {
                    dialog.open_saved(query);
                }
            }
        ));
        let forget = gio::SimpleAction::new("forget", Some(glib::VariantTy::STRING));
        forget.connect_activate(clone!(
            #[weak]
            dialog,
            move |_, query| {
                if let Some(query) = query.and_then(|query| query.str()) {
                    dialog.forget(query);
                }
            }
        ));
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&open_record);
        actions.add_action(&open_saved);
        actions.add_action(&forget);
        dialog.insert_action_group("picker", Some(&actions));
        dialog.connect_closed(clone!(
            #[weak]
            dialog,
            move |_| {
                // The dialog outlives its own close, so an in-flight request has
                // to be dropped here rather than by disposal.
                dialog.abort_task();
                dialog.set_busy(false);
                dialog.set_shown(false);
            }
        ));
        dialog
    }

    /// Puts the dialog back into a state fit to be shown again: whatever was loading is
    /// dropped, the page it shows is the one its entry asks for, and its rows come back
    /// once it is on screen (`fox_picker` module docs).
    fn prepare_to_show(&self) {
        self.abort_task();
        self.set_busy(false);
        self.widgets().banner.set_revealed(false);
        self.refill();
        self.show_idle_page();
    }

    fn refresh_actions(&self) {
        let widgets = self.widgets();
        let idle = !self.is_busy();
        // Read-only rather than insensitive while busy: an insensitive entry gives up the
        // focus, and GTK moves it on — possibly to a record, where Enter opens a game.
        widgets.entry.set_editable(idle);
        widgets
            .search_button
            .set_sensitive(idle && !widgets.entry.text().trim().is_empty());
        widgets.refresh_button.set_sensitive(idle);
    }

    fn set_busy(&self, busy: bool) {
        if busy {
            self.set_busy_text(self.widgets().entry.text().to_string());
        }
        self.set_busy_flag(busy);
        self.refresh_actions();
    }

    /// Shows what the dialog holds for the entry's text while nothing loads: the records
    /// found for it, a failure reported for it, the saved searches it matches — every one
    /// when it is empty — or, failing all of those, how to search.
    fn show_idle_page(&self) {
        if self.is_busy() {
            // Only the clear icon gets past a read-only entry; what is loading is still
            // what it read.
            let entry = self.widgets().entry;
            let text = self.busy_text();
            if entry.text() != text {
                entry.set_text(&text);
            }
            return;
        }
        let widgets = self.widgets();
        let text = widgets.entry.text();
        let text = text.trim();
        if self.has_games() && text == self.shown_query() {
            widgets.stack.set_visible_child(&widgets.results_page);
        } else if !text.is_empty() && text == self.status_query() {
            widgets.stack.set_visible_child(&widgets.status_page);
        } else {
            self.sync_recent();
            if self.filter_recent(text) > 0 {
                widgets.stack.set_visible_child(&widgets.recent_page);
            } else {
                // The status page stops reporting on the query it held.
                self.set_status_query(String::new());
                self.show_status(
                    "system-search-symbolic",
                    &i18n::gettext("Find a Fox Player"),
                    &i18n::gettext(
                        "Enter an exact nickname or UID to browse their recent public games.",
                    ),
                );
            }
        }
        self.refresh_actions();
    }

    fn show_status(&self, icon: &str, title: &str, description: &str) {
        let widgets = self.widgets();
        widgets.status_page.set_icon_name(Some(icon));
        widgets.status_page.set_title(title);
        widgets.status_page.set_description(Some(description));
        widgets.stack.set_visible_child(&widgets.status_page);
        self.refresh_actions();
    }

    /// Brings the recent rows up to the history, if it has changed since they were filled.
    fn sync_recent(&self) {
        let Some((revision, history)) = history() else {
            return;
        };
        if self.recent_revision() != Some(revision) {
            self.set_recent(revision, history.searches.iter().map(recent_text).collect());
        }
    }

    fn turn_page(&self, older: bool) {
        let page = self.page();
        self.show_page(if older {
            page + 1
        } else {
            page.saturating_sub(1)
        });
        self.refresh_actions();
    }

    /// Searches for the entry's text. A search made before answers at once, with the
    /// records Fox sent then; the results' refresh button is what asks Fox again.
    fn start_search(&self) {
        if self.is_busy() {
            return;
        }
        let query = self.widgets().entry.text().trim().to_string();
        match saved_search(&query) {
            Some(saved) => self.open_saved_search(saved),
            None => self.fetch_search(query),
        }
    }

    /// Shows the saved search for `query`, picked from the recent list. Another window's
    /// picker may have forgotten it since the list was filled; then the list catches up.
    fn open_saved(&self, query: &str) {
        match saved_search(query) {
            Some(saved) => self.open_saved_search(saved),
            None => self.show_idle_page(),
        }
    }

    fn open_saved_search(&self, saved: SavedSearch) {
        change_history(HistoryOp::Touch(saved.query.clone()), self.runtime());
        self.widgets().banner.set_revealed(false);
        self.show_saved(saved);
    }

    /// Forgets the saved search for `query`, and the records this dialog holds for it:
    /// typing the query again must ask Fox, not show what was just forgotten.
    fn forget(&self, query: &str) {
        change_history(HistoryOp::Forget(query.to_string()), self.runtime());
        if self.shown_query() == query {
            self.clear_games();
        }
        if self.status_query() == query {
            self.set_status_query(String::new());
        }
        self.show_idle_page();
    }

    /// Asks Fox for `query`'s records.
    fn fetch_search(&self, query: String) {
        if self.is_busy() || query.is_empty() {
            return;
        }
        let widgets = self.widgets();
        widgets.banner.set_revealed(false);
        widgets
            .loading_page
            .set_title(&i18n::gettext("Searching Fox"));
        widgets.loading_page.set_description(Some(&i18n::gettext(
            "Looking up the player and their recent games.",
        )));
        widgets.stack.set_visible_child(&widgets.loading_page);
        self.set_busy(true);

        let weak = self.downgrade();
        let runtime = self.runtime().clone();
        let task = glib::spawn_future_local(async move {
            let result = search_games(&query).await;
            // A failed lookup falls back to the saved search, so the history must have
            // been read by then; a search made the moment the picker first opened may
            // have outrun the read.
            if result.is_err() && history().is_none() {
                let _ = runtime.spawn_blocking(load_history).await;
            }
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
                let saved = SavedSearch {
                    query,
                    account: found.account,
                    saved: unix_now(),
                    rows: found.rows,
                };
                // An account with no public games takes no place in the history, and one
                // that has lost them all loses its old records: only asking Fox again can
                // tell whether it has some yet.
                let op = if saved.rows.is_empty() {
                    HistoryOp::Forget(saved.query.clone())
                } else {
                    HistoryOp::Remember(saved.clone())
                };
                change_history(op, self.runtime());
                self.show_saved(saved);
            }
            // Offline, a search made before still shows what Fox sent then.
            Err(error) => match saved_search(&query) {
                Some(saved) => {
                    let time = saved_time(saved.saved);
                    self.show_saved(saved);
                    let widgets = self.widgets();
                    // Translators: {error} is why Fox could not be asked; {time} is when
                    // the games shown were saved, such as 2026-07-29 22:57.
                    widgets.banner.set_title(&i18n::gettext_f(
                        "{error}. Showing the games saved {time}.",
                        &[("error", &fox_error_message(&error)), ("time", &time)],
                    ));
                    widgets.banner.set_revealed(true);
                }
                None => {
                    self.clear_games();
                    self.set_status_query(query);
                    self.show_status(
                        "dialog-warning-symbolic",
                        &i18n::gettext("Couldn’t load games"),
                        &fox_error_message(&error),
                    );
                }
            },
        }
    }

    /// Shows a search's records from the first page, and puts its query in the entry. The
    /// records go in first, so the entry's change finds them its own.
    fn show_saved(&self, saved: SavedSearch) {
        let widgets = self.widgets();
        if saved.rows.is_empty() {
            self.clear_games();
            self.set_status_query(saved.query.clone());
            if widgets.entry.text().trim() != saved.query {
                widgets.entry.set_text(&saved.query);
            }
            self.show_status(
                "edit-find-symbolic",
                &i18n::gettext("No public games"),
                &i18n::gettext("This account has no visible games in Fox's recent-history window."),
            );
            return;
        }
        self.set_status_query(String::new());

        let count = saved.rows.len() as u64;
        let account = fox::display_text(&saved.account);
        // Translators: {account} is a player name, or "UID" and a number.
        let heading = i18n::ngettext_f(
            "{account} · {count} recent game",
            "{account} · {count} recent games",
            count,
            &[("account", &account), ("count", &count.to_string())],
        );
        // The group's title and description are markup; a nickname is plain text.
        widgets
            .result_group
            .set_title(&glib::markup_escape_text(&heading));
        // Translators: {time} is when Fox sent these games, such as 2026-07-29 22:57.
        widgets.result_group.set_description(Some(&i18n::gettext_f(
            "Saved {time}",
            &[("time", &saved_time(saved.saved))],
        )));
        let texts: Vec<RecordText> = saved.rows.iter().map(record_text).collect();
        self.replace_games(saved.query.clone(), saved.rows, texts);
        if widgets.entry.text().trim() != saved.query {
            widgets.entry.set_text(&saved.query);
        }
        widgets.stack.set_visible_child(&widgets.results_page);
        self.refresh_actions();
    }

    /// Reads the saved searches if no picker has yet — on the blocking pool — then shows
    /// the most recent one if the dialog has nothing else to show. The wait is not the
    /// dialog's task: a search started meanwhile must not cancel it.
    fn restore_history(&self) {
        if history().is_some() {
            self.history_ready();
            return;
        }
        let weak = self.downgrade();
        let job = self.runtime().spawn_blocking(load_history);
        glib::spawn_future_local(async move {
            if job.await.is_ok()
                && let Some(dialog) = weak.upgrade()
            {
                dialog.history_ready();
            }
        });
    }

    fn history_ready(&self) {
        if self.is_busy() || !self.is_shown() {
            return;
        }
        if !self.has_games()
            && self.widgets().entry.text().trim().is_empty()
            && let Some((_, history)) = history()
            && let Some(last) = history.searches.first()
        {
            self.show_saved(last.clone());
            return;
        }
        self.show_idle_page();
    }

    fn start_download(&self, game: FoxGame) {
        if self.is_busy() {
            return;
        }
        let widgets = self.widgets();
        widgets.banner.set_revealed(false);
        widgets
            .loading_page
            .set_title(&i18n::gettext("Downloading game"));
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
                let widgets = self.widgets();
                widgets.stack.set_visible_child(&widgets.results_page);
                self.set_busy(false);
                widgets.banner.set_title(&i18n::gettext_f(
                    "Could not download the game: {error}",
                    &[("error", &fox_error_message(&error))],
                ));
                widgets.banner.set_revealed(true);
            }
        }
    }
}

/// Readies GIO's TLS backend on the blocking pool, once per process.
///
/// Otherwise the first `https://` connection does it on the GTK thread while libsoup builds
/// the TLS client: 20–35 ms, reading the system certificate bundle. glib-networking loads
/// that store twice, once for the default database and once more for the credentials every
/// connection shares; building a throwaway client connection here fills both, behind locks,
/// so a lookup that races this merely waits for it. Without glib-networking the constructor
/// just fails.
fn warm_tls(runtime: &tokio::runtime::Handle) {
    static WARM: std::sync::Once = std::sync::Once::new();
    WARM.call_once(|| {
        runtime.spawn_blocking(|| {
            let stream = gio::SimpleIOStream::new(
                &gio::MemoryInputStream::new(),
                &gio::MemoryOutputStream::new_resizable(),
            );
            let _ = gio::TlsClientConnection::new(&stream, None::<&gio::SocketConnectable>);
        });
    });
}

/// Shows the picker for `parent`'s window, reusing the one kept in `slot`.
///
/// One picker per window: libadwaita refuses to present the same dialog in two
/// windows at once, and several mirai windows are normal. Keeping it also keeps
/// its records and its page, so opening it again needs no lookup and no file read.
pub(crate) fn present(
    parent: &impl IsA<gtk::Widget>,
    slot: &std::cell::RefCell<Option<FoxPickerDialog>>,
    runtime: tokio::runtime::Handle,
    on_open: impl Fn(DownloadedGame) + 'static,
) {
    let dialog = slot
        .borrow_mut()
        .get_or_insert_with(FoxPickerDialog::wired)
        .clone();
    // Ctrl+Shift+O reaches the window even with the picker up. Preparing it again would
    // abort the search in flight.
    if dialog.is_shown() {
        return;
    }
    warm_tls(&runtime);
    dialog.set_runtime(runtime);
    dialog.install_handler(on_open);
    dialog.prepare_to_show();
    dialog.set_shown(true);
    dialog.present(Some(parent));
    // The first picker of the process reads the saved searches once it is on screen.
    dialog.restore_history();
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
    fn a_record_reads_as_one_line_of_plain_text() {
        let text = record_text(&row());
        assert_eq!(text.title, "柯洁 (6d) vs 申真谞 (6d)");
        assert_eq!(
            text.subtitle,
            "2024-01-01 12:00:00 · 19×19 · 241 moves · B+0.75"
        );
    }

    fn search(query: &str, saved: i64) -> SavedSearch {
        SavedSearch {
            query: query.into(),
            account: query.into(),
            saved,
            rows: vec![row()],
        }
    }

    fn queries(history: &SearchHistory) -> Vec<&str> {
        history.searches.iter().map(|s| s.query.as_str()).collect()
    }

    #[test]
    fn the_history_keeps_one_search_per_query_most_recent_first() {
        let mut history = SearchHistory::default();
        assert!(history.apply(&HistoryOp::Remember(search("柯洁", 1))));
        assert!(history.apply(&HistoryOp::Remember(search("申真谞", 2))));
        // Asking again replaces the older answer and moves it first.
        assert!(history.apply(&HistoryOp::Remember(search("柯洁", 3))));
        assert_eq!(queries(&history), ["柯洁", "申真谞"]);
        assert_eq!(history.find("柯洁").map(|s| s.saved), Some(3));

        // Opening a saved search moves it first; one already first changes nothing.
        assert!(history.apply(&HistoryOp::Touch("申真谞".into())));
        assert_eq!(queries(&history), ["申真谞", "柯洁"]);
        assert!(!history.apply(&HistoryOp::Touch("申真谞".into())));
        assert!(!history.apply(&HistoryOp::Touch("党毅飞".into())));

        assert!(history.apply(&HistoryOp::Forget("申真谞".into())));
        assert!(!history.apply(&HistoryOp::Forget("申真谞".into())));
        assert_eq!(queries(&history), ["柯洁"]);
    }

    #[test]
    fn the_history_drops_its_oldest_search_past_the_limit() {
        let mut history = SearchHistory::default();
        for i in 0..=SAVED_SEARCHES {
            history.apply(&HistoryOp::Remember(search(&i.to_string(), i as i64)));
        }
        assert_eq!(history.searches.len(), SAVED_SEARCHES);
        assert_eq!(history.searches[0].query, SAVED_SEARCHES.to_string());
        assert!(history.find("0").is_none(), "the oldest search goes");
        assert!(history.find("1").is_some());
    }

    #[test]
    fn the_history_round_trips_through_json() {
        // Asking for a saved search again must not need Fox: the history and its records
        // are written to a file and read back as-is.
        let dir = std::env::temp_dir().join(format!("mirai-fox-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fox-searches.json");
        let mut history = SearchHistory::default();
        history.apply(&HistoryOp::Remember(search("柯洁", 1_785_337_045)));
        history.apply(&HistoryOp::Remember(search("6757425", 1_785_337_046)));
        write_history(&path, &history).expect("write the history");
        assert_eq!(read_history(&path).as_ref(), Some(&history));

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            read_history(&path),
            None,
            "a corrupt history must be ignored, not fatal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads the file at `path` through [`read_capped`].
    fn read_file_capped(path: &Path, cap: usize) -> Result<Vec<u8>, BodyError> {
        glib::MainContext::new().block_on(async {
            let stream = gio::File::for_path(path)
                .read_future(glib::Priority::DEFAULT)
                .await
                .expect("open the file");
            read_capped(stream.upcast_ref(), cap).await
        })
    }

    #[test]
    fn an_endless_response_is_cut_off_at_the_cap() {
        // `/dev/zero` never ends: buffering it whole before checking its size would run the
        // process out of memory. The capped reader must stop once it passes the cap.
        let result = read_file_capped(Path::new("/dev/zero"), 1024 * 1024);
        assert!(matches!(result, Err(BodyError::TooLarge)), "{result:?}");
    }

    #[test]
    fn a_body_within_the_cap_is_returned_whole() {
        let path = std::env::temp_dir().join(format!("mirai-fox-body-{}", std::process::id()));
        let body: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        std::fs::write(&path, &body).unwrap();
        let exact = read_file_capped(&path, body.len());
        let over = read_file_capped(&path, body.len() - 1);
        let _ = std::fs::remove_file(&path);
        assert_eq!(exact.expect("a body exactly at the cap is accepted"), body);
        assert!(matches!(over, Err(BodyError::TooLarge)), "{over:?}");
    }

    #[test]
    fn only_a_2xx_body_is_taken_for_the_reply() {
        // libsoup hands over an error page's body like any other. Parsed as Fox's JSON, a
        // 503 would surface as a baffling parser message, or pass for an empty record list.
        let reply = br#"{"result":0,"chesslist":[]}"#;
        let (ok, busy) = glib::MainContext::new().block_on(async {
            let server = soup::Server::builder().build();
            server.add_handler(None, move |_, message, path, _| {
                let status = if path == "/busy" { 503 } else { 200 };
                message.set_status(status, None);
                message.set_response(Some("application/json"), soup::MemoryUse::Copy, reply);
            });
            server
                .listen_local(0, soup::ServerListenOptions::IPV4_ONLY)
                .expect("listen on loopback");
            let base = server.uris()[0].to_string();
            let session = soup::Session::new();
            // A proxy taken from the environment must not intercept a loopback request.
            session.set_proxy_resolver(None::<&gio::ProxyResolver>);
            let ok = fetch(&session, &format!("{base}ok")).await;
            let busy = fetch(&session, &format!("{base}busy")).await;
            (ok, busy)
        });
        assert_eq!(ok.expect("a 200 is the reply"), reply);
        assert!(matches!(busy, Err(BodyError::Status(503))), "{busy:?}");
    }
}
