// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Public Fox Go game lookup, SGF normalisation and the game picker dialog.

use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use mirai_core::{Color, GameTree, Node, NodeId, Point, sgf};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::fox_picker::FoxPickerDialog;

const USER_URL: &str = "https://newframe.foxwq.com/cgi/QueryUserInfoPanel";
const GAMES_URL: &str = "https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList";
const SGF_URL: &str = "https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess";
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
    #[error("Invalid game record: {0}")]
    Sgf(#[from] sgf::SgfError),
}

#[derive(Debug, Deserialize)]
struct UserResponse {
    result: Option<i32>,
    errcode: Option<i32>,
    uid: Option<String>,
    username: Option<String>,
    hide_game_record: Option<i32>,
    resultstr: Option<String>,
    errmsg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GamesResponse {
    result: Option<i32>,
    #[serde(default)]
    chesslist: Vec<FoxGame>,
    resultstr: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct FoxGame {
    #[serde(default)]
    chessid: String,
    #[serde(default)]
    blacknick: String,
    #[serde(default)]
    blackenname: String,
    #[serde(default)]
    whitenick: String,
    #[serde(default)]
    whiteenname: String,
    #[serde(default)]
    blackdan: i32,
    #[serde(default)]
    whitedan: i32,
    #[serde(default)]
    blackocc: i32,
    #[serde(default)]
    whiteocc: i32,
    #[serde(default)]
    winner: i32,
    #[serde(default)]
    point: i32,
    #[serde(default)]
    movenum: u32,
    #[serde(default = "default_board_size")]
    boardsize: u8,
    #[serde(default)]
    starttime: String,
    #[serde(default)]
    title: String,
}

const fn default_board_size() -> u8 {
    19
}

#[derive(Debug, Deserialize)]
struct SgfResponse {
    result: Option<i32>,
    chess: Option<String>,
    resultstr: Option<String>,
}

struct Games {
    account: String,
    rows: Vec<FoxGame>,
}

pub(crate) struct DownloadedGame {
    pub(crate) tree: GameTree,
    pub(crate) label: String,
}

fn api_error(
    result: Option<i32>,
    errcode: Option<i32>,
    resultstr: Option<&str>,
    errmsg: Option<&str>,
) -> Result<(), FoxError> {
    let code = result.or(errcode).unwrap_or(-1);
    if code == 0 {
        return Ok(());
    }
    let message = resultstr
        .filter(|s| !s.trim().is_empty())
        .or_else(|| errmsg.filter(|s| !s.trim().is_empty()))
        .unwrap_or("Fox rejected the request");
    Err(FoxError::Service(display_text(message)))
}

fn query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, FoxError> {
    let mut last_error = "request failed".to_string();
    for attempt in 1..=RETRIES {
        let file = gio::File::for_uri(url);
        match glib::future_with_timeout(REQUEST_TIMEOUT, file.load_contents_future()).await {
            Ok(Ok((bytes, _))) if bytes.len() > MAX_RESPONSE_BYTES => {
                last_error = "response was unexpectedly large".to_string();
            }
            Ok(Ok((bytes, _))) => match serde_json::from_slice(bytes.as_ref()) {
                Ok(value) => return Ok(value),
                Err(error) => last_error = format!("response was not valid JSON ({error})"),
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

    let (uid, account) = if query.bytes().all(|b| b.is_ascii_digit()) {
        (query.to_string(), format!("UID {query}"))
    } else {
        let url = format!("{USER_URL}?srcuid=0&username={}", query_escape(query));
        let response: UserResponse = get_json(&url).await?;
        api_error(
            response.result,
            response.errcode,
            response.resultstr.as_deref(),
            response.errmsg.as_deref(),
        )?;
        if response.hide_game_record == Some(1) {
            return Err(FoxError::Service(
                "This player has hidden their game records".to_string(),
            ));
        }
        let uid = response
            .uid
            .filter(|uid| !uid.is_empty())
            .ok_or_else(|| FoxError::InvalidData("the player UID is missing".to_string()))?;
        let account = response
            .username
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| query.to_string());
        (uid, account)
    };

    let url = format!(
        "{GAMES_URL}?dstuid={}&type=1&fetchnum=200",
        query_escape(&uid)
    );
    let response: GamesResponse = get_json(&url).await?;
    api_error(response.result, None, response.resultstr.as_deref(), None)?;
    let rows = response
        .chesslist
        .into_iter()
        .filter(|game| !game.chessid.is_empty())
        .collect();
    Ok(Games { account, rows })
}

async fn fetch_game(game: &FoxGame) -> Result<DownloadedGame, FoxError> {
    let url = format!("{SGF_URL}?chessid={}", query_escape(&game.chessid));
    let response: SgfResponse = get_json(&url).await?;
    api_error(response.result, None, response.resultstr.as_deref(), None)?;
    let text = response
        .chess
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| FoxError::InvalidData("the SGF is empty".to_string()))?;
    let tree = parse_fox_sgf(&text)?;
    Ok(DownloadedGame {
        tree,
        label: game.matchup(),
    })
}

impl FoxGame {
    fn black_name(&self) -> &str {
        player_name(&self.blacknick, &self.blackenname, "Black")
    }

    fn white_name(&self) -> &str {
        player_name(&self.whitenick, &self.whiteenname, "White")
    }

    fn matchup(&self) -> String {
        format!(
            "{} ({}) vs {} ({})",
            display_text(self.black_name()),
            rank(self.blackdan, self.blackocc),
            display_text(self.white_name()),
            rank(self.whitedan, self.whiteocc)
        )
    }

    fn result(&self) -> String {
        let colour = match self.winner {
            1 => "B",
            2 => "W",
            _ => return "No result".to_string(),
        };
        match self.point {
            -1 => format!("{colour}+R"),
            -2 => format!("{colour}+T"),
            n if n >= 0 => format!("{colour}+{}", hundredths(n as u32)),
            _ => format!("{colour} wins"),
        }
    }

    fn row(&self) -> adw::ActionRow {
        let mut details = Vec::with_capacity(5);
        if !self.starttime.is_empty() {
            details.push(display_text(&self.starttime));
        }
        details.push(format!("{}×{}", self.boardsize, self.boardsize));
        details.push(format!("{} moves", self.movenum));
        details.push(self.result());
        if !self.title.is_empty() {
            details.push(display_text(&self.title));
        }
        let title = glib::markup_escape_text(&self.matchup());
        let subtitle = glib::markup_escape_text(&details.join(" · "));
        adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .activatable(true)
            .build()
    }
}

fn player_name<'a>(nickname: &'a str, english: &'a str, fallback: &'a str) -> &'a str {
    if !nickname.is_empty() {
        nickname
    } else if !english.is_empty() {
        english
    } else {
        fallback
    }
}

fn rank(dan: i32, occupation: i32) -> String {
    if occupation != 0 {
        return match dan - 99 {
            level @ 1..=9 => format!("P{level}"),
            _ => "Pro".to_string(),
        };
    }
    let level = dan - 17;
    if level > 0 {
        format!("{level}d")
    } else {
        format!("{}k", 1 - level)
    }
}

fn hundredths(value: u32) -> String {
    let whole = value / 100;
    let fraction = value % 100;
    if fraction == 0 {
        whole.to_string()
    } else if fraction.is_multiple_of(10) {
        format!("{whole}.{}", fraction / 10)
    } else {
        format!("{whole}.{fraction:02}")
    }
}

fn display_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '�' } else { c })
        .collect()
}

// Fox inserts literal `\r` / `\n` pairs between properties. Replacing each pair with
// whitespace, while leaving escapes inside `[...]` untouched, keeps property names separate.
fn clean_sgf(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut chars = text.chars().peekable();
    let mut out = String::with_capacity(text.len());
    let mut in_value = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_value {
            out.push(if c == '\0' { '�' } else { c });
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == ']' {
                in_value = false;
            }
            continue;
        }
        match c {
            '[' => {
                in_value = true;
                out.push(c);
            }
            '\\' => {
                if chars.peek().is_some_and(|next| matches!(next, 'r' | 'n')) {
                    chars.next();
                }
                out.push('\n');
            }
            '\0' => out.push('�'),
            _ => out.push(c),
        }
    }
    out
}

fn root_number(text: &str, property: &str) -> Option<f32> {
    let bytes = text.as_bytes();
    let root = bytes.iter().position(|&b| b == b';')? + 1;
    let mut end = root;
    let mut in_value = false;
    let mut escaped = false;
    while end < bytes.len() {
        let byte = bytes[end];
        if in_value {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b']' {
                in_value = false;
            }
        } else if byte == b'[' {
            in_value = true;
        } else if matches!(byte, b';' | b'(' | b')') {
            break;
        }
        end += 1;
    }

    let node = &text[root..end];
    let needle = format!("{property}[");
    let start = node.find(&needle)? + needle.len();
    let value = node[start..].split(']').next()?;
    value.trim().parse().ok()
}

fn fox_komi(raw: f32) -> f32 {
    let scaled = if raw.abs() >= 200.0 { raw / 100.0 } else { raw };
    let hundredths = (scaled.abs().fract() * 100.0).round() as i32;
    if scaled.abs() <= 4.0 && matches!(hundredths, 25 | 75) {
        scaled * 2.0
    } else {
        scaled
    }
}

fn parse_fox_sgf(text: &str) -> Result<GameTree, FoxError> {
    let cleaned = clean_sgf(text);
    let komi = root_number(&cleaned, "KM").map(fox_komi);
    let mut trees = sgf::parse_str(&cleaned)?;
    if trees.is_empty() {
        return Err(FoxError::InvalidData(
            "the response holds no game".to_string(),
        ));
    }
    let mut tree = trees.remove(0);
    if let Some(komi) = komi {
        tree.info.komi = komi;
    }
    Ok(normalize_handicap(tree))
}

fn plain_node(node: &Node) -> bool {
    node.marks.is_empty()
        && node.comment.is_empty()
        && node.move_number_override.is_none()
        && node.to_play_override.is_none()
        && node.analysis.is_none()
        && node.unknown_props.is_empty()
}

fn setup_point(node: &Node) -> Option<Point> {
    (node.mv.is_none()
        && plain_node(node)
        && node.setup.add_black.len() == 1
        && node.setup.add_white.is_empty()
        && node.setup.add_empty.is_empty())
    .then(|| node.setup.add_black[0])
}

fn black_move_point(node: &Node) -> Option<Point> {
    (node.setup.is_empty() && plain_node(node))
        .then_some(node.mv)
        .flatten()
        .and_then(|(color, point)| (color == Color::Black && !point.is_pass()).then_some(point))
}

fn prefix(tree: &GameTree, point_of: fn(&Node) -> Option<Point>) -> (Vec<NodeId>, Vec<Point>) {
    let mut ids = Vec::new();
    let mut points = Vec::new();
    let mut parent = tree.root();
    while let Some(&child) = tree.children(parent).first() {
        let Some(point) = point_of(tree.node(child)) else {
            break;
        };
        ids.push(child);
        points.push(point);
        if tree.children(child).len() != 1 {
            break;
        }
        parent = child;
    }
    (ids, points)
}

fn copy_payload(source: &Node, target: &mut Node) {
    let parent = target.parent;
    *target = source.clone();
    target.parent = parent;
    target.children.clear();
}

fn copy_branch(source: &GameTree, source_id: NodeId, target: &mut GameTree, parent: NodeId) {
    let id = target.add_child(parent);
    copy_payload(source.node(source_id), target.node_mut(id));
    for &child in source.children(source_id) {
        copy_branch(source, child, target, id);
    }
}

fn normalize_handicap(mut tree: GameTree) -> GameTree {
    let (mut skipped, mut stones) = prefix(&tree, setup_point);
    if stones.len() < 2 {
        (skipped, stones) = prefix(&tree, black_move_point);
    }
    if stones.len() < 2 {
        let root = tree.root();
        let count = tree.node(root).setup.add_black.len();
        if count >= 2 {
            tree.info.handicap = tree.info.handicap.max(count.min(u8::MAX as usize) as u8);
            tree.info.komi = 0.0;
        }
        return tree;
    }

    let source_root = tree.root();
    let last = *skipped.last().expect("two-node prefix");
    let mut continuations = tree.children(last).to_vec();
    continuations.extend(tree.children(source_root).iter().skip(1).copied());
    let mut rebuilt = GameTree::new(tree.info.clone());
    let target_root = rebuilt.root();
    copy_payload(tree.node(source_root), rebuilt.node_mut(target_root));
    {
        let root = rebuilt.node_mut(target_root);
        for point in stones.drain(..) {
            if !root.setup.add_black.contains(&point) {
                root.setup.add_black.push(point);
            }
        }
    }
    let count = rebuilt.node(target_root).setup.add_black.len();
    rebuilt.info.handicap = rebuilt.info.handicap.max(count.min(u8::MAX as usize) as u8);
    rebuilt.info.komi = 0.0;
    for child in continuations {
        copy_branch(&tree, child, &mut rebuilt, target_root);
    }
    rebuilt
}

impl FoxPickerDialog {
    fn with_handler(on_open: impl Fn(DownloadedGame) + 'static) -> Self {
        let dialog = FoxPickerDialog::new();
        dialog.install_handler(on_open);
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
        widgets.result_list.connect_row_selected(glib::clone!(
            #[weak]
            dialog,
            move |_, _| dialog.refresh_actions()
        ));
        widgets.result_list.connect_row_activated(glib::clone!(
            #[weak]
            dialog,
            move |list, row| {
                list.select_row(Some(row));
                dialog.start_download();
            }
        ));
        dialog
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
            .set_sensitive(idle && widgets.result_list.selected_row().is_some());
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
                dialog.finish_search(result);
            }
        });
        self.replace_task(task);
    }

    fn finish_search(&self, result: Result<Games, FoxError>) {
        self.set_busy(false);
        let widgets = self.widgets();
        match result {
            Ok(found) if found.rows.is_empty() => {
                self.clear_games();
                widgets
                    .status_page
                    .set_icon_name(Some("edit-find-symbolic"));
                widgets.status_page.set_title("No public games");
                widgets.status_page.set_description(Some(
                    "This account has no visible games in Fox's recent-history window.",
                ));
                widgets.stack.set_visible_child(&widgets.status_page);
            }
            Ok(found) => {
                while let Some(child) = widgets.result_list.first_child() {
                    widgets.result_list.remove(&child);
                }
                let count = found.rows.len();
                widgets.result_label.set_label(&format!(
                    "{} · {count} recent games",
                    display_text(&found.account)
                ));
                for game in &found.rows {
                    widgets.result_list.append(&game.row());
                }
                self.replace_games(found.rows);
                if let Some(first) = widgets.result_list.row_at_index(0) {
                    widgets.result_list.select_row(Some(&first));
                }
                widgets.stack.set_visible_child(&widgets.results_page);
                self.refresh_actions();
            }
            Err(error) => {
                self.clear_games();
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

    fn start_download(&self) {
        if self.is_busy() {
            return;
        }
        let widgets = self.widgets();
        let Some(index) = widgets
            .result_list
            .selected_row()
            .map(|row| row.index() as usize)
        else {
            return;
        };
        let Some(game) = self.game(index) else {
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

pub(crate) fn present(parent: &impl IsA<gtk::Widget>, on_open: impl Fn(DownloadedGame) + 'static) {
    let dialog = FoxPickerDialog::with_handler(on_open);
    dialog.present(Some(parent));
    dialog.widgets().entry.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_utf8_percent_encoded() {
        assert_eq!(query_escape("柯洁"), "%E6%9F%AF%E6%B4%81");
        assert_eq!(query_escape("6757425"), "6757425");
        assert_eq!(query_escape("a b&c"), "a%20b%26c");
    }

    #[test]
    fn fox_escapes_and_quarter_point_komi_are_normalised() {
        let raw = "\u{feff}(;GM[1]\\r\\nFF[4]\\r\\nSZ[19]KM[375]PB[Black]PW[White];B[pd]C[a\\]b])";
        let tree = parse_fox_sgf(raw).expect("Fox SGF");
        assert_eq!(tree.info.komi, 7.5);
        assert_eq!(tree.main_line().len(), 2);
        assert_eq!(tree.node(tree.main_line()[1]).comment, "a]b");
    }

    #[test]
    fn fox_setup_nodes_become_one_root_handicap() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[0]HA[3]PB[柯洁]PW[党毅飞];AB[dd];AB[pd];AB[dp];W[dm])";
        let tree = parse_fox_sgf(raw).expect("Fox handicap SGF");
        let root = tree.root();
        assert_eq!(tree.info.handicap, 3);
        assert_eq!(tree.node(root).setup.add_black.len(), 3);
        // The rebuild must carry the metadata across: the window titles itself from it.
        assert_eq!(tree.info.players[0].name, "柯洁");
        assert_eq!(tree.info.players[1].name, "党毅飞");
        assert_eq!(tree.main_line().len(), 2);
        assert_eq!(
            tree.node(tree.main_line()[1]).mv.map(|m| m.0),
            Some(Color::White)
        );
    }

    #[test]
    fn handicap_normalisation_preserves_root_variations() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[0](;AB[dd];AB[pd];W[qq])(;B[aa]))";
        let tree = parse_fox_sgf(raw).expect("Fox handicap variations");
        assert_eq!(tree.node(tree.root()).setup.add_black.len(), 2);
        assert_eq!(tree.children(tree.root()).len(), 2);
    }

    #[test]
    fn consecutive_black_handicap_moves_are_promoted_too() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[0];B[dd];B[pd];B[dp];W[dm])";
        let tree = parse_fox_sgf(raw).expect("Fox handicap SGF");
        assert_eq!(tree.info.handicap, 3);
        assert_eq!(tree.node(tree.root()).setup.add_black.len(), 3);
        assert_eq!(tree.main_line().len(), 2);
    }

    #[test]
    fn fox_result_text_uses_the_documented_units() {
        assert_eq!(hundredths(350), "3.5");
        assert_eq!(hundredths(375), "3.75");
        assert_eq!(rank(23, 0), "6d");
        assert_eq!(rank(3, 0), "15k");
        assert_eq!(rank(108, 2), "P9");

        assert_eq!(display_text("<event>\0name"), "<event>�name");
    }
}
