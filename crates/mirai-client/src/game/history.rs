// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Incremental document history for [`super::GameSession`].
//!
//! One GUI action is one [`Edit`]. Undo/redo swap the recorded payload with the
//! tree; they never snapshot the whole record and never store analysis reports.

use mirai_core::{Color, DetachedBranch, GameTree, Marks, NodeId, Setup};

pub(super) struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    enabled: bool,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            enabled: true,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Clears stacks. When `enabled` is false, further [`Self::record`] calls
    /// are ignored. Dirty/checkpoint live on the session.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.clear();
        self.enabled = enabled;
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn record(&mut self, edit: Edit) {
        if self.enabled {
            self.redo.clear();
            self.undo.push(edit);
        }
    }

    pub fn pop_undo(&mut self) -> Option<Edit> {
        self.undo.pop()
    }

    pub fn pop_redo(&mut self) -> Option<Edit> {
        self.redo.pop()
    }

    pub fn push_undo(&mut self, edit: Edit) {
        self.undo.push(edit);
    }

    pub fn push_redo(&mut self, edit: Edit) {
        self.redo.push(edit);
    }
}

pub(super) struct Edit {
    pub before_cursor: NodeId,
    pub after_cursor: NodeId,
    pub before_doc: u64,
    pub after_doc: u64,
    pub kind: EditKind,
}

pub(super) enum EditKind {
    /// A child that is either in the tree (`detached == None`) or sitting in
    /// this edit (`Some`). Used for new moves, new setup nodes, and deleting a
    /// variation.
    Branch {
        root: NodeId,
        detached: Option<DetachedBranch>,
    },
    Setup {
        id: NodeId,
        setup: Setup,
        to_play: Option<Color>,
    },
    Marks {
        id: NodeId,
        marks: Marks,
    },
    Comment {
        id: NodeId,
        text: String,
    },
    /// `(parent, child, index)` for each child that actually moved. Apply swaps
    /// the stored index with the child's current index.
    Promote {
        changes: Vec<(NodeId, NodeId, usize)>,
    },
}

/// Applies `edit` in either direction. Returns whether stones, PL, or attached
/// nodes changed, so the session can bump `position_revision`.
pub(super) fn apply(tree: &mut GameTree, edit: &mut Edit) -> bool {
    match &mut edit.kind {
        EditKind::Branch { root, detached } => {
            if let Some(branch) = detached.take() {
                tree.restore_branch(branch);
            } else {
                *detached = Some(
                    tree.detach_branch(*root)
                        .expect("history branch is attached"),
                );
            }
            tree.invalidate_position();
            true
        }
        EditKind::Setup { id, setup, to_play } => {
            tree.swap_setup(*id, setup, to_play);
            true
        }
        EditKind::Marks { id, marks } => {
            tree.swap_marks(*id, marks);
            false
        }
        EditKind::Comment { id, text } => {
            tree.swap_comment(*id, text);
            false
        }
        EditKind::Promote { changes } => {
            for (parent, child, index) in changes {
                let current = tree
                    .children(*parent)
                    .iter()
                    .position(|&c| c == *child)
                    .expect("promoted child still present");
                let dest = *index;
                *index = current;
                tree.move_child(*parent, *child, dest);
            }
            false
        }
    }
}
