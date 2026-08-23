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

use mirai_core::{
    Color, GameInfo, GameTree, IllegalMove, MarkKind, NodeId, Point, RuleSet, Size, sgf,
};
use mirai_engine::{AnalyzeReq, Want};

use crate::analysis;

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
    modified: bool,
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
            modified: false,
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
    pub fn tree_mut(&mut self) -> &mut GameTree {
        self.touch();
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
        self.modified
    }

    pub fn file_path(&self) -> Option<&str> {
        self.file_path.as_deref()
    }

    pub fn set_file_path(&mut self, path: Option<String>) {
        self.file_path = path;
        self.revision += 1;
    }

    /// Marks the record dirty and bumps the revision.
    fn touch(&mut self) {
        self.modified = true;
        self.revision += 1;
    }

    /// Bumps the revision without claiming the record changed on disk.
    fn moved(&mut self) {
        self.revision += 1;
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
    /// move. An illegal move changes nothing — not even the revision.
    pub fn play(&mut self, p: Point) -> Result<NodeId, IllegalMove> {
        let color = self.to_play();
        let id = self.tree.play(self.cursor, color, p)?;
        self.cursor = id;
        self.touch();
        Ok(id)
    }

    pub fn pass(&mut self) -> Result<NodeId, IllegalMove> {
        self.play(Point::PASS)
    }

    /// Plays `p` as a new variation even when the same move already exists.
    pub fn play_variation(&mut self, p: Point) -> Result<NodeId, IllegalMove> {
        let color = self.to_play();
        let id = self.tree.add_variation(self.cursor, color, p)?;
        self.cursor = id;
        self.touch();
        Ok(id)
    }

    pub fn set_comment(&mut self, text: &str) {
        if self.tree.node(self.cursor).comment == text {
            return;
        }
        let cursor = self.cursor;
        self.tree.set_comment(cursor, text);
        self.touch();
    }

    pub fn toggle_mark(&mut self, kind: MarkKind, p: Point) {
        let cursor = self.cursor;
        self.tree.toggle_mark(cursor, kind, p);
        self.touch();
    }

    /// Deletes the branch starting at the cursor and steps to its parent. The root is never
    /// deleted.
    pub fn delete_branch(&mut self) -> bool {
        let Some(parent) = self.tree.parent(self.cursor) else {
            return false;
        };
        let doomed = self.cursor;
        self.cursor = parent;
        self.tree.delete_branch(doomed);
        self.touch();
        true
    }

    pub fn promote_to_main_line(&mut self) {
        let cursor = self.cursor;
        self.tree.promote_to_main_line(cursor);
        self.touch();
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
        self.modified = false;
        self.revision += 1;
        self.epoch += 1;
    }

    /// Replaces the record from crash-recovery data.
    ///
    /// An autosave is not the user's file: it has no Save target and must remain dirty until
    /// the frontend completes Save As.
    pub fn restore(&mut self, tree: GameTree) {
        self.adopt(tree, None);
        self.modified = true;
    }

    pub fn to_sgf(&self, include_analysis: bool) -> String {
        sgf::write(&self.tree, include_analysis)
    }

    /// Records that the current tree was written to `path`.
    pub fn saved_to(&mut self, path: String) {
        self.file_path = Some(path);
        self.modified = false;
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
    pub fn set_analysis(&mut self, id: NodeId, analysis: Option<mirai_core::NodeAnalysis>) {
        self.tree.set_analysis(id, analysis);
        self.moved();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess() -> GameSession {
        GameSession::blank()
    }

    fn p(size: Size, x: u8, y: u8) -> Point {
        size.point(x, y)
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
    }

    #[test]
    fn playing_the_same_move_twice_reuses_the_child_but_a_variation_does_not() {
        let mut s = sess();
        let size = s.tree().info.size;
        let first = s.play(p(size, 3, 3)).unwrap();
        s.go_back(1);
        let again = s.play(p(size, 3, 3)).unwrap();
        assert_eq!(first, again, "an identical move must reuse its node");

        s.go_back(1);
        let branched = s.play_variation(p(size, 3, 3)).unwrap();
        assert_ne!(branched, first);
        assert_eq!(s.tree().children(s.tree().root()).len(), 2);
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
}
