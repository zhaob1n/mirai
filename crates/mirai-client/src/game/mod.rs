// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The record the user is editing: which node they are looking at, what editing it means,
//! and whether it needs saving.
//!
//! `mirai-core` owns the tree, the rules and SGF fidelity. This owns the *cursor* and the
//! bookkeeping a frontend would otherwise reinvent: a revision counter to redraw against, a
//! dirty flag, and the file Save writes to.
//!
//! Every mutation goes through a method here rather than through `&mut GameTree` directly,
//! so nothing can change the record without the frontend hearing about it.

mod history;

use mirai_core::{
    Color, GameInfo, GameTree, IllegalMove, MarkKind, Marks, Node, NodeAnalysis, NodeId, Point,
    RuleSet, Setup, Size, sgf,
};
use mirai_engine::{AnalyzeReq, Want};

use crate::analysis;
use history::{Edit, EditKind, History};

/// SGF move-node properties mirai does not model. A node that still carries one is not a
/// pure setup node — writing `AB`/`AW`/`AE`/`PL` onto it would mix move and setup.
const MOVE_PROPS: &[&str] = &["KO", "BM", "DO", "IT", "TE", "BL", "WL", "OB", "OW"];

pub struct GameSession {
    tree: GameTree,
    cursor: NodeId,
    /// Bumped by anything the UI would have to redraw for.
    revision: u64,
    /// Bumped only when the record is replaced wholesale.
    epoch: u64,
    /// Where Save writes. `None` after New Game, after a paste, and after restoring an
    /// autosave — all of which must go through Save As.
    file_path: Option<String>,
    /// Unique document-state token. Dirty is `saved != Some(doc)`, not undo depth.
    doc: u64,
    next_doc: u64,
    saved: Option<u64>,
    position_revision: u64,
    history: History,
}

impl GameSession {
    pub fn new(info: GameInfo) -> GameSession {
        let tree = GameTree::new(info);
        let cursor = tree.root();
        GameSession {
            tree,
            cursor,
            revision: 1,
            epoch: 0,
            file_path: None,
            doc: 0,
            next_doc: 1,
            saved: Some(0),
            position_revision: 0,
            history: History::new(),
        }
    }

    /// A fresh 19x19 Chinese game — what a frontend opens with.
    pub fn blank() -> GameSession {
        GameSession::new(GameInfo::new(Size::square(19), RuleSet::Chinese))
    }

    pub fn tree(&self) -> &GameTree {
        &self.tree
    }

    /// Mutable access for the cases a method here cannot express — game info edits, setup
    /// stones. Marks the record dirty, because the caller is about to change it.
    ///
    /// Drops undo/redo, assigns a new document token, bumps [`Self::position_revision`] and
    /// drops the position cache. Does not strip cached analysis from nodes.
    pub fn tree_mut(&mut self) -> &mut GameTree {
        self.history.clear();
        self.doc = self.next_doc;
        self.next_doc += 1;
        self.position_revision += 1;
        self.tree.invalidate_position();
        self.revision += 1;
        &mut self.tree
    }

    /// `&mut GameTree` for work that changes nothing the user would save: filling the
    /// replay cache, projecting a line. Neither dirties the record nor bumps the revision.
    pub fn tree_cached_mut(&mut self) -> &mut GameTree {
        &mut self.tree
    }

    pub fn cursor(&self) -> NodeId {
        self.cursor
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bumped only when the record is replaced wholesale, so a frontend can invalidate
    /// node ids it kept across the swap.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn modified(&self) -> bool {
        self.saved != Some(self.doc)
    }

    pub fn file_path(&self) -> Option<&str> {
        self.file_path.as_deref()
    }

    pub fn set_file_path(&mut self, path: Option<String>) {
        self.file_path = path;
        self.revision += 1;
    }

    /// Bumps the revision without claiming the record changed on disk.
    fn moved(&mut self) {
        self.revision += 1;
    }

    pub fn position_revision(&self) -> u64 {
        self.position_revision
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Clears history and, when `enabled` is false, stops recording. Dirty/checkpoint
    /// are left alone; re-enabling starts from the current document.
    pub fn set_edit_history_enabled(&mut self, enabled: bool) {
        self.history.set_enabled(enabled);
    }

    // -----------------------------------------------------------------------------------
    // Editing
    // -----------------------------------------------------------------------------------

    /// The derived board state at the cursor. Filling `GameTree`'s replay cache is not a
    /// record edit, so unlike [`GameSession::tree_mut`] this does not mark the game dirty.
    pub fn position(&mut self) -> &mirai_core::Position {
        self.tree.position(self.cursor)
    }

    pub fn to_play(&mut self) -> Color {
        self.tree.position(self.cursor).to_play
    }

    /// Plays at `p` for whoever is to move, reusing an existing child if it is the same
    /// move. An illegal move changes nothing — not even the revision. Reusing a child is
    /// navigation: no undo item, no dirty flag.
    pub fn play(&mut self, p: Point) -> Result<NodeId, IllegalMove> {
        let color = self.to_play();
        let at = self.cursor;
        if let Some(&existing) = self
            .tree
            .children(at)
            .iter()
            .find(|&&c| self.tree.node(c).mv == Some((color, p)))
        {
            self.cursor = existing;
            self.moved();
            return Ok(existing);
        }
        let id = self.tree.play(at, color, p)?;
        self.finish_new_branch(at, id);
        Ok(id)
    }

    pub fn pass(&mut self) -> Result<NodeId, IllegalMove> {
        self.play(Point::PASS)
    }

    /// Plays `p` as a new variation even when the same move already exists.
    pub fn play_variation(&mut self, p: Point) -> Result<NodeId, IllegalMove> {
        let color = self.to_play();
        let at = self.cursor;
        let id = self.tree.add_variation(at, color, p)?;
        self.finish_new_branch(at, id);
        Ok(id)
    }

    pub fn set_comment(&mut self, text: &str) {
        let cursor = self.cursor;
        let _ = self.set_comment_at(cursor, text);
    }

    pub fn set_comment_at(&mut self, id: NodeId, text: &str) -> bool {
        let Some(node) = self.tree.get(id) else {
            return false;
        };
        if node.comment == text {
            return false;
        }
        let mut old = text.to_string();
        self.tree.swap_comment(id, &mut old);
        self.commit(
            EditKind::Comment { id, text: old },
            self.cursor,
            self.cursor,
        );
        true
    }

    pub fn toggle_mark(&mut self, kind: MarkKind, p: Point) {
        let cursor = self.cursor;
        let _ = self.toggle_mark_at(cursor, kind, p);
    }

    fn toggle_mark_at(&mut self, id: NodeId, kind: MarkKind, p: Point) -> bool {
        if !self.tree.contains(id) || !self.tree.info.size.contains(p) {
            return false;
        }
        let mut marks = self.tree.node(id).marks.clone();
        marks.toggle(kind, p);
        self.commit_marks(id, marks)
    }

    pub fn set_label(&mut self, p: Point, text: &str) -> bool {
        if p.is_pass() || !self.tree.info.size.contains(p) {
            return false;
        }
        let id = self.cursor;
        let normalized = normalize_label(text);
        let mut marks = self.tree.node(id).marks.clone();
        marks.set_label(p, &normalized);
        self.commit_marks(id, marks)
    }

    pub fn clear_mark(&mut self, p: Point) -> bool {
        if p.is_pass() || !self.tree.info.size.contains(p) {
            return false;
        }
        let id = self.cursor;
        if !self.tree.node(id).marks.contains(p) {
            return false;
        }
        let mut marks = self.tree.node(id).marks.clone();
        marks.remove_at(p);
        self.commit_marks(id, marks)
    }

    pub fn set_setup_stone(&mut self, p: Point, color: Option<Color>) -> bool {
        if p.is_pass() || !self.tree.info.size.contains(p) {
            return false;
        }
        if self.position().board.at(p) == color {
            return false;
        }
        let at = self.cursor;
        if self.is_editable_setup_leaf(at) {
            self.apply_setup_in_place(at, Some((p, color)), None)
        } else {
            self.add_setup_child(Some((p, color)), None)
        }
    }

    pub fn set_to_play(&mut self, color: Color) -> bool {
        if self.to_play() == color {
            return false;
        }
        let at = self.cursor;
        if self.is_editable_setup_leaf(at) {
            self.apply_setup_in_place(at, None, Some(color))
        } else {
            self.add_setup_child(None, Some(color))
        }
    }

    /// Deletes the branch starting at the cursor and steps to its parent. The root is never
    /// deleted.
    pub fn delete_branch(&mut self) -> bool {
        let cursor = self.cursor;
        self.delete_branch_at(cursor)
    }

    pub fn delete_branch_at(&mut self, id: NodeId) -> bool {
        if id == self.tree.root() || !self.tree.contains(id) {
            return false;
        }
        let parent = self.tree.parent(id).expect("non-root has a parent");
        let before = self.cursor;
        let after = if self.tree.path_to(before).contains(&id) {
            parent
        } else {
            before
        };
        let detached = self.tree.detach_branch(id).expect("id was live");
        self.position_revision += 1;
        self.commit(
            EditKind::Branch {
                root: id,
                detached: Some(detached),
            },
            before,
            after,
        );
        true
    }

    pub fn promote_to_main_line(&mut self) {
        let cursor = self.cursor;
        let _ = self.promote_to_main_line_at(cursor);
    }

    pub fn promote_to_main_line_at(&mut self, id: NodeId) -> bool {
        if !self.tree.contains(id) {
            return false;
        }
        let path = self.tree.path_to(id);
        let mut changes = Vec::new();
        for w in path.windows(2) {
            let parent = w[0];
            let child = w[1];
            let Some(index) = self.tree.children(parent).iter().position(|&c| c == child) else {
                continue;
            };
            if index == 0 {
                continue;
            }
            self.tree.move_child(parent, child, 0);
            changes.push((parent, child, index));
        }
        if changes.is_empty() {
            return false;
        }
        self.commit(EditKind::Promote { changes }, self.cursor, self.cursor);
        true
    }

    pub fn set_result(&mut self, result: String) {
        if self.tree.info.result == result {
            return;
        }
        self.tree.info.result = result;
        self.doc = self.next_doc;
        self.next_doc += 1;
        // Result is not on the undo stack, so a prior save token must not become
        // clean again via undo/redo of unrelated edits.
        self.saved = None;
        self.revision += 1;
    }

    pub fn undo(&mut self) -> bool {
        let Some(mut edit) = self.history.pop_undo() else {
            return false;
        };
        if history::apply(&mut self.tree, &mut edit) {
            self.position_revision += 1;
        }
        self.cursor = edit.before_cursor;
        self.doc = edit.before_doc;
        self.revision += 1;
        self.history.push_redo(edit);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut edit) = self.history.pop_redo() else {
            return false;
        };
        if history::apply(&mut self.tree, &mut edit) {
            self.position_revision += 1;
        }
        self.cursor = edit.after_cursor;
        self.doc = edit.after_doc;
        self.revision += 1;
        self.history.push_undo(edit);
        true
    }

    // -----------------------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------------------

    pub fn go_to(&mut self, id: NodeId) {
        if self.tree.contains(id) && id != self.cursor {
            self.cursor = id;
            self.moved();
        }
    }

    pub fn go_first(&mut self) {
        self.go_to(self.tree.root());
    }

    pub fn go_last(&mut self) {
        let mut at = self.cursor;
        while let Some(&next) = self.tree.children(at).first() {
            at = next;
        }
        self.go_to(at);
    }

    pub fn go_back(&mut self, n: usize) {
        let mut at = self.cursor;
        for _ in 0..n {
            match self.tree.parent(at) {
                Some(p) => at = p,
                None => break,
            }
        }
        self.go_to(at);
    }

    pub fn go_forward(&mut self, n: usize) {
        let mut at = self.cursor;
        for _ in 0..n {
            match self.tree.children(at).first() {
                Some(&c) => at = c,
                None => break,
            }
        }
        self.go_to(at);
    }

    /// Moves to another child of the cursor's parent — the variation switcher.
    pub fn go_sibling(&mut self, delta: i32) {
        let Some(parent) = self.tree.parent(self.cursor) else {
            return;
        };
        let siblings = self.tree.children(parent);
        if siblings.len() < 2 {
            return;
        }
        let Some(at) = siblings.iter().position(|&s| s == self.cursor) else {
            return;
        };
        let len = siblings.len() as i32;
        let next = (at as i32 + delta).rem_euclid(len) as usize;
        let target = siblings[next];
        self.go_to(target);
    }

    // -----------------------------------------------------------------------------------
    // SGF
    // -----------------------------------------------------------------------------------

    /// Parses a whole SGF file. The caller picks which record to adopt.
    pub fn parse(bytes: &[u8]) -> Result<Vec<GameTree>, sgf::SgfError> {
        sgf::parse(bytes)
    }

    /// Replaces the record. `path` is the file Save will write to, if any.
    pub fn adopt(&mut self, tree: GameTree, path: Option<String>) {
        self.tree = tree;
        self.cursor = self.tree.root();
        self.file_path = path;
        self.history.clear();
        self.doc = self.next_doc;
        self.next_doc += 1;
        self.saved = Some(self.doc);
        self.position_revision = self.position_revision.saturating_add(1);
        self.revision += 1;
        self.epoch += 1;
    }

    /// Replaces the record from crash-recovery data.
    ///
    /// An autosave is not the user's file: it has no Save target and must remain dirty until
    /// the frontend completes Save As.
    pub fn restore(&mut self, tree: GameTree) {
        self.adopt(tree, None);
        self.saved = None;
    }

    pub fn to_sgf(&self, include_analysis: bool) -> String {
        sgf::write(&self.tree, include_analysis)
    }

    /// Records that the current tree was written to `path`.
    pub fn saved_to(&mut self, path: String) {
        self.file_path = Some(path);
        self.saved = Some(self.doc);
        self.revision += 1;
    }

    /// True when the record holds anything worth autosaving: a move, a setup stone or a
    /// comment. A blank tree must not leave a file behind for the crash-recovery prompt.
    pub fn has_content(&self) -> bool {
        self.tree.has_content()
    }

    // -----------------------------------------------------------------------------------
    // Analysis
    // -----------------------------------------------------------------------------------

    /// Writes an engine evaluation onto a node. Not a record edit: it bumps the revision so
    /// the UI redraws, but leaves the dirty flag alone — pondering must never make a saved
    /// file look unsaved.
    pub fn set_analysis(&mut self, id: NodeId, analysis: Option<NodeAnalysis>) {
        self.tree.set_analysis(id, analysis);
        self.moved();
    }

    /// Like [`Self::set_analysis`], but refuses a report for a stale position or a deleted
    /// node.
    pub fn set_analysis_at(
        &mut self,
        id: NodeId,
        expected_position_revision: u64,
        analysis: Option<NodeAnalysis>,
    ) -> bool {
        if self.position_revision != expected_position_revision || !self.tree.contains(id) {
            return false;
        }
        self.tree.set_analysis(id, analysis);
        self.moved();
        true
    }

    /// The request for one node of this record.
    pub fn request_for(&mut self, id: NodeId, want: Want, max_visits: u32) -> AnalyzeReq {
        analysis::request_for_node(&mut self.tree, id, want, max_visits)
    }

    /// The request for the node the user is looking at.
    pub fn request_for_cursor(&mut self, want: Want, max_visits: u32) -> AnalyzeReq {
        let cursor = self.cursor;
        self.request_for(cursor, want, max_visits)
    }

    fn finish_new_branch(&mut self, before: NodeId, id: NodeId) {
        self.position_revision += 1;
        self.commit(
            EditKind::Branch {
                root: id,
                detached: None,
            },
            before,
            id,
        );
    }

    fn commit(&mut self, kind: EditKind, before_cursor: NodeId, after_cursor: NodeId) {
        let before_doc = self.doc;
        self.doc = self.next_doc;
        self.next_doc += 1;
        self.revision += 1;
        self.cursor = after_cursor;
        self.history.record(Edit {
            before_cursor,
            after_cursor,
            before_doc,
            after_doc: self.doc,
            kind,
        });
    }

    fn commit_marks(&mut self, id: NodeId, mut marks: Marks) -> bool {
        if marks == self.tree.node(id).marks {
            return false;
        }
        self.tree.swap_marks(id, &mut marks);
        self.commit(EditKind::Marks { id, marks }, self.cursor, self.cursor);
        true
    }

    fn is_editable_setup_leaf(&self, id: NodeId) -> bool {
        let Some(node) = self.tree.get(id) else {
            return false;
        };
        node.children.is_empty()
            && node.mv.is_none()
            && node.move_number_override.is_none()
            && !has_move_annotation(node)
    }

    fn natural_to_play(&mut self, id: NodeId) -> Color {
        match self.tree.parent(id) {
            Some(parent) => self.tree.position(parent).to_play,
            None if self.tree.info.handicap >= 2 => Color::White,
            None => Color::Black,
        }
    }

    fn parent_stone(&mut self, id: NodeId, p: Point) -> Option<Color> {
        match self.tree.parent(id) {
            Some(parent) => self.tree.position(parent).board.at(p),
            None => None,
        }
    }

    fn apply_setup_in_place(
        &mut self,
        id: NodeId,
        stone: Option<(Point, Option<Color>)>,
        to_play: Option<Color>,
    ) -> bool {
        let mut setup = self.tree.node(id).setup.clone();
        let mut pl = self.tree.node(id).to_play_override;
        if let Some((p, color)) = stone {
            let parent_color = self.parent_stone(id, p);
            apply_setup_point(&mut setup, p, color, parent_color);
        }
        if let Some(color) = to_play {
            let natural = self.natural_to_play(id);
            pl = if color == natural { None } else { Some(color) };
        }
        if setup == self.tree.node(id).setup && pl == self.tree.node(id).to_play_override {
            return false;
        }
        self.tree.swap_setup(id, &mut setup, &mut pl);
        self.position_revision += 1;
        self.commit(
            EditKind::Setup {
                id,
                setup,
                to_play: pl,
            },
            self.cursor,
            self.cursor,
        );
        true
    }

    fn add_setup_child(
        &mut self,
        stone: Option<(Point, Option<Color>)>,
        to_play: Option<Color>,
    ) -> bool {
        let parent = self.cursor;
        let parent_stone = stone.map(|(p, _)| self.tree.position(parent).board.at(p));
        let natural = self.tree.position(parent).to_play;
        let id = self.tree.add_child(parent);
        let mut setup = Setup::default();
        let mut pl = None;
        if let Some((p, color)) = stone {
            apply_setup_point(&mut setup, p, color, parent_stone.flatten());
        }
        if let Some(color) = to_play {
            pl = if color == natural { None } else { Some(color) };
        }
        self.tree.swap_setup(id, &mut setup, &mut pl);
        self.position_revision += 1;
        self.commit(
            EditKind::Branch {
                root: id,
                detached: None,
            },
            parent,
            id,
        );
        true
    }
}

fn has_move_annotation(node: &Node) -> bool {
    node.unknown_props
        .iter()
        .any(|(k, _)| MOVE_PROPS.contains(&k.as_ref()))
}

fn apply_setup_point(setup: &mut Setup, p: Point, color: Option<Color>, parent: Option<Color>) {
    setup.add_black.retain(|&q| q != p);
    setup.add_white.retain(|&q| q != p);
    setup.add_empty.retain(|&q| q != p);
    if color != parent {
        match color {
            Some(Color::Black) => setup.add_black.push(p),
            Some(Color::White) => setup.add_white.push(p),
            None => setup.add_empty.push(p),
        }
    }
}

fn normalize_label(text: &str) -> String {
    text.trim()
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess() -> GameSession {
        GameSession::blank()
    }

    fn sess9() -> GameSession {
        GameSession::new(GameInfo::new(Size::square(9), RuleSet::Japanese))
    }

    fn p(size: Size, x: u8, y: u8) -> Point {
        size.point(x, y)
    }

    fn gtp(s: &GameSession, coord: &str) -> Point {
        s.tree().info.size.from_gtp(coord).expect(coord)
    }

    fn dummy_analysis(visits: u32) -> NodeAnalysis {
        NodeAnalysis {
            visits,
            winrate: 0.5,
            score_lead: 0.0,
            score_stdev: 0.0,
            candidates: Vec::new(),
            ownership: None,
        }
    }

    #[test]
    fn an_illegal_move_changes_nothing_at_all() {
        let mut s = sess();
        let size = s.tree().info.size;
        s.play(p(size, 3, 3)).unwrap();
        let before = s.revision();
        let cursor = s.cursor();

        assert_eq!(s.play(p(size, 3, 3)), Err(IllegalMove::Occupied));
        assert_eq!(s.revision(), before, "a rejected move bumped the revision");
        assert_eq!(s.cursor(), cursor);
        assert!(s.modified());
        assert!(s.can_undo());
        assert!(s.undo());
        assert!(!s.can_undo());
        assert_eq!(s.cursor(), s.tree().root());
    }

    #[test]
    fn playing_the_same_move_twice_reuses_the_child_but_a_variation_does_not() {
        let mut s = sess();
        let size = s.tree().info.size;
        let first = s.play(p(size, 3, 3)).unwrap();
        s.go_back(1);
        let again = s.play(p(size, 3, 3)).unwrap();
        assert_eq!(first, again, "an identical move must reuse its node");
        assert!(s.can_undo());
        assert!(!s.can_redo());
        assert!(s.undo());
        assert!(!s.can_undo());
        assert_eq!(s.cursor(), s.tree().root());
        assert!(s.tree().children(s.tree().root()).is_empty());
        assert!(s.redo());
        assert_eq!(s.cursor(), first);

        s.go_back(1);
        let branched = s.play_variation(p(size, 3, 3)).unwrap();
        assert_ne!(branched, first);
        assert_eq!(s.tree().children(s.tree().root()).len(), 2);
        assert!(s.undo());
        assert_eq!(s.tree().children(s.tree().root()), &[first]);
        assert!(s.can_undo());
    }

    #[test]
    fn navigation_walks_the_first_child_and_stops_at_the_ends() {
        let mut s = sess();
        let size = s.tree().info.size;
        for i in 0..5u8 {
            s.play(p(size, 3 + i, 3)).unwrap();
        }
        s.go_first();
        assert_eq!(s.cursor(), s.tree().root());
        s.go_back(10);
        assert_eq!(
            s.cursor(),
            s.tree().root(),
            "walking off the top is a no-op"
        );

        s.go_forward(2);
        assert_eq!(s.tree().move_number(s.cursor()), 2);
        s.go_last();
        assert_eq!(s.tree().move_number(s.cursor()), 5);
        s.go_forward(3);
        assert_eq!(s.tree().move_number(s.cursor()), 5, "walked off the end");
    }

    #[test]
    fn sibling_navigation_wraps_around_the_variations() {
        let mut s = sess();
        let size = s.tree().info.size;
        let root = s.tree().root();
        s.play(p(size, 3, 3)).unwrap();
        s.go_to(root);
        s.play_variation(p(size, 15, 15)).unwrap();
        s.go_to(root);
        s.play_variation(p(size, 3, 15)).unwrap();

        let third = s.cursor();
        s.go_sibling(1);
        assert_ne!(s.cursor(), third);
        s.go_sibling(-1);
        assert_eq!(
            s.cursor(),
            third,
            "stepping back must land where it started"
        );
        // Three siblings: three steps forward is the identity.
        s.go_sibling(1);
        s.go_sibling(1);
        s.go_sibling(1);
        assert_eq!(s.cursor(), third);
    }

    #[test]
    fn deleting_a_branch_steps_to_the_parent_and_never_removes_the_root() {
        let mut s = sess();
        let size = s.tree().info.size;
        s.play(p(size, 3, 3)).unwrap();
        let first = s.cursor();
        s.play(p(size, 15, 15)).unwrap();

        assert!(s.delete_branch());
        assert_eq!(s.cursor(), first);
        assert!(s.tree().children(first).is_empty());

        s.go_first();
        assert!(!s.delete_branch(), "the root must survive");
    }

    #[test]
    fn content_detection_ignores_a_blank_record() {
        let mut s = sess();
        assert!(!s.has_content(), "a fresh game must not be autosaved");
        s.set_comment("   ");
        assert!(!s.has_content(), "whitespace is not content");
        s.set_comment("joseki");
        assert!(s.has_content());
    }

    #[test]
    fn a_pass_is_a_move_and_hands_the_turn_over() {
        let mut s = sess();
        assert_eq!(s.to_play(), Color::Black);
        s.pass().unwrap();
        assert_eq!(s.to_play(), Color::White);

        let req = s.request_for_cursor(Want::empty(), 100);
        assert_eq!(req.moves, vec![(Color::Black, Point::PASS)]);
        assert_eq!(req.to_play(), Color::White);
    }

    #[test]
    fn adopting_a_record_clears_the_dirty_flag_and_the_path() {
        let sgf = b"(;GM[1]FF[4]SZ[19]KM[7.5];B[dd];W[pp])";
        let trees = GameSession::parse(sgf).expect("parse");
        assert_eq!(trees.len(), 1);

        let mut s = sess();
        s.set_comment("scratch");
        assert!(s.modified());
        s.adopt(trees.into_iter().next().unwrap(), Some("/tmp/a.sgf".into()));
        assert!(!s.modified());
        assert!(!s.can_undo());
        assert_eq!(s.file_path(), Some("/tmp/a.sgf"));
        assert_eq!(s.cursor(), s.tree().root());

        s.go_last();
        assert_eq!(s.tree().move_number(s.cursor()), 2);
    }

    #[test]
    fn restoring_an_autosave_requires_save_as() {
        let tree = GameSession::parse(b"(;SZ[19];B[dd])")
            .expect("parse")
            .remove(0);
        let mut s = sess();
        s.restore(tree);

        assert!(s.modified());
        assert!(!s.can_undo());
        assert_eq!(s.file_path(), None);
        assert_eq!(s.tree().move_number(s.cursor()), 0);
        s.go_last();
        assert_eq!(s.tree().move_number(s.cursor()), 1);
    }

    #[test]
    fn unknown_properties_and_marks_survive_a_round_trip() {
        let sgf = b"(;GM[1]FF[4]SZ[19]KM[6.5]RU[Japanese]ZZ[keep me];B[dd]TR[dd]C[hi];W[pp])";
        let tree = GameSession::parse(sgf).expect("parse").remove(0);
        let mut s = sess();
        s.adopt(tree, None);

        let text = s.to_sgf(false);
        assert!(text.contains("ZZ[keep me]"), "{text}");
        assert!(text.contains("TR[dd]"), "{text}");
        assert!(text.contains("C[hi]"), "{text}");

        // And it parses back to the same shape.
        let again = GameSession::parse(text.as_bytes())
            .expect("reparse")
            .remove(0);
        assert_eq!(again.info.komi, 6.5);
        assert_eq!(again.info.rules, RuleSet::Japanese);
    }

    /// Saving must clear the dirty flag but not the revision: the title bar still redraws.
    #[test]
    fn saving_clears_the_dirty_flag_and_records_the_path() {
        let mut s = sess();
        s.play(p(s.tree().info.size, 3, 3)).unwrap();
        let before = s.revision();

        s.saved_to("/tmp/b.sgf".into());

        assert!(!s.modified());
        assert_eq!(s.file_path(), Some("/tmp/b.sgf"));
        assert!(s.revision() > before, "the frontend must be told to redraw");
    }
    #[test]
    fn reading_the_cursor_position_is_not_an_edit() {
        let mut s = sess();
        let revision = s.revision();
        assert_eq!(s.position().board.stone_count(Color::Black), 0);
        assert!(!s.modified());
        assert_eq!(s.revision(), revision);
    }

    #[test]
    fn setup_stones_do_not_count_as_moves() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        let f6 = gtp(&s, "F6");
        assert!(s.set_setup_stone(d4, Some(Color::Black)));
        assert_eq!(s.tree().len(), 1, "empty root is edited in place");
        assert_eq!(s.cursor(), s.tree().root());
        assert!(s.set_setup_stone(f6, Some(Color::White)));
        assert_eq!(s.position().board.at(d4), Some(Color::Black));
        assert_eq!(s.position().board.at(f6), Some(Color::White));
        assert_eq!(s.position().to_play, Color::Black);
        assert_eq!(s.position().move_number, 0);
        assert_eq!(s.position().board.captures, [0, 0]);

        assert!(s.set_setup_stone(d4, Some(Color::White)));
        assert_eq!(s.position().board.at(d4), Some(Color::White));
        assert!(s.set_setup_stone(f6, None));
        assert_eq!(s.position().board.at(f6), None);

        let sgf = s.to_sgf(false);
        let mut round = GameSession::blank();
        round.adopt(GameSession::parse(sgf.as_bytes()).unwrap().remove(0), None);
        assert_eq!(round.position().board.at(d4), Some(Color::White));
        assert_eq!(round.position().board.at(f6), None);

        assert!(s.undo());
        assert_eq!(s.position().board.at(f6), Some(Color::White));
        assert!(s.undo());
        assert_eq!(s.position().board.at(d4), Some(Color::Black));
        assert!(s.undo());
        assert_eq!(s.position().board.at(f6), None);
        assert!(s.undo());
        assert_eq!(s.position().board.at(d4), None);
        assert!(!s.can_undo());
        assert!(s.redo());
        assert_eq!(s.position().board.at(d4), Some(Color::Black));
    }

    #[test]
    fn setup_appends_a_variation_and_keeps_existing_children() {
        let tree =
            GameSession::parse(b"(;FF[4]GM[1]SZ[9]KM[6.5];B[dd](;W[ee]C[main])(;W[ff]C[other]))")
                .unwrap()
                .remove(0);
        let mut s = sess9();
        s.adopt(tree, None);
        s.go_forward(1);
        let at = s.cursor();
        let original: Vec<_> = s.tree().children(at).to_vec();
        assert_eq!(original.len(), 2);
        let comments: Vec<_> = original
            .iter()
            .map(|&id| s.tree().node(id).comment.clone())
            .collect();
        assert_eq!(comments, vec!["main", "other"]);

        let aa = gtp(&s, "A9");
        assert!(s.set_setup_stone(aa, Some(Color::White)));
        let setup = s.cursor();
        assert_ne!(setup, at);
        assert_eq!(s.tree().parent(setup), Some(at));
        assert_eq!(s.tree().children(at)[..2], original[..]);
        assert_eq!(s.tree().children(at).len(), 3);
        assert_eq!(s.tree().node(original[0]).comment, "main");
        assert_eq!(s.tree().node(original[1]).comment, "other");
        assert!(s.tree().node(setup).mv.is_none());
        assert_eq!(s.position().board.at(aa), Some(Color::White));

        assert!(s.undo());
        assert_eq!(s.cursor(), at);
        assert!(!s.tree().contains(setup));
        assert_eq!(s.tree().children(at), original);
        assert!(s.redo());
        assert_eq!(s.cursor(), setup);
        assert!(s.tree().contains(setup));

        let bb = gtp(&s, "B9");
        assert!(s.set_setup_stone(bb, Some(Color::Black)));
        let cc = gtp(&s, "C9");
        assert!(s.set_setup_stone(cc, Some(Color::Black)));
        assert_eq!(s.cursor(), setup);
        assert_eq!(s.tree().len(), 5); // root, B, two W, one setup
        assert!(s.tree().node(at).setup.is_empty());
        assert!(s.undo());
        assert_eq!(s.cursor(), setup);
        assert_eq!(s.position().board.at(cc), None);
        assert!(s.undo());
        assert_eq!(s.cursor(), setup);
        assert_eq!(s.position().board.at(bb), None);
        assert!(s.undo());
        assert_eq!(s.cursor(), at);
        assert!(!s.tree().contains(setup));
        assert!(!s.can_undo());
    }

    #[test]
    fn set_to_play_writes_pl_on_its_own_node() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        let c3 = gtp(&s, "C3");
        s.play(d4).unwrap();
        assert!(s.set_to_play(Color::Black));
        assert_eq!(s.to_play(), Color::Black);
        assert!(s.tree().node(s.cursor()).mv.is_none());
        assert_eq!(
            s.tree().node(s.cursor()).to_play_override,
            Some(Color::Black)
        );
        s.play(c3).unwrap();
        assert_eq!(s.to_play(), Color::White);
        let text = s.to_sgf(false);
        assert!(text.contains("PL[B]"), "{text}");
        assert!(!text.contains("B[]"), "{text}");
        assert_eq!(s.tree().node(s.cursor()).mv, Some((Color::Black, c3)));
        assert!(s.undo());
        assert_eq!(s.to_play(), Color::Black);
        assert!(s.undo());
        assert_eq!(s.to_play(), Color::White);
        assert_eq!(s.tree().node(s.cursor()).mv, Some((Color::Black, d4)));
    }

    #[test]
    fn noops_do_not_dirty_or_record_history() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        let oob = Point(s.tree().info.size.points() as u16);
        assert!(!s.set_setup_stone(Point::PASS, Some(Color::Black)));
        assert!(!s.set_setup_stone(oob, Some(Color::Black)));
        assert!(!s.set_label(oob, "x"));
        s.toggle_mark(MarkKind::Triangle, oob);
        s.toggle_mark(MarkKind::Circle, Point::PASS);
        assert!(!s.set_setup_stone(d4, None), "erasing empty is a no-op");
        assert!(!s.set_to_play(Color::Black));
        assert!(!s.clear_mark(d4));
        assert!(!s.set_label(d4, ""));
        assert!(!s.modified());
        assert!(!s.can_undo());
        s.play(d4).unwrap();
        assert_eq!(s.play(d4), Err(IllegalMove::Occupied));
        assert!(s.can_undo());
        assert!(s.undo());
        assert!(!s.can_undo());
    }

    #[test]
    fn save_checkpoint_follows_the_document_token() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        let f6 = gtp(&s, "F6");
        s.play(d4).unwrap();
        s.saved_to("/tmp/c.sgf".into());
        assert!(!s.modified());
        s.play(f6).unwrap();
        assert!(s.modified());
        s.undo();
        assert!(!s.modified(), "undo back to the save point is clean");
        s.redo();
        assert!(s.modified());
        s.undo();
        s.set_setup_stone(gtp(&s, "A9"), Some(Color::White));
        assert!(s.modified());
        assert!(!s.can_redo());
        s.saved_to("/tmp/c.sgf".into());
        s.go_first();
        assert!(!s.modified(), "navigation is not an edit");
        assert!(!s.can_redo());
        s.set_analysis(s.cursor(), Some(dummy_analysis(3)));
        assert!(!s.modified());
        assert!(!s.can_redo());
    }

    #[test]
    fn marks_and_comments_do_not_change_the_position() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        s.play(d4).unwrap();
        s.set_analysis(s.cursor(), Some(dummy_analysis(11)));
        let pos_rev = s.position_revision();
        let to_play = s.to_play();
        assert!(s.set_label(d4, "死活:A\\]"));
        assert!(s.toggle_mark_at(s.cursor(), MarkKind::Triangle, gtp(&s, "F6")));
        s.set_comment("note");
        assert_eq!(s.position_revision(), pos_rev);
        assert_eq!(s.to_play(), to_play);
        assert!(s.tree().node(s.cursor()).analysis.is_some());
        assert_eq!(s.tree().node(s.cursor()).marks.labels[0].1, "死活:A\\]");
        let round = GameSession::parse(s.to_sgf(false).as_bytes())
            .unwrap()
            .remove(0);
        let saved_move = round.children(round.root())[0];
        assert_eq!(
            round.node(saved_move).marks.labels,
            vec![(d4, "死活:A\\]".into())]
        );
        s.undo();
        s.undo();
        s.undo();
        assert_eq!(s.position().board.at(d4), Some(Color::Black));
        assert!(s.tree().node(s.cursor()).analysis.is_some());
        assert!(s.tree().node(s.cursor()).marks.is_empty());
        assert_eq!(s.position_revision(), pos_rev);
    }

    #[test]
    fn label_empty_and_whitespace_remove() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        assert!(s.set_label(d4, "  a\tb\n "));
        assert_eq!(s.tree().node(s.cursor()).marks.labels[0].1, "a b");
        assert!(s.set_label(d4, "   "));
        assert!(s.tree().node(s.cursor()).marks.is_empty());
    }

    #[test]
    fn promote_and_delete_are_reversible() {
        let mut s = sess9();
        let root = s.tree().root();
        let a = s.play(gtp(&s, "D4")).unwrap();
        s.go_to(root);
        let b = s.play_variation(gtp(&s, "F6")).unwrap();
        assert_eq!(s.tree().children(root), &[a, b]);
        assert!(s.promote_to_main_line_at(b));
        assert_eq!(s.tree().children(root), &[b, a]);
        s.undo();
        assert_eq!(s.tree().children(root), &[a, b]);
        s.redo();
        assert_eq!(s.tree().children(root), &[b, a]);

        let keep = s.cursor();
        assert!(s.delete_branch_at(a));
        assert_eq!(s.cursor(), keep);
        assert!(!s.tree().contains(a));
        s.undo();
        assert!(s.tree().contains(a));
        assert_eq!(s.tree().children(root), &[b, a]);
    }

    #[test]
    fn set_analysis_at_rejects_a_stale_revision() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        s.set_setup_stone(d4, Some(Color::Black));
        let id = s.cursor();
        let rev = s.position_revision();
        s.set_analysis(id, Some(dummy_analysis(50)));
        s.undo();
        assert_eq!(s.cursor(), id);
        assert!(s.position_revision() != rev);
        assert!(!s.set_analysis_at(id, rev, Some(dummy_analysis(5))));
        assert!(s.tree().node(id).analysis.is_none());
    }

    #[test]
    fn tree_mut_invalidates_position_cache() {
        let mut s = sess();
        let root = s.tree().root();
        let _ = s.position();
        s.tree_mut().info.handicap = 2;
        assert_eq!(s.tree_cached_mut().position(root).to_play, Color::White);
        assert!(!s.can_undo());
        assert!(s.modified());
    }

    #[test]
    fn history_can_be_disabled_without_touching_dirty() {
        let mut s = sess9();
        s.play(gtp(&s, "D4")).unwrap();
        assert!(s.modified());
        s.set_edit_history_enabled(false);
        assert!(s.modified());
        assert!(!s.can_undo());
        s.play(gtp(&s, "F6")).unwrap();
        assert!(!s.can_undo());
        s.set_edit_history_enabled(true);
        s.play(gtp(&s, "C3")).unwrap();
        assert!(s.can_undo());
        s.undo();
        assert_eq!(s.tree().move_number(s.cursor()), 2);
    }

    #[test]
    fn setup_and_undo_drop_cached_analysis() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        let f6 = gtp(&s, "F6");
        s.play(d4).unwrap();
        let sibling = {
            let root = s.tree().root();
            s.go_to(root);
            s.play_variation(f6).unwrap()
        };
        s.set_analysis(sibling, Some(dummy_analysis(9)));
        s.go_to(s.tree().children(s.tree().root())[0]);
        s.set_analysis(s.cursor(), Some(dummy_analysis(8)));
        assert!(s.set_setup_stone(gtp(&s, "A9"), Some(Color::White)));
        let setup = s.cursor();
        let parent = s.tree().parent(setup).unwrap();
        assert!(s.tree().node(parent).analysis.is_some());
        // The new setup node has no analysis; the sibling of the move still does.
        assert!(s.tree().node(sibling).analysis.is_some());
        s.go_to(parent);
        let rev = s.position_revision();
        s.set_to_play(Color::Black);
        assert!(s.position_revision() > rev);
        assert!(s.tree().node(s.cursor()).analysis.is_none());
        assert!(s.tree().node(sibling).analysis.is_some());
    }

    #[test]
    fn set_result_dirties_without_touching_history_or_position() {
        let mut s = sess9();
        let d4 = gtp(&s, "D4");
        let f6 = gtp(&s, "F6");
        s.play(d4).unwrap();
        s.play(f6).unwrap();
        s.undo();
        s.saved_to("/tmp/r.sgf".into());
        assert!(!s.modified());
        assert!(s.can_redo());
        let pos = s.position_revision();
        s.set_result("B+R".into());
        assert!(s.modified());
        assert_eq!(s.position_revision(), pos);
        assert!(s.can_redo(), "set_result must not clear redo");
        assert_eq!(s.tree().info.result, "B+R");
        s.set_result("B+R".into());
        s.redo();
        assert!(s.modified(), "redo onto the old save token stays dirty");
        assert_eq!(s.tree().info.result, "B+R");
        s.undo();
        s.undo();
        assert!(s.modified());
        assert_eq!(s.tree().info.result, "B+R");
        assert!(!s.can_undo());
    }

    #[test]
    fn adopt_bumps_position_revision_monotonically() {
        let mut s = sess9();
        let old = s.position_revision();
        assert_eq!(old, 0);
        assert!(s.set_analysis_at(s.tree().root(), old, Some(dummy_analysis(4))));
        let tree = GameSession::parse(b"(;FF[4]GM[1]SZ[9])").unwrap().remove(0);
        s.adopt(tree, Some("/tmp/n.sgf".into()));
        assert!(s.position_revision() > old);
        assert!(!s.can_undo());
        assert!(!s.modified());
        assert!(!s.set_analysis_at(s.tree().root(), old, Some(dummy_analysis(9))));
        assert!(s.tree().node(s.tree().root()).analysis.is_none());
    }

    #[test]
    fn deleting_an_ancestor_moves_the_cursor() {
        let mut s = sess9();
        let root = s.tree().root();
        let first = s.play(gtp(&s, "D4")).unwrap();
        let second = s.play(gtp(&s, "F6")).unwrap();
        assert!(s.delete_branch_at(first));
        assert_eq!(s.cursor(), root);
        assert!(!s.tree().contains(first));
        assert!(!s.tree().contains(second));
        s.undo();
        assert_eq!(s.cursor(), second);
        assert!(s.tree().contains(first));
        assert!(s.tree().contains(second));
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let mut s = sess9();
        let gone = s.play(gtp(&s, "D4")).unwrap();
        s.go_first();
        assert!(s.delete_branch_at(gone));
        assert!(!s.set_comment_at(gone, "x"));
        assert!(!s.promote_to_main_line_at(gone));
        assert!(!s.delete_branch_at(gone));
        assert!(!s.set_analysis_at(gone, s.position_revision(), Some(dummy_analysis(1))));
        assert!(!s.set_comment_at(NodeId(u32::MAX), "x"));
    }

    #[test]
    fn move_annotations_force_a_new_setup_child() {
        let tree = GameSession::parse(b"(;FF[4]GM[1]SZ[9]KO[])")
            .unwrap()
            .remove(0);
        let mut s = sess9();
        s.adopt(tree, None);
        let root = s.tree().root();
        assert!(s.set_setup_stone(gtp(&s, "D4"), Some(Color::Black)));
        assert_ne!(s.cursor(), root);
        assert!(s.tree().node(root).setup.is_empty());
        assert_eq!(s.tree().node(s.cursor()).setup.add_black.len(), 1);
    }

    #[test]
    fn history_restores_payloads_and_sibling_analysis() {
        let sgf = "(;FF[4]GM[1]SZ[9]ZZ[keep]C[死活];B[dd]TR[dd](;W[ee]C[main])(;W[ff]C[other]))";
        let tree = GameSession::parse(sgf.as_bytes()).unwrap().remove(0);
        let mut s = sess9();
        s.adopt(tree, None);
        s.go_forward(1);
        let at = s.cursor();
        let kids = s.tree().children(at).to_vec();
        assert_eq!(kids.len(), 2);
        s.set_analysis(kids[0], Some(dummy_analysis(7)));
        s.set_analysis(kids[1], Some(dummy_analysis(8)));
        let other = kids[1];
        assert!(s.delete_branch_at(other));
        assert!(!s.tree().contains(other));
        assert!(s.tree().node(kids[0]).analysis.is_some());
        assert_eq!(s.tree().node(s.tree().root()).comment, "死活");
        assert!(s.undo());
        assert!(s.tree().contains(other));
        assert_eq!(other, s.tree().children(at)[1]);
        assert_eq!(s.tree().node(other).comment, "other");
        assert!(s.tree().node(other).analysis.is_some());
        assert_eq!(s.tree().node(at).marks.triangle.len(), 1);
        let text = s.to_sgf(false);
        assert!(text.contains("ZZ[keep]"), "{text}");
        assert!(text.contains("死活"), "{text}");
        s.redo();
        assert!(!s.tree().contains(other));
        assert!(s.tree().node(kids[0]).analysis.is_some());
    }

    #[test]
    fn promote_along_a_deep_path_is_reversible() {
        let mut s = sess9();
        let root = s.tree().root();
        let a = s.play(gtp(&s, "D4")).unwrap();
        s.go_to(root);
        let b = s.play_variation(gtp(&s, "F6")).unwrap();
        let b1 = s.play(gtp(&s, "C3")).unwrap();
        s.go_to(b);
        let b2 = s.play_variation(gtp(&s, "E5")).unwrap();
        assert_eq!(s.tree().children(root), &[a, b]);
        assert_eq!(s.tree().children(b), &[b1, b2]);
        let pos = s.position_revision();
        assert!(s.promote_to_main_line_at(b2));
        assert_eq!(s.tree().children(root), &[b, a]);
        assert_eq!(s.tree().children(b), &[b2, b1]);
        assert_eq!(s.position_revision(), pos);
        s.undo();
        assert_eq!(s.tree().children(root), &[a, b]);
        assert_eq!(s.tree().children(b), &[b1, b2]);
        assert_eq!(s.position_revision(), pos);
        s.redo();
        assert_eq!(s.tree().children(root), &[b, a]);
        assert_eq!(s.tree().children(b), &[b2, b1]);
    }

    #[test]
    fn first_setup_child_undo_removes_the_node() {
        let mut s = sess9();
        s.play(gtp(&s, "D4")).unwrap();
        let parent = s.cursor();
        assert!(s.set_setup_stone(gtp(&s, "A9"), Some(Color::White)));
        let setup = s.cursor();
        assert_ne!(setup, parent);
        assert!(s.undo());
        assert_eq!(s.cursor(), parent);
        assert!(!s.tree().contains(setup));
        assert!(s.redo());
        assert_eq!(s.cursor(), setup);
    }
}
