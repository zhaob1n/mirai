// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Fox (foxwq) kifu lookup. HTTP is behind [`Fetch`] so each frontend supplies its client.

use std::future::Future;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoxGame {
    pub chess_id: String,
    pub black: String,
    pub white: String,
    pub result: String,
    pub date: String,
    pub moves: u32,
}

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
            .unwrap_or("fox request failed");
        return Err(msg.to_string());
    }
    Ok(value)
}

pub async fn lookup_user<F: Fetch>(fetch: &F, nickname: &str) -> Result<FoxUser, String> {
    let nick = urlencoding(nickname);
    let url = format!("https://newframe.foxwq.com/cgi/QueryUserInfoPanel?srcuid=0&username={nick}");
    let body = fetch.get(&url).await?;
    let value = json_err(&body)?;
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
            .unwrap_or(nickname)
            .to_string(),
        hidden: value
            .get("hide_game_record")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            == 1,
    })
}

pub async fn list_games<F: Fetch>(fetch: &F, uid: &str) -> Result<Vec<FoxGame>, String> {
    let url = format!(
        "https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChessList?dstuid={uid}&type=1&fetchnum=200"
    );
    let body = fetch.get(&url).await?;
    let value = json_err(&body)?;
    let list = value
        .get("chesslist")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|row| {
            let chess_id = row.get("chessid")?.as_str()?.to_string();
            let black = row
                .get("blacknick")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| row.get("blackenname").and_then(|v| v.as_str()))
                .unwrap_or("Black")
                .to_string();
            let white = row
                .get("whitenick")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| row.get("whiteenname").and_then(|v| v.as_str()))
                .unwrap_or("White")
                .to_string();
            let winner = row.get("winner").and_then(|v| v.as_i64()).unwrap_or(0);
            let result = match winner {
                1 => "B+",
                2 => "W+",
                _ => "",
            }
            .to_string();
            let date = row
                .get("starttime")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let moves = row.get("movenum").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            Some(FoxGame {
                chess_id,
                black,
                white,
                result,
                date,
                moves,
            })
        })
        .collect())
}

pub async fn fetch_sgf<F: Fetch>(fetch: &F, chess_id: &str) -> Result<String, String> {
    let url = format!(
        "https://h5.foxwq.com/yehuDiamond/chessbook_local/YHWQFetchChess?chessid={chess_id}"
    );
    let body = fetch.get(&url).await?;
    let value = json_err(&body)?;
    let raw = value
        .get("chess")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "fox response has no SGF".to_string())?;
    Ok(normalize_fox_sgf(raw))
}

/// Percent-encode a nickname without pulling in a crate.
fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Fox SGF dialect: expand its C-style escapes, and scale huge KM.
///
/// Fox writes `\r`, `\n` and `\t` both between properties and *inside* text values. Outside
/// a value, dropping only the backslash leaves a literal `rn`, which is not whitespace, so a
/// following variation stops being recognised as a sibling tree. Inside a value the damage is
/// quieter but worse: SGF's own rule is that `\` escapes the next character, so `C[a\nb]`
/// parses as the comment `anb` — every line break in a Fox comment turned into the letter
/// `n`. Both cases become real whitespace here, and every other escape is passed through
/// untouched so the SGF parser still sees `\]` and `\\`.
pub fn normalize_fox_sgf(raw: &str) -> String {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_value = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && i + 1 < bytes.len() {
            let whitespace = match bytes[i + 1] {
                b'r' => Some('\r'),
                b'n' => Some('\n'),
                b't' => Some('\t'),
                _ => None,
            };
            if let Some(ch) = whitespace {
                out.push(ch);
                i += 2;
                continue;
            }
            if !in_value {
                // An escape Fox did not mean: outside a value there is nothing to escape.
                i += 1;
                continue;
            }
            // `\]`, `\\` and friends belong to the parser, not to us.
            out.push('\\');
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if !in_value && c == b'\\' {
            i += 1;
            continue;
        }
        if c == b'[' {
            in_value = true;
        } else if c == b']' && in_value {
            // Every backslash pair above is consumed whole, so a `]` reached here is real.
            in_value = false;
        }
        let n = match c {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        let end = (i + n).min(bytes.len());
        out.push_str(&text[i..end]);
        i = end;
    }
    scale_fox_komi(&out)
}

fn scale_fox_komi(sgf: &str) -> String {
    let Some(start) = sgf.find("KM[") else {
        return sgf.to_string();
    };
    let rest = &sgf[start + 3..];
    let Some(end) = rest.find(']') else {
        return sgf.to_string();
    };
    let raw = &rest[..end];
    let Ok(value) = raw.parse::<f32>() else {
        return sgf.to_string();
    };
    if value < 200.0 {
        return sgf.to_string();
    }
    let mut scaled = value / 100.0;
    if (-4.0..=4.0).contains(&scaled)
        || (scaled.fract() - 0.25).abs() < 1e-3
        || (scaled.fract() - 0.75).abs() < 1e-3
    {
        scaled *= 2.0;
    }
    format!("{}KM[{scaled}]{}", &sgf[..start], &rest[end..])
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
}
