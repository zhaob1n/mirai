// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The branch graph: every node of a [`GameTree`] placed on a grid of depth by lane.
//!
//! The depth is the node's distance from the root; the lane is handed out by a depth-first
//! walk that keeps the main line on lane 0. Which axis each runs along is the frontend's
//! choice — the placement is the same for every frontend, so a record's variations sit in
//! the same lanes wherever it is opened.
//!
//! A frontend caches the graph on [`GameTree::structure_revision`] (and its own record
//! epoch, since node ids are arena-local), not on [`GameTree::revision`]: storing an
//! analysis is a revision too, and a running engine would rebuild the graph ten times a
//! second.

use std::collections::HashMap;

use crate::point::{Color, Point};
use crate::tree::{GameTree, NodeId};

/// A node placed on the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub id: NodeId,
    /// Depth from the root; the root is 0.
    pub depth: u32,
    /// Branch lane; the main line is lane 0 and later variations take higher lanes.
    pub lane: u32,
    /// Index into [`BranchGraph::nodes`] of this node's parent.
    pub parent: Option<usize>,
    pub mv: Option<(Color, Point)>,
}

/// The whole tree, placed.
///
/// `nodes` is in depth-first pre-order with `children[0]` first, so the main line is
/// `nodes[0..=main line length]` in order, every parent comes before its children, and a
/// node's first child, if it has one, is the next entry.
#[derive(Clone, Debug, Default)]
pub struct BranchGraph {
    pub nodes: Vec<Placed>,
    pub index: HashMap<NodeId, usize>,
    /// Depth of the deepest node.
    pub depth: u32,
    /// Highest lane in use.
    pub lanes: u32,
}

impl GameTree {
    /// Places every node on the grid.
    ///
    /// The walk is depth-first in child order, and `children[0]` is the main line, so lane 0
    /// is claimed by the main line before any variation can ask for it. A branch takes the
    /// lowest lane at or after its parent's that is still free from its depth onwards, which
    /// lets a short variation share a lane with a later, disjoint one. Iterative, so a long
    /// line costs no stack.
    pub fn branch_graph(&self) -> BranchGraph {
        let mut graph = BranchGraph {
            nodes: Vec::with_capacity(self.len()),
            index: HashMap::with_capacity(self.len()),
            depth: 0,
            lanes: 0,
        };
        // `free[l]` is the first depth in lane `l` that nothing occupies yet.
        let mut free: Vec<u32> = Vec::new();
        // (node, depth, parent lane, parent slot)
        let mut stack: Vec<(NodeId, u32, u32, Option<usize>)> = vec![(self.root(), 0, 0, None)];

        while let Some((id, depth, parent_lane, parent)) = stack.pop() {
            let mut lane = parent_lane;
            while (lane as usize) < free.len() && free[lane as usize] > depth {
                lane += 1;
            }
            while free.len() <= lane as usize {
                free.push(0);
            }
            free[lane as usize] = depth + 1;

            let slot = graph.nodes.len();
            graph.nodes.push(Placed {
                id,
                depth,
                lane,
                parent,
                mv: self.node(id).mv,
            });
            graph.index.insert(id, slot);
            graph.depth = graph.depth.max(depth);
            graph.lanes = graph.lanes.max(lane);

            // Reversed, so `children[0]` is popped first and keeps the parent's lane.
            for &child in self.children(id).iter().rev() {
                stack.push((child, depth + 1, lane, Some(slot)));
            }
        }
        graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GameInfo, RuleSet, Size};

    fn lane(graph: &BranchGraph, id: NodeId) -> Option<u32> {
        graph.index.get(&id).map(|&i| graph.nodes[i].lane)
    }

    fn tree19() -> (GameTree, Size) {
        let size = Size::square(19);
        (GameTree::new(GameInfo::new(size, RuleSet::default())), size)
    }

    /// Main line D4-D16-Q16-Q4-C10, a variation off move 3, and a variation off that.
    #[test]
    fn lanes_keep_the_main_line_on_zero() {
        let (mut tree, size) = tree19();
        let p = |x: u8, y: u8| size.point(x, y);
        let root = tree.root();

        let m1 = tree.play(root, Color::Black, p(3, 15)).unwrap();
        let m2 = tree.play(m1, Color::White, p(3, 3)).unwrap();
        let m3 = tree.play(m2, Color::Black, p(15, 3)).unwrap();
        let m4 = tree.play(m3, Color::White, p(15, 15)).unwrap();
        let m5 = tree.play(m4, Color::Black, p(9, 9)).unwrap();

        // Variation off move 3: a second child of m3.
        let v1 = tree.add_variation(m3, Color::White, p(9, 3)).unwrap();
        let v2 = tree.play(v1, Color::Black, p(9, 15)).unwrap();
        // Variation off the variation: a second child of v1.
        let w1 = tree.add_variation(v1, Color::Black, p(2, 9)).unwrap();

        let graph = tree.branch_graph();
        assert_eq!(graph.nodes.len(), 9);
        for id in [root, m1, m2, m3, m4, m5] {
            assert_eq!(lane(&graph, id), Some(0), "main line node {id:?}");
        }
        assert_eq!(lane(&graph, v1), Some(1));
        assert_eq!(lane(&graph, v2), Some(1));
        assert_eq!(lane(&graph, w1), Some(2));
        assert_eq!(graph.lanes, 2);
        assert_eq!(graph.depth, 5);
    }

    /// What a frontend walks without a lookup: the main line first and in order, and a
    /// parent's first child right after it.
    #[test]
    fn pre_order_puts_the_main_line_first_and_a_first_child_next() {
        let (mut tree, size) = tree19();
        let p = |x: u8, y: u8| size.point(x, y);
        let root = tree.root();
        let a = tree.play(root, Color::Black, p(3, 3)).unwrap();
        let side = tree.add_variation(root, Color::Black, p(15, 15)).unwrap();
        let side_next = tree.play(side, Color::White, p(3, 15)).unwrap();
        let b = tree.play(a, Color::White, p(15, 3)).unwrap();

        let graph = tree.branch_graph();
        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![root, a, b, side, side_next]);
        for (slot, node) in graph.nodes.iter().enumerate() {
            if let Some(&first) = tree.children(node.id).first() {
                assert_eq!(graph.nodes[slot + 1].id, first);
                assert_eq!(graph.nodes[slot + 1].parent, Some(slot));
            }
        }
    }

    #[test]
    fn depths_follow_the_tree_and_parents_are_linked() {
        let (mut tree, size) = tree19();
        let root = tree.root();
        let a = tree.play(root, Color::Black, size.point(3, 3)).unwrap();
        let b = tree.play(a, Color::White, size.point(15, 15)).unwrap();

        let graph = tree.branch_graph();
        let slot = |id: NodeId| graph.nodes[graph.index[&id]];
        assert_eq!(slot(root).depth, 0);
        assert_eq!(slot(a).depth, 1);
        assert_eq!(slot(b).depth, 2);
        assert_eq!(slot(root).parent, None);
        assert_eq!(graph.nodes[slot(b).parent.unwrap()].id, a);
        assert_eq!(slot(b).mv, Some((Color::White, size.point(15, 15))));
    }

    /// Siblings of the same parent never collide, and a variation never takes a lane before
    /// the lane of the branch it hangs off.
    #[test]
    fn sibling_variations_get_distinct_lanes() {
        let (mut tree, size) = tree19();
        let p = |x: u8, y: u8| size.point(x, y);
        let root = tree.root();
        let main = tree.play(root, Color::Black, p(3, 3)).unwrap();
        let a = tree.add_variation(root, Color::Black, p(15, 15)).unwrap();
        let b = tree.add_variation(root, Color::Black, p(15, 3)).unwrap();

        let graph = tree.branch_graph();
        assert_eq!(lane(&graph, root), Some(0));
        assert_eq!(lane(&graph, main), Some(0));
        assert_eq!(lane(&graph, a), Some(1));
        assert_eq!(lane(&graph, b), Some(2));
    }

    /// A lane still taken at a branch's depth is skipped even when no sibling sits in it.
    /// The walk finishes the main line, deepest branch first, before it reaches the root's
    /// second child, so that sibling finds lane 1 already taken.
    #[test]
    fn a_lane_taken_at_the_branch_depth_is_skipped() {
        let (mut tree, size) = tree19();
        let p = |x: u8, y: u8| size.point(x, y);
        let root = tree.root();
        let main = tree.play(root, Color::Black, p(3, 3)).unwrap();
        tree.play(main, Color::White, p(4, 4)).unwrap();
        let variation = tree.add_variation(main, Color::White, p(5, 5)).unwrap();
        let sibling = tree.add_variation(root, Color::Black, p(15, 15)).unwrap();

        let graph = tree.branch_graph();
        assert_eq!(lane(&graph, main), Some(0));
        assert_eq!(lane(&graph, variation), Some(1));
        assert_eq!(lane(&graph, sibling), Some(2), "the taken lane stays empty");
    }

    /// The walk is iterative, so a long game must not blow the stack.
    #[test]
    fn deep_lines_do_not_recurse() {
        let (mut tree, _) = tree19();
        let mut cur = tree.root();
        // A long ladder of passes: legal under every ruleset and cheap to build.
        for i in 0..2000u32 {
            let color = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            cur = tree.add_variation(cur, color, Point::PASS).unwrap();
        }
        let graph = tree.branch_graph();
        assert_eq!(graph.nodes.len(), 2001);
        assert_eq!(graph.depth, 2000);
        assert_eq!(graph.lanes, 0);
        assert_eq!(lane(&graph, cur), Some(0));
    }

    /// A deleted branch leaves a tombstone in the arena; the graph holds live nodes only.
    #[test]
    fn a_deleted_branch_is_not_placed() {
        let (mut tree, size) = tree19();
        let root = tree.root();
        tree.play(root, Color::Black, size.point(3, 3)).unwrap();
        let gone = tree
            .add_variation(root, Color::Black, size.point(15, 15))
            .unwrap();
        tree.play(gone, Color::White, size.point(3, 15)).unwrap();
        tree.delete_branch(gone);

        let graph = tree.branch_graph();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.lanes, 0);
        assert!(!graph.index.contains_key(&gone));
    }
}
