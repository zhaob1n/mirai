// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Yike (弈客) kifu lookup: the professionals' library and the members' own games.
//!
//! The two are different services with different ids. HTTP is not here: [`crate::kifu`]
//! runs the lookup over a frontend's transport. What the replies look like, and how a
//! Chinese result becomes an SGF `RE`, is this module
//! ([`YIKE_KIFU_API_SPEC.md`](../../../docs/dev/YIKE_KIFU_API_SPEC.md)).

use mirai_core::{Color, GameTree, sgf};
use serde_json::Value;

use crate::kifu::{Player, Record, Source, display_text, urlencoding};

/// Rows a library page holds. The list does not take a page size.
pub const LIBRARY_PAGE: usize = 20;
/// Rows an account page holds.
pub const ACCOUNT_PAGE: usize = 30;

pub struct LibraryPage {
    /// Every record the player has in the library, not only this page's.
    pub total: usize,
    pub records: Vec<Record>,
}

pub struct AccountPage {
    /// `None` when there is no such account.
    pub name: Option<String>,
    /// The account's grade as `Record` ranks read (`2.6d`), or empty.
    pub grade: String,
    pub records: Vec<Record>,
}

const LIBRARY: &str = "https://home.yikeweiqi.com/player/api";
const ACCOUNT: &str = "https://api.yikeweiqi.com";

// -- urls ---------------------------------------------------------------------------------

pub fn library_search_url(name: &str) -> String {
    format!("{LIBRARY}/player_name_search?key={}", urlencoding(name))
}

pub fn account_search_url(text: &str) -> String {
    format!(
        "{ACCOUNT}/reguser/search?condition={}&page=1",
        urlencoding(text)
    )
}

pub fn library_games_url(pid: &str, start: usize) -> String {
    format!(
        "{LIBRARY}/game_search_player?pid={}&start={start}",
        urlencoding(pid)
    )
}

/// Online games only. Offline tournament rows (`type` 0 and 2) come back as an empty
/// SGF, so they are not what a download can open.
pub fn account_games_url(id: &str, page: usize) -> String {
    format!("{ACCOUNT}/reguser/games/{}/1/{page}", urlencoding(id))
}

pub fn library_sgf_url(gid: &str) -> String {
    format!("{LIBRARY}/game_sgf?gid={}", urlencoding(gid))
}

pub fn account_sgf_url(id: &str) -> String {
    format!("{ACCOUNT}/usersgf/detail?id={}&type=1", urlencoding(id))
}

// -- search -------------------------------------------------------------------------------

pub fn parse_library_search(body: &str, query: &str) -> Result<Vec<Player>, String> {
    let value = json(body)?;
    let query = query.trim();
    let mut players = Vec::new();
    for row in value
        .get("matches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = text(row.get("name"));
        if name != query {
            continue;
        }
        let id = text(row.get("pid"));
        if id.is_empty() {
            continue;
        }
        players.push(Player {
            source: Source::YikeLibrary,
            id,
            name: display_text(&name),
            rank: String::new(),
        });
    }
    Ok(players)
}

pub fn parse_account_search(body: &str, query: &str) -> Result<Vec<Player>, String> {
    let value = status_ok(body)?;
    let query = query.trim();
    let mut players = Vec::new();
    let rows = value
        .get("Result")
        .and_then(|v| v.get("data"))
        .and_then(Value::as_array);
    for row in rows.into_iter().flatten() {
        let nickname = text(row.get("nickname"));
        let cgf = text(row.get("cgf_id"));
        let name_hit = nickname.eq_ignore_ascii_case(query);
        let cgf_hit = !cgf.is_empty() && cgf.eq_ignore_ascii_case(query);
        if !name_hit && !cgf_hit {
            continue;
        }
        let id = text(row.get("id"));
        if id.is_empty() {
            continue;
        }
        players.push(Player {
            source: Source::YikeAccount,
            id,
            name: display_text(&nickname),
            rank: account_rank(&text(row.get("grade"))),
        });
    }
    Ok(players)
}

// -- lists --------------------------------------------------------------------------------

pub fn parse_library_games(body: &str) -> Result<LibraryPage, String> {
    let value = json(body)?;
    let total = value
        .get("total")
        .map(json_usize)
        .transpose()?
        .ok_or_else(|| "yike library list has no total".to_string())?;
    let games = value
        .get("games")
        .and_then(Value::as_array)
        .ok_or_else(|| "yike library list has no games".to_string())?;
    let mut records = Vec::with_capacity(games.len());
    for row in games {
        let id = text(row.get("id"));
        if id.is_empty() {
            continue;
        }
        records.push(Record {
            source: Source::YikeLibrary,
            id,
            black: display_text(&text(row.get("black"))),
            black_rank: library_rank(&text(row.get("black_rank"))),
            white: display_text(&text(row.get("white"))),
            white_rank: library_rank(&text(row.get("white_rank"))),
            result: result_text(&text(row.get("result"))),
            moves: None,
            board_size: None,
            date: text(row.get("date")),
            event: display_text(&text(row.get("event"))),
        });
    }
    Ok(LibraryPage { total, records })
}

pub fn parse_account_games(body: &str) -> Result<AccountPage, String> {
    let value = status_ok(body)?;
    let result = value.get("Result").cloned().unwrap_or(Value::Null);
    let user = result.get("user");
    let name = user
        .and_then(|u| u.get("name"))
        .and_then(Value::as_str)
        .map(display_text);
    // The key is `grage` on the wire. A missing account sends `name: null` and a
    // placeholder grade; the name is what says the account is not there.
    let grade = account_rank(&text(user.and_then(|u| u.get("grage"))));
    let list = result.get("list").and_then(Value::as_array);
    let mut records = Vec::new();
    for row in list.into_iter().flatten() {
        let id = text(row.get("GameId"));
        if id.is_empty() {
            continue;
        }
        let date = text(row.get("GameDate"));
        let location = text(row.get("GameLocation"));
        let tour = text(row.get("TourName"));
        let event = if tour == format!("{date}_{location}") {
            location
        } else {
            tour
        };
        records.push(Record {
            source: Source::YikeAccount,
            id,
            black: display_text(&text(row.get("BlackName"))),
            black_rank: account_rank(&text(row.get("BlackPlayerScore"))),
            white: display_text(&text(row.get("WhiteName"))),
            white_rank: account_rank(&text(row.get("WhitePlayerScore"))),
            result: account_result(&text(row.get("Result")), &text(row.get("ResultDesc"))),
            moves: row.get("HandsCount").and_then(json_u32),
            board_size: row.get("BoardSize").and_then(json_u8),
            date,
            event: display_text(&event),
        });
    }
    Ok(AccountPage {
        name,
        grade,
        records,
    })
}

// -- records ------------------------------------------------------------------------------

pub fn parse_library_sgf(body: &str) -> Result<GameTree, String> {
    let value = json(body)?;
    if value.get("code").and_then(Value::as_i64) == Some(1) {
        let msg = text(value.get("msg"));
        return Err(if msg.is_empty() {
            "yike library record was not found".to_string()
        } else {
            display_text(&msg)
        });
    }
    let raw = value
        .get("sgf")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "yike library record has no sgf".to_string())?;
    let mut tree = one_tree(raw)?;
    if let Ok(komi) = text(value.get("komi")).parse::<f32>() {
        tree.info.komi = komi;
    }
    fill_player(
        &mut tree,
        Color::Black,
        value.get("black"),
        value.get("black_rank"),
    );
    fill_player(
        &mut tree,
        Color::White,
        value.get("white"),
        value.get("white_rank"),
    );
    let result = text(value.get("result"));
    if !result.is_empty() {
        tree.info.result = result_text(&result);
    }
    let date = text(value.get("date"));
    if !date.is_empty() {
        tree.info.date = date;
    }
    let event = text(value.get("event"));
    if !event.is_empty() {
        tree.info.event = display_text(&event);
    }
    Ok(tree)
}

pub fn parse_account_sgf(body: &str) -> Result<GameTree, String> {
    let value = status_ok(body)?;
    let raw = value
        .get("Result")
        .and_then(|v| v.get("Sgf"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "yike account record has no sgf".to_string())?;
    let mut tree = one_tree(raw)?;
    if !has_play(&tree) {
        return Err("no moves recorded".to_string());
    }
    // The file's `RE` is the Chinese description. The list and the file must agree.
    if !tree.info.result.is_empty() {
        tree.info.result = result_text(&tree.info.result);
    }
    Ok(tree)
}

fn fill_player(tree: &mut GameTree, color: Color, name: Option<&Value>, rank: Option<&Value>) {
    let slot = &mut tree.info.players[color.index()];
    let name = text(name);
    if !name.is_empty() {
        slot.name = display_text(&name);
    }
    // Detail ranks are already `9p`. The list's `九段` is a different field.
    let rank = text(rank);
    if !rank.is_empty() {
        slot.rank = rank;
    }
}

fn one_tree(raw: &str) -> Result<GameTree, String> {
    let mut trees = sgf::parse_str(raw).map_err(|e| e.to_string())?;
    if trees.is_empty() {
        return Err("the response holds no game".to_string());
    }
    Ok(trees.remove(0))
}

fn has_play(tree: &GameTree) -> bool {
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        if node.mv.is_some() || !node.setup.add_black.is_empty() || !node.setup.add_white.is_empty()
        {
            return true;
        }
        stack.extend(node.children.iter().copied());
    }
    false
}

// -- results and ranks --------------------------------------------------------------------

/// A Yike outcome as an SGF `RE`.
///
/// The library writes a sentence (`黑胜1又3/4子`). An account row also has a code (`B+`,
/// `W+`, `BL`, `D`) whose description may be only the margin, or nothing. A 子 is two
/// points; 目 and 点 are already points. A bare number is read as points, because it has
/// no 子 to double. Nothing this can read becomes `""`, not a guess.
pub fn result_text(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    if text.eq_ignore_ascii_case("BL") || text.contains("双负") {
        return "Void".to_string();
    }
    if text.eq_ignore_ascii_case("D") || is_draw(text) {
        return "0".to_string();
    }
    let Some((winner, rest)) = winner(text) else {
        return String::new();
    };
    if let Some(kind) = decision(rest) {
        return format!("{winner}+{kind}");
    }
    match margin(rest) {
        Some(margin) => format!("{winner}+{margin}"),
        None => format!("{winner}+"),
    }
}

fn account_result(code: &str, desc: &str) -> String {
    let code = code.trim();
    let desc = desc.trim();
    if code.eq_ignore_ascii_case("BL") || desc.contains("双负") {
        return result_text("BL");
    }
    if code.eq_ignore_ascii_case("D") || is_draw(desc) {
        return result_text("D");
    }
    if desc.starts_with('黑') || desc.starts_with('白') {
        return result_text(desc);
    }
    if let Some(colour) = code_colour(code) {
        if desc.is_empty() {
            return result_text(code);
        }
        let han = if colour == "B" { "黑" } else { "白" };
        return result_text(&format!("{han}{desc}"));
    }
    result_text(desc)
}

fn code_colour(code: &str) -> Option<&'static str> {
    let bare = code.strip_suffix('+').unwrap_or(code);
    if bare.eq_ignore_ascii_case("B") {
        Some("B")
    } else if bare.eq_ignore_ascii_case("W") {
        Some("W")
    } else {
        None
    }
}

fn is_draw(text: &str) -> bool {
    text == "和" || text == "和棋" || text == "和局" || text.starts_with('和')
}

fn winner(text: &str) -> Option<(&'static str, &str)> {
    if let Some(rest) = text.strip_prefix('黑') {
        return Some(("B", rest.trim_start_matches('胜')));
    }
    if let Some(rest) = text.strip_prefix('白') {
        return Some(("W", rest.trim_start_matches('胜')));
    }
    if let Some(rest) = strip_ascii_prefix(text, "B+") {
        return Some(("B", rest));
    }
    if let Some(rest) = strip_ascii_prefix(text, "W+") {
        return Some(("W", rest));
    }
    None
}

fn strip_ascii_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let bytes = text.as_bytes();
    let pre = prefix.as_bytes();
    if bytes.len() >= pre.len() && bytes[..pre.len()].eq_ignore_ascii_case(pre) {
        // `prefix` is ASCII, so this index is a char boundary.
        Some(&text[pre.len()..])
    } else {
        None
    }
}

fn decision(text: &str) -> Option<&'static str> {
    if text.contains("中盘") || text.contains("认输") {
        Some("R")
    } else if text.contains("超时") {
        Some("T")
    } else if text.contains("犯规") || text.contains("弃权") {
        Some("F")
    } else if text.contains("不计点") {
        Some("")
    } else {
        None
    }
}

fn margin(text: &str) -> Option<String> {
    if let Some(value) = you_fraction(text) {
        return Some(value);
    }
    if let Some(value) = slash_zi(text) {
        return Some(value);
    }
    if text.contains("半目") {
        return Some("0.5".to_string());
    }
    if let Some(n) = digits_before(text, "目半") {
        return Some(format_ratio(n * 2 + 1, 2));
    }
    if let Some(n) = digits_before(text, "目") {
        return Some(format_ratio(n, 1));
    }
    if let Some(n) = digits_before(text, "子") {
        return Some(format_ratio(n * 2, 1));
    }
    if let Some(n) = digits_before(text, "点") {
        return Some(format_ratio(n, 1));
    }
    digits_in(text).map(|n| format_ratio(n, 1))
}

/// `N又a/b子`: the whole is two points per 子.
fn you_fraction(text: &str) -> Option<String> {
    let idx = text.find('又')?;
    let n = trailing_digits(&text[..idx])?;
    let after = &text[idx + '又'.len_utf8()..];
    let (a, b, used) = leading_fraction(after)?;
    if b == 0 || !after[used..].starts_with('子') {
        return None;
    }
    Some(format_ratio((n * b + a) * 2, b))
}

fn slash_zi(text: &str) -> Option<String> {
    let slash = text.find('/')?;
    // A fraction after 又 belongs to `N又a/b子`, already taken.
    if text[..slash].contains('又') {
        return None;
    }
    let a = trailing_digits(&text[..slash])?;
    let after = &text[slash + 1..];
    let (b, used) = leading_digits(after)?;
    if b == 0 || !after[used..].starts_with('子') {
        return None;
    }
    Some(format_ratio(a * 2, b))
}

fn leading_fraction(text: &str) -> Option<(i128, i128, usize)> {
    let (a, used_a) = leading_digits(text)?;
    let rest = &text[used_a..];
    if !rest.starts_with('/') {
        return None;
    }
    let (b, used_b) = leading_digits(&rest[1..])?;
    Some((a, b, used_a + 1 + used_b))
}

fn digits_before(text: &str, suffix: &str) -> Option<i128> {
    let idx = text.find(suffix)?;
    trailing_digits(&text[..idx])
}

// Keep each input within i64, but leave room for `(whole * denominator + numerator) * 2`
// and the decimal remainder's `* 10` when a server supplies extreme numbers.
fn trailing_digits(text: &str) -> Option<i128> {
    let start = text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    text[start..].parse::<i64>().ok().map(i128::from)
}

fn leading_digits(text: &str) -> Option<(i128, usize)> {
    let end = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    text[..end]
        .parse::<i64>()
        .ok()
        .map(|n| (i128::from(n), end))
}

fn digits_in(text: &str) -> Option<i128> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    leading_digits(&text[start..]).map(|(n, _)| n)
}

fn format_ratio(num: i128, den: i128) -> String {
    if den == 0 {
        return String::new();
    }
    let whole = num / den;
    let mut rem = num % den;
    if rem == 0 {
        return whole.to_string();
    }
    let mut frac = String::new();
    for _ in 0..6 {
        rem *= 10;
        frac.push(char::from(b'0' + (rem / den) as u8));
        rem %= den;
        if rem == 0 {
            break;
        }
    }
    format!("{whole}.{frac}")
}

/// `九段` is `P9` and `初段` is `P1`. Anything that is not one of those words is kept:
/// the detail endpoint already writes `9p`, and a made-up conversion would hide that.
fn library_rank(raw: &str) -> String {
    let raw = raw.trim();
    if raw == "初段" || raw == "一段" {
        return "P1".to_string();
    }
    const DANS: [&str; 8] = [
        "二段", "三段", "四段", "五段", "六段", "七段", "八段", "九段",
    ];
    if let Some(i) = DANS.iter().position(|dan| *dan == raw) {
        return format!("P{}", i + 2);
    }
    raw.to_string()
}

/// `2.7D` and `11.5K` are the same grades the list shows, only with a lower-case letter
/// so they read like every other server's `6d` / `15k`.
fn account_rank(raw: &str) -> String {
    let raw = raw.trim();
    let Some((head, last)) = raw.split_at_checked(raw.len().saturating_sub(1)) else {
        return raw.to_string();
    };
    if last.eq_ignore_ascii_case("d") || last.eq_ignore_ascii_case("k") {
        format!("{head}{}", last.to_ascii_lowercase())
    } else {
        raw.to_string()
    }
}

fn json(body: &str) -> Result<Value, String> {
    serde_json::from_str(body).map_err(|e| format!("yike response is not JSON: {e}"))
}

fn status_ok(body: &str) -> Result<Value, String> {
    let value = json(body)?;
    let status = value.get("Status").and_then(Value::as_i64).unwrap_or(-1);
    if status != 1200 {
        let msg = text(value.get("Message"));
        return Err(if msg.is_empty() {
            "yike request failed".to_string()
        } else {
            display_text(&msg)
        });
    }
    Ok(value)
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => json_number(n),
        _ => String::new(),
    }
}

fn json_number(n: &serde_json::Number) -> String {
    if let Some(u) = n.as_u64() {
        u.to_string()
    } else if let Some(i) = n.as_i64() {
        i.to_string()
    } else {
        n.to_string()
    }
}

fn json_usize(value: &Value) -> Result<usize, String> {
    match value {
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| "yike total does not fit".to_string()),
        Value::String(s) => s
            .parse()
            .map_err(|_| "yike total is not a number".to_string()),
        _ => Err("yike total is not a number".to_string()),
    }
}

fn json_u32(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn json_u8(value: &Value) -> Option<u8> {
    value.as_u64().and_then(|n| u8::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_sentences_become_sgf_results() {
        let cases = [
            ("黑中盘胜", "B+R"),
            ("白中盘胜", "W+R"),
            ("黑认输", "B+R"),
            ("黑超时胜", "B+T"),
            ("白胜-黑超时", "W+T"),
            ("白犯规胜", "W+F"),
            ("黑弃权", "B+F"),
            ("黑不计点胜", "B+"),
            ("黑胜", "B+"),
            ("黑胜半目", "B+0.5"),
            ("黑胜2目半", "B+2.5"),
            ("白胜1目半", "W+1.5"),
            ("黑胜2目", "B+2"),
            ("黑胜3点", "B+3"),
            ("黑胜3/4子", "B+1.5"),
            ("白胜1/4子", "W+0.5"),
            ("白胜1又1/4子", "W+2.5"),
            ("黑胜1又3/4子", "B+3.5"),
            ("黑胜3又3/4子", "B+7.5"),
            ("黑胜1/2子", "B+1"),
            ("黑胜2子", "B+4"),
            ("和棋", "0"),
            ("D", "0"),
            ("BL", "Void"),
            ("双负", "Void"),
            ("", ""),
            ("没写", ""),
        ];
        for (raw, want) in cases {
            assert_eq!(result_text(raw), want, "{raw}");
        }
    }
    #[test]
    fn large_result_numbers_do_not_overflow_the_ratio() {
        assert_eq!(
            result_text("黑胜9223372036854775807子"),
            "B+18446744073709551614"
        );
        assert_eq!(
            result_text("白胜9223372036854775807又9223372036854775807/9223372036854775807子"),
            "W+18446744073709551616"
        );
        assert_eq!(
            result_text("黑胜9223372036854775806/9223372036854775807子"),
            "B+1.999999"
        );
    }

    #[test]
    fn account_codes_supply_the_winner_a_bare_margin_lacks() {
        assert_eq!(account_result("B+", "49又3/4子"), "B+99.5");
        assert_eq!(account_result("W+", "10"), "W+10");
        assert_eq!(account_result("B+", ""), "B+");
        assert_eq!(account_result("W+", "黑胜"), "B+");
        assert_eq!(account_result("BL", ""), "Void");
        assert_eq!(account_result("D", ""), "0");
        assert_eq!(account_result("", ""), "");
    }

    #[test]
    fn library_search_keeps_only_the_exact_name() {
        let body = r#"{"matches":[{"name":"\u67ef\u6d01","pid":"1195"},{"name":"\u67ef\u6770\u6656","pid":"1837"}]}"#;
        let found = parse_library_search(body, " 柯洁 ").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, Source::YikeLibrary);
        assert_eq!(found[0].id, "1195");
        assert_eq!(found[0].name, "柯洁");
        assert_eq!(found[0].rank, "");
    }

    #[test]
    fn account_search_matches_nickname_or_yike_number() {
        let body = r#"{"Status":1200,"Result":{"data":[
            {"id":1,"cgf_id":"CGF00001","nickname":"沈尧","grade":"2.6D"},
            {"id":9,"cgf_id":"CGF00009","nickname":"别人","grade":"1D"},
            {"id":1323,"cgf_id":"CGF01324","nickname":"柯洁","grade":"10.5D"}
        ]}}"#;
        let by_name = parse_account_search(body, "沈尧").unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].id, "1");
        assert_eq!(by_name[0].rank, "2.6d");
        assert_eq!(by_name[0].source, Source::YikeAccount);

        let by_number = parse_account_search(body, "cgf01324").unwrap();
        assert_eq!(by_number.len(), 1);
        assert_eq!(by_number[0].id, "1323");
        assert_eq!(by_number[0].name, "柯洁");
    }

    #[test]
    fn library_ranks_and_account_grades_normalise_apart() {
        assert_eq!(library_rank("九段"), "P9");
        assert_eq!(library_rank("初段"), "P1");
        assert_eq!(library_rank("一段"), "P1");
        assert_eq!(library_rank("9p"), "9p");
        assert_eq!(account_rank("2.7D"), "2.7d");
        assert_eq!(account_rank("11.5K"), "11.5k");
        assert_eq!(account_rank(""), "");
    }

    #[test]
    fn a_missing_account_is_a_null_name_not_an_error() {
        let body = r#"{"Status":1200,"Result":{"user":{"grage":"21.5K","name":null},"list":[]}}"#;
        let page = parse_account_games(body).unwrap();
        assert_eq!(page.name, None);
        assert_eq!(page.grade, "21.5k");
        assert!(page.records.is_empty());
    }

    #[test]
    fn a_synthetic_tour_name_is_the_room_not_the_date() {
        let body = r#"{"Status":1200,"Result":{"user":{"name":"沈尧","grage":"2.6D"},"list":[{
            "GameId":17205763,"HandsCount":34,"BoardSize":7,
            "BlackName":"沈尧","WhiteName":"沈知行",
            "BlackPlayerScore":"2.7D","WhitePlayerScore":"11.5K",
            "Result":"W+","ResultDesc":"白胜-黑超时",
            "GameDate":"2019-12-13","GameLocation":"标准区非即时",
            "TourName":"2019-12-13_标准区非即时"
        }]}}"#;
        let page = parse_account_games(body).unwrap();
        assert_eq!(page.name.as_deref(), Some("沈尧"));
        let row = &page.records[0];
        assert_eq!(row.id, "17205763");
        assert_eq!(row.black_rank, "2.7d");
        assert_eq!(row.white_rank, "11.5k");
        assert_eq!(row.result, "W+T");
        assert_eq!(row.moves, Some(34));
        assert_eq!(row.board_size, Some(7));
        assert_eq!(row.event, "标准区非即时");
    }

    #[test]
    fn library_sgf_takes_komi_players_and_result_from_the_json() {
        let body = r#"{"komi":"7.5","black":"\u67ef\u6d01","black_rank":"9p","white":"\u515a\u6bc5\u98de","white_rank":"9p","result":"\u9ed1\u4e2d\u76d8\u80dc","date":"2026-07-30","event":"\u51b3\u8d5b","sgf":"(;EV[];B[pd];W[dd])"}"#;
        let tree = parse_library_sgf(body).unwrap();
        assert_eq!(tree.info.komi, 7.5);
        assert_eq!(tree.info.players[0].name, "柯洁");
        assert_eq!(tree.info.players[0].rank, "9p");
        assert_eq!(tree.info.players[1].name, "党毅飞");
        assert_eq!(tree.info.result, "B+R");
        assert_eq!(tree.info.date, "2026-07-30");
        assert_eq!(tree.info.event, "决赛");
        let first = tree.children(tree.root())[0];
        assert_eq!(tree.node(first).mv.map(|(c, _)| c), Some(Color::Black));
    }

    #[test]
    fn an_empty_account_sgf_is_not_a_game() {
        let body = r#"{"Status":1200,"Result":{"Sgf":"(;GM[1]FF[4]CA[UTF-8]SZ[19])"}}"#;
        let err = parse_account_sgf(body).unwrap_err();
        assert_eq!(err, "no moves recorded");
    }

    #[test]
    fn a_handicap_account_sgf_is_a_game_and_its_result_is_rewritten() {
        let body = r#"{"Status":1200,"Result":{"Sgf":"(;GM[1]PB[甲]PW[乙]KM[0]HA[2]SZ[19]RE[白中盘胜]AB[dd][pp];W[jj])"}}"#;
        let tree = parse_account_sgf(body).unwrap();
        assert_eq!(tree.info.result, "W+R");
        assert_eq!(tree.info.handicap, 2);
        assert!(!tree.node(tree.root()).setup.add_black.is_empty());
    }

    #[test]
    fn service_errors_are_the_server_message() {
        let missing = parse_library_sgf(r#"{"code":1,"msg":"gone","data":""}"#).unwrap_err();
        assert_eq!(missing, "gone");
        let refused =
            parse_account_sgf(r#"{"Status":1404,"Message":"invalid user token"}"#).unwrap_err();
        assert_eq!(refused, "invalid user token");
    }

    #[test]
    fn urls_encode_the_query_and_pin_the_online_list() {
        assert!(library_search_url("柯洁").contains("key=%E6%9F%AF%E6%B4%81"));
        assert!(account_search_url("沈尧").ends_with("&page=1"));
        assert_eq!(
            account_games_url("1", 2),
            "https://api.yikeweiqi.com/reguser/games/1/1/2"
        );
    }
}
