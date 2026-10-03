// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! eWeiqi (弈城) kifu lookup: the catalog search, the GIB record, and the dialect mirai
//! has to undo before it can open one.
//!
//! HTTP is not here: [`crate::kifu`] runs the lookup over a frontend's transport and turns
//! these rows into its [`Record`]. This module is what eWeiqi's half of that looks like
//! ([`EWEIQI_KIFU_API_SPEC.md`](../../../docs/dev/EWEIQI_KIFU_API_SPEC.md)).
//!
//! A member's own history is a different, logged-in service. mirai does not speak it.

use std::borrow::Cow;

use mirai_core::{Color, GameInfo, GameTree, NodeId, Point, RuleSet, Size, fixed_handicap};
use serde::Deserialize;

use crate::kifu::{Record, Source, display_text, urlencoding};

/// Records a search keeps: the newest this many. The catalog has no cursor, and a famous
/// name returns more than a list should show.
pub const MAX_RECORDS: usize = 200;

const HOST: &str = "http://client.eweiqi.com/gibo";

/// `gibo_load_list.php?type=5&sword={query}&lang=cn`.
///
/// Plain `http`: `https://client.eweiqi.com` presents a certificate that does not name that
/// host, so the TLS request never leaves the client.
pub fn search_url(query: &str) -> String {
    format!(
        "{HOST}/gibo_load_list.php?type=5&sword={}&lang=cn",
        urlencoding(query)
    )
}

#[derive(Deserialize)]
struct List<'a> {
    #[serde(borrow, default)]
    list: Vec<Row<'a>>,
}

/// One catalog row, borrowed from the reply. Every field the server sends is a string; a
/// `null` reads as absent.
#[derive(Deserialize)]
struct Row<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "BName")]
    black: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "BNick")]
    black_nick: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "BRank")]
    black_rank: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "WName")]
    white: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "WNick")]
    white_nick: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "WRank")]
    white_rank: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "GameResult")]
    result: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "Susun")]
    moves: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "Date")]
    date: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "CateName")]
    category: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "SubCateName")]
    subcategory: Option<Cow<'a, str>>,
}

fn field<'s>(value: &'s Option<Cow<'_, str>>) -> &'s str {
    value.as_deref().unwrap_or_default()
}

/// The catalog rows a search returned, as records: newest first, at most [`MAX_RECORDS`].
///
/// A bare BOM is no match, `Ok(empty)`. The server's own order is not stable across two
/// identical requests, so the date is what orders them. A broad search answers with every
/// match — `a` brought 9,158 rows, 3.5 MB — and this runs on a frontend's UI thread, so the
/// rows are read borrowed and only the ones kept become records: 8–9 ms for that reply in a
/// release build, where a `serde_json::Value` took 26–34.
pub fn parse_search(body: &str) -> Result<Vec<Record>, String> {
    let body = strip_bom(body).trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let list: List =
        serde_json::from_str(body).map_err(|e| format!("eweiqi response is not JSON: {e}"))?;
    let mut rows: Vec<Row> = list
        .list
        .into_iter()
        .filter(|row| !field(&row.id).is_empty())
        .collect();
    // Newest first; a missing date is not newer than a real one.
    rows.sort_by(|a, b| {
        let (a, b) = (field(&a.date).trim(), field(&b.date).trim());
        match (a.is_empty(), b.is_empty()) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => b.cmp(a),
        }
    });
    let mut seen = std::collections::HashSet::new();
    Ok(rows
        .iter()
        .filter(|row| seen.insert(field(&row.id)))
        .take(MAX_RECORDS)
        .map(|row| Record {
            source: Source::Eweiqi,
            id: field(&row.id).to_string(),
            black: player(field(&row.black), field(&row.black_nick)),
            black_rank: rank_text(field(&row.black_rank)),
            white: player(field(&row.white), field(&row.white_nick)),
            white_rank: rank_text(field(&row.white_rank)),
            result: list_result(field(&row.result)),
            moves: field(&row.moves).trim().parse().ok(),
            board_size: None,
            date: field(&row.date).trim().to_string(),
            event: event(field(&row.category), field(&row.subcategory)),
        })
        .collect())
}

/// `gibo_load_data.php?id={id}` — never `mode=my`.
///
/// `mode=my` asks for the caller's own copy of the game. On a catalog id the server answers
/// that the record does not exist, and the same id without the parameter is the game.
pub fn record_url(id: &str) -> String {
    format!("{HOST}/gibo_load_data.php?id={}", urlencoding(id))
}

/// A GIB record as a game tree.
///
/// Coordinates are already top-down, the same way mirai numbers rows (`y = 0` is the top).
/// Flipping `y` mirrors the board: the opening of catalog game `209557` is `B[pd]`, which is
/// `(15, 3)` unflipped.
pub fn parse_record(body: &str) -> Result<GameTree, String> {
    let body = strip_bom(body).trim();
    if body.starts_with("[Error]") {
        return Err(display_text(body));
    }
    if !body.starts_with("\\HS") {
        return Err("eweiqi record is not a GIB".to_string());
    }
    let gs = body.find("\\GS").ok_or("eweiqi record has no moves")?;
    let ge_rel = body[gs..]
        .find("\\GE")
        .ok_or("eweiqi record is truncated")?;
    let ge = gs + ge_rel;
    let header = &body[..gs];
    let main = &body[gs..ge];

    let size = board_size(header)?;
    let mut info = GameInfo::new(size, RuleSet::Chinese);
    if let Some(komi) = tenths(header_field(header, "GONGJE")) {
        info.komi = komi;
    }
    info.result = gib_result(header_field(header, "GRLT"), header_field(header, "ZIPSU"));
    info.players[0].name = player(header_field(header, "BID"), "");
    info.players[1].name = player(header_field(header, "WID"), "");
    info.players[0].rank = rank_text(header_field(header, "BLV"));
    info.players[1].rank = rank_text(header_field(header, "WLV"));
    info.date = gdate_day(header_field(header, "GDATE"));

    let mut tree = GameTree::new(info);
    let handicap = handicap_count(main);
    if (2..=9).contains(&handicap) {
        let root = tree.root();
        for point in fixed_handicap(size, handicap) {
            tree.set_setup_stone(root, point, Some(Color::Black));
        }
        tree.info.handicap = handicap;
    }

    // Every move in order, passes included: a commentary block names the move it follows.
    let mut moves: Vec<NodeId> = Vec::new();
    let mut cursor = tree.root();
    // Handicap stones are Black's, so White is the side not to have moved.
    let mut last = if tree.info.handicap >= 2 {
        Some(Color::Black)
    } else {
        None
    };
    for line in main.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("STO") {
            let (color, point) = sto(rest, size)?;
            cursor = push_move(&mut tree, cursor, color, point);
            moves.push(cursor);
            last = Some(color);
        } else if line.starts_with("SKI") {
            let color = last.map_or(Color::Black, Color::other);
            cursor = push_move(&mut tree, cursor, color, Point::PASS);
            moves.push(cursor);
            last = Some(color);
        }
    }
    attach_comments(&mut tree, &body[ge..], &moves);
    Ok(tree)
}

/// eWeiqi's rank code, as a list writes one: `P9`, `9d`, `18k`. Empty when the code is not
/// one of those three ranges.
pub fn rank(code: i32) -> String {
    if code >= 27 {
        format!("P{}", code - 26)
    } else if (18..=26).contains(&code) {
        format!("{}d", code - 17)
    } else if (0..=17).contains(&code) {
        format!("{}k", 18 - code)
    } else {
        String::new()
    }
}

fn strip_bom(body: &str) -> &str {
    body.strip_prefix('\u{feff}').unwrap_or(body)
}

fn player(name: &str, nick: &str) -> String {
    let name = name.trim();
    let chosen = if name.is_empty() { nick.trim() } else { name };
    display_text(chosen)
}

fn event(cate: &str, sub: &str) -> String {
    display_text(
        &[cate.trim(), sub.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn rank_text(raw: &str) -> String {
    raw.trim().parse().map(rank).unwrap_or_default()
}

/// List `GameResult`: the sign is the winner, `999` a resignation, `888` a lost flag.
fn list_result(raw: &str) -> String {
    let Ok(n) = raw.trim().parse::<f64>() else {
        return String::new();
    };
    if n == 0.0 {
        return String::new();
    }
    let colour = if n < 0.0 { "B" } else { "W" };
    let mag = n.abs();
    if mag == 999.0 {
        format!("{colour}+R")
    } else if mag == 888.0 {
        format!("{colour}+T")
    } else {
        format!("{colour}+{}", points(mag))
    }
}

fn points(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        let text = format!("{n}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn header_field<'a>(header: &'a str, key: &str) -> &'a str {
    let needle = format!("{key}:");
    let Some(at) = header.find(&needle) else {
        return "";
    };
    let value = &header[at + needle.len()..];
    let end = value.find(['\\', ',', '\n', '\r']).unwrap_or(value.len());
    value[..end].trim()
}

fn board_size(header: &str) -> Result<Size, String> {
    let raw = header_field(header, "LINE");
    let n = if raw.is_empty() {
        19
    } else {
        raw.parse::<u8>()
            .map_err(|_| format!("eweiqi board size {raw}"))?
    };
    Size::new(n, n).ok_or_else(|| format!("eweiqi board size {n} is out of range"))
}

fn tenths(raw: &str) -> Option<f32> {
    let z = raw.trim().parse::<i32>().ok()?;
    Some(z as f32 / 10.0)
}

/// `GRLT` and `ZIPSU`: the margin is whole tenths of a point, written from the integer so
/// that `12` is `1.2`, not the `1.2000000476837158` an `f32` comes back as.
fn gib_result(grlt: &str, zipsu: &str) -> String {
    let Ok(code) = grlt.trim().parse::<i32>() else {
        return String::new();
    };
    let margin = || {
        let z = zipsu.trim().parse::<u32>().unwrap_or(0);
        match z % 10 {
            0 => (z / 10).to_string(),
            f => format!("{}.{f}", z / 10),
        }
    };
    match code {
        0 => format!("B+{}", margin()),
        1 => format!("W+{}", margin()),
        3 => "B+R".to_string(),
        4 => "W+R".to_string(),
        7 => "B+T".to_string(),
        8 => "W+T".to_string(),
        _ => String::new(),
    }
}

fn gdate_day(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 10 && raw.as_bytes()[4] == b'-' && raw.as_bytes()[7] == b'-' {
        raw[..10].to_string()
    } else {
        String::new()
    }
}

/// `INI 0 1 {n}`: token 3 is the handicap count. Sabaki's `getHandicapPlacement(n,
/// {tygem: true})` uses the same corner order as [`fixed_handicap`]; no live handicap
/// record was available to confirm the server still writes it this way.
fn handicap_count(main: &str) -> u8 {
    for line in main.lines() {
        let line = line.trim();
        if !line.starts_with("INI") {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let n = tokens.nth(3).and_then(|t| t.parse().ok()).unwrap_or(0);
        if (2..=9).contains(&n) {
            return n;
        }
    }
    0
}

/// `STO 0 {seq} {colour} {x} {y}`. The sequence number counts the record's messages, not
/// its moves, so the file's order is what orders the moves.
fn sto(rest: &str, size: Size) -> Result<(Color, Point), String> {
    let mut tokens = rest.split_whitespace().skip(2);
    let color = match tokens.next() {
        Some("1") => Color::Black,
        Some("2") => Color::White,
        _ => return Err("eweiqi move has no colour".to_string()),
    };
    let x = tokens
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or("eweiqi move has no coordinate")?;
    let y = tokens
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or("eweiqi move has no coordinate")?;
    let point = size
        .try_point(x, y)
        .ok_or_else(|| format!("eweiqi move is off the board ({x}, {y})"))?;
    Ok((color, point))
}

fn push_move(tree: &mut GameTree, at: NodeId, color: Color, point: Point) -> NodeId {
    let id = tree.add_child(at);
    tree.node_mut(id).mv = Some((color, point));
    id
}

/// Commentary after `\GE`. A block that carries `REFGIBO` is a variation diagram: its
/// `STO` lines replay the game from the start and then the side line, and reading them as
/// the game multiplies the move count.
///
/// `REFSUSUN` is the number of moves played when the remark was made: in game `209557` the
/// diagram at `31` replays 31 game moves before its own, and `73` is where the commentator
/// asks about move 73. `0`, or no number, is the position before any move, the root. A
/// number past the last move goes on the last, so a closing remark sits on the final
/// position.
fn attach_comments(tree: &mut GameTree, after_ge: &str, moves: &[NodeId]) {
    for block in after_ge.split("\\RS").skip(1) {
        let Some(end) = block.find("\\RE") else {
            continue;
        };
        let block = &block[..end];
        if block.contains("REFGIBO=") {
            continue;
        }
        let Some(text) = explain(block) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let played = tagged(block, "REFSUSUN")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let id = match played.min(moves.len()) {
            0 => tree.root(),
            n => moves[n - 1],
        };
        append_comment(tree, id, &text);
    }
}

fn explain(block: &str) -> Option<String> {
    let at = block.find("REFEXPLAIN=")? + "REFEXPLAIN=".len();
    let rest = block[at..].trim_start_matches(['\r', '\n']);
    let end = rest.find("\\]")?;
    let text = rest[..end].trim_end_matches(['\r', '\n']);
    // A newline is part of the remark. `display_text` would turn it into the replacement
    // character along with every other control.
    Some(
        text.chars()
            .filter(|c| *c != '\r')
            .map(|c| {
                if c == '\n' || !c.is_control() {
                    c
                } else {
                    '\u{fffd}'
                }
            })
            .collect(),
    )
}

fn tagged<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let at = block.find(&needle)? + needle.len();
    let rest = &block[at..];
    let end = rest.find(['\\', '\n', '\r']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}

fn append_comment(tree: &mut GameTree, id: NodeId, text: &str) {
    let comment = &mut tree.node_mut(id).comment;
    if !comment.is_empty() {
        comment.push('\n');
    }
    comment.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opening() -> &'static str {
        r"
\HS
\[GAMEINFOMAIN=GBKIND:2,GRLT:3,ZIPSU:0,GONGJE:75,TCNT:5,LINE:19\]
\[GAMEINFOSUB=GDATE:2026-07-30-13-31-33\]
\[BUSERINFO=BID:柯洁 ,BLV:35\]
\[WUSERINFO=WID:党毅飞 ,WLV:35\]
\HE
\GS
2 1 0
INI 0 1 0 &4
STO 0 2 1 15 3
STO 0 3 2 3 3
STO 0 4 1 15 16
STO 0 5 2 3 16
STO 0 6 1 2 2
\GE
"
    }

    fn moves(tree: &GameTree) -> Vec<(Color, Point)> {
        tree.main_line()
            .into_iter()
            .filter_map(|id| tree.node(id).mv)
            .collect()
    }

    #[test]
    fn the_209557_opening_is_top_down() {
        let tree = parse_record(opening()).expect("opening");
        let size = Size::square(19);
        assert_eq!(
            moves(&tree),
            vec![
                (Color::Black, size.point(15, 3)),
                (Color::White, size.point(3, 3)),
                (Color::Black, size.point(15, 16)),
                (Color::White, size.point(3, 16)),
                (Color::Black, size.point(2, 2)),
            ]
        );
        let sgf = mirai_core::sgf::write(&tree, false);
        assert!(
            sgf.contains(";B[pd];W[dd];B[pq];W[dq];B[cc]"),
            "flipping y mirrors the board: {sgf}"
        );
        assert_eq!(tree.info.komi, 7.5);
        assert_eq!(tree.info.result, "B+R");
        assert_eq!(tree.info.players[0].name, "柯洁");
        assert_eq!(tree.info.players[0].rank, "P9");
        assert_eq!(tree.info.players[1].rank, "P9");
        assert_eq!(tree.info.date, "2026-07-30");
        assert_eq!(tree.info.handicap, 0);
    }

    #[test]
    fn a_variation_diagram_is_not_the_game() {
        let raw = r"
\HS
\[GAMEINFOMAIN=LINE:19,GRLT:4,GONGJE:75\]
\HE
\GS
STO 0 2 1 15 3
STO 0 3 2 3 3
\GE
\RS
\[REFSUSUN=3\]
\[REFEXPLAIN=
不要这条
\]
\[REFGIBO=
STO 0 2 1 0 0
STO 0 3 2 1 1
STO 0 4 1 2 2
STO 0 5 2 3 3
STO 0 6 1 4 4
\]
\RE
";
        let tree = parse_record(raw).expect("record");
        assert_eq!(
            moves(&tree).len(),
            2,
            "REFGIBO stones must not extend the main line"
        );
        assert!(
            tree.main_line()
                .iter()
                .all(|id| tree.node(*id).comment.is_empty()),
            "a diagram block is not a remark"
        );
        assert_eq!(tree.info.result, "W+R");
    }

    #[test]
    fn a_remark_lands_on_the_move_it_follows() {
        // `REFSUSUN` counts moves played: 1 is after the first stone, not the root, and a
        // number past the end is a closing remark on the last move.
        let raw = r"
\HS
\[GAMEINFOMAIN=LINE:19\]
\HE
\GS
STO 0 2 1 15 3
STO 0 3 2 3 3
STO 0 4 1 15 16
\GE
\RS
\[REFSUSUN=2\]
\[REFEXPLAIN=
白点三三
\]
\RE
\RS
\[REFSUSUN=2\]
\[REFEXPLAIN=
第二句
\]
\RE
\RS
\[REFSUSUN=0\]
\[REFEXPLAIN=
开局前
\]
\RE
\RS
\[REFSUSUN=9\]
\[REFEXPLAIN=
黑 中盘胜
\]
\RE
";
        let tree = parse_record(raw).expect("record");
        let line = tree.main_line();
        assert_eq!(tree.node(line[0]).comment, "开局前");
        assert!(
            tree.node(line[1]).comment.is_empty(),
            "move 1 was not mentioned"
        );
        assert_eq!(tree.node(line[2]).comment, "白点三三\n第二句");
        assert_eq!(tree.node(line[3]).comment, "黑 中盘胜");
    }

    #[test]
    fn a_pass_is_the_side_not_to_have_moved() {
        let raw = r"
\HS
\[GAMEINFOMAIN=LINE:19\]
\HE
\GS
STO 0 2 1 3 3
SKI 0 3 1
\GE
";
        let tree = parse_record(raw).expect("pass");
        let line = moves(&tree);
        assert_eq!(line[1], (Color::White, Point::PASS));

        let handicap = r"
\HS
\[GAMEINFOMAIN=LINE:19,GONGJE:0\]
\HE
\GS
INI 0 1 2 &4
SKI 0 2 1
\GE
";
        let tree = parse_record(handicap).expect("handicap pass");
        assert_eq!(
            moves(&tree),
            vec![(Color::White, Point::PASS)],
            "the colour byte on SKI is not the side to play"
        );
    }

    #[test]
    fn an_ini_count_is_a_fixed_handicap() {
        let raw = r"
\HS
\[GAMEINFOMAIN=LINE:19,GONGJE:0\]
\HE
\GS
INI 0 1 4 &4
STO 0 2 2 9 9
\GE
";
        let tree = parse_record(raw).expect("handicap");
        let size = Size::square(19);
        let stones = tree.node(tree.root()).setup.add_black.to_vec();
        assert_eq!(stones, fixed_handicap(size, 4).into_vec());
        assert_eq!(tree.info.handicap, 4);
        assert_eq!(tree.info.komi, 0.0);
        assert_eq!(moves(&tree)[0].0, Color::White);
    }

    #[test]
    fn score_results_are_tenths_of_a_point() {
        let raw = |grlt, zipsu| {
            format!(
                "\\HS\n\\[GAMEINFOMAIN=LINE:19,GRLT:{grlt},ZIPSU:{zipsu},GONGJE:75\\]\n\\HE\n\\GS\n\\GE\n"
            )
        };
        assert_eq!(parse_record(&raw(0, 55)).unwrap().info.result, "B+5.5");
        assert_eq!(parse_record(&raw(1, 15)).unwrap().info.result, "W+1.5");
        // Not every tenth survives a float: written from the integer.
        assert_eq!(parse_record(&raw(0, 12)).unwrap().info.result, "B+1.2");
        assert_eq!(parse_record(&raw(1, 1)).unwrap().info.result, "W+0.1");
        assert_eq!(parse_record(&raw(7, 0)).unwrap().info.result, "B+T");
        assert_eq!(parse_record(&raw(8, 0)).unwrap().info.result, "W+T");
        assert_eq!(parse_record(&raw(5, 0)).unwrap().info.result, "");
        let six = "\\HS\n\\[GAMEINFOMAIN=LINE:19,GONGJE:65\\]\n\\HE\n\\GS\n\\GE\n";
        assert_eq!(parse_record(six).unwrap().info.komi, 6.5);
    }

    #[test]
    fn an_off_board_stone_is_rejected() {
        let raw = r"
\HS
\[GAMEINFOMAIN=LINE:19\]
\HE
\GS
STO 0 2 1 19 0
\GE
";
        assert!(parse_record(raw).is_err());
    }

    #[test]
    fn a_search_is_newest_first_deduped_and_capped() {
        let mut rows = Vec::new();
        for i in 0..205 {
            let date = format!("2020-01-{:02} 00:00:00", (i % 28) + 1);
            rows.push(format!(
                r#"{{"id":"{i}","BName":"A","WName":"B","Date":"{date}","GameResult":"-5.5","Susun":"10","BRank":"26","CateName":" 杯 ","SubCateName":"决赛"}}"#
            ));
        }
        // An older duplicate of id 3 must not displace the newer row, and must not appear twice.
        rows.push(
            r#"{"id":"3","BName":"old","WName":"old","Date":"2010-01-01 00:00:00","GameResult":"0"}"#
                .to_string(),
        );
        let body = format!("{{\n\"list\":[{}]\n}}", rows.join(","));
        let records = parse_search(&body).expect("list");
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records.windows(2).all(|w| w[0].date >= w[1].date));
        assert_eq!(records.iter().filter(|r| r.id == "3").count(), 1);
        let kept = records.iter().find(|r| r.id == "3").unwrap();
        assert_eq!(kept.black, "A");
        assert_ne!(kept.date, "2010-01-01 00:00:00");
        assert_eq!(kept.result, "B+5.5");
        assert_eq!(kept.black_rank, "9d");
        assert_eq!(kept.event, "杯 决赛");
        assert_eq!(kept.board_size, None);
        assert_eq!(kept.source, Source::Eweiqi);
    }

    #[test]
    fn a_bare_bom_is_no_match_and_an_error_is_not_a_record() {
        assert_eq!(parse_search("\u{feff}").unwrap(), Vec::new());
        assert_eq!(parse_search("\u{feff}\n").unwrap(), Vec::new());
        let err = parse_record("\u{feff}[Error]:기보 데이터가 없습니다.").unwrap_err();
        assert!(err.contains("[Error]"), "{err}");
        assert!(parse_record("not a record").is_err());
    }

    #[test]
    fn list_results_and_ranks_use_the_documented_codes() {
        assert_eq!(list_result("-999"), "B+R");
        assert_eq!(list_result("999"), "W+R");
        assert_eq!(list_result("-888"), "B+T");
        assert_eq!(list_result("888"), "W+T");
        assert_eq!(list_result("1.5"), "W+1.5");
        assert_eq!(list_result("-5.5"), "B+5.5");
        assert_eq!(list_result("0"), "");
        assert_eq!(list_result("nope"), "");
        assert_eq!(rank(35), "P9");
        assert_eq!(rank(27), "P1");
        assert_eq!(rank(18), "1d");
        assert_eq!(rank(17), "1k");
        assert_eq!(rank(0), "18k");
        assert_eq!(rank(-1), "");
    }

    #[test]
    fn the_urls_are_the_anonymous_catalog() {
        assert_eq!(
            search_url("柯洁"),
            "http://client.eweiqi.com/gibo/gibo_load_list.php?type=5&sword=%E6%9F%AF%E6%B4%81&lang=cn"
        );
        let url = record_url("209557");
        assert_eq!(
            url,
            "http://client.eweiqi.com/gibo/gibo_load_data.php?id=209557"
        );
        assert!(!url.contains("mode"));
    }
}
