// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! SGF (FF[4]) reading and writing, hand-written.
//!
//! The point of not using an SGF crate is [`Node::unknown_props`]: every property mirai
//! does not model is kept verbatim and written back, so loading and saving a LizzieYzy
//! record does not destroy its `LZ` / `LZOP` / `DZ` analysis blobs.
//!
//! mirai's own cached analysis rides on a single root property, `MRAI`:
//! `base64(zstd(0x01 ++ postcard(Vec<(u32, NodeAnalysis)>)))`, where the `u32` is the
//! node's index in document order — which is exactly the [`NodeId`] a reload assigns.

use base64::prelude::{BASE64_STANDARD, Engine as _};

use crate::point::{Color, Point, Size};
use crate::rules::RuleSet;
use crate::tree::{GameInfo, GameTree, MarkKind, NodeAnalysis, NodeId, Setup};

/// Guards against stack exhaustion on pathologically nested (or hostile) files.
const MAX_DEPTH: u32 = 256;

/// Hard ceiling for the decompressed `MRAI` payload. A 300-node 19x19 game with the
/// default 10 suggestions and 50-point PVs encodes to 465,239 bytes including the version;
/// even 300-point PVs only take 1,968,239 bytes. 16 MiB leaves ample headroom.
const MAX_ANALYSIS_BYTES: usize = 16 * 1024 * 1024;

/// Caps the allocation performed by base64 decoding. This admits a 16 MiB incompressible
/// zstd frame plus more than 12% overhead while rejecting oversized SGF properties early.
const MAX_ANALYSIS_BLOB_BYTES: usize = 24 * 1024 * 1024;

/// Version byte in front of the postcard payload of the `MRAI` property.
const MRAI_VERSION: u8 = 0x01;

#[derive(Debug, thiserror::Error)]
pub enum SgfError {
    #[error("no game tree found in SGF input")]
    Empty,
    #[error("unexpected end of SGF input")]
    UnexpectedEof,
    #[error("expected `{expected}` at byte {at}")]
    Expected { expected: char, at: usize },
    #[error("game tree nested more than {MAX_DEPTH} deep")]
    TooDeep,
    #[error("invalid board size `{0}`")]
    BadSize(String),
    #[error("invalid point `{value}` in property `{prop}`")]
    BadPoint { prop: &'static str, value: String },
}

/// Parses a whole collection, detecting the file's encoding first.
pub fn parse(bytes: &[u8]) -> Result<Vec<GameTree>, SgfError> {
    parse_str(&decode_bytes(bytes))
}

/// Parses a collection from already-decoded text.
pub fn parse_str(text: &str) -> Result<Vec<GameTree>, SgfError> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    let mut games = Vec::new();
    loop {
        while p.i < p.b.len() && p.b[p.i] != b'(' {
            p.i += 1;
        }
        if p.i >= p.b.len() {
            break;
        }
        let mut arena = Vec::new();
        p.tree(&mut arena, None, 0)?;
        if !arena.is_empty() {
            games.push(build(&arena)?);
        }
    }
    if games.is_empty() {
        return Err(SgfError::Empty);
    }
    Ok(games)
}

// ---------------------------------------------------------------------------- encoding

/// `CA` has to be found before the text can be decoded, so it is located in the raw
/// bytes. The scan only accepts `CA[` that starts a property identifier.
fn find_charset(bytes: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"CA["
            && (i == 0 || !bytes[i - 1].is_ascii_alphabetic())
            && let Some(end) = bytes[i + 3..].iter().position(|&c| c == b']')
        {
            let v = bytes[i + 3..i + 3 + end].trim_ascii();
            return (!v.is_empty()).then_some(v);
        }
        i += 1;
    }
    None
}

fn decode_bytes(bytes: &[u8]) -> String {
    if let Some(label) = find_charset(bytes)
        && let Some(enc) = encoding_rs::Encoding::for_label(label)
    {
        return enc.decode(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.strip_prefix('\u{feff}').unwrap_or(s).to_owned(),
        // Windows-1252-looking Chinese records are really GB18030; so are most files that
        // claim nothing at all and fail UTF-8.
        Err(_) => encoding_rs::GB18030.decode(bytes).0.into_owned(),
    }
}

// ------------------------------------------------------------------------------ parser

/// A node exactly as it appears in the file: identifiers and unescaped values, nothing
/// interpreted yet. Indices are assigned in document (pre)order.
struct RawNode {
    props: Vec<(String, Vec<String>)>,
    parent: Option<usize>,
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    #[inline]
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), SgfError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(SgfError::Expected {
                expected: c as char,
                at: self.i,
            })
        }
    }

    /// `GameTree = "(" Node+ GameTree* ")"`. Sub-trees hang off the last node parsed.
    fn tree(
        &mut self,
        arena: &mut Vec<RawNode>,
        parent: Option<usize>,
        depth: u32,
    ) -> Result<(), SgfError> {
        if depth > MAX_DEPTH {
            return Err(SgfError::TooDeep);
        }
        self.ws();
        self.expect(b'(')?;
        let mut last = parent;
        loop {
            self.ws();
            match self.peek() {
                Some(b';') => {
                    self.i += 1;
                    last = Some(self.node(arena, last));
                }
                Some(b'(') | Some(b')') => break,
                // Junk between nodes: SGF in the wild has it; skipping keeps the file usable.
                Some(_) => self.i += 1,
                None => return Err(SgfError::UnexpectedEof),
            }
        }
        while {
            self.ws();
            self.peek() == Some(b'(')
        } {
            self.tree(arena, last, depth + 1)?;
        }
        self.ws();
        self.expect(b')')
    }

    fn node(&mut self, arena: &mut Vec<RawNode>, parent: Option<usize>) -> usize {
        let id = arena.len();
        arena.push(RawNode {
            props: Vec::new(),
            parent,
        });
        loop {
            self.ws();
            let Some(c) = self.peek() else { break };
            if !c.is_ascii_alphabetic() {
                break;
            }
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                self.i += 1;
            }
            let ident = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
            let mut values = Vec::new();
            loop {
                self.ws();
                if self.peek() != Some(b'[') {
                    break;
                }
                values.push(self.value());
            }
            // A bare identifier with no value is not a property; dropping it keeps what
            // we write back valid.
            if !values.is_empty() {
                arena[id].props.push((ident, values));
            }
        }
        id
    }

    /// Reads one `[...]` value, undoing SGF escaping. Soft line breaks disappear.
    fn value(&mut self) -> String {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        while self.i < self.b.len() {
            match self.b[self.i] {
                b']' => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    self.i += 1;
                    let Some(c) = self.peek() else { break };
                    if c == b'\n' || c == b'\r' {
                        self.i += 1;
                        // `\r\n` and `\n\r` are one soft break, not two.
                        if self
                            .peek()
                            .is_some_and(|d| (d == b'\n' || d == b'\r') && d != c)
                        {
                            self.i += 1;
                        }
                        continue;
                    }
                    let n = utf8_len(c);
                    let end = (self.i + n).min(self.b.len());
                    out.extend_from_slice(&self.b[self.i..end]);
                    self.i = end;
                }
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }
}

#[inline]
const fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

// ------------------------------------------------------------------- raw tree -> GameTree

/// SGF point lists admit `aa:cc` rectangles; both forms land in `out`.
fn points(size: Size, values: &[String], out: &mut Vec<Point>) {
    for v in values {
        let b = v.as_bytes();
        if let Some(colon) = b.iter().position(|&c| c == b':')
            && let (Some(from), Some(to)) =
                (size.from_sgf(&b[..colon]), size.from_sgf(&b[colon + 1..]))
            && !from.is_pass()
            && !to.is_pass()
        {
            let (x0, y0) = size.xy(from);
            let (x1, y1) = size.xy(to);
            for y in y0.min(y1)..=y0.max(y1) {
                for x in x0.min(x1)..=x0.max(x1) {
                    out.push(size.point(x, y));
                }
            }
            continue;
        }
        // A stray unparsable coordinate in a decoration list is not worth failing a load.
        if let Some(p) = size.from_sgf(b)
            && !p.is_pass()
        {
            out.push(p);
        }
    }
}

fn parse_komi(raw: &str) -> Option<f32> {
    let k: f32 = raw.trim().parse().ok()?;
    // LizzieYzy's convention for quarter-point Chinese komi: 750 means 7.5.
    Some(if k.abs() >= 200.0 { k / 100.0 } else { k })
}

fn parse_size(raw: &str) -> Result<Size, SgfError> {
    let bad = || SgfError::BadSize(raw.to_owned());
    let (w, h) = match raw.split_once(':') {
        Some((w, h)) => (w.trim(), h.trim()),
        None => (raw.trim(), raw.trim()),
    };
    let w: u8 = w.parse().map_err(|_| bad())?;
    let h: u8 = h.parse().map_err(|_| bad())?;
    Size::new(w, h).ok_or_else(bad)
}

fn parse_color(raw: &str) -> Option<Color> {
    match raw.trim().as_bytes().first()? {
        b'B' | b'b' | b'1' => Some(Color::Black),
        b'W' | b'w' | b'2' => Some(Color::White),
        _ => None,
    }
}

/// Root properties consumed into [`GameInfo`] or regenerated on write; they must not be
/// echoed back out of `unknown_props`.
fn is_root_info_prop(name: &str) -> bool {
    matches!(
        name,
        "FF" | "GM"
            | "CA"
            | "AP"
            | "SZ"
            | "KM"
            | "RU"
            | "HA"
            | "RE"
            | "PB"
            | "PW"
            | "BR"
            | "WR"
            | "DT"
            | "EV"
            | "TM"
            | "OT"
            | "MRAI"
    )
}

fn build(arena: &[RawNode]) -> Result<GameTree, SgfError> {
    let root_props = &arena[0].props;
    let find = |name: &str| -> Option<&str> {
        root_props
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.first())
            .map(String::as_str)
    };

    let size = match find("SZ") {
        Some(v) => parse_size(v)?,
        None => Size::square(19),
    };
    let ruleset = find("RU").and_then(RuleSet::from_katago_name);
    let mut info = GameInfo::new(size, ruleset.unwrap_or_default());
    if let Some(k) = find("KM").and_then(parse_komi) {
        info.komi = k;
    }
    if let Some(h) = find("HA").and_then(|v| v.trim().parse::<u8>().ok()) {
        info.handicap = h;
    }
    let set = |field: &mut String, prop: &str| {
        if let Some(v) = find(prop) {
            *field = v.to_owned();
        }
    };
    set(&mut info.result, "RE");
    set(&mut info.date, "DT");
    set(&mut info.event, "EV");
    set(&mut info.time_limit, "TM");
    set(&mut info.overtime, "OT");
    set(&mut info.players[0].name, "PB");
    set(&mut info.players[1].name, "PW");
    set(&mut info.players[0].rank, "BR");
    set(&mut info.players[1].rank, "WR");

    let mut tree = GameTree::new(info);
    // Pre-order in the file is pre-order in the arena, and `add_child` appends, so the
    // node created for `arena[i]` is always `NodeId(i)`.
    for (i, raw) in arena.iter().enumerate().skip(1) {
        let parent = NodeId(raw.parent.unwrap_or(0) as u32);
        let id = tree.add_child(parent);
        debug_assert_eq!(id, NodeId(i as u32));
    }
    for (i, raw) in arena.iter().enumerate() {
        let id = NodeId(i as u32);
        let is_root = i == 0;
        let mut setup = Setup::default();
        let node = tree.node_mut(id);
        for (name, values) in &raw.props {
            let first = values.first().map(String::as_str).unwrap_or("");
            match name.as_str() {
                "B" | "W" => {
                    let color = if name == "B" {
                        Color::Black
                    } else {
                        Color::White
                    };
                    let p = size
                        .from_sgf(first.as_bytes())
                        .ok_or_else(|| SgfError::BadPoint {
                            prop: if color == Color::Black { "B" } else { "W" },
                            value: first.to_owned(),
                        })?;
                    node.mv = Some((color, p));
                }
                "AB" => points(size, values, &mut setup.add_black),
                "AW" => points(size, values, &mut setup.add_white),
                "AE" => points(size, values, &mut setup.add_empty),
                "C" => node.comment = first.to_owned(),
                "PL" => node.to_play_override = parse_color(first),
                "MN" => node.move_number_override = first.trim().parse().ok(),
                "LB" => {
                    for v in values {
                        let Some((coord, text)) = v.split_once(':') else {
                            continue;
                        };
                        if let Some(p) = size.from_sgf(coord.as_bytes())
                            && !p.is_pass()
                        {
                            node.marks.labels.push((p, text.to_owned()));
                        }
                    }
                }
                "TR" => points(size, values, &mut node.marks.triangle),
                "SQ" => points(size, values, &mut node.marks.square),
                "CR" => points(size, values, &mut node.marks.circle),
                "MA" => points(size, values, &mut node.marks.cross),
                _ if is_root && is_root_info_prop(name) => {}
                _ => node.unknown_props.push((
                    name.as_str().into(),
                    values.iter().map(|v| v.as_str().into()).collect(),
                )),
            }
        }
        node.setup = setup;
    }

    // Analysis is best-effort: a foreign or future blob just means "no analysis".
    if let Some(blob) = root_props
        .iter()
        .find(|(k, _)| k == "MRAI")
        .and_then(|(_, v)| v.first())
        && let Some(entries) = decode_analysis(blob)
    {
        for (idx, a) in entries {
            let id = NodeId(idx);
            if tree.contains(id) {
                tree.set_analysis(id, Some(a));
            }
        }
    }
    Ok(tree)
}

fn decode_analysis(blob: &str) -> Option<Vec<(u32, NodeAnalysis)>> {
    let blob = blob.trim();
    if blob.len() > MAX_ANALYSIS_BLOB_BYTES {
        return None;
    }
    let raw = BASE64_STANDARD.decode(blob.as_bytes()).ok()?;
    let mut plain = Vec::new();
    zstd::stream::copy_decode(
        &raw[..],
        crate::BoundedWriter::new(&mut plain, MAX_ANALYSIS_BYTES, "analysis"),
    )
    .ok()?;
    let (&version, rest) = plain.split_first()?;
    if version != MRAI_VERSION {
        return None;
    }
    postcard::from_bytes(rest).ok()
}

fn encode_analysis(entries: &[(u32, NodeAnalysis)]) -> Option<String> {
    let mut plain = Vec::with_capacity(1024);
    plain.push(MRAI_VERSION);
    plain.append(&mut postcard::to_stdvec(entries).ok()?);
    let packed = zstd::stream::encode_all(&plain[..], 3).ok()?;
    Some(BASE64_STANDARD.encode(&packed))
}

// ------------------------------------------------------------------------------ writer

/// Serialises one game. With `include_analysis`, cached [`NodeAnalysis`] is attached to
/// the root as `MRAI`.
pub fn write(tree: &GameTree, include_analysis: bool) -> String {
    let mut mrai = None;
    if include_analysis {
        let mut entries = Vec::new();
        collect_analysis(tree, tree.root(), &mut 0, &mut entries);
        if !entries.is_empty() {
            mrai = encode_analysis(&entries);
        }
    }

    let mut out = String::with_capacity(1024);
    out.push('(');
    write_sequence(tree, tree.root(), mrai.as_deref(), &mut out);
    out.push(')');
    out.push('\n');
    out
}

/// Walks in exactly the order [`write_sequence`] emits, so the index recorded for a node
/// is the [`NodeId`] a reload will give it.
fn collect_analysis(
    tree: &GameTree,
    id: NodeId,
    next: &mut u32,
    out: &mut Vec<(u32, NodeAnalysis)>,
) {
    let index = *next;
    *next += 1;
    if let Some(a) = &tree.node(id).analysis {
        out.push((index, a.clone()));
    }
    for &c in tree.children(id) {
        collect_analysis(tree, c, next, out);
    }
}

fn write_sequence(tree: &GameTree, from: NodeId, mrai: Option<&str>, out: &mut String) {
    let mut cur = from;
    loop {
        out.push(';');
        write_node(tree, cur, mrai.filter(|_| cur == tree.root()), out);
        let children = tree.children(cur);
        match children {
            [] => break,
            [only] => cur = *only,
            _ => {
                for &c in children {
                    out.push('(');
                    write_sequence(tree, c, None, out);
                    out.push(')');
                }
                break;
            }
        }
    }
}

fn write_node(tree: &GameTree, id: NodeId, mrai: Option<&str>, out: &mut String) {
    let node = tree.node(id);
    if id == tree.root() {
        let info = &tree.info;
        out.push_str("FF[4]GM[1]CA[UTF-8]");
        prop(out, "AP", &format!("mirai:{}", env!("CARGO_PKG_VERSION")));
        if info.size.is_square() {
            prop(out, "SZ", &info.size.w.to_string());
        } else {
            prop(out, "SZ", &format!("{}:{}", info.size.w, info.size.h));
        }
        prop(out, "KM", &format!("{}", info.komi));
        prop(out, "RU", info.rules.katago_name());
        if info.handicap > 0 {
            prop(out, "HA", &info.handicap.to_string());
        }
        for (value, name) in [
            (&info.result, "RE"),
            (&info.players[0].name, "PB"),
            (&info.players[0].rank, "BR"),
            (&info.players[1].name, "PW"),
            (&info.players[1].rank, "WR"),
            (&info.date, "DT"),
            (&info.event, "EV"),
            (&info.time_limit, "TM"),
            (&info.overtime, "OT"),
        ] {
            if !value.is_empty() {
                prop(out, name, value);
            }
        }
        if let Some(blob) = mrai {
            prop(out, "MRAI", blob);
        }
    }

    let size = tree.info.size;
    if let Some((color, p)) = node.mv {
        out.push_str(if color == Color::Black { "B[" } else { "W[" });
        if !p.is_pass() {
            let c = size.to_sgf(p);
            out.push(c[0] as char);
            out.push(c[1] as char);
        }
        out.push(']');
    }
    if let Some(c) = node.to_play_override {
        prop(out, "PL", if c == Color::Black { "B" } else { "W" });
    }
    point_list(out, "AB", size, &node.setup.add_black);
    point_list(out, "AW", size, &node.setup.add_white);
    point_list(out, "AE", size, &node.setup.add_empty);
    if let Some(m) = node.move_number_override {
        prop(out, "MN", &m.to_string());
    }
    if !node.comment.is_empty() {
        prop(out, "C", &node.comment);
    }
    if !node.marks.labels.is_empty() {
        out.push_str("LB");
        for (p, text) in &node.marks.labels {
            let c = size.to_sgf(*p);
            out.push('[');
            out.push(c[0] as char);
            out.push(c[1] as char);
            out.push(':');
            escape(out, text, true);
            out.push(']');
        }
    }
    for (name, kind) in [
        ("TR", MarkKind::Triangle),
        ("SQ", MarkKind::Square),
        ("CR", MarkKind::Circle),
        ("MA", MarkKind::Cross),
    ] {
        point_list(out, name, size, node.marks.of(kind));
    }
    for (name, values) in &node.unknown_props {
        out.push_str(name);
        for v in values {
            out.push('[');
            escape(out, v, false);
            out.push(']');
        }
    }
}

fn point_list(out: &mut String, name: &str, size: Size, ps: &[Point]) {
    if ps.is_empty() {
        return;
    }
    out.push_str(name);
    for &p in ps {
        if p.is_pass() {
            continue;
        }
        let c = size.to_sgf(p);
        out.push('[');
        out.push(c[0] as char);
        out.push(c[1] as char);
        out.push(']');
    }
}

fn prop(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push('[');
    escape(out, value, false);
    out.push(']');
}

/// `\` and `]` always need escaping; `:` only inside a composed value such as `LB`.
fn escape(out: &mut String, value: &str, compose: bool) {
    for ch in value.chars() {
        match ch {
            '\\' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            ':' if compose => {
                out.push('\\');
                out.push(':');
            }
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::tree::Candidate;

    /// A record mirai wrote itself, out of a real KataGo search.
    const OWN_SGF: &[u8] = include_bytes!("../tests/data/katago-selfplay.sgf");

    /// A record *another program* wrote — KataGo self-play serialised by LizzieYzy Next.
    /// Same engine behind the numbers, different bytes: property orders, empty values and
    /// analysis blobs mirai's own writer never emits. Foreign input is the only thing that
    /// catches a writer which merely agrees with its own parser.
    const FOREIGN_SGF: &[u8] = include_bytes!("../tests/data/lizzieyzy-autoGame1.sgf");

    /// Raw (still escaped) value of the first occurrence of a property.
    fn raw_value(text: &str, name: &str) -> String {
        let b = text.as_bytes();
        let key = format!("{name}[");
        let mut from = 0;
        while let Some(rel) = text[from..].find(&key) {
            let at = from + rel;
            if at == 0 || !b[at - 1].is_ascii_alphabetic() {
                let start = at + key.len();
                let end = start + text[start..].find(']').expect("unterminated value");
                return text[start..end].to_owned();
            }
            from = at + key.len();
        }
        panic!("property {name} not found");
    }

    /// Replays a record's main line, panicking on the first illegal move; returns the
    /// number of moves played.
    fn replay_main_line(t: &GameTree) -> usize {
        let rules = t.info.rules.rules();
        let mut board = Board::new(t.info.size);
        let mut moves = 0;
        for id in t.main_line() {
            let node = t.node(id);
            for &p in &node.setup.add_black {
                board.set(p, Some(Color::Black));
            }
            for &p in &node.setup.add_white {
                board.set(p, Some(Color::White));
            }
            for &p in &node.setup.add_empty {
                board.set(p, None);
            }
            if let Some((color, p)) = node.mv {
                board
                    .play(color, p, &rules)
                    .unwrap_or_else(|e| panic!("illegal move {p:?} at {id:?}: {e}"));
                moves += 1;
            }
        }
        moves
    }

    #[test]
    fn komi_normalisation() {
        let t = &parse_str("(;SZ[19]KM[750])").unwrap()[0];
        assert_eq!(t.info.komi, 7.5);
        let t = &parse_str("(;SZ[19]KM[7.5])").unwrap()[0];
        assert_eq!(t.info.komi, 7.5);
        let t = &parse_str("(;SZ[19]KM[650])").unwrap()[0];
        assert_eq!(t.info.komi, 6.5);
        let t = &parse_str("(;SZ[19]KM[0])").unwrap()[0];
        assert_eq!(t.info.komi, 0.0);
    }

    #[test]
    fn rectangular_size_and_multi_game_collections() {
        let games = parse_str("(;SZ[19:13];B[aa])(;SZ[9];B[cc])").unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].info.size, Size { w: 19, h: 13 });
        assert_eq!(games[1].info.size, Size::square(9));
        assert_eq!(games[1].len(), 2);
    }

    #[test]
    fn escapes_and_soft_line_breaks() {
        let t = &parse_str("(;SZ[19]C[a\\]b\\\\c\\\nd])").unwrap()[0];
        assert_eq!(t.node(t.root()).comment, "a]b\\cd");
        let text = write(t, false);
        assert!(text.contains("C[a\\]b\\\\cd]"), "{text}");
        let back = &parse_str(&text).unwrap()[0];
        assert_eq!(back.node(back.root()).comment, "a]b\\cd");
    }

    #[test]
    fn branches_round_trip() {
        let t = &parse_str("(;SZ[9];B[aa](;W[bb];B[cc])(;W[dd]))").unwrap()[0];
        let root = t.root();
        let first = t.children(root)[0];
        assert_eq!(t.children(first).len(), 2);
        let text = write(t, false);
        let back = &parse_str(&text).unwrap()[0];
        let bfirst = back.children(back.root())[0];
        assert_eq!(back.children(bfirst).len(), 2);
        assert_eq!(back.len(), t.len());
        assert_eq!(back.main_line().len(), 4);
    }

    #[test]
    fn hand_built_tree_round_trips() {
        let size = Size::square(19);
        let mut info = GameInfo::new(size, RuleSet::Japanese);
        info.komi = 6.5;
        info.handicap = 2;
        info.result = "B+Resign".into();
        info.players[0].name = "Kaya".into();
        info.players[1].rank = "9d".into();
        let mut t = GameTree::new(info);
        let root = t.root();
        t.set_setup_stone(root, size.point(3, 3), Some(Color::Black));
        t.set_setup_stone(root, size.point(15, 15), Some(Color::Black));
        t.set_setup_stone(root, size.point(4, 4), None);
        t.set_comment(root, "two stone handicap: 白 [x] \\ y");

        let a = t.play(root, Color::White, size.point(15, 3)).unwrap();
        let b = t.play(a, Color::Black, size.point(3, 15)).unwrap();
        let _pass = t.play(b, Color::White, Point::PASS).unwrap();
        let alt = t.add_variation(a, Color::Black, size.point(9, 9)).unwrap();
        t.set_comment(b, "joseki");
        t.toggle_mark(b, MarkKind::Triangle, size.point(2, 2));
        t.toggle_mark(b, MarkKind::Cross, size.point(5, 5));
        t.node_mut(alt)
            .marks
            .labels
            .push((size.point(6, 6), "A:1".into()));

        let analysis = NodeAnalysis {
            visits: 1234,
            winrate: 0.5625,
            score_lead: -1.25,
            score_stdev: 12.5,
            candidates: vec![Candidate {
                mv: size.point(15, 3),
                visits: 900,
                winrate: 0.5,
                score_lead: 0.25,
                prior: 0.125,
                pv: vec![size.point(15, 3), Point::PASS],
            }],
            ownership: Some(vec![0i8; size.points()].into_boxed_slice()),
        };
        t.set_analysis(b, Some(analysis.clone()));
        t.set_analysis(alt, None);

        let text = write(&t, true);
        let back = &parse_str(&text).unwrap()[0];

        assert_eq!(back.info.komi, 6.5);
        assert_eq!(back.info.handicap, 2);
        assert_eq!(back.info.result, "B+Resign");
        assert_eq!(back.info.rules, RuleSet::Japanese);
        assert_eq!(back.info.players[0].name, "Kaya");
        assert_eq!(back.info.players[1].rank, "9d");

        let broot = back.root();
        assert_eq!(back.node(broot).comment, "two stone handicap: 白 [x] \\ y");
        assert_eq!(
            back.node(broot).setup.add_black,
            vec![size.point(3, 3), size.point(15, 15)]
        );
        assert_eq!(back.node(broot).setup.add_empty, vec![size.point(4, 4)]);

        let ba = back.children(broot)[0];
        let bb = back.children(ba)[0];
        let bpass = back.children(bb)[0];
        let balt = back.children(ba)[1];
        assert_eq!(back.node(ba).mv, Some((Color::White, size.point(15, 3))));
        assert_eq!(back.node(bb).mv, Some((Color::Black, size.point(3, 15))));
        assert_eq!(back.node(bpass).mv, Some((Color::White, Point::PASS)));
        assert_eq!(back.node(bb).comment, "joseki");
        assert_eq!(back.node(bb).marks.triangle, vec![size.point(2, 2)]);
        assert_eq!(back.node(bb).marks.cross, vec![size.point(5, 5)]);
        assert_eq!(
            back.node(balt).marks.labels,
            vec![(size.point(6, 6), "A:1".to_owned())]
        );

        // Analysis landed on the node it was attached to, and nowhere else.
        assert_eq!(back.node(bb).analysis.as_ref(), Some(&analysis));
        assert!(back.node(broot).analysis.is_none());
        assert!(back.node(ba).analysis.is_none());
        assert!(back.node(balt).analysis.is_none());
        assert!(back.node(bpass).analysis.is_none());
        let _ = alt;

        // ... and is omitted entirely when the caller says so.
        let plain = write(&t, false);
        assert!(!plain.contains("MRAI["));
        let plain = &parse_str(&plain).unwrap()[0];
        let pb = plain.children(plain.children(plain.root())[0])[0];
        assert!(plain.node(pb).analysis.is_none());
    }

    #[test]
    fn analysis_blob_is_skipped_when_the_version_byte_is_wrong() {
        let plain = [0x02u8, 0, 0, 0];
        let packed = zstd::stream::encode_all(&plain[..], 3).unwrap();
        let sgf = format!("(;SZ[19]MRAI[{}];B[aa])", BASE64_STANDARD.encode(&packed));
        let t = &parse_str(&sgf).unwrap()[0];
        assert!(t.node(t.root()).analysis.is_none());
        assert!(t.node(t.children(t.root())[0]).analysis.is_none());
        // Garbage is equally harmless.
        let t = &parse_str("(;SZ[19]MRAI[not base64 at all!!];B[aa])").unwrap()[0];
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn oversized_analysis_blob_is_rejected() {
        let analysis = NodeAnalysis {
            visits: 1,
            winrate: 0.5,
            score_lead: 0.0,
            score_stdev: 0.0,
            candidates: Vec::new(),
            ownership: Some(vec![0; MAX_ANALYSIS_BYTES].into_boxed_slice()),
        };
        let blob = encode_analysis(&[(0, analysis)]).expect("compress");
        assert!(decode_analysis(&blob).is_none());
    }

    /// A record mirai itself saved out of a real KataGo search: `examples/selfplay.rs` in
    /// `mirai-client` regenerates it. Nothing here is hand-built, so this is where a
    /// writer that only round-trips its own idea of a tree gets caught.
    #[test]
    fn engine_record_parses_replays_and_keeps_its_analysis() {
        let bytes = OWN_SGF;
        assert_eq!(bytes.len(), 17862);
        let games = parse(bytes).expect("parse");
        assert_eq!(games.len(), 1);
        let t = &games[0];
        assert_eq!(t.info.size, Size::square(19));
        assert_eq!(t.info.komi, 7.5);
        assert_eq!(t.info.rules, RuleSet::Chinese);
        assert_eq!(t.info.players[0].name, "KataGo");
        assert_eq!(t.len(), 29);

        // The main line must be a legal game: 25 played moves, then the first of the three
        // continuations the engine offered at the final position.
        let line = t.main_line();
        assert_eq!(replay_main_line(t), 26);
        assert_eq!(line.len(), 27);
        assert_eq!(t.children(line[25]).len(), 3);

        // One evaluation per played position, and every one of them survives
        // parse -> write -> parse. They all ride on a single root `MRAI` blob keyed by
        // document order, so a mismatch anywhere is a mismatch in that keying.
        let analysed = line.iter().filter(|&&id| t.node(id).analysis.is_some());
        assert_eq!(analysed.count(), 26, "the continuations carry none");
        let root = t.node(t.root()).analysis.as_ref().expect("root analysis");
        assert_eq!(root.candidates.len(), 8);
        assert_eq!(root.ownership.as_ref().map(|o| o.len()), Some(361));

        let written = write(t, true);
        let back = &parse_str(&written).unwrap()[0];
        assert_eq!(back.len(), t.len());
        let back_line = back.main_line();
        assert_eq!(back_line.len(), line.len());
        for (&a, &b) in line.iter().zip(&back_line) {
            assert_eq!(
                back.node(b).analysis,
                t.node(a).analysis,
                "analysis changed at move {}",
                t.move_number(a)
            );
        }

        // Saving without analysis costs the record the blob and nothing else.
        let plain = write(t, false);
        assert!(!plain.contains("MRAI["));
        let plain = &parse_str(&plain).unwrap()[0];
        assert_eq!(plain.len(), t.len());
        assert!(plain.node(plain.root()).analysis.is_none());
    }

    /// The same writer, judged by bytes it did not produce. Everything asserted before the
    /// blobs is a shape mirai never emits: `KM` ahead of `SZ`, an empty `PB[]`, a `PL[B]`
    /// it only writes when a tree carries an override.
    #[test]
    fn lizzieyzy_file_parses_replays_and_preserves_unknown_properties() {
        let bytes = FOREIGN_SGF;
        assert_eq!(bytes.len(), 25765);
        let games = parse(bytes).expect("parse");
        assert_eq!(games.len(), 1);
        let t = &games[0];
        assert_eq!(t.info.size, Size::square(19));
        assert_eq!(t.info.komi, 7.5);
        assert_eq!(t.len(), 30);
        assert_eq!(t.node(t.root()).to_play_override, Some(Color::Black));
        // Empty is not absent.
        assert_eq!(t.info.players[0].name, "");
        assert_eq!(t.info.result, "");

        // 25 recorded moves, then the main line dives into the first of three variations.
        assert_eq!(replay_main_line(t), 26);
        assert_eq!(t.main_line().len(), 27);
        assert_eq!(t.children(t.main_line()[25]).len(), 3);

        // Unknown-property preservation, against real data.
        let text = std::str::from_utf8(bytes).unwrap();
        let written = write(t, false);
        for name in ["DZ", "LZOP"] {
            assert_eq!(
                raw_value(&written, name),
                raw_value(text, name),
                "{name} value changed"
            );
        }
        // Every node's LZ blob too, not just the root's.
        assert_eq!(written.matches("LZ[").count(), text.matches("LZ[").count());
        let reparsed = &parse_str(&written).unwrap()[0];
        assert_eq!(reparsed.len(), t.len());
        assert_eq!(
            reparsed.node(reparsed.root()).unknown_props,
            t.node(t.root()).unknown_props
        );
    }

    /// Why this crate parses SGF by hand: a property mirai does not model has to come back
    /// byte-identical, or saving a record silently destroys what another program wrote into
    /// it — another engine's analysis blobs, a server's own bookkeeping.
    #[test]
    fn unmodelled_properties_survive_a_round_trip() {
        let text = concat!(
            "(;FF[4]SZ[19]KM[7.5]DZ[G]LZOP[net 64.0 668]VENDOR[a\\]b][second]",
            ";B[pd]LZ[net 35.7 507 pv D4 Q3]",
            "(;W[dd]OB[3]C[note])",
            "(;W[dp]))"
        );
        let t = &parse_str(text).unwrap()[0];
        let written = write(t, false);

        for name in ["DZ", "LZOP", "VENDOR", "LZ", "OB"] {
            assert_eq!(
                raw_value(&written, name),
                raw_value(text, name),
                "{name} changed"
            );
        }
        // Multi-value properties keep every value, and an escaped `]` stays escaped.
        assert!(
            written.contains("VENDOR[a\\]b][second]"),
            "value list lost: {written}"
        );
        // A second save must not shed what the first one kept.
        let again = &parse_str(&written).unwrap()[0];
        assert_eq!(again.len(), t.len());
        assert_eq!(
            again.node(again.root()).unknown_props,
            t.node(t.root()).unknown_props
        );
    }
}
