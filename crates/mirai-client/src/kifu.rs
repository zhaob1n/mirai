// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Public game records from the Go servers mirai searches: Fox, eWeiqi and Yike.
//!
//! A search is the same three steps on every server — the players a query names, one
//! player's recent records, one record's moves — and this module runs them. What a
//! server's endpoints and replies look like is its own module's business: [`crate::fox`],
//! [`crate::eweiqi`], [`crate::yike`]. Each turns its replies into the [`Player`] and
//! [`Record`] here, so a frontend lists, saves and opens them without knowing which server
//! sent them.
//!
//! HTTP is not here either. A frontend implements [`Fetch`] over the transport it has. The
//! future it returns need not be `Send`: GIO's are not.

use mirai_core::{GameInfo, GameTree};
use serde::{Deserialize, Serialize};

use crate::{eweiqi, fox, yike};

const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

/// The user agent every server here answers best: a phone's browser.
pub fn user_agent() -> &'static str {
    UA
}

/// A frontend's HTTP: GETs a URL and hands back its body as text.
pub trait Fetch {
    /// Why a request did not complete. The frontend words it; nothing here looks inside.
    type Error;
    fn get(&self, url: &str) -> impl Future<Output = Result<String, Self::Error>>;
}

/// A server a search can be made on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Server {
    #[default]
    Fox,
    Eweiqi,
    Yike,
}

impl Server {
    pub const ALL: [Server; 3] = [Server::Fox, Server::Eweiqi, Server::Yike];

    /// A stable name for the server, as a frontend's widgets and files key it.
    pub fn as_str(self) -> &'static str {
        match self {
            Server::Fox => "fox",
            Server::Eweiqi => "eweiqi",
            Server::Yike => "yike",
        }
    }

    pub fn from_name(name: &str) -> Option<Server> {
        Server::ALL
            .into_iter()
            .find(|server| server.as_str() == name)
    }
}

/// Where a player's records are listed and a record's moves fetched. Yike keeps two: the
/// professionals' library and its members' own games are separate services with separate
/// ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Fox,
    Eweiqi,
    YikeLibrary,
    YikeAccount,
}

impl Source {
    pub fn server(self) -> Server {
        match self {
            Source::Fox => Server::Fox,
            Source::Eweiqi => Server::Eweiqi,
            Source::YikeLibrary | Source::YikeAccount => Server::Yike,
        }
    }
}

/// Someone a search found, whose records [`games`] lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub source: Source,
    /// What the source lists records by: a Fox UID, the word eWeiqi's catalog is searched
    /// for, a Yike library pid or a Yike account id. A string, as the servers send it.
    pub id: String,
    /// As the server names them; empty when only the id is known.
    #[serde(default)]
    pub name: String,
    /// Rank as [`Record`] writes one, or empty.
    #[serde(default)]
    pub rank: String,
}

/// One game in a player's list, in the same shape whichever server sent it.
///
/// A frontend keeps these as its record of a search and writes them back out, so the
/// fields are plain data, already decoded from the server's own codes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub source: Source,
    /// What the source fetches the moves by. A string: Fox's ids overflow an `i64` read as
    /// a float, and none of them is arithmetic.
    pub id: String,
    pub black: String,
    /// `P9` for a professional, `6d`, `15k`, or a server's own grade such as `2.6d`;
    /// empty when the server gave none.
    #[serde(default)]
    pub black_rank: String,
    pub white: String,
    #[serde(default)]
    pub white_rank: String,
    /// SGF `RE`: `B+R`, `W+3.5`, `B+` for an unrecorded margin, `0` for a draw, `Void`, or
    /// empty when the server recorded none.
    #[serde(default)]
    pub result: String,
    /// `None` when the list does not say.
    #[serde(default)]
    pub moves: Option<u32>,
    #[serde(default)]
    pub board_size: Option<u8>,
    /// As the server writes it: `2026-07-30 17:58:44`, or just the day.
    #[serde(default)]
    pub date: String,
    /// The event or title; empty for a casual game.
    #[serde(default)]
    pub event: String,
}

impl Record {
    /// `"柯洁 (P9) vs 党毅飞 (P9)"` — how the record names itself in a list or a title.
    pub fn matchup(&self) -> String {
        format!(
            "{} vs {}",
            labelled(&self.black, &self.black_rank, "Black"),
            labelled(&self.white, &self.white_rank, "White")
        )
    }
}

fn labelled(name: &str, rank: &str, fallback: &str) -> String {
    let name = if name.trim().is_empty() {
        fallback.to_string()
    } else {
        display_text(name.trim())
    };
    if rank.is_empty() {
        name
    } else {
        format!("{name} ({})", display_text(rank))
    }
}

/// Why a search or a download gave nothing.
#[derive(Debug)]
pub enum Error<E> {
    /// The request did not complete: the frontend's own error.
    Transport(E),
    /// The server's own complaint, or a reply this build cannot read, in English.
    Service(String),
    /// A reply that should hold a game or a player but does not hold a usable one.
    Invalid(String),
    /// Nobody by that name.
    NoPlayer,
    /// The player hides their records, and asking for them is not bypassed.
    Hidden,
}

/// Replaces control characters, which servers do put in nicknames and titles, so a label
/// cannot be made to do something a label should not.
pub fn display_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// Percent-encodes a query parameter without pulling in a crate.
pub fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

async fn get<F: Fetch>(fetch: &F, url: &str) -> Result<String, Error<F::Error>> {
    fetch.get(url).await.map_err(Error::Transport)
}

/// The players `query` names on `server`: one when the server's names are unique or the
/// query is an id, possibly several on Yike, whose nicknames are not.
///
/// A numeric query is an id where the server has them — a Fox UID, a Yike account — and
/// Fox's is taken as it stands, without a lookup. eWeiqi has no account a stranger can
/// look up, only a catalog of games searched by player name, so its "player" is the
/// query itself.
pub async fn players<F: Fetch>(
    fetch: &F,
    server: Server,
    query: &str,
) -> Result<Vec<Player>, Error<F::Error>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(Error::NoPlayer);
    }
    let numeric = query.bytes().all(|b| b.is_ascii_digit());
    match server {
        Server::Fox if numeric => Ok(vec![Player {
            source: Source::Fox,
            id: query.to_string(),
            name: String::new(),
            rank: String::new(),
        }]),
        Server::Fox => {
            let body = get(fetch, &fox::user_url(query)).await?;
            let user = fox::parse_user(&body, query).map_err(Error::Service)?;
            if user.hidden {
                return Err(Error::Hidden);
            }
            if user.uid.is_empty() {
                return Err(Error::Invalid("the player UID is missing".to_string()));
            }
            Ok(vec![Player {
                source: Source::Fox,
                id: user.uid,
                name: user.name,
                rank: String::new(),
            }])
        }
        Server::Eweiqi => Ok(vec![Player {
            source: Source::Eweiqi,
            id: query.to_string(),
            name: query.to_string(),
            rank: String::new(),
        }]),
        Server::Yike if numeric => {
            let body = get(fetch, &yike::account_games_url(query, 1)).await?;
            let page = yike::parse_account_games(&body).map_err(Error::Service)?;
            let Some(name) = page.name else {
                return Err(Error::NoPlayer);
            };
            Ok(vec![Player {
                source: Source::YikeAccount,
                id: query.to_string(),
                name,
                rank: page.grade,
            }])
        }
        Server::Yike => {
            let body = get(fetch, &yike::library_search_url(query)).await?;
            let mut found = yike::parse_library_search(&body, query).map_err(Error::Service)?;
            let body = get(fetch, &yike::account_search_url(query)).await?;
            found.extend(yike::parse_account_search(&body, query).map_err(Error::Service)?);
            if found.is_empty() {
                return Err(Error::NoPlayer);
            }
            Ok(found)
        }
    }
}

/// Records fetched for a Yike player: the latest this many, a few pages of them.
pub const YIKE_RECORDS: usize = 100;

/// `player`'s recent public records, newest first.
pub async fn games<F: Fetch>(fetch: &F, player: &Player) -> Result<Vec<Record>, Error<F::Error>> {
    let mut records = match player.source {
        Source::Fox => {
            let body = get(fetch, &fox::games_url(&player.id)).await?;
            let rows = fox::parse_games(&body).map_err(Error::Service)?;
            rows.iter().map(fox::FoxGame::record).collect()
        }
        Source::Eweiqi => {
            let body = get(fetch, &eweiqi::search_url(&player.id)).await?;
            eweiqi::parse_search(&body).map_err(Error::Service)?
        }
        Source::YikeLibrary => {
            let mut records = Vec::new();
            loop {
                let body = get(fetch, &yike::library_games_url(&player.id, records.len())).await?;
                let page = yike::parse_library_games(&body).map_err(Error::Service)?;
                let full = page.records.len() >= yike::LIBRARY_PAGE;
                records.extend(page.records);
                if !full || records.len() >= YIKE_RECORDS || records.len() >= page.total {
                    break records;
                }
            }
        }
        Source::YikeAccount => {
            let mut records = Vec::new();
            for number in 1.. {
                let body = get(fetch, &yike::account_games_url(&player.id, number)).await?;
                let page = yike::parse_account_games(&body).map_err(Error::Service)?;
                let full = page.records.len() >= yike::ACCOUNT_PAGE;
                records.extend(page.records);
                if !full || records.len() >= YIKE_RECORDS {
                    break;
                }
            }
            records
        }
    };
    // A game that arrives between two page requests pushes one already listed onto the
    // next page.
    let mut seen = std::collections::HashSet::new();
    records.retain(|record| seen.insert(record.id.clone()));
    if player.source.server() == Server::Yike {
        records.truncate(YIKE_RECORDS);
    }
    Ok(records)
}

/// `record`'s game, as a tree mirai can open: each server's dialect normalised, and what
/// the list knew that the record itself does not say filled in.
pub async fn download<F: Fetch>(fetch: &F, record: &Record) -> Result<GameTree, Error<F::Error>> {
    let mut tree = match record.source {
        Source::Fox => {
            let body = get(fetch, &fox::sgf_url(&record.id)).await?;
            let raw = fox::sgf_field(&body).map_err(Error::Service)?;
            fox::parse_record(&raw).map_err(Error::Invalid)?
        }
        Source::Eweiqi => {
            let body = get(fetch, &eweiqi::record_url(&record.id)).await?;
            eweiqi::parse_record(&body).map_err(Error::Invalid)?
        }
        Source::YikeLibrary => {
            let body = get(fetch, &yike::library_sgf_url(&record.id)).await?;
            yike::parse_library_sgf(&body).map_err(Error::Invalid)?
        }
        Source::YikeAccount => {
            let body = get(fetch, &yike::account_sgf_url(&record.id)).await?;
            yike::parse_account_sgf(&body).map_err(Error::Invalid)?
        }
    };
    fill(&mut tree.info, record);
    Ok(tree)
}

/// Fills what the record's root left empty from its row in the list.
fn fill(info: &mut GameInfo, record: &Record) {
    let empty = |s: &str| s.trim().is_empty();
    if empty(&info.players[0].name) && !empty(&record.black) {
        info.players[0].name = record.black.trim().to_string();
    }
    if empty(&info.players[1].name) && !empty(&record.white) {
        info.players[1].name = record.white.trim().to_string();
    }
    if empty(&info.event) && !empty(&record.event) {
        info.event = record.event.trim().to_string();
    }
    if empty(&info.result) {
        info.result = record.result.clone();
    }
    if empty(&info.date) {
        // SGF wants the day; the lists add the time.
        let day = record.date.split_whitespace().next().unwrap_or_default();
        info.date = day.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record {
            source: Source::Eweiqi,
            id: "209557".into(),
            black: "柯洁 ".into(),
            black_rank: "P9".into(),
            white: "党毅飞".into(),
            white_rank: String::new(),
            result: "B+R".into(),
            moves: Some(205),
            board_size: None,
            date: "2026-07-30 17:58:44".into(),
            event: "王中王 第六届".into(),
        }
    }

    #[test]
    fn a_matchup_drops_a_rank_the_server_did_not_give() {
        assert_eq!(record().matchup(), "柯洁 (P9) vs 党毅飞");
        let mut nameless = record();
        nameless.black.clear();
        nameless.white = "a\u{7}b".into();
        assert_eq!(nameless.matchup(), "Black (P9) vs a\u{fffd}b");
    }

    #[test]
    fn the_list_fills_only_what_the_record_left_empty() {
        let mut info = GameInfo::new(mirai_core::Size::square(19), Default::default());
        info.players[1].name = "脚柳辑".into();
        info.result = "W+R".into();
        fill(&mut info, &record());
        assert_eq!(info.players[0].name, "柯洁");
        assert_eq!(info.players[1].name, "脚柳辑", "the record's own name wins");
        assert_eq!(info.result, "W+R", "the record's own result wins");
        assert_eq!(info.event, "王中王 第六届");
        assert_eq!(info.date, "2026-07-30", "SGF dates are days");
    }

    /// Answers each URL it knows with its body, and records what was asked.
    #[derive(Default)]
    struct Canned {
        replies: std::collections::HashMap<String, String>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl Fetch for Canned {
        type Error = String;

        async fn get(&self, url: &str) -> Result<String, String> {
            self.asked.borrow_mut().push(url.to_string());
            self.replies
                .get(url)
                .cloned()
                .ok_or_else(|| format!("no reply for {url}"))
        }
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    /// A library page of `count` games, ids counting down from `first`.
    fn library_page(total: usize, first: usize, count: usize) -> String {
        let games: Vec<String> = (0..count)
            .map(|i| format!(r#"{{"id":"{}","black":"甲","white":"乙"}}"#, first - i))
            .collect();
        format!(r#"{{"total":{total},"games":[{}]}}"#, games.join(","))
    }

    fn library_player() -> Player {
        Player {
            source: Source::YikeLibrary,
            id: "1195".into(),
            name: "柯洁".into(),
            rank: String::new(),
        }
    }

    #[test]
    fn a_library_list_pages_by_offset_until_the_player_has_no_more() {
        let mut fetch = Canned::default();
        for (start, count) in [(0, 20), (20, 20), (40, 5)] {
            fetch.replies.insert(
                yike::library_games_url("1195", start),
                library_page(45, 1000 - start, count),
            );
        }
        let records = run(games(&fetch, &library_player())).expect("records");
        assert_eq!(records.len(), 45);
        assert_eq!(fetch.asked.borrow().len(), 3, "a short page is the last");
        assert_eq!(
            records[20].id, "980",
            "the second page starts where the first ended"
        );
    }

    #[test]
    fn a_long_library_list_stops_at_the_records_kept() {
        let mut fetch = Canned::default();
        for start in (0..300).step_by(yike::LIBRARY_PAGE) {
            fetch.replies.insert(
                yike::library_games_url("1195", start),
                library_page(938, 5000 - start, yike::LIBRARY_PAGE),
            );
        }
        let records = run(games(&fetch, &library_player())).expect("records");
        assert_eq!(records.len(), YIKE_RECORDS);
        assert_eq!(
            fetch.asked.borrow().len(),
            YIKE_RECORDS / yike::LIBRARY_PAGE,
            "no page past what is kept is asked for"
        );
    }

    #[test]
    fn a_yike_name_offers_the_professional_before_the_accounts() {
        let mut fetch = Canned::default();
        fetch.replies.insert(
            yike::library_search_url("柯洁"),
            r#"{"matches":[{"name":"\u67ef\u6d01","pid":"1195"}]}"#.into(),
        );
        fetch.replies.insert(
            yike::account_search_url("柯洁"),
            r#"{"Status":1200,"Result":{"data":[
                {"id":1323,"cgf_id":"CGF01324","nickname":"柯洁","grade":"10.5D"},
                {"id":63286,"cgf_id":null,"nickname":"柯洁的粉丝","grade":"21.5K"}
            ]}}"#
                .into(),
        );
        let found = run(players(&fetch, Server::Yike, " 柯洁 ")).expect("players");
        let found: Vec<(Source, &str)> = found.iter().map(|p| (p.source, p.id.as_str())).collect();
        assert_eq!(
            found,
            [(Source::YikeLibrary, "1195"), (Source::YikeAccount, "1323")]
        );
    }

    #[test]
    fn a_yike_number_names_an_account_only_if_it_exists() {
        let mut fetch = Canned::default();
        fetch.replies.insert(
            yike::account_games_url("999999999", 1),
            r#"{"Status":1200,"Result":{"user":{"grage":"21.5K","name":null},"list":[]}}"#.into(),
        );
        assert!(matches!(
            run(players(&fetch, Server::Yike, "999999999")),
            Err(Error::NoPlayer)
        ));
        // A numeric Fox query is a UID as it stands: nothing is asked.
        let found = run(players(&fetch, Server::Fox, "6757425")).expect("a UID");
        assert_eq!(found[0].id, "6757425");
        assert_eq!(fetch.asked.borrow().len(), 1);
    }
}
