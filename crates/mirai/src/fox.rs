// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The GTK half of Fox Go lookup: a libsoup transport, the last-search cache, and the
//! picker dialog.
//!
//! The endpoints, the reply shapes and the SGF dialect live in [`mirai_client::fox`], which
//! the HarmonyOS client uses as well. This file does not reimplement any of them — it cannot
//! even use that module's `Fetch` trait, because libsoup's futures are `!Send`, so it
//! composes the URL builders with the pure parsers instead.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
use crate::fox_picker::{FoxPickerDialog, FoxRow};
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

struct SearchCache {
    /// False until the file has been read, or a search has been stored. The static starts
    /// cold so the first time the picker opens does not `read_to_string` on the GTK thread.
    loaded: bool,
    value: Option<LastSearch>,
}

static LAST_SEARCH: Mutex<SearchCache> = Mutex::new(SearchCache {
    loaded: false,
    value: None,
});

/// Serialises cache writes. A later search must not be overwritten by an earlier write
/// that finishes second; the writer re-reads the cache under this lock.
static WRITE_GATE: Mutex<()> = Mutex::new(());

fn cache_lock() -> std::sync::MutexGuard<'static, SearchCache> {
    LAST_SEARCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    mirai_proto::atomic::write_atomic(path, text.as_bytes()).map_err(|error| error.to_string())
}

fn cached_search() -> Option<LastSearch> {
    cache_lock().value.clone()
}

/// Reads the cache file once, unless a search was stored while the read was in flight.
fn load_cache_if_cold() -> Option<LastSearch> {
    if cache_lock().loaded {
        return cached_search();
    }
    let loaded = load_last_search();
    let mut cache = cache_lock();
    if !cache.loaded {
        cache.loaded = true;
        cache.value = loaded;
    }
    cache.value.clone()
}

fn store_cached(search: LastSearch) {
    let mut cache = cache_lock();
    cache.loaded = true;
    cache.value = Some(search);
}

/// Updates the in-memory cache immediately and writes the file on the runtime's blocking
/// pool. The write syncs the file and its directory; doing that before the result list
/// appears dropped the frame that showed the search.
fn remember_search(search: LastSearch, runtime: &tokio::runtime::Handle) {
    store_cached(search);
    runtime.spawn_blocking(|| {
        let _gate = WRITE_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(search) = cached_search() else {
            return;
        };
        let Some(path) = last_search_path() else {
            return;
        };
        if let Err(error) = write_last_search(&path, &search) {
            tracing::warn!(%error, "could not cache the Fox search");
        }
    });
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

/// How one record reads in the list. Plain text: the labels bound to it do not parse markup,
/// so a nickname with `<` in it is not a problem.
fn list_row(game: &FoxGame) -> FoxRow {
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
    FoxRow::new(&game.matchup(), &details.join(" · "))
}

impl FoxPickerDialog {
    fn wired() -> Self {
        let dialog = FoxPickerDialog::new();
        let widgets = dialog.widgets();
        widgets.stack.set_visible_child(&widgets.status_page);

        widgets.cancel_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        widgets.entry.connect_search_changed(clone!(
            #[weak]
            dialog,
            move |_| dialog.refresh_actions()
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
        widgets.open_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| dialog.start_download()
        ));
        dialog.connect_selection_changed(clone!(
            #[weak]
            dialog,
            move || dialog.refresh_actions()
        ));
        widgets.result_list.connect_activate(clone!(
            #[weak]
            dialog,
            move |_, _| dialog.start_download()
        ));
        dialog.connect_closed(clone!(
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
        widgets
            .loading_page
            .set_title(&i18n::gettext("Searching Fox"));
        widgets.loading_page.set_description(Some(&i18n::gettext(
            "Looking up the player and their recent games.",
        )));
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
                remember_search(
                    LastSearch {
                        query,
                        account: found.account.clone(),
                        rows: found.rows.clone(),
                    },
                    self.runtime(),
                );
                self.show_search(found);
            }
            Err(error) => {
                self.clear_games();
                let widgets = self.widgets();
                widgets
                    .status_page
                    .set_icon_name(Some("dialog-warning-symbolic"));
                widgets
                    .status_page
                    .set_title(&i18n::gettext("Couldn’t load games"));
                widgets
                    .status_page
                    .set_description(Some(&fox_error_message(&error)));
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
            widgets
                .status_page
                .set_title(&i18n::gettext("No public games"));
            widgets.status_page.set_description(Some(&i18n::gettext(
                "This account has no visible games in Fox's recent-history window.",
            )));
            widgets.stack.set_visible_child(&widgets.status_page);
            return;
        }

        let count = found.rows.len() as u64;
        let account = fox::display_text(&found.account);
        // Translators: {account} is a player name, or "UID" and a number.
        let heading = i18n::ngettext_f(
            "{account} · {count} recent game",
            "{account} · {count} recent games",
            count,
            &[("account", &account), ("count", &count.to_string())],
        );
        widgets.result_label.set_label(&heading);
        let rows: Vec<FoxRow> = found.rows.iter().map(list_row).collect();
        self.replace_games(found.rows, &rows);
        widgets.stack.set_visible_child(&widgets.results_page);
        self.refresh_actions();
    }

    fn restore_last_search(&self) {
        // The file read used to run inside the `LazyLock`, on the frame that first
        // opened the picker. The list fills when the blocking pool finishes.
        let weak = self.downgrade();
        let job = self.runtime().spawn_blocking(load_cache_if_cold);
        let task = glib::spawn_future_local(async move {
            let Ok(last) = job.await else {
                return;
            };
            let Some(last) = last else {
                return;
            };
            let Some(dialog) = weak.upgrade() else {
                return;
            };
            if dialog.has_games() || dialog.is_busy() {
                return;
            }
            dialog.widgets().entry.set_text(&last.query);
            dialog.show_search(Games {
                account: last.account,
                rows: last.rows,
            });
        });
        self.replace_task(task);
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
                self.set_busy(false);
                let widgets = self.widgets();
                widgets.stack.set_visible_child(&widgets.results_page);
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
/// its record list, so opening it again costs nothing.
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
    warm_tls(&runtime);
    dialog.set_runtime(runtime);
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
