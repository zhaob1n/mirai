// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The game tree: an index-based arena of [`Node`]s plus a cached [`Position`].
//!
//! Nodes are addressed by [`NodeId`], never by reference, so the whole tree is a plain
//! `Vec` with no reference counting and ids stay usable across edits. Deleting a branch
//! *tombstones* its nodes, so every surviving [`NodeId`] keeps pointing at the same node.
//!
//! A [`Position`] is derived state: it is recomputed by replaying the line from the root.
//! The tree caches the last one it computed, so stepping to a child costs one move.

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::board::{Board, IllegalMove};
use crate::point::{Color, Point, Size};
use crate::rules::{Ko, RuleSet, Rules};

/// Handle to a node in a [`GameTree`]'s arena. Stable across every edit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    #[inline]
    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// SGF `AB` / `AW` / `AE`: stones placed or removed without playing a move.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub add_black: Vec<Point>,
    pub add_white: Vec<Point>,
    pub add_empty: Vec<Point>,
}

impl Setup {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.add_black.is_empty() && self.add_white.is_empty() && self.add_empty.is_empty()
    }
}

/// The four shape marks SGF defines (`TR` / `SQ` / `CR` / `MA`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkKind {
    Triangle,
    Square,
    Circle,
    Cross,
}

/// Board decorations attached to a node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub labels: Vec<(Point, String)>,
    pub triangle: Vec<Point>,
    pub square: Vec<Point>,
    pub circle: Vec<Point>,
    pub cross: Vec<Point>,
}

impl Marks {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
            && self.triangle.is_empty()
            && self.square.is_empty()
            && self.circle.is_empty()
            && self.cross.is_empty()
    }

    /// Adds `p` to `kind`, or removes it if it is already there.
    pub fn toggle(&mut self, kind: MarkKind, p: Point) {
        let v = self.of_mut(kind);
        match v.iter().position(|&q| q == p) {
            Some(i) => {
                v.remove(i);
            }
            None => v.push(p),
        }
    }

    #[inline]
    pub fn of(&self, kind: MarkKind) -> &[Point] {
        match kind {
            MarkKind::Triangle => &self.triangle,
            MarkKind::Square => &self.square,
            MarkKind::Circle => &self.circle,
            MarkKind::Cross => &self.cross,
        }
    }

    #[inline]
    fn of_mut(&mut self, kind: MarkKind) -> &mut Vec<Point> {
        match kind {
            MarkKind::Triangle => &mut self.triangle,
            MarkKind::Square => &mut self.square,
            MarkKind::Circle => &mut self.circle,
            MarkKind::Cross => &mut self.cross,
        }
    }
}

/// One engine candidate move, dequantised. Black's perspective, like everything stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub mv: Point,
    pub visits: u32,
    pub winrate: f32,
    pub score_lead: f32,
    pub prior: f32,
    pub pv: Vec<Point>,
}

/// Cached engine evaluation of a node's position. Black's perspective throughout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeAnalysis {
    pub visits: u32,
    pub winrate: f32,
    pub score_lead: f32,
    pub score_stdev: f32,
    pub candidates: Vec<Candidate>,
    pub ownership: Option<Box<[i8]>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerInfo {
    pub name: String,
    pub rank: String,
}

/// Whole-game metadata, i.e. the SGF root properties mirai understands.
#[derive(Clone, Debug, PartialEq)]
pub struct GameInfo {
    pub size: Size,
    pub rules: RuleSet,
    pub komi: f32,
    pub handicap: u8,
    pub result: String,
    /// Indexed by [`Color::index`]: `players[0]` is Black.
    pub players: [PlayerInfo; 2],
    pub date: String,
    pub event: String,
    pub time_limit: String,
    pub overtime: String,
}

impl GameInfo {
    pub fn new(size: Size, rules: RuleSet) -> GameInfo {
        GameInfo {
            size,
            rules,
            komi: rules.default_komi(),
            handicap: 0,
            result: String::new(),
            players: [PlayerInfo::default(), PlayerInfo::default()],
            date: String::new(),
            event: String::new(),
            time_limit: String::new(),
            overtime: String::new(),
        }
    }
}

impl Default for GameInfo {
    fn default() -> GameInfo {
        GameInfo::new(Size::square(19), RuleSet::default())
    }
}

/// One node of the tree. Every field is public: the tree owns structure, not content.
#[derive(Clone, Debug, Default)]
pub struct Node {
    pub parent: Option<NodeId>,
    /// `children[0]` is the main line.
    pub children: SmallVec<[NodeId; 2]>,
    /// `None` on the root and on pure setup nodes; [`Point::PASS`] for a pass.
    pub mv: Option<(Color, Point)>,
    pub setup: Setup,
    pub marks: Marks,
    pub comment: String,
    /// SGF `MN`.
    pub move_number_override: Option<u16>,
    /// SGF `PL`: forces the side to move after this node.
    pub to_play_override: Option<Color>,
    pub analysis: Option<NodeAnalysis>,
    /// Everything mirai does not model, preserved verbatim so foreign files survive a
    /// load/save round trip (LizzieYzy's `LZ` / `LZOP` / `DZ` blobs, for instance).
    pub unknown_props: Vec<(Box<str>, Vec<Box<str>>)>,
}

/// The board state a node stands for, derived by replaying from the root.
#[derive(Clone, Debug)]
pub struct Position {
    pub board: Board,
    pub to_play: Color,
    /// [`Board::zobrist`] of this position and of every position before it on this line.
    pub hash_history: Vec<u64>,
    /// The same positions hashed with their side to move, for situational superko.
    pub situational_history: Vec<u64>,
    pub move_number: u16,
}

/// A game record: an arena of nodes rooted at [`GameTree::root`], plus its [`GameInfo`].
#[derive(Clone, Debug)]
pub struct GameTree {
    /// `None` marks a tombstone left by [`GameTree::delete_branch`].
    nodes: Vec<Option<Node>>,
    live: usize,
    root: NodeId,
    cache: Option<(NodeId, Position)>,
    revision: u64,
    pub info: GameInfo,
}

impl GameTree {
    pub fn new(info: GameInfo) -> GameTree {
        GameTree {
            nodes: vec![Some(Node::default())],
            live: 1,
            root: NodeId(0),
            cache: None,
            revision: 0,
            info,
        }
    }

    #[inline]
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Number of live nodes; tombstones do not count.
    #[inline]
    pub fn len(&self) -> usize {
        self.live
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Panics if `id` was deleted; use [`GameTree::get`] when that is possible.
    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        self.get(id).expect("stale NodeId")
    }

    /// Bumps the revision and drops the cached position: the caller may change anything.
    #[inline]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.revision += 1;
        self.cache = None;
        self.nodes[id.index()].as_mut().expect("stale NodeId")
    }

    #[inline]
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())?.as_ref()
    }

    #[inline]
    pub fn contains(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    #[inline]
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    #[inline]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    /// The root, then `children[0]` for as long as there is one.
    pub fn main_line(&self) -> Vec<NodeId> {
        let mut out = vec![self.root];
        let mut cur = self.root;
        while let Some(&next) = self.node(cur).children.first() {
            out.push(next);
            cur = next;
        }
        out
    }

    /// Root first, `id` last.
    pub fn path_to(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            out.push(c);
            cur = self.node(c).parent;
        }
        out.reverse();
        out
    }

    /// The move number a node carries, honouring every `MN` override on the way down.
    pub fn move_number(&self, id: NodeId) -> u16 {
        let mut n = 0u16;
        let mut cur = Some(id);
        let mut path: SmallVec<[NodeId; 64]> = SmallVec::new();
        while let Some(c) = cur {
            path.push(c);
            cur = self.node(c).parent;
        }
        for &p in path.iter().rev() {
            let node = self.node(p);
            if node.mv.is_some() {
                n = n.saturating_add(1);
            }
            if let Some(m) = node.move_number_override {
                n = m;
            }
        }
        n
    }

    /// The position at `id`, replaying only what the cache does not already cover.
    pub fn position(&mut self, id: NodeId) -> &Position {
        self.ensure_position(id);
        &self.cache.as_ref().expect("cache filled above").1
    }

    fn ensure_position(&mut self, id: NodeId) {
        if matches!(&self.cache, Some((cached, _)) if *cached == id) {
            return;
        }
        let rules = self.info.rules.rules();
        let parent = self.node(id).parent;
        // Stepping to a child of the cached node is the navigation the GUI does on every
        // arrow key; it must not replay the line.
        if let Some((cached, mut pos)) = self.cache.take()
            && Some(cached) == parent
        {
            self.step(&mut pos, id, &rules);
            self.cache = Some((id, pos));
            return;
        }
        let mut pos = self.initial_position();
        let path = self.path_to(id);
        for &n in &path {
            self.step(&mut pos, n, &rules);
        }
        self.cache = Some((id, pos));
    }

    fn initial_position(&self) -> Position {
        Position {
            board: Board::new(self.info.size),
            to_play: Color::Black,
            hash_history: Vec::new(),
            situational_history: Vec::new(),
            move_number: 0,
        }
    }

    /// Applies one node to `pos` and records the resulting hashes.
    fn step(&self, pos: &mut Position, id: NodeId, rules: &Rules) {
        let node = self.node(id);
        for &p in &node.setup.add_black {
            pos.board.set(p, Some(Color::Black));
        }
        for &p in &node.setup.add_white {
            pos.board.set(p, Some(Color::White));
        }
        for &p in &node.setup.add_empty {
            pos.board.set(p, None);
        }
        if node.parent.is_none() && self.info.handicap >= 2 {
            // Black has already placed the handicap stones.
            pos.to_play = Color::White;
        }
        if let Some((color, p)) = node.mv {
            // Replay never rejects: a stored line is history, not a legality question.
            let _ = pos.board.play(color, p, rules);
            pos.to_play = color.other();
            pos.move_number = pos.move_number.saturating_add(1);
        }
        if let Some(c) = node.to_play_override {
            pos.to_play = c;
        }
        if let Some(m) = node.move_number_override {
            pos.move_number = m;
        }
        pos.hash_history.push(pos.board.zobrist());
        pos.situational_history
            .push(pos.board.situational_hash(pos.to_play));
    }

    /// Plays `color` at `p` after `at`, reusing an existing child with the same move
    /// instead of creating a duplicate branch.
    pub fn play(&mut self, at: NodeId, color: Color, p: Point) -> Result<NodeId, IllegalMove> {
        if let Some(&existing) = self
            .children(at)
            .iter()
            .find(|&&c| self.node(c).mv == Some((color, p)))
        {
            return Ok(existing);
        }
        self.insert_move(at, color, p)
    }

    /// Like [`GameTree::play`] but always creates a new child.
    pub fn add_variation(
        &mut self,
        at: NodeId,
        color: Color,
        p: Point,
    ) -> Result<NodeId, IllegalMove> {
        self.insert_move(at, color, p)
    }

    fn insert_move(&mut self, at: NodeId, color: Color, p: Point) -> Result<NodeId, IllegalMove> {
        let rules = self.info.rules.rules();
        self.ensure_position(at);
        let pos = &self.cache.as_ref().expect("cache filled above").1;
        // A pass never repeats a position by itself, and is legal under every ruleset.
        if !p.is_pass() {
            let mut next = pos.board.clone();
            next.play(color, p, &rules)?;
            let repeated = match rules.ko {
                Ko::Positional => pos.hash_history.contains(&next.zobrist()),
                Ko::Situational => pos
                    .situational_history
                    .contains(&next.situational_hash(color.other())),
                // Simple ko is the board's own business.
                Ko::Simple => false,
            };
            if repeated {
                return Err(IllegalMove::Ko);
            }
        }
        let id = self.push(Node {
            parent: Some(at),
            mv: Some((color, p)),
            ..Node::default()
        });
        self.nodes[at.index()]
            .as_mut()
            .expect("stale NodeId")
            .children
            .push(id);
        self.revision += 1;
        Ok(id)
    }

    /// An empty child, for SGF loading and board-setup editing.
    pub fn add_child(&mut self, at: NodeId) -> NodeId {
        let id = self.push(Node {
            parent: Some(at),
            ..Node::default()
        });
        self.nodes[at.index()]
            .as_mut()
            .expect("stale NodeId")
            .children
            .push(id);
        self.revision += 1;
        id
    }

    fn push(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Some(node));
        self.live += 1;
        id
    }

    /// Removes `id` and everything below it. Ids of surviving nodes stay valid.
    /// Deleting the root is a no-op — a tree always has one.
    pub fn delete_branch(&mut self, id: NodeId) {
        if id == self.root || !self.contains(id) {
            return;
        }
        if let Some(parent) = self.node(id).parent {
            self.nodes[parent.index()]
                .as_mut()
                .expect("stale NodeId")
                .children
                .retain(|c| *c != id);
        }
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes[n.index()].take() {
                self.live -= 1;
                stack.extend_from_slice(&node.children);
            }
        }
        if matches!(&self.cache, Some((cached, _)) if !self.contains(*cached)) {
            self.cache = None;
        }
        self.revision += 1;
    }

    /// Makes the line through `id` the main line, all the way up to the root.
    pub fn promote_to_main_line(&mut self, id: NodeId) {
        if !self.contains(id) {
            return;
        }
        let path = self.path_to(id);
        for w in path.windows(2) {
            let children = &mut self.nodes[w[0].index()]
                .as_mut()
                .expect("stale NodeId")
                .children;
            if let Some(i) = children.iter().position(|&c| c == w[1])
                && i != 0
            {
                let c = children.remove(i);
                children.insert(0, c);
            }
        }
        self.revision += 1;
    }

    pub fn set_comment(&mut self, id: NodeId, text: impl Into<String>) {
        self.nodes[id.index()]
            .as_mut()
            .expect("stale NodeId")
            .comment = text.into();
        self.revision += 1;
    }

    pub fn toggle_mark(&mut self, id: NodeId, kind: MarkKind, p: Point) {
        self.nodes[id.index()]
            .as_mut()
            .expect("stale NodeId")
            .marks
            .toggle(kind, p);
        self.revision += 1;
    }

    /// Adds `p` to this node's setup as `c`, or as `AE` when `c` is `None`.
    pub fn set_setup_stone(&mut self, id: NodeId, p: Point, c: Option<Color>) {
        let setup = &mut self.nodes[id.index()].as_mut().expect("stale NodeId").setup;
        setup.add_black.retain(|&q| q != p);
        setup.add_white.retain(|&q| q != p);
        setup.add_empty.retain(|&q| q != p);
        match c {
            Some(Color::Black) => setup.add_black.push(p),
            Some(Color::White) => setup.add_white.push(p),
            None => setup.add_empty.push(p),
        }
        self.cache = None;
        self.revision += 1;
    }

    pub fn set_analysis(&mut self, id: NodeId, a: Option<NodeAnalysis>) {
        self.nodes[id.index()]
            .as_mut()
            .expect("stale NodeId")
            .analysis = a;
        self.revision += 1;
    }

    /// Bumped by every mutation, so views can tell whether they are stale in O(1).
    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(rules: RuleSet) -> GameTree {
        GameTree::new(GameInfo::new(Size::square(5), rules))
    }

    /// A textbook ko at `q = (1,2)` / `p = (2,2)` on 5x5, with Black already at `p`.
    /// White capturing at `q` and Black recapturing at `p` restores the exact position.
    fn ko_setup(t: &mut GameTree) -> (Point, Point) {
        let s = t.info.size;
        let p = s.point(2, 2);
        let q = s.point(1, 2);
        let root = t.root();
        for (x, y) in [(1, 1), (0, 2), (1, 3)] {
            t.set_setup_stone(root, s.point(x, y), Some(Color::Black));
        }
        t.set_setup_stone(root, p, Some(Color::Black));
        for (x, y) in [(2, 1), (3, 2), (2, 3)] {
            t.set_setup_stone(root, s.point(x, y), Some(Color::White));
        }
        (p, q)
    }

    #[test]
    fn positional_superko_rejects_what_situational_allows() {
        // The setup position records Black to move; the recorded continuation starts with
        // White, so the repeat two plies later happens with the *other* side to move.
        for (ruleset, expected) in [
            (RuleSet::TrompTaylor, Err(IllegalMove::Ko)),
            (RuleSet::Aga, Ok(())),
        ] {
            let mut t = tree(ruleset);
            let (p, q) = ko_setup(&mut t);
            let root = t.root();
            t.node_mut(root).to_play_override = Some(Color::Black);

            let a = t.play(root, Color::White, q).expect("white takes");
            assert_eq!(t.position(a).board.at(p), None, "black stone captured");

            let repeat = t.play(a, Color::Black, p);
            match expected {
                Err(e) => assert_eq!(repeat.unwrap_err(), e, "{ruleset:?}"),
                Ok(()) => {
                    let b = repeat.expect("situational superko permits the flipped repeat");
                    let pos = t.position(b);
                    assert_eq!(pos.board.at(q), None);
                    assert_eq!(pos.board.at(p), Some(Color::Black));
                    assert_eq!(pos.to_play, Color::White);
                }
            }
        }
    }

    #[test]
    fn simple_ko_is_left_to_the_board() {
        let mut t = tree(RuleSet::Chinese);
        let (p, q) = ko_setup(&mut t);
        let root = t.root();
        let a = t.play(root, Color::White, q).expect("white takes");
        assert_eq!(t.play(a, Color::Black, p), Err(IllegalMove::Ko));
    }

    #[test]
    fn superko_ignores_passes() {
        let mut t = tree(RuleSet::TrompTaylor);
        let root = t.root();
        let a = t.play(root, Color::Black, Point::PASS).expect("pass");
        let b = t.play(a, Color::White, Point::PASS).expect("pass");
        // Two passes leave the board untouched, which positional superko must not read
        // as a repetition.
        let c = t.play(b, Color::Black, Point::PASS).expect("pass");
        assert_eq!(t.position(c).move_number, 3);
    }

    /// Stepping forward through a line uses the cache; it must produce exactly what a
    /// replay from the root produces.
    #[test]
    fn incremental_stepping_matches_a_cold_replay() {
        let mut t = GameTree::new(GameInfo::new(Size::square(19), RuleSet::Chinese));
        let s = t.info.size;
        let mut cur = t.root();
        let mut ids = vec![cur];
        // Two full edge rows, alternating colours: every move is legal and nothing is
        // ever captured, so the expected board is obvious.
        for i in 0..30u16 {
            let color = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            let p = s.point((i % 19) as u8, 2 * (i / 19) as u8);
            cur = t.play(cur, color, p).expect("legal");
            ids.push(cur);
        }
        let warm: Vec<(u64, u16)> = ids
            .iter()
            .map(|&id| {
                let pos = t.position(id);
                (pos.board.zobrist(), pos.move_number)
            })
            .collect();
        for (i, &id) in ids.iter().enumerate() {
            let mut cold = t.clone();
            let root = cold.root();
            cold.node_mut(root); // drops the cached position
            let pos = cold.position(id);
            assert_eq!((pos.board.zobrist(), pos.move_number), warm[i], "node {i}");
            assert_eq!(pos.hash_history.len(), i + 1);
        }
    }

    #[test]
    fn position_tracks_navigation_and_variations() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(2, 2)).unwrap();
        let b = t.play(a, Color::White, s.point(1, 1)).unwrap();
        let c = t.play(b, Color::Black, s.point(3, 3)).unwrap();
        let alt = t.add_variation(a, Color::White, s.point(4, 4)).unwrap();

        // Forward.
        assert_eq!(t.position(c).move_number, 3);
        assert_eq!(t.position(c).board.at(s.point(3, 3)), Some(Color::Black));
        // Back.
        let pos = t.position(a);
        assert_eq!(pos.move_number, 1);
        assert_eq!(pos.board.at(s.point(1, 1)), None);
        assert_eq!(pos.to_play, Color::White);
        // Into a sibling variation.
        let pos = t.position(alt);
        assert_eq!(pos.move_number, 2);
        assert_eq!(pos.board.at(s.point(4, 4)), Some(Color::White));
        assert_eq!(pos.board.at(s.point(1, 1)), None);
        // And back onto the main line.
        assert_eq!(t.position(b).board.at(s.point(1, 1)), Some(Color::White));
        assert_eq!(t.position(b).board.at(s.point(4, 4)), None);
    }

    #[test]
    fn setup_nodes_do_not_advance_the_move_number() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(0, 0)).unwrap();
        let setup = t.add_child(a);
        t.set_setup_stone(setup, s.point(4, 4), Some(Color::White));
        let pos = t.position(setup);
        assert_eq!(pos.move_number, 1);
        assert_eq!(pos.board.at(s.point(4, 4)), Some(Color::White));
    }

    #[test]
    fn play_reuses_a_child_and_add_variation_branches() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(2, 2)).unwrap();
        let again = t.play(root, Color::Black, s.point(2, 2)).unwrap();
        assert_eq!(a, again);
        assert_eq!(t.children(root).len(), 1);

        let dup = t.add_variation(root, Color::Black, s.point(2, 2)).unwrap();
        assert_ne!(a, dup);
        assert_eq!(t.children(root), &[a, dup]);
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn promote_and_delete_keep_ids_valid() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(0, 0)).unwrap();
        let a1 = t.play(a, Color::White, s.point(1, 0)).unwrap();
        let b = t.add_variation(root, Color::Black, s.point(2, 2)).unwrap();
        let b1 = t.play(b, Color::White, s.point(3, 3)).unwrap();

        assert_eq!(t.main_line(), vec![root, a, a1]);
        t.promote_to_main_line(b1);
        assert_eq!(t.main_line(), vec![root, b, b1]);
        assert_eq!(t.children(root), &[b, a]);

        let rev = t.revision();
        t.delete_branch(a);
        assert!(t.revision() > rev);
        assert_eq!(t.children(root), &[b]);
        assert_eq!(t.len(), 3);
        assert!(!t.contains(a) && !t.contains(a1));
        // Survivors keep their ids and their positions.
        assert!(t.contains(b) && t.contains(b1));
        assert_eq!(t.node(b1).mv, Some((Color::White, s.point(3, 3))));
        assert_eq!(t.position(b1).board.at(s.point(3, 3)), Some(Color::White));

        t.delete_branch(root);
        assert!(t.contains(root));
    }

    #[test]
    fn handicap_makes_white_move_first() {
        let mut t = tree(RuleSet::Chinese);
        t.info.handicap = 2;
        let s = t.info.size;
        let root = t.root();
        t.set_setup_stone(root, s.point(1, 1), Some(Color::Black));
        t.set_setup_stone(root, s.point(3, 3), Some(Color::Black));
        assert_eq!(t.position(root).to_play, Color::White);
        assert_eq!(t.position(root).move_number, 0);
    }

    #[test]
    fn move_number_override_wins() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(0, 0)).unwrap();
        t.node_mut(a).move_number_override = Some(42);
        let b = t.play(a, Color::White, s.point(1, 1)).unwrap();
        assert_eq!(t.move_number(b), 43);
        assert_eq!(t.position(b).move_number, 43);
    }

    #[test]
    fn marks_toggle() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let p = s.point(2, 2);
        t.toggle_mark(root, MarkKind::Triangle, p);
        assert_eq!(t.node(root).marks.of(MarkKind::Triangle), &[p]);
        t.toggle_mark(root, MarkKind::Triangle, p);
        assert!(t.node(root).marks.is_empty());
    }
}
