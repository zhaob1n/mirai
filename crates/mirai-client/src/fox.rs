// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Fox (foxwq) kifu lookup: the endpoints, the reply shapes, and the two things Fox's SGF
//! needs before mirai can open it.
//!
//! HTTP is not here. A frontend that can satisfy [`Fetch`] — a `Send` future — gets the
//! async wrappers; one that cannot (GIO's futures are `!Send`) composes the [`user_url`] /
//! [`parse_user`] pairs against its own transport. Either way there is one description of
//! the protocol.

use mirai_core::{Color, GameTree, Node, NodeId, Point, sgf};
use serde::{Deserialize, Serialize};

const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

pub fn user_agent() -> &'static str {
    UA
}

pub trait Fetch {
    fn get(&self, url: &str) -> impl Future<Output = Result<String, String>> + Send;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoxUser {
    pub uid: String,
    pub name: String,
    pub hidden: bool,
}

/// One row of a player's game list, as Fox sends it.
///
/// The field names are Fox's; the Rust names are not. Both frontends deserialise this from
/// the API, and the desktop also writes it back out as its last-search cache, so the two
/// must agree byte for byte — which they do because there is only one type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FoxGame {
    #[serde(rename = "chessid", default)]
    pub chess_id: String,
    #[serde(rename = "blacknick", default)]
    pub black_nick: String,
    #[serde(rename = "blackenname", default)]
    pub black_en_name: String,
    #[serde(rename = "whitenick", default)]
    pub white_nick: String,
    #[serde(rename = "whiteenname", default)]
    pub white_en_name: String,
    #[serde(rename = "blackdan", default)]
    pub black_dan: i32,
    #[serde(rename = "whitedan", default)]
    pub white_dan: i32,
    #[serde(rename = "blackocc", default)]
    pub black_occ: i32,
    #[serde(rename = "whiteocc", default)]
    pub white_occ: i32,
    #[serde(default)]
    pub winner: i32,
    /// Margin in hundredths of a point; `-1` is a resignation and `-2` a lost flag.
    #[serde(default)]
    pub point: i32,
    #[serde(rename = "movenum", default)]
    pub moves: u32,
    #[serde(rename = "boardsize", default = "default_board_size")]
    pub board_size: u8,
    #[serde(rename = "starttime", default)]
    pub date: String,
    #[serde(default)]
    pub title: String,
}

const fn default_board_size() -> u8 {
    19
}

impl FoxGame {
    /// Nickname, falling back to the English name, then to the colour.
    pub fn black(&self) -> &str {
        player_name(&self.black_nick, &self.black_en_name, "Black")
    }

    pub fn white(&self) -> &str {
        player_name(&self.white_nick, &self.white_en_name, "White")
    }

    /// `"柯洁 (P9) vs 党毅飞 (P8)"` — how the record names itself in a list or a title.
    pub fn matchup(&self) -> String {
        format!(
            "{} ({}) vs {} ({})",
            display_text(self.black()),
            rank(self.black_dan, self.black_occ),
            display_text(self.white()),
            rank(self.white_dan, self.white_occ)
        )
    }

    /// SGF result text: `"B+3.5"`, `"W+R"`, or `"No result"` when Fox reported no winner.
    pub fn result(&self) -> String {
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

/// Fox's rank encoding: `occupation != 0` is a professional, otherwise `dan` counts up from
/// 17 = 1 dan and down into kyu.
pub fn rank(dan: i32, occupation: i32) -> String {
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

/// Fox counts points in hundredths: `350` is `3.5`, `375` is `3.75`, `700` is `7`.
pub fn hundredths(value: u32) -> String {
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

/// Replaces control characters, which Fox does put in nicknames and titles, so a label
/// cannot be made to do something a label should not.
pub fn display_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

// -- endpoints ----------------------------------------------------------------------------

pub fn user_url(nickname: &str) -> String {
    format!(
        "https://newframe.foxwq.com/cgi/QueryUserInfoPanel?srcuid=0&username={}",
        urlencoding(nickname)
    )
}

pub fn games_url(uid: &str) -> String {
    format!(
        "https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList?dstuid={}&type=1&fetchnum=200",
        urlencoding(uid)
    )
}

pub fn sgf_url(chess_id: &str) -> String {
    format!(
        "https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess?chessid={}",
        urlencoding(chess_id)
    )
}

/// Percent-encode a nickname without pulling in a crate.
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

// -- replies ------------------------------------------------------------------------------

fn json_err(body: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("fox response is not JSON: {e}"))?;
    let result = value.get("result").and_then(|v| v.as_i64()).unwrap_or(-1);
    let errcode = value.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
    if result != 0 || errcode != 0 {
        let msg = value
            .get("resultstr")
            .or_else(|| value.get("errmsg"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("fox request failed");
        return Err(display_text(msg));
    }
    Ok(value)
}

pub fn parse_user(body: &str, nickname: &str) -> Result<FoxUser, String> {
    let value = json_err(body)?;
    Ok(FoxUser {
        uid: value
            .get("uid")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .unwrap_or_default(),
        name: value
            .get("username")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(nickname)
            .to_string(),
        hidden: value
            .get("hide_game_record")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            == 1,
    })
}

pub fn parse_games(body: &str) -> Result<Vec<FoxGame>, String> {
    let value = json_err(body)?;
    let Some(list) = value.get("chesslist").cloned() else {
        return Ok(Vec::new());
    };
    let rows: Vec<FoxGame> = serde_json::from_value(list)
        .map_err(|e| format!("fox sent a game list this build cannot read: {e}"))?;
    Ok(rows
        .into_iter()
        .filter(|game| !game.chess_id.is_empty())
        .collect())
}

/// The raw `chess` field of a `YHWQFetchChess` reply, exactly as Fox sent it.
pub fn sgf_field(body: &str) -> Result<String, String> {
    let value = json_err(body)?;
    value
        .get("chess")
        .and_then(|v| v.as_str())
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "fox response has no SGF".to_string())
}

/// The `chess` field of a `YHWQFetchChess` reply, with the dialect normalised.
pub fn parse_sgf_body(body: &str) -> Result<String, String> {
    sgf_field(body).map(|raw| normalize_fox_sgf(&raw))
}

pub async fn lookup_user<F: Fetch>(fetch: &F, nickname: &str) -> Result<FoxUser, String> {
    let body = fetch.get(&user_url(nickname)).await?;
    parse_user(&body, nickname)
}

pub async fn list_games<F: Fetch>(fetch: &F, uid: &str) -> Result<Vec<FoxGame>, String> {
    let body = fetch.get(&games_url(uid)).await?;
    parse_games(&body)
}

pub async fn fetch_sgf<F: Fetch>(fetch: &F, chess_id: &str) -> Result<String, String> {
    let body = fetch.get(&sgf_url(chess_id)).await?;
    parse_sgf_body(&body)
}

// -- the SGF dialect ------------------------------------------------------------------------

/// Fox SGF dialect: expand its C-style escapes, and scale a komi written in *zi*.
///
/// Fox writes `\r`, `\n` and `\t` both between properties and *inside* text values. Outside
/// a value, dropping only the backslash leaves a literal `rn`, which is not whitespace, so a
/// following variation stops being recognised as a sibling tree. Inside a value the damage is
/// quieter but worse: SGF's own rule is that `\` escapes the next character, so `C[a\nb]`
/// parses as the comment `anb` — every line break in a Fox comment turned into the letter
/// `n`. Both cases become real whitespace here, and every other escape is passed through
/// untouched — the whole next scalar, not the next byte — so the SGF parser still sees
/// `\]`, `\\` and `\中`.
pub fn normalize_fox_sgf(raw: &str) -> String {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_value = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            let Some(next) = chars.peek().copied() else {
                // A trailing backslash outside a value is Fox noise; inside one it is an
                // incomplete escape the parser still has to see.
                if in_value {
                    out.push('\\');
                }
                break;
            };
            if let Some(ch) = match next {
                'r' => Some('\r'),
                'n' => Some('\n'),
                't' => Some('\t'),
                _ => None,
            } {
                chars.next();
                out.push(ch);
                continue;
            }
            if !in_value {
                // An escape Fox did not mean: outside a value there is nothing to escape.
                continue;
            }
            // `\]`, `\\` and a multibyte scalar belong to the parser. Copying one byte
            // here panics on the next char boundary and writes mojibake.
            chars.next();
            out.push('\\');
            out.push(next);
            continue;
        }
        if c == '[' {
            in_value = true;
        } else if c == ']' && in_value {
            // Every escape above is consumed whole, so a `]` reached here is real.
            in_value = false;
        }
        out.push(c);
    }
    scale_fox_komi(&out)
}

/// Fox writes Chinese-rules komi in *zi*, hundredths: `375` is 3¾ zi, which is 7.5 points.
///
/// A value at or beyond ±200 is in hundredths and is divided down. It is then doubled only
/// when the result is a quarter or three-quarters of a point within ±4 — the shape a komi in
/// zi has. Anything else is already in points and is left exactly alone, so a record that
/// says 7.5 does not come out as 15.
fn scale_fox_komi(sgf: &str) -> String {
    // First root only. A later game's KM is not this record's komi.
    let Some(span) = sgf::root_property(sgf.as_bytes(), b"KM") else {
        return sgf.to_string();
    };
    let Ok(raw) = sgf[span.clone()].trim().parse::<f32>() else {
        return sgf.to_string();
    };
    let scaled = if raw.abs() >= 200.0 { raw / 100.0 } else { raw };
    let hundredths = (scaled.abs().fract() * 100.0).round() as i32;
    let komi = if scaled.abs() <= 4.0 && matches!(hundredths, 25 | 75) {
        scaled * 2.0
    } else {
        scaled
    };
    if komi == raw {
        return sgf.to_string();
    }
    format!("{}{komi}{}", &sgf[..span.start], &sgf[span.end..])
}

// -- handicap ------------------------------------------------------------------------------

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

/// Copies each `roots` subtree of `source` under `parent` in `target`, in order.
///
/// Iterative, not recursive: a Fox main line is one node per move, and this runs on the GTK
/// main thread, whose stack a frame per move would overflow on a long game. Popping the
/// stack creates nodes in preorder, so pushing each node's children in reverse still adds
/// every parent's children in their original order.
fn copy_branches(source: &GameTree, roots: &[NodeId], target: &mut GameTree, parent: NodeId) {
    let mut stack: Vec<(NodeId, NodeId)> = roots.iter().rev().map(|&id| (id, parent)).collect();
    while let Some((source_id, parent)) = stack.pop() {
        let id = target.add_child(parent);
        copy_payload(source.node(source_id), target.node_mut(id));
        stack.extend(
            source
                .children(source_id)
                .iter()
                .rev()
                .map(|&child| (child, id)),
        );
    }
}

/// Folds Fox's leading run of one-stone nodes into a single root handicap.
///
/// Fox records a handicap as a chain of nodes with one `AB` each — or, in some rooms, as a
/// chain of Black moves — which every other program reads as a game where Black played
/// several times in a row. Everything after the run is copied onto the rebuilt root, root
/// variations included.
pub fn normalize_handicap(mut tree: GameTree) -> GameTree {
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
    copy_branches(&tree, &continuations, &mut rebuilt, target_root);
    rebuilt
}

/// Fox's SGF as a record mirai can open: escapes expanded, komi scaled, and the leading run
/// of single-stone nodes folded into one root handicap.
pub fn parse_record(text: &str) -> Result<GameTree, String> {
    let cleaned = normalize_fox_sgf(text);
    let mut trees = sgf::parse_str(&cleaned).map_err(|e| e.to_string())?;
    if trees.is_empty() {
        return Err("the response holds no game".to_string());
    }
    Ok(normalize_handicap(trees.remove(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backslashes_outside_values_are_dropped() {
        let raw = "(;GM[1]FF[4]\\r\\nSZ[19]\\r\\nKM[375]\\r\\nC[hello\\]world];B[pd])";
        let out = normalize_fox_sgf(raw);
        assert!(!out.contains("\\r"), "{out}");
        assert!(out.contains("\r\nSZ[19]"), "{out}");
        assert!(out.contains("KM[7.5]"), "{out}");
        assert!(out.contains("C[hello\\]world]"), "{out}");
    }

    #[test]
    fn chinese_names_stay_utf8() {
        let raw = "(;GM[1]FF[4]PB[柯洁]\\r\\nPW[党毅飞]KM[375])";
        let out = normalize_fox_sgf(raw);
        assert!(out.contains("PB[柯洁]"), "{out}");
        assert!(out.contains("PW[党毅飞]"), "{out}");
    }

    #[test]
    fn escaped_newlines_keep_sibling_variations() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[375]\\r\\n(;B[pd])\\r\\n(;B[dd]))";
        let out = normalize_fox_sgf(raw);
        let trees = mirai_core::sgf::parse(out.as_bytes()).expect(out.as_str());
        assert_eq!(trees.len(), 1);
    }

    /// Fox writes its comments with C-style `\n`. SGF's own rule is that a backslash escapes
    /// the next character, so leaving them alone turned every line break into the letter `n`.
    #[test]
    fn escaped_newlines_inside_a_comment_become_line_breaks() {
        let raw = "(;GM[1]FF[4]SZ[19]C[黑中盘胜！\\n这盘棋右下角打劫\\n包括111的找劫也亏损];B[pd])";
        let out = normalize_fox_sgf(raw);
        assert!(out.contains("胜！\n这盘棋"), "{out}");

        let trees = mirai_core::sgf::parse(out.as_bytes()).expect(out.as_str());
        let tree = &trees[0];
        let comment = &tree.node(tree.root()).comment;
        assert_eq!(comment.lines().count(), 3, "{comment:?}");
        assert!(!comment.contains("n这盘棋"), "{comment:?}");
    }

    /// Komi in zi is scaled and doubled; komi already in points is left exactly alone, and
    /// a reverse komi keeps its sign — `value < 200.0` used to skip every negative value.
    #[test]
    fn komi_is_scaled_only_when_it_is_written_in_zi() {
        let komi_of = |sgf: &str| {
            let trees = sgf::parse_str(&normalize_fox_sgf(sgf)).expect(sgf);
            trees[0].info.komi
        };
        assert_eq!(komi_of("(;GM[1]FF[4]SZ[19]KM[375])"), 7.5);
        assert_eq!(komi_of("(;GM[1]FF[4]SZ[19]KM[-375])"), -7.5);
        assert_eq!(komi_of("(;GM[1]FF[4]SZ[19]KM[750])"), 7.5);
        assert_eq!(komi_of("(;GM[1]FF[4]SZ[19]KM[650])"), 6.5);
        assert_eq!(komi_of("(;GM[1]FF[4]SZ[19]KM[7.5])"), 7.5);
        assert_eq!(komi_of("(;GM[1]FF[4]SZ[19]KM[0])"), 0.0);
    }

    /// A comment mentioning `KM[` must not be mistaken for the root's komi.
    #[test]
    fn only_the_root_komi_is_rescaled() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[375]C[not KM[999] here];B[pd])";
        let out = normalize_fox_sgf(raw);
        assert!(out.contains("KM[7.5]"), "{out}");
        assert!(out.contains("KM[999]"), "{out}");
    }

    /// An unknown escape copies the next scalar, not the next byte. Slicing one byte
    /// off a multibyte UTF-8 character panics on the following char boundary, and the
    /// byte pushed in its place is mojibake.
    #[test]
    fn an_unknown_escape_keeps_a_multibyte_scalar() {
        let raw = "(;GM[1]FF[4]SZ[19]C[a\\中b\\🙂c])";
        let out = normalize_fox_sgf(raw);
        assert!(out.contains("C[a\\中b\\🙂c]"), "{out}");

        // The same escape between properties must not panic either.
        let raw = "(;GM[1]FF[4]\\中SZ[19]KM[375])";
        let out = normalize_fox_sgf(raw);
        assert!(out.contains("中SZ[19]"), "{out}");
        assert!(out.contains("KM[7.5]"), "{out}");
    }

    /// `KM[` inside a comment that precedes the real property used to be the one
    /// that was scaled, because the root-node slice was then searched with `find`.
    #[test]
    fn a_komi_inside_a_preceding_comment_is_not_rescaled() {
        let raw = "(;GM[1]FF[4]SZ[19]C[not KM[999]]KM[375])";
        let out = normalize_fox_sgf(raw);
        assert!(out.contains("KM[7.5]"), "{out}");
        assert!(out.contains("KM[999]"), "{out}");
        assert!(!out.contains("KM[9.99]"), "{out}");
    }

    #[test]
    fn fox_escapes_and_quarter_point_komi_are_normalised() {
        let raw = "\u{feff}(;GM[1]\\r\\nFF[4]\\r\\nSZ[19]KM[375]PB[Black]PW[White];B[pd]C[a\\]b])";
        let tree = parse_record(raw).expect("Fox SGF");
        assert_eq!(tree.info.komi, 7.5);
        let first = tree.children(tree.root())[0];
        assert_eq!(tree.node(first).comment, "a]b");
    }

    #[test]
    fn fox_setup_nodes_become_one_root_handicap() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[0]HA[3]PB[柯洁]PW[党毅飞];AB[dd];AB[pd];AB[dp];W[dm])";
        let tree = parse_record(raw).expect("Fox handicap SGF");
        let root = tree.root();
        assert_eq!(tree.node(root).setup.add_black.len(), 3);
        assert_eq!(tree.info.handicap, 3);
        assert_eq!(tree.info.komi, 0.0);
        let first = tree.children(root)[0];
        assert_eq!(tree.node(first).mv.map(|(c, _)| c), Some(Color::White));
    }

    #[test]
    fn handicap_normalisation_preserves_root_variations() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[0](;AB[dd];AB[pd];W[qq])(;B[aa]))";
        let tree = parse_record(raw).expect("Fox handicap variations");
        assert_eq!(tree.node(tree.root()).setup.add_black.len(), 2);
        assert_eq!(tree.children(tree.root()).len(), 2);
    }

    #[test]
    fn consecutive_black_handicap_moves_are_promoted_too() {
        let raw = "(;GM[1]FF[4]SZ[19]KM[0];B[dd];B[pd];B[dp];W[dm])";
        let tree = parse_record(raw).expect("Fox handicap SGF");
        assert_eq!(tree.info.handicap, 3);
        assert_eq!(tree.node(tree.root()).setup.add_black.len(), 3);
    }

    /// Copying the game after the prefix used to recurse once per node, so a handicap game
    /// with a long main line overflowed the stack — the GTK main thread's, when the Fox
    /// dialog opens a record. The variation checks that siblings still come out in order.
    #[test]
    fn a_long_handicap_game_is_copied_whole_and_in_order() {
        use mirai_core::{GameInfo, RuleSet, Size};
        const MOVES: u32 = 200_000;
        let size = Size::square(19);
        let mut tree = GameTree::new(GameInfo::new(size, RuleSet::Chinese));
        let mut id = tree.root();
        for stone in [size.point(3, 3), size.point(15, 3)] {
            id = tree.add_child(id);
            tree.node_mut(id).setup.add_black.push(stone);
        }
        let mut forked = id;
        for i in 0..MOVES {
            id = tree.add_child(id);
            let color = [Color::White, Color::Black][i as usize % 2];
            let node = tree.node_mut(id);
            node.mv = Some((color, Point((i % 361) as u16)));
            node.comment = i.to_string();
            if i == 2 {
                forked = id;
            }
        }
        let side = tree.add_child(forked);
        tree.node_mut(side).comment = "side".into();

        let tree = normalize_handicap(tree);
        assert_eq!(tree.info.handicap, 2);
        let mut id = tree.root();
        assert_eq!(tree.node(id).setup.add_black.len(), 2);
        for i in 0..MOVES {
            let children = tree.children(id);
            assert_eq!(children.len(), if i == 3 { 2 } else { 1 }, "move {i}");
            id = children[0];
            let node = tree.node(id);
            let color = [Color::White, Color::Black][i as usize % 2];
            assert_eq!(node.mv, Some((color, Point((i % 361) as u16))), "move {i}");
            assert_eq!(node.comment, i.to_string());
            if i == 2 {
                assert_eq!(tree.node(tree.children(id)[1]).comment, "side");
            }
        }
        assert!(tree.children(id).is_empty());
    }

    #[test]
    fn fox_result_text_uses_the_documented_units() {
        assert_eq!(hundredths(350), "3.5");
        assert_eq!(hundredths(375), "3.75");
        assert_eq!(hundredths(700), "7");
        assert_eq!(rank(23, 0), "6d");
        assert_eq!(rank(17, 0), "1k");
        assert_eq!(rank(108, 1), "P9");
    }

    #[test]
    fn a_game_row_names_itself_from_whichever_name_fox_sent() {
        let row = FoxGame {
            chess_id: "1".to_string(),
            black_nick: String::new(),
            black_en_name: "kejie".to_string(),
            white_nick: "党毅飞".to_string(),
            white_en_name: String::new(),
            black_dan: 108,
            white_dan: 107,
            black_occ: 1,
            white_occ: 1,
            winner: 1,
            point: 350,
            moves: 210,
            board_size: 19,
            date: "2026-01-01".to_string(),
            title: String::new(),
        };
        assert_eq!(row.black(), "kejie");
        assert_eq!(row.white(), "党毅飞");
        assert_eq!(row.matchup(), "kejie (P9) vs 党毅飞 (P8)");
        assert_eq!(row.result(), "B+3.5");
    }

    /// The API reply and the desktop's cache file are the same shape, so a cached row
    /// reopens as the row it was.
    #[test]
    fn a_game_row_round_trips_through_foxs_own_field_names() {
        let body = r#"{"result":0,"chesslist":[
            {"chessid":"abc","blacknick":"A","whitenick":"B","movenum":42,"winner":2,"point":-1},
            {"chessid":"","blacknick":"skip"}
        ]}"#;
        let rows = parse_games(body).expect("game list");
        assert_eq!(rows.len(), 1, "rows without a chessid are dropped");
        assert_eq!(rows[0].chess_id, "abc");
        assert_eq!(rows[0].moves, 42);
        assert_eq!(rows[0].board_size, 19, "a missing boardsize defaults to 19");
        assert_eq!(rows[0].result(), "W+R");

        let json = serde_json::to_string(&rows[0]).expect("cache write");
        let back: FoxGame = serde_json::from_str(&json).expect("cache read");
        assert_eq!(back, rows[0]);
    }

    #[test]
    fn a_service_error_is_reported_with_foxs_own_message() {
        let body = r#"{"result":1,"resultstr":"no such user"}"#;
        assert_eq!(parse_user(body, "nobody"), Err("no such user".to_string()));
    }
}
