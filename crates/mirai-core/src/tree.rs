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

    /// Adds `p` to `kind`, or clears every mark at `p` if `kind` is already there.
    /// A different shape or label at `p` is replaced; other points are left as they are.
    pub fn toggle(&mut self, kind: MarkKind, p: Point) {
        let had = self.of(kind).contains(&p);
        self.remove_at(p);
        if !had {
            self.of_mut(kind).push(p);
        }
    }

    pub fn contains(&self, p: Point) -> bool {
        self.labels.iter().any(|(q, _)| *q == p)
            || self.triangle.contains(&p)
            || self.square.contains(&p)
            || self.circle.contains(&p)
            || self.cross.contains(&p)
    }

    pub fn remove_at(&mut self, p: Point) {
        self.labels.retain(|(q, _)| *q != p);
        self.triangle.retain(|&q| q != p);
        self.square.retain(|&q| q != p);
        self.circle.retain(|&q| q != p);
        self.cross.retain(|&q| q != p);
    }

    /// Replaces any mark at `p` with `text`. An empty string removes the mark.
    pub fn set_label(&mut self, p: Point, text: &str) {
        self.remove_at(p);
        if !text.is_empty() {
            self.labels.push((p, text.to_string()));
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
    /// Black-perspective KataGo utility — the blend of win rate and score the GUI colours a
    /// candidate by. `None` on records saved before MRAI v2, which fall back to the means.
    pub utility: Option<f32>,
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
    /// Bumped by only the subset of mutations a branch graph is placed from.
    structure_revision: u64,
    pub info: GameInfo,
}

/// A subtree taken out of a [`GameTree`] by [`GameTree::detach_branch`].
///
/// Not [`Clone`]: restoring is a transfer of ownership, and a detached branch
/// must not be applied to a different tree. Replacing the record drops any
/// history that holds one of these.
#[derive(Debug)]
pub struct DetachedBranch {
    root: NodeId,
    parent: NodeId,
    index: usize,
    nodes: Vec<(NodeId, Node)>,
}

impl GameTree {
    pub fn new(info: GameInfo) -> GameTree {
        GameTree {
            nodes: vec![Some(Node::default())],
            live: 1,
            root: NodeId(0),
            cache: None,
            revision: 0,
            structure_revision: 0,
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

    /// Bumps both revisions and drops the cached position: the caller may change anything,
    /// a node's move included.
    #[inline]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.touch_structure();
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

    /// True when the record holds anything a user would miss: a move, a setup stone,
    /// a mark, an explicit to-play override, or a comment. A blank tree must leave
    /// nothing behind for a crash-recovery prompt.
    ///
    /// Walks the arena rather than `0..len()`: [`GameTree::len`] counts live nodes, and a
    /// tombstone left by [`GameTree::delete_branch`] makes the two disagree.
    pub fn has_content(&self) -> bool {
        self.nodes.iter().flatten().any(|n| {
            n.mv.is_some()
                || !n.setup.is_empty()
                || !n.marks.is_empty()
                || n.to_play_override.is_some()
                || !n.comment.trim().is_empty()
        })
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
    ///
    /// Walks up instead of collecting the path first. The deepest override wins, and by the
    /// time the walk reaches it the moves below it are exactly what has been counted — so
    /// one upward pass answers what two passes over a buffered path used to. That buffer
    /// was a `SmallVec<[NodeId; 64]>`, which every node past move 64 spilled onto the heap,
    /// once per call, and a whole-game sweep calls this once per main-line node.
    pub fn move_number(&self, id: NodeId) -> u16 {
        let mut n = 0u16;
        let mut cur = Some(id);
        while let Some(c) = cur {
            let node = self.node(c);
            // An override replaces the count *at its own node*, so that node's own move
            // never adds to it — only the moves already counted below it do.
            if let Some(m) = node.move_number_override {
                return m.saturating_add(n);
            }
            if node.mv.is_some() {
                n = n.saturating_add(1);
            }
            cur = node.parent;
        }
        n
    }

    /// The move number standing on every point of the board at `id`, `0` where no numbered
    /// stone was played.
    ///
    /// Numbering follows [`GameTree::move_number`], `MN` overrides included; when a point was
    /// played more than once on this line the later move wins, which is what a board showing
    /// stone numbers must display. Passes and off-board points are skipped, and a captured
    /// stone leaves its number behind — a caller draws numbers only where a stone stands.
    /// Setup stones (`AB`/`AW`/`AE`) clear any inherited number at those points and do not
    /// themselves count as a move.
    pub fn move_numbers(&self, id: NodeId) -> Box<[u16]> {
        let size = self.info.size;
        let mut numbers = vec![0u16; size.points()];
        let mut n = 0u16;
        for at in self.path_to(id) {
            let node = self.node(at);
            for &p in node
                .setup
                .add_black
                .iter()
                .chain(&node.setup.add_white)
                .chain(&node.setup.add_empty)
            {
                if !p.is_pass() && size.contains(p) {
                    numbers[p.index()] = 0;
                }
            }
            if node.mv.is_some() {
                n = n.saturating_add(1);
            }
            if let Some(m) = node.move_number_override {
                n = m;
            }
            if let Some((_, p)) = node.mv
                && !p.is_pass()
                && size.contains(p)
            {
                numbers[p.index()] = n;
            }
        }
        numbers.into_boxed_slice()
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
    ///
    /// A non-empty setup is a rules-history boundary: both superko histories are dropped
    /// and the ko ban is cleared. A pure `PL` override only changes [`Position::to_play`].
    fn step(&self, pos: &mut Position, id: NodeId, rules: &Rules) {
        let node = self.node(id);
        let had_setup = !node.setup.is_empty();
        for &p in &node.setup.add_black {
            pos.board.set(p, Some(Color::Black));
        }
        for &p in &node.setup.add_white {
            pos.board.set(p, Some(Color::White));
        }
        for &p in &node.setup.add_empty {
            pos.board.set(p, None);
        }
        if had_setup {
            pos.board.clear_ko();
            pos.hash_history.clear();
            pos.situational_history.clear();
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
        self.touch_structure();
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
        self.touch_structure();
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
        let _ = self.detach_branch(id);
    }

    /// Takes `id` and its descendants out of the arena, leaving tombstones.
    ///
    /// The root and unknown ids return `None` with no mutation. Original sibling
    /// order is recorded so [`GameTree::restore_branch`] can put the branch back.
    pub fn detach_branch(&mut self, id: NodeId) -> Option<DetachedBranch> {
        if id == self.root || !self.contains(id) {
            return None;
        }
        let parent = self.node(id).parent?;
        let index = self.node(parent).children.iter().position(|&c| c == id)?;
        self.nodes[parent.index()]
            .as_mut()
            .expect("stale NodeId")
            .children
            .remove(index);

        let mut stack = vec![id];
        let mut nodes = Vec::new();
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes[n.index()].take() {
                self.live -= 1;
                for &c in &node.children {
                    stack.push(c);
                }
                nodes.push((n, node));
            }
        }
        if matches!(&self.cache, Some((cached, _)) if !self.contains(*cached)) {
            self.cache = None;
        }
        self.touch_structure();
        Some(DetachedBranch {
            root: id,
            parent,
            index,
            nodes,
        })
    }

    /// Puts a branch from [`GameTree::detach_branch`] back at its original ids
    /// and sibling index. History-internal: slots must still be tombstones and
    /// the parent must still exist.
    pub fn restore_branch(&mut self, branch: DetachedBranch) -> NodeId {
        assert!(
            self.contains(branch.parent),
            "restore_branch parent must exist"
        );
        for (id, _) in &branch.nodes {
            assert!(
                self.nodes.get(id.index()).is_some_and(Option::is_none),
                "restore_branch overwrites only tombstones"
            );
        }
        assert!(branch.index <= self.children(branch.parent).len());
        let root = branch.root;
        let parent = branch.parent;
        let index = branch.index;
        for (id, node) in branch.nodes {
            self.nodes[id.index()] = Some(node);
            self.live += 1;
        }
        let children = &mut self.nodes[parent.index()]
            .as_mut()
            .expect("stale NodeId")
            .children;
        children.insert(index, root);
        self.invalidate_position();
        self.touch_structure();
        root
    }

    /// Reorders `child` among `parent`'s children. No-op when `child` is not a
    /// child of `parent` or already sits at `index`.
    pub fn move_child(&mut self, parent: NodeId, child: NodeId, index: usize) {
        let children = &mut self.nodes[parent.index()]
            .as_mut()
            .expect("stale NodeId")
            .children;
        let Some(from) = children.iter().position(|&c| c == child) else {
            return;
        };
        let index = index.min(children.len() - 1);
        if from == index {
            return;
        }
        let c = children.remove(from);
        children.insert(index, c);
        self.touch_structure();
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
        self.touch_structure();
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
        if !self.info.size.contains(p) {
            return;
        }
        {
            let setup = &mut self.nodes[id.index()].as_mut().expect("stale NodeId").setup;
            let counts = [
                setup.add_black.iter().filter(|&&q| q == p).count(),
                setup.add_white.iter().filter(|&&q| q == p).count(),
                setup.add_empty.iter().filter(|&&q| q == p).count(),
            ];
            let expected = match c {
                Some(Color::Black) => [1, 0, 0],
                Some(Color::White) => [0, 1, 0],
                None => [0, 0, 1],
            };
            if counts == expected {
                return;
            }
            setup.add_black.retain(|&q| q != p);
            setup.add_white.retain(|&q| q != p);
            setup.add_empty.retain(|&q| q != p);
            match c {
                Some(Color::Black) => setup.add_black.push(p),
                Some(Color::White) => setup.add_white.push(p),
                None => setup.add_empty.push(p),
            }
        }
        self.invalidate_setup(id);
    }

    /// Swaps this node's setup and to-play override with the caller's. Equal values
    /// are a no-op. A real change drops the position cache and this node's analysis
    /// plus every descendant's.
    pub fn swap_setup(&mut self, id: NodeId, setup: &mut Setup, to_play: &mut Option<Color>) {
        {
            let node = self.nodes[id.index()].as_mut().expect("stale NodeId");
            if node.setup == *setup && node.to_play_override == *to_play {
                return;
            }
            std::mem::swap(&mut node.setup, setup);
            std::mem::swap(&mut node.to_play_override, to_play);
        }
        self.invalidate_setup(id);
    }

    pub fn swap_marks(&mut self, id: NodeId, marks: &mut Marks) {
        let node = self.nodes[id.index()].as_mut().expect("stale NodeId");
        if node.marks == *marks {
            return;
        }
        std::mem::swap(&mut node.marks, marks);
        self.revision += 1;
    }

    pub fn swap_comment(&mut self, id: NodeId, text: &mut String) {
        let node = self.nodes[id.index()].as_mut().expect("stale NodeId");
        if node.comment == *text {
            return;
        }
        std::mem::swap(&mut node.comment, text);
        self.revision += 1;
    }

    #[inline]
    pub fn invalidate_position(&mut self) {
        self.cache = None;
    }

    fn invalidate_setup(&mut self, id: NodeId) {
        self.invalidate_position();
        self.clear_analysis_from(id);
        self.revision += 1;
    }

    fn clear_analysis_from(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.get_mut(n.index()).and_then(Option::as_mut) {
                node.analysis = None;
                for &c in &node.children {
                    stack.push(c);
                }
            }
        }
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

    /// Bumped only when the node set, the child order, or a node's move changes — exactly
    /// what placing a branch graph depends on, and nothing else.
    ///
    /// A view that lays the tree out keys its cache on this rather than on
    /// [`GameTree::revision`]. Storing an analysis is a mutation too, so against the general
    /// counter a running engine invalidated the layout ten times a second: the cache missed
    /// on every navigation step, precisely while the user was navigating.
    #[inline]
    pub fn structure_revision(&self) -> u64 {
        self.structure_revision
    }

    /// Records a change to the node set, to child order, or to a node's move. Bumps the
    /// general revision as well, so a structural change can never be recorded as less than
    /// a change.
    #[inline]
    fn touch_structure(&mut self) {
        self.revision += 1;
        self.structure_revision += 1;
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

    fn empty_19() -> GameTree {
        GameTree::new(GameInfo::new(Size::square(19), RuleSet::Chinese))
    }

    #[test]
    fn a_blank_record_has_no_content() {
        // This is the case that used to greet users with "mirai did not shut down
        // cleanly" after a session in which they did nothing at all.
        assert!(!empty_19().has_content());
    }

    #[test]
    fn a_single_move_or_a_pass_is_content() {
        let mut t = empty_19();
        let root = t.root();
        t.play(root, Color::Black, Size::square(19).point(3, 3))
            .expect("D16 is legal on an empty board");
        assert!(t.has_content());

        let mut passed = empty_19();
        let root = passed.root();
        passed.play(root, Color::Black, Point::PASS).expect("pass");
        assert!(passed.has_content());
    }

    #[test]
    fn a_comment_or_setup_stone_alone_is_enough() {
        let mut commented = empty_19();
        let root = commented.root();
        commented.set_comment(root, "  ");
        assert!(!commented.has_content(), "whitespace is not real content");
        commented.set_comment(root, "study this");
        assert!(commented.has_content());

        let mut setup = empty_19();
        let root = setup.root();
        setup.set_setup_stone(root, Size::square(19).point(3, 3), Some(Color::Black));
        assert!(setup.has_content());
    }

    #[test]
    fn marks_alone_are_content() {
        let mut t = empty_19();
        let root = t.root();
        t.toggle_mark(root, MarkKind::Triangle, Size::square(19).point(3, 3));
        assert!(t.has_content());
    }

    #[test]
    fn to_play_override_alone_is_content() {
        let mut t = empty_19();
        let root = t.root();
        t.node_mut(root).to_play_override = Some(Color::White);
        assert!(t.has_content());
    }

    /// The arena is tombstoned, so a scan bounded by `len()` — the *live* count — would
    /// stop short of a node that outlived a deleted branch.
    #[test]
    fn content_is_found_past_a_deleted_branch() {
        let mut t = empty_19();
        let size = Size::square(19);
        let root = t.root();
        let doomed = t.play(root, Color::Black, size.point(3, 3)).expect("D16");
        let kept = t.add_child(root);
        let deep = t.add_child(kept);
        t.delete_branch(doomed);
        assert!(!t.has_content(), "empty nodes are not content");

        t.play(deep, Color::White, size.point(15, 15)).expect("Q4");
        assert!(
            t.has_content(),
            "a live node above the live count must still be seen"
        );
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

    /// What a board draws stone numbers from: the number of the stone standing on each point,
    /// after every `MN` override and after a point has been played twice.
    #[test]
    fn per_point_move_numbers_take_the_override_and_the_later_stone() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let contested = s.point(3, 3);
        let first = t.play(root, Color::Black, contested).unwrap();
        t.node_mut(first).move_number_override = Some(42);
        let mut at = first;
        for (x, y) in [(3, 2), (2, 3), (4, 3), (3, 4)] {
            at = t.play(at, Color::White, s.point(x, y)).unwrap();
        }
        // Black is captured, so White may have the point itself.
        assert_eq!(t.position(at).board.at(contested), None);
        let retaken = t.play(at, Color::White, contested).unwrap();

        let numbers = t.move_numbers(retaken);
        assert_eq!(numbers.len(), s.points());
        assert_eq!(numbers[s.point(3, 2).index()], 43);
        // 47, not the 42 the captured stone carried.
        assert_eq!(numbers[contested.index()], 47);
        assert_eq!(numbers[s.point(0, 0).index()], 0);

        // A pass is numbered but occupies nothing.
        let passed = t.play(retaken, Color::Black, Point::PASS).unwrap();
        assert_eq!(t.move_numbers(passed)[contested.index()], 47);
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

    /// Placing a branch graph depends on the node set, the child order and each node's move
    /// — and on nothing else. Storing an analysis ten times a second must therefore not read
    /// as a structural change, or every view keyed on it rebuilds while the engine runs.
    #[test]
    fn only_structural_edits_bump_the_structure_revision() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(3, 3)).unwrap();

        let structure = t.structure_revision();
        let revision = t.revision();

        t.set_analysis(a, None);
        t.set_comment(a, "note");
        t.toggle_mark(a, MarkKind::Triangle, s.point(0, 0));
        t.set_setup_stone(root, s.point(1, 1), Some(Color::White));

        assert_eq!(
            t.structure_revision(),
            structure,
            "node metadata is not tree structure"
        );
        assert!(
            t.revision() > revision,
            "but they are still mutations a UI redraws for"
        );

        t.play(a, Color::White, s.point(4, 4)).unwrap();
        assert!(
            t.structure_revision() > structure,
            "a new node changes the structure"
        );
    }

    fn dummy_analysis() -> NodeAnalysis {
        NodeAnalysis {
            visits: 7,
            winrate: 0.5,
            score_lead: 0.0,
            score_stdev: 0.0,
            candidates: Vec::new(),
            ownership: None,
        }
    }

    #[test]
    fn marks_are_exclusive_per_point() {
        let mut m = Marks::default();
        let p = Size::square(5).point(2, 2);
        let q = Size::square(5).point(1, 1);
        m.toggle(MarkKind::Triangle, p);
        m.toggle(MarkKind::Square, p);
        assert!(!m.triangle.contains(&p));
        assert_eq!(m.of(MarkKind::Square), &[p]);
        m.set_label(p, "A");
        assert!(m.of(MarkKind::Square).is_empty());
        assert_eq!(m.labels, vec![(p, "A".into())]);
        m.toggle(MarkKind::Circle, q);
        m.set_label(p, "");
        assert!(!m.contains(p));
        assert!(m.contains(q));
        m.set_label(q, "死活:A\\]");
        assert_eq!(m.labels, vec![(q, "死活:A\\]".into())]);
        assert!(m.of(MarkKind::Circle).is_empty());
    }

    #[test]
    fn setup_clears_inherited_move_numbers() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let p = s.point(0, 0);
        let a = t.play(root, Color::Black, p).unwrap();
        assert_eq!(t.move_numbers(a)[p.index()], 1);
        let setup = t.add_child(a);
        t.set_setup_stone(setup, p, Some(Color::White));
        let numbers = t.move_numbers(setup);
        assert_eq!(numbers[p.index()], 0);
        assert_eq!(t.position(setup).move_number, 1);
        assert_eq!(t.position(setup).board.at(p), Some(Color::White));
    }

    #[test]
    fn nonempty_setup_clears_ko_and_superko_history() {
        let mut t = tree(RuleSet::Chinese);
        let (p, q) = ko_setup(&mut t);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::White, q).expect("white takes");
        assert_eq!(t.position(a).board.ko_ban(), Some(p));
        let before = t.position(a).hash_history.len();
        assert!(before >= 2);

        let setup = t.add_child(a);
        t.set_setup_stone(setup, s.point(0, 0), Some(Color::Black));
        let pos = t.position(setup);
        assert_eq!(pos.board.ko_ban(), None);
        assert_eq!(pos.hash_history.len(), 1);
        assert_eq!(pos.situational_history.len(), 1);
        assert_eq!(pos.move_number, 1);
    }

    #[test]
    fn moves_after_setup_still_enforce_superko() {
        let mut t = tree(RuleSet::TrompTaylor);
        let (p, q) = ko_setup(&mut t);
        let root = t.root();
        let capture = t.play(root, Color::White, q).unwrap();
        // An explicit AE on an already empty point establishes a fresh position
        // boundary without changing the ko shape or prisoner count.
        let boundary = t.add_child(capture);
        let empty = t.info.size.point(4, 4);
        t.set_setup_stone(boundary, empty, None);
        let recapture = t.play(boundary, Color::Black, p).unwrap();
        assert_eq!(t.position(recapture).board.captures, [1, 1]);
        assert_eq!(t.play(recapture, Color::White, q), Err(IllegalMove::Ko));
    }

    #[test]
    fn ae_only_setup_clears_ko_ban() {
        let mut t = tree(RuleSet::Chinese);
        let (p, q) = ko_setup(&mut t);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::White, q).expect("white takes");
        assert_eq!(t.position(a).board.ko_ban(), Some(p));
        let setup = t.add_child(a);
        t.set_setup_stone(setup, s.point(1, 1), None);
        assert_eq!(t.position(setup).board.ko_ban(), None);
        assert_eq!(t.position(setup).board.at(s.point(1, 1)), None);
    }

    #[test]
    fn pure_pl_keeps_ko_and_hash_history() {
        let mut t = tree(RuleSet::Chinese);
        let (p, q) = ko_setup(&mut t);
        let root = t.root();
        let a = t.play(root, Color::White, q).expect("white takes");
        assert_eq!(t.position(a).board.ko_ban(), Some(p));
        let hashes = t.position(a).hash_history.clone();
        let pl_node = t.add_child(a);
        let mut setup = Setup::default();
        let mut pl = Some(Color::White);
        t.swap_setup(pl_node, &mut setup, &mut pl);
        let pos = t.position(pl_node);
        assert_eq!(pos.board.ko_ban(), Some(p));
        assert_eq!(&pos.hash_history[..hashes.len()], hashes.as_slice());
        assert_eq!(pos.to_play, Color::White);
        assert_eq!(pl, None);
        assert!(setup.is_empty());
    }

    #[test]
    fn equal_swaps_do_not_bump_revision() {
        let mut t = tree(RuleSet::Chinese);
        let root = t.root();
        let revision = t.revision();
        let mut setup = t.node(root).setup.clone();
        let mut pl = t.node(root).to_play_override;
        t.swap_setup(root, &mut setup, &mut pl);
        let mut marks = t.node(root).marks.clone();
        t.swap_marks(root, &mut marks);
        let mut comment = t.node(root).comment.clone();
        t.swap_comment(root, &mut comment);
        assert_eq!(t.revision(), revision);
    }

    #[test]
    fn setup_clears_descendant_analysis_not_siblings() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(0, 0)).unwrap();
        let child = t.play(a, Color::White, s.point(1, 1)).unwrap();
        let sibling = t.add_variation(root, Color::Black, s.point(2, 2)).unwrap();
        t.set_analysis(a, Some(dummy_analysis()));
        t.set_analysis(child, Some(dummy_analysis()));
        t.set_analysis(sibling, Some(dummy_analysis()));
        t.set_setup_stone(a, s.point(4, 4), Some(Color::White));
        assert!(t.node(a).analysis.is_none());
        assert!(t.node(child).analysis.is_none());
        assert!(t.node(sibling).analysis.is_some());
    }

    #[test]
    fn marks_and_comment_swaps_keep_analysis() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        t.set_analysis(root, Some(dummy_analysis()));
        let mut marks = Marks::default();
        marks.toggle(MarkKind::Triangle, s.point(0, 0));
        t.swap_marks(root, &mut marks);
        let mut comment = "note".into();
        t.swap_comment(root, &mut comment);
        assert!(t.node(root).analysis.is_some());
        assert_eq!(t.position(root).to_play, Color::Black);
    }

    #[test]
    fn detach_restore_keeps_ids_order_and_payload() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(0, 0)).unwrap();
        let a1 = t.play(a, Color::White, s.point(1, 0)).unwrap();
        let b = t.add_variation(root, Color::Black, s.point(2, 2)).unwrap();
        t.set_comment(b, "other");
        t.set_analysis(a, Some(dummy_analysis()));
        t.node_mut(a1)
            .unknown_props
            .push(("XX".into(), vec!["v".into()]));

        let rev = t.revision();
        assert!(t.detach_branch(root).is_none());
        assert_eq!(t.revision(), rev);
        assert!(t.detach_branch(NodeId(9999)).is_none());

        let detached = t.detach_branch(a).unwrap();
        assert!(!t.contains(a) && !t.contains(a1));
        assert_eq!(t.children(root), &[b]);
        assert_eq!(t.node(b).comment, "other");

        let restored = t.restore_branch(detached);
        assert_eq!(restored, a);
        assert_eq!(t.children(root), &[a, b]);
        assert_eq!(t.node(a1).mv, Some((Color::White, s.point(1, 0))));
        assert!(t.node(a).analysis.is_some());
        assert_eq!(
            t.node(a1).unknown_props,
            vec![("XX".into(), vec!["v".into()])]
        );
    }

    #[test]
    fn move_child_reorders_without_copying() {
        let mut t = tree(RuleSet::Chinese);
        let s = t.info.size;
        let root = t.root();
        let a = t.play(root, Color::Black, s.point(0, 0)).unwrap();
        let b = t.add_variation(root, Color::Black, s.point(2, 2)).unwrap();
        assert_eq!(t.children(root), &[a, b]);
        let structure = t.structure_revision();
        t.move_child(root, b, 0);
        assert_eq!(t.children(root), &[b, a]);
        assert!(t.structure_revision() > structure);
        let structure = t.structure_revision();
        t.move_child(root, b, 0);
        assert_eq!(t.structure_revision(), structure);
    }

    #[test]
    fn detach_restore_long_line_does_not_overflow() {
        let mut t = empty_19();
        let mut cur = t.root();
        for _ in 0..8000 {
            cur = t.add_child(cur);
        }
        let child = t.children(t.root())[0];
        let detached = t.detach_branch(child).unwrap();
        assert_eq!(t.len(), 1);
        let restored = t.restore_branch(detached);
        assert_eq!(restored, child);
        assert_eq!(t.len(), 8001);
        assert_eq!(t.path_to(cur).len(), 8001);
    }
}
