// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The GTK half of searching Go servers for public game records: a libsoup transport, the
//! saved searches, and the picker dialog's behaviour.
//!
//! What each server's endpoints, replies and SGF dialect look like, and the steps a search
//! takes, live in [`mirai_client::kifu`] and the server modules beside it, which the
//! HarmonyOS client can use as well. This file lends them libsoup as their [`kifu::Fetch`],
//! and words, saves and shows what they find.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use adw::prelude::*;
use glib::clone;
use glib::translate::IntoGlib;
use gtk::{gio, glib};
use mirai_client::kifu::{self, Player, Record, Server, Source};
use mirai_client::play::{Outcome, outcome};
use mirai_core::GameTree;
use serde::{Deserialize, Serialize};
use soup::prelude::*;

use crate::config::Config;
use crate::i18n;
use crate::kifu_picker::{KifuPickerDialog, RecentText, RecordText, SearchKey};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const RETRIES: u32 = 3;
const RETRY_BASE: Duration = Duration::from_millis(350);
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Why a request did not complete.
#[derive(Debug)]
enum TransportError {
    RequestFailed,
    NotUtf8(String),
    TooLarge,
    TimedOut,
    Io(String),
}

type LookupError = kifu::Error<TransportError>;

/// A server's name, as the toggles above the search show it.
fn server_name(server: Server) -> String {
    match server {
        Server::Fox => i18n::pgettext("server", "Fox"),
        Server::Eweiqi => i18n::pgettext("server", "eWeiqi"),
        Server::Yike => i18n::pgettext("server", "Yike"),
    }
}

/// The sentence shown for `error` from `server`. Server and I/O detail stays inside
/// `{error}`.
fn error_message(server: Server, error: &LookupError) -> String {
    let name = server_name(server);
    let server = ("server", name.as_str());
    match error {
        kifu::Error::Transport(TransportError::RequestFailed) => {
            i18n::gettext_f("Could not reach {server}: request failed", &[server])
        }
        kifu::Error::Transport(TransportError::NotUtf8(detail)) => i18n::gettext_f(
            "Could not reach {server}: response was not UTF-8 ({error})",
            &[server, ("error", detail)],
        ),
        kifu::Error::Transport(TransportError::TooLarge) => i18n::gettext_f(
            "Could not reach {server}: response was unexpectedly large",
            &[server],
        ),
        kifu::Error::Transport(TransportError::TimedOut) => {
            i18n::gettext_f("Could not reach {server}: request timed out", &[server])
        }
        kifu::Error::Transport(TransportError::Io(detail)) => i18n::gettext_f(
            "Could not reach {server}: {error}",
            &[server, ("error", detail)],
        ),
        // Parser and server text from mirai-client. The status-page title is the frame.
        kifu::Error::Service(detail) => detail.clone(),
        kifu::Error::Invalid(detail) => i18n::gettext_f(
            "{server} returned invalid data: {error}",
            &[server, ("error", detail)],
        ),
        kifu::Error::NoPlayer => i18n::gettext_f("{server} has no player by that name", &[server]),
        kifu::Error::Hidden => i18n::gettext("This player has hidden their game records"),
    }
}

/// How the dialog asks for a search on each server, and what it promises to find.
struct ServerTexts {
    placeholder: String,
    caption: String,
    title: String,
    description: String,
}

fn server_texts(server: Server) -> ServerTexts {
    match server {
        Server::Fox => ServerTexts {
            placeholder: i18n::gettext("Exact Fox nickname or numeric UID"),
            caption: i18n::gettext(
                "Public records only · Fox exposes at most the latest 200 games",
            ),
            title: i18n::gettext("Find a Fox Player"),
            description: i18n::gettext(
                "Enter an exact nickname or UID to browse their recent public games.",
            ),
        },
        Server::Eweiqi => ServerTexts {
            placeholder: i18n::gettext("Player name or nickname on eWeiqi"),
            caption: i18n::gettext(
                "Public tournament records only · names match in part · at most the latest 200 games",
            ),
            title: i18n::gettext("Search eWeiqi’s Records"),
            description: i18n::gettext(
                "Enter a player’s name or nickname to browse the tournament games eWeiqi publishes for them.",
            ),
        },
        Server::Yike => ServerTexts {
            placeholder: i18n::gettext("Yike nickname, account number or professional’s name"),
            caption: i18n::gettext("Public records only · at most the latest 100 games"),
            title: i18n::gettext("Find a Yike Player"),
            description: i18n::gettext(
                "Enter a nickname, an account number or a professional’s name to browse their recent games.",
            ),
        },
    }
}

/// Searches kept, most recent first: room for the players someone follows.
const SAVED_SEARCHES: usize = 20;

/// One search a server answered, kept so that asking it again needs no lookup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SavedSearch {
    server: Server,
    /// What was typed, trimmed: with the server, the key the search is found again by.
    query: String,
    /// Whose records these are; asking again asks for this player's.
    player: Player,
    /// When the server answered, in seconds since the Unix epoch.
    saved: i64,
    rows: Vec<Record>,
}

impl SavedSearch {
    fn key(&self) -> SearchKey {
        SearchKey {
            server: self.server,
            query: self.query.clone(),
        }
    }

    fn is(&self, key: &SearchKey) -> bool {
        self.server == key.server && self.query == key.query
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct SearchHistory {
    /// Most recent first, at most [`SAVED_SEARCHES`], one per server and query.
    searches: Vec<SavedSearch>,
}

/// A change to the history, kept as a value so one made before the file has been read can
/// be applied again on top of what it holds.
#[derive(Clone, Debug)]
enum HistoryOp {
    /// Puts a search first, replacing an older one for the same server and query.
    Remember(SavedSearch),
    /// Moves a search first.
    Touch(SearchKey),
    Forget(SearchKey),
}

impl SearchHistory {
    fn find(&self, key: &SearchKey) -> Option<&SavedSearch> {
        self.searches.iter().find(|search| search.is(key))
    }

    fn position(&self, key: &SearchKey) -> Option<usize> {
        self.searches.iter().position(|search| search.is(key))
    }

    /// Applies `op`; false if it changed nothing.
    fn apply(&mut self, op: &HistoryOp) -> bool {
        match op {
            HistoryOp::Remember(search) => {
                if let Some(i) = self.position(&search.key()) {
                    self.searches.remove(i);
                }
                self.searches.insert(0, search.clone());
                self.searches.truncate(SAVED_SEARCHES);
                true
            }
            HistoryOp::Touch(key) => match self.position(key) {
                Some(0) | None => false,
                Some(i) => {
                    let search = self.searches.remove(i);
                    self.searches.insert(0, search);
                    true
                }
            },
            HistoryOp::Forget(key) => match self.position(key) {
                Some(i) => {
                    self.searches.remove(i);
                    true
                }
                None => false,
            },
        }
    }
}

/// What a search came to: one player's records, or several players to choose between.
enum Found {
    Games(Player, Vec<Record>),
    Players(Vec<Player>),
}

pub(crate) struct DownloadedGame {
    pub(crate) tree: GameTree,
    pub(crate) label: String,
}

fn history_path() -> Option<PathBuf> {
    Config::data_dir()
        .ok()
        .map(|dir| dir.join("kifu-searches.json"))
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
        tracing::warn!(%error, "could not save the record searches");
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

/// The search saved under `key`, if the history is loaded and holds one.
fn saved_search(key: &SearchKey) -> Option<SavedSearch> {
    history()?.1.find(key).cloned()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// When a search was saved, in local time, as the servers write their own dates.
fn saved_time(saved: i64) -> String {
    glib::DateTime::from_unix_local(saved)
        .and_then(|time| time.format("%Y-%m-%d %H:%M"))
        .map(String::from)
        .unwrap_or_default()
}

/// libsoup as the servers' transport: GETs a URL as text, retrying a few times with a
/// growing pause.
///
/// libsoup rather than a Rust HTTP crate: its futures run on the GLib main context where
/// the dialog lives, and it takes the desktop's proxy settings from GIO. Not
/// `gio::File::for_uri`, which reaches `https://` only through GVfs's daemon — libsoup
/// underneath, and absent on desktops that do not install it. One per search, so its
/// requests and their retries share connections.
struct Soup(soup::Session);

impl Soup {
    fn new() -> Self {
        Soup(
            soup::Session::builder()
                .user_agent(kifu::user_agent())
                .build(),
        )
    }
}

impl kifu::Fetch for Soup {
    type Error = TransportError;

    async fn get(&self, url: &str) -> Result<String, TransportError> {
        let mut last_error = TransportError::RequestFailed;
        for attempt in 1..=RETRIES {
            match glib::future_with_timeout(REQUEST_TIMEOUT, fetch(&self.0, url)).await {
                Ok(Ok(body)) => match String::from_utf8(body) {
                    Ok(text) => return Ok(text),
                    Err(error) => last_error = TransportError::NotUtf8(error.to_string()),
                },
                Ok(Err(BodyError::TooLarge)) => last_error = TransportError::TooLarge,
                Ok(Err(BodyError::Status(status))) => {
                    last_error = TransportError::Io(format!("HTTP {status}"));
                }
                Ok(Err(BodyError::Io(error))) => last_error = TransportError::Io(error),
                Err(_) => last_error = TransportError::TimedOut,
            }
            if attempt < RETRIES {
                glib::timeout_future(RETRY_BASE * attempt).await;
            }
        }
        Err(last_error)
    }
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
    // libsoup hands over the body whatever the status; an error page is not the server's
    // reply.
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

/// The players `key` names, and the records of the one it names if there is only one.
async fn search(key: &SearchKey) -> Result<Found, LookupError> {
    let fetch = Soup::new();
    let mut players = kifu::players(&fetch, key.server, &key.query).await?;
    match players.len() {
        0 => Err(kifu::Error::NoPlayer),
        1 => {
            let player = players.remove(0);
            let rows = kifu::games(&fetch, &player).await?;
            Ok(Found::Games(player, rows))
        }
        _ => Ok(Found::Players(players)),
    }
}

async fn fetch_game(record: &Record) -> Result<DownloadedGame, LookupError> {
    let tree = kifu::download(&Soup::new(), record).await?;
    Ok(DownloadedGame {
        tree,
        label: record.matchup(),
    })
}

/// How a player reads in the heading over their records and in the recent list: their
/// name, or what they were found by when the server gave none.
fn account_label(player: &Player) -> String {
    if !player.name.is_empty() {
        return kifu::display_text(&player.name);
    }
    let id = kifu::display_text(&player.id);
    match player.source {
        Source::Fox => i18n::gettext_f("UID {uid}", &[("uid", &id)]),
        Source::YikeAccount => i18n::gettext_f("Account {id}", &[("id", &id)]),
        Source::Eweiqi | Source::YikeLibrary => id,
    }
}

/// How one player a search may mean reads in the list to choose from.
fn player_text(player: &Player) -> RecordText {
    let id = kifu::display_text(&player.id);
    let rank = kifu::display_text(&player.rank);
    let subtitle = match player.source {
        Source::YikeLibrary => i18n::gettext("Professional · Yike game library"),
        Source::YikeAccount if rank.is_empty() => i18n::gettext_f("Account {id}", &[("id", &id)]),
        // Translators: {rank} is a grade such as 2.6d.
        Source::YikeAccount => {
            i18n::gettext_f("Account {id} · {rank}", &[("id", &id), ("rank", &rank)])
        }
        Source::Fox | Source::Eweiqi => rank,
    };
    RecordText {
        title: account_label(player),
        subtitle,
    }
}

/// How one record reads in the list. Plain text: the rows do not parse markup, so a nickname
/// with `<` in it is not a problem.
fn record_text(record: &Record) -> RecordText {
    let mut details = Vec::with_capacity(5);
    if !record.date.is_empty() {
        details.push(kifu::display_text(&record.date));
    }
    if let Some(size) = record.board_size {
        details.push(format!("{size}×{size}"));
    }
    if let Some(moves) = record.moves {
        let count = moves.to_string();
        details.push(i18n::ngettext_f(
            "{moves} move",
            "{moves} moves",
            moves as u64,
            &[("moves", &count)],
        ));
    }
    // Compact SGF notation (B+3.5, W+R) where it says everything; words where it cannot.
    details.push(match outcome(&record.result) {
        Outcome::None | Outcome::Win(_) => i18n::result_phrase(&record.result),
        _ => kifu::display_text(&record.result),
    });
    if !record.event.is_empty() {
        details.push(kifu::display_text(&record.event));
    }
    RecordText {
        title: record.matchup(),
        subtitle: details.join(" · "),
    }
}

/// How a saved search reads in the recent list.
fn recent_text(search: &SavedSearch) -> RecentText {
    let count = search.rows.len();
    RecentText {
        query: search.query.clone(),
        title: account_label(&search.player),
        // Translators: {time} is when the server sent the games, such as 2026-07-29 22:57.
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

impl KifuPickerDialog {
    fn wired() -> Self {
        let dialog = KifuPickerDialog::new();
        let widgets = dialog.widgets();
        widgets.stack.set_visible_child(&widgets.status_page);

        widgets.server_group.connect_active_name_notify(clone!(
            #[weak]
            dialog,
            move |_| dialog.server_changed()
        ));
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
            move |_| dialog.search_again()
        ));
        // Rows and their buttons name what they act on as their action's target — the row of
        // the page, the saved search, the player — so a row refilled with another needs no
        // handler of its own.
        let open_record = gio::SimpleAction::new("open-record", Some(glib::VariantTy::UINT32));
        open_record.connect_activate(clone!(
            #[weak]
            dialog,
            move |_, slot| {
                if let Some(record) = slot
                    .and_then(|slot| slot.get::<u32>())
                    .and_then(|slot| dialog.game_at(slot as usize))
                {
                    dialog.start_download(record);
                }
            }
        ));
        let open_player = gio::SimpleAction::new("open-player", Some(glib::VariantTy::UINT32));
        open_player.connect_activate(clone!(
            #[weak]
            dialog,
            move |_, index| {
                if let Some(player) = index
                    .and_then(|index| index.get::<u32>())
                    .and_then(|index| dialog.player_at(index as usize))
                {
                    dialog.fetch_games(dialog.players_key(), player);
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
        actions.add_action(&open_player);
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
        dialog.sync_server_texts();
        dialog
    }

    /// The server the toggles above the search choose.
    fn server(&self) -> Server {
        self.widgets()
            .server_group
            .active_name()
            .and_then(|name| Server::from_name(&name))
            .unwrap_or_default()
    }

    /// Shows `server`'s toggle as the active one; changing it words the dialog for it.
    fn select_server(&self, server: Server) {
        if self.server() != server {
            self.widgets()
                .server_group
                .set_active_name(Some(server.as_str()));
        }
    }

    /// The search the entry's text names on the chosen server.
    fn entry_key(&self) -> SearchKey {
        SearchKey {
            server: self.server(),
            query: self.widgets().entry.text().trim().to_string(),
        }
    }

    fn sync_server_texts(&self) {
        let widgets = self.widgets();
        let texts = server_texts(self.server());
        widgets.entry.set_placeholder_text(Some(&texts.placeholder));
        widgets.caption.set_label(&texts.caption);
    }

    /// Words the dialog for the newly chosen server, and shows what it holds for the
    /// entry's text there.
    fn server_changed(&self) {
        self.sync_server_texts();
        self.widgets().banner.set_revealed(false);
        self.show_idle_page();
    }

    /// Puts the dialog back into a state fit to be shown again: whatever was loading is
    /// dropped, the page it shows is the one its entry asks for, and its rows come back
    /// once it is on screen (`kifu_picker` module docs).
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
        // What is loading was asked of the server chosen when it began.
        widgets.server_group.set_sensitive(idle);
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

    /// Shows what the dialog holds for the entry's text on the chosen server while nothing
    /// loads: the records found for it, the players it may mean, a failure reported for it,
    /// the saved searches it matches — every one when it is empty — or, failing all of
    /// those, how to search.
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
        let key = self.entry_key();
        if self.has_games() && key == self.shown_key() {
            widgets.stack.set_visible_child(&widgets.results_page);
        } else if self.has_players() && key == self.players_key() {
            widgets.stack.set_visible_child(&widgets.players_page);
        } else if !key.query.is_empty() && key == self.status_key() {
            widgets.stack.set_visible_child(&widgets.status_page);
        } else {
            self.sync_recent();
            if self.filter_recent(&key.query) > 0 {
                widgets.stack.set_visible_child(&widgets.recent_page);
            } else {
                // The status page stops reporting on the search it held.
                self.set_status_key(SearchKey::default());
                let texts = server_texts(key.server);
                self.show_status("system-search-symbolic", &texts.title, &texts.description);
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

    /// Brings the recent rows up to the history and the chosen server, if either has
    /// changed since they were filled.
    fn sync_recent(&self) {
        let Some((revision, history)) = history() else {
            return;
        };
        let server = self.server();
        if self.recent_stamp() != Some((revision, server)) {
            let entries = history
                .searches
                .iter()
                .filter(|search| search.server == server)
                .map(recent_text)
                .collect();
            self.set_recent((revision, server), entries);
        }
    }

    /// Searches for the entry's text. A search made before answers at once, with the
    /// records the server sent then; the results' refresh button is what asks it again.
    fn start_search(&self) {
        if self.is_busy() {
            return;
        }
        let key = self.entry_key();
        match saved_search(&key) {
            Some(saved) => self.open_saved_search(saved),
            None => self.fetch_search(key),
        }
    }

    /// Shows the saved search for `query` on the chosen server, picked from the recent
    /// list. Another window's picker may have forgotten it since the list was filled; then
    /// the list catches up.
    fn open_saved(&self, query: &str) {
        let key = SearchKey {
            server: self.server(),
            query: query.to_string(),
        };
        match saved_search(&key) {
            Some(saved) => self.open_saved_search(saved),
            None => self.show_idle_page(),
        }
    }

    fn open_saved_search(&self, saved: SavedSearch) {
        change_history(HistoryOp::Touch(saved.key()), self.runtime());
        self.widgets().banner.set_revealed(false);
        self.show_saved(saved);
    }

    /// Forgets the saved search for `query` on the chosen server, and the records and
    /// players this dialog holds for it: typing the query again must ask the server, not
    /// show what was just forgotten.
    fn forget(&self, query: &str) {
        let key = SearchKey {
            server: self.server(),
            query: query.to_string(),
        };
        change_history(HistoryOp::Forget(key.clone()), self.runtime());
        if self.shown_key() == key {
            self.clear_games();
        }
        if self.status_key() == key {
            self.set_status_key(SearchKey::default());
        }
        self.drop_players(&key);
        self.show_idle_page();
    }

    /// Drops the players `key` was found to mean. The list outranks a status page for the
    /// same words, so a later answer for them, or forgetting them, must take it away.
    fn drop_players(&self, key: &SearchKey) {
        if self.players_key() == *key {
            self.set_players(SearchKey::default(), Vec::new());
        }
    }

    /// Asks the server for the records the results show again: the same player's, without
    /// looking them up anew.
    fn search_again(&self) {
        let key = self.shown_key();
        match saved_search(&key) {
            Some(saved) => self.fetch_games(key, saved.player),
            None => self.fetch_search(key),
        }
    }

    fn show_loading(&self, server: Server) {
        let widgets = self.widgets();
        widgets.banner.set_revealed(false);
        widgets.loading_page.set_title(&i18n::gettext_f(
            "Searching {server}",
            &[("server", &server_name(server))],
        ));
        widgets.loading_page.set_description(Some(&i18n::gettext(
            "Looking up the player and their recent games.",
        )));
        widgets.stack.set_visible_child(&widgets.loading_page);
        self.set_busy(true);
    }

    /// Asks the server whom `key` names, and for their records when it names one.
    fn fetch_search(&self, key: SearchKey) {
        if self.is_busy() || key.query.is_empty() {
            return;
        }
        self.show_loading(key.server);
        let weak = self.downgrade();
        let runtime = self.runtime().clone();
        let task = glib::spawn_future_local(async move {
            let result = search(&key).await;
            finish(weak, runtime, key, result).await;
        });
        self.replace_task(task);
    }

    /// Asks the server for `player`'s records, as the search `key` found them.
    fn fetch_games(&self, key: SearchKey, player: Player) {
        if self.is_busy() || key.query.is_empty() {
            return;
        }
        self.show_loading(key.server);
        let weak = self.downgrade();
        let runtime = self.runtime().clone();
        let task = glib::spawn_future_local(async move {
            let fetch = Soup::new();
            let result = kifu::games(&fetch, &player)
                .await
                .map(|rows| Found::Games(player, rows));
            finish(weak, runtime, key, result).await;
        });
        self.replace_task(task);
    }

    fn finish_search(&self, key: SearchKey, result: Result<Found, LookupError>) {
        self.set_busy(false);
        if !matches!(result, Ok(Found::Players(_))) {
            // Whatever the answer, the earlier list of players no longer stands for these
            // words: the pick is kept with the records, and a failure must show.
            self.drop_players(&key);
        }
        match result {
            Ok(Found::Games(player, rows)) => {
                let saved = SavedSearch {
                    server: key.server,
                    query: key.query,
                    player,
                    saved: unix_now(),
                    rows,
                };
                // An account with no public games takes no place in the history, and one
                // that has lost them all loses its old records: only asking again can tell
                // whether it has some yet.
                let op = if saved.rows.is_empty() {
                    HistoryOp::Forget(saved.key())
                } else {
                    HistoryOp::Remember(saved.clone())
                };
                change_history(op, self.runtime());
                self.show_saved(saved);
            }
            Ok(Found::Players(players)) => {
                let texts = players
                    .into_iter()
                    .map(|player| {
                        let text = player_text(&player);
                        (player, text)
                    })
                    .collect();
                self.set_players(key, texts);
                let widgets = self.widgets();
                widgets.stack.set_visible_child(&widgets.players_page);
                self.refresh_actions();
            }
            // Offline, a search made before still shows what the server sent then.
            Err(error) => match saved_search(&key) {
                Some(saved) => {
                    let time = saved_time(saved.saved);
                    self.show_saved(saved);
                    let widgets = self.widgets();
                    // Translators: {error} is why the server could not be asked; {time} is
                    // when the games shown were saved, such as 2026-07-29 22:57.
                    widgets.banner.set_title(&i18n::gettext_f(
                        "{error}. Showing the games saved {time}.",
                        &[
                            ("error", &error_message(key.server, &error)),
                            ("time", &time),
                        ],
                    ));
                    widgets.banner.set_revealed(true);
                }
                None => {
                    self.clear_games();
                    let message = error_message(key.server, &error);
                    self.set_status_key(key);
                    self.show_status(
                        "dialog-warning-symbolic",
                        &i18n::gettext("Couldn’t load games"),
                        &message,
                    );
                }
            },
        }
    }

    /// Shows a search's records from the first page, on its server, and puts its query in
    /// the entry. The records go in first, so the entry's change finds them its own.
    fn show_saved(&self, saved: SavedSearch) {
        self.select_server(saved.server);
        let widgets = self.widgets();
        if saved.rows.is_empty() {
            self.clear_games();
            self.set_status_key(saved.key());
            if widgets.entry.text().trim() != saved.query {
                widgets.entry.set_text(&saved.query);
            }
            self.show_status(
                "edit-find-symbolic",
                &i18n::gettext("No public games"),
                &i18n::gettext_f(
                    "{server} lists no public games for this search.",
                    &[("server", &server_name(saved.server))],
                ),
            );
            return;
        }
        self.set_status_key(SearchKey::default());

        let count = saved.rows.len() as u64;
        let account = account_label(&saved.player);
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
        // Translators: {time} is when the server sent these games, such as 2026-07-29 22:57.
        widgets.result_group.set_description(Some(&i18n::gettext_f(
            "Saved {time}",
            &[("time", &saved_time(saved.saved))],
        )));
        let texts: Vec<RecordText> = saved.rows.iter().map(record_text).collect();
        self.replace_games(saved.key(), saved.rows, texts);
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
            && !self.has_players()
            && self.widgets().entry.text().trim().is_empty()
            && let Some((_, history)) = history()
            && let Some(last) = history.searches.first()
        {
            self.show_saved(last.clone());
            return;
        }
        self.show_idle_page();
    }

    fn start_download(&self, record: Record) {
        if self.is_busy() {
            return;
        }
        let widgets = self.widgets();
        widgets.banner.set_revealed(false);
        widgets
            .loading_page
            .set_title(&i18n::gettext("Downloading game"));
        widgets
            .loading_page
            .set_description(Some(&record.matchup()));
        widgets.stack.set_visible_child(&widgets.loading_page);
        self.set_busy(true);

        let weak = self.downgrade();
        let task = glib::spawn_future_local(async move {
            let result = fetch_game(&record).await;
            if let Some(dialog) = weak.upgrade() {
                dialog.finish_download(record.source.server(), result);
            }
        });
        self.replace_task(task);
    }

    fn finish_download(&self, server: Server, result: Result<DownloadedGame, LookupError>) {
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
                    &[("error", &error_message(server, &error))],
                ));
                widgets.banner.set_revealed(true);
            }
        }
    }
}

/// Hands a search's result to the dialog, if it is still there. A failed search falls back
/// to the saved one, so the history must have been read by then; a search made the moment
/// the picker first opened may have outrun the read.
async fn finish(
    weak: glib::WeakRef<KifuPickerDialog>,
    runtime: tokio::runtime::Handle,
    key: SearchKey,
    result: Result<Found, LookupError>,
) {
    if result.is_err() && history().is_none() {
        let _ = runtime.spawn_blocking(load_history).await;
    }
    if let Some(dialog) = weak.upgrade() {
        dialog.finish_search(key, result);
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
    slot: &std::cell::RefCell<Option<KifuPickerDialog>>,
    runtime: tokio::runtime::Handle,
    on_open: impl Fn(DownloadedGame) + 'static,
) {
    let dialog = slot
        .borrow_mut()
        .get_or_insert_with(KifuPickerDialog::wired)
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

    fn row() -> Record {
        Record {
            source: Source::Fox,
            id: "abc".into(),
            black: "柯洁".into(),
            black_rank: "6d".into(),
            white: "申真谞".into(),
            white_rank: "6d".into(),
            result: "B+0.75".into(),
            moves: Some(241),
            board_size: Some(19),
            date: "2024-01-01 12:00:00".into(),
            event: String::new(),
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
        // A list that does not give the size or the length says nothing of them.
        let mut library = row();
        library.board_size = None;
        library.moves = None;
        library.event = "第6届嵊州杯".into();
        assert_eq!(
            record_text(&library).subtitle,
            "2024-01-01 12:00:00 · B+0.75 · 第6届嵊州杯"
        );
    }

    fn search(server: Server, query: &str, saved: i64) -> SavedSearch {
        SavedSearch {
            server,
            query: query.into(),
            player: Player {
                source: Source::Fox,
                id: query.into(),
                name: query.into(),
                rank: String::new(),
            },
            saved,
            rows: vec![row()],
        }
    }

    fn key(server: Server, query: &str) -> SearchKey {
        SearchKey {
            server,
            query: query.into(),
        }
    }

    fn queries(history: &SearchHistory) -> Vec<(Server, &str)> {
        history
            .searches
            .iter()
            .map(|s| (s.server, s.query.as_str()))
            .collect()
    }

    #[test]
    fn the_history_keeps_one_search_per_server_and_query_most_recent_first() {
        let mut history = SearchHistory::default();
        assert!(history.apply(&HistoryOp::Remember(search(Server::Fox, "柯洁", 1))));
        assert!(history.apply(&HistoryOp::Remember(search(Server::Fox, "申真谞", 2))));
        // The same words on another server are another search.
        assert!(history.apply(&HistoryOp::Remember(search(Server::Yike, "柯洁", 3))));
        // Asking again replaces the older answer and moves it first.
        assert!(history.apply(&HistoryOp::Remember(search(Server::Fox, "柯洁", 4))));
        assert_eq!(
            queries(&history),
            [
                (Server::Fox, "柯洁"),
                (Server::Yike, "柯洁"),
                (Server::Fox, "申真谞")
            ]
        );
        assert_eq!(
            history.find(&key(Server::Fox, "柯洁")).map(|s| s.saved),
            Some(4)
        );
        assert_eq!(
            history.find(&key(Server::Yike, "柯洁")).map(|s| s.saved),
            Some(3)
        );

        // Opening a saved search moves it first; one already first changes nothing.
        assert!(history.apply(&HistoryOp::Touch(key(Server::Fox, "申真谞"))));
        assert_eq!(queries(&history)[0], (Server::Fox, "申真谞"));
        assert!(!history.apply(&HistoryOp::Touch(key(Server::Fox, "申真谞"))));
        assert!(!history.apply(&HistoryOp::Touch(key(Server::Eweiqi, "申真谞"))));

        assert!(history.apply(&HistoryOp::Forget(key(Server::Yike, "柯洁"))));
        assert!(!history.apply(&HistoryOp::Forget(key(Server::Yike, "柯洁"))));
        assert_eq!(
            queries(&history),
            [(Server::Fox, "申真谞"), (Server::Fox, "柯洁")]
        );
    }

    #[test]
    fn the_history_drops_its_oldest_search_past_the_limit() {
        let mut history = SearchHistory::default();
        for i in 0..=SAVED_SEARCHES {
            history.apply(&HistoryOp::Remember(search(
                Server::Fox,
                &i.to_string(),
                i as i64,
            )));
        }
        assert_eq!(history.searches.len(), SAVED_SEARCHES);
        assert_eq!(history.searches[0].query, SAVED_SEARCHES.to_string());
        assert!(
            history.find(&key(Server::Fox, "0")).is_none(),
            "the oldest search goes"
        );
        assert!(history.find(&key(Server::Fox, "1")).is_some());
    }

    #[test]
    fn the_history_round_trips_through_json() {
        // Asking for a saved search again must not need the server: the history and its
        // records are written to a file and read back as-is.
        let dir = std::env::temp_dir().join(format!("mirai-kifu-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kifu-searches.json");
        let mut history = SearchHistory::default();
        history.apply(&HistoryOp::Remember(search(
            Server::Fox,
            "柯洁",
            1_785_337_045,
        )));
        let mut yike = search(Server::Yike, "沈尧", 1_785_337_046);
        yike.player.source = Source::YikeAccount;
        yike.rows[0].source = Source::YikeAccount;
        yike.rows[0].moves = None;
        history.apply(&HistoryOp::Remember(yike));
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
        let path = std::env::temp_dir().join(format!("mirai-kifu-body-{}", std::process::id()));
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
        // libsoup hands over an error page's body like any other. Parsed as the server's
        // JSON, a 503 would surface as a baffling parser message, or pass for an empty
        // record list.
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
