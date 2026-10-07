// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The branch graph: every node of a [`GameTree`] placed on a grid of depth by lane.
//!
//! The depth is the node's distance from the root. The lane belongs to a line — a node and
//! its first children for as long as they go — so a line is straight; the main line is lane
//! 0, and each variation takes the first lane after its parent's where its moves fit. Which
//! axis each runs along is the frontend's choice — the placement is the same for every
//! frontend, so a record's variations sit in the same lanes wherever it is opened.
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

/// A stretch of one lane that is taken: a line of moves, or one cell of the trunk a parent's
/// branches drop down, at the parent's own depth, to the lane each branch runs in.
#[derive(Clone, Copy)]
struct Taken {
    from: u32,
    to: u32,
    /// The parent whose trunk this is; `None` for moves. Siblings share their parent's trunk.
    trunk_of: Option<NodeId>,
}

/// Whether a line from `from` to `to` fits in `lane`, its elbow at `from - 1` included.
fn fits(lane: &[Taken], from: u32, to: u32, parent: NodeId) -> bool {
    lane.iter().all(|t| {
        let on_line = t.from <= to && from <= t.to;
        let on_elbow = t.from < from && from - 1 <= t.to;
        !on_line && (!on_elbow || t.trunk_of == Some(parent))
    })
}

/// Whether no move in the lanes past a candidate stands at any of `forks`: the depths where
/// the candidate line branches again, whose trunks will cross those lanes on the way down.
/// The lanes not taken yet are clear, so a line always finds one.
fn clear_below(lanes: &[Vec<Taken>], forks: &[u32]) -> bool {
    forks.iter().all(|&fork| {
        lanes
            .iter()
            .flatten()
            .all(|t| t.trunk_of.is_some() || fork < t.from || t.to < fork)
    })
}

impl GameTree {
    /// Places every node on the grid.
    ///
    /// The walk is depth-first in child order, and `children[0]` is the main line, so lane 0
    /// is claimed by the main line before any variation can ask for it. A variation takes the
    /// first lane after its parent's in which its whole line, and the elbow its parent's
    /// trunk turns into it at, are free, so short variations far apart share a lane however
    /// they are ordered. The trunk is taken too, in every lane it crosses, so no later line
    /// puts a move on it; and a line that branches again takes no lane above a move standing
    /// at that depth, which its own trunk would cross. Iterative, so a long line costs no
    /// stack.
    pub fn branch_graph(&self) -> BranchGraph {
        let mut graph = BranchGraph {
            nodes: Vec::with_capacity(self.len()),
            index: HashMap::with_capacity(self.len()),
            depth: 0,
            lanes: 0,
        };
        let mut taken: Vec<Vec<Taken>> = Vec::new();
        // (node, depth, parent lane, parent slot)
        let mut stack: Vec<(NodeId, u32, u32, Option<usize>)> = vec![(self.root(), 0, 0, None)];

        while let Some((id, depth, parent_lane, parent)) = stack.pop() {
            let parent_id = parent.map(|slot| graph.nodes[slot].id);
            let continues = parent_id.is_some_and(|p| self.children(p)[0] == id);
            let lane = if continues {
                parent_lane
            } else {
                let mut to = depth;
                let mut at = id;
                // Depths on this line that branch again: their trunks will drop down from
                // whatever lane this takes, through every lane below it.
                let mut forks: Vec<u32> = Vec::new();
                loop {
                    let children = self.children(at);
                    if children.len() > 1 {
                        forks.push(to);
                    }
                    let Some(&next) = children.first() else { break };
                    to += 1;
                    at = next;
                }
                let (first, owner) = match parent_id {
                    Some(p) => (parent_lane + 1, p),
                    None => (0, id),
                };
                let mut lane = first;
                while (lane as usize) < taken.len()
                    && !(fits(&taken[lane as usize], depth, to, owner)
                        && clear_below(&taken[lane as usize + 1..], &forks))
                {
                    lane += 1;
                }
                while taken.len() <= lane as usize {
                    taken.push(Vec::new());
                }
                taken[lane as usize].push(Taken {
                    from: depth,
                    to,
                    trunk_of: None,
                });
                if let Some(p) = parent_id {
                    for crossed in &mut taken[first as usize..=lane as usize] {
                        let elbow = depth - 1;
                        if !crossed
                            .iter()
                            .any(|t| t.from == elbow && t.trunk_of == Some(p))
                        {
                            crossed.push(Taken {
                                from: elbow,
                                to: elbow,
                                trunk_of: Some(p),
                            });
                        }
                    }
                }
                lane
            };

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

    /// Plays a line of moves from `at`, alternating colours from `first`, one per column of
    /// `cols` on row `row`, so no move captures or repeats; returns the last node.
    fn line(
        tree: &mut GameTree,
        at: NodeId,
        first: Color,
        row: u8,
        cols: std::ops::Range<u8>,
    ) -> NodeId {
        let size = tree.info.size;
        let mut color = first;
        let mut at = at;
        for x in cols {
            at = tree.add_variation(at, color, size.point(x, row)).unwrap();
            color = color.other();
        }
        at
    }

    /// The walk meets the later variation first. A short early one still fits in the lane
    /// before it, rather than under it: lanes are for lines that overlap, not for the order
    /// the walk happens to reach them in.
    #[test]
    fn an_early_short_variation_shares_a_later_ones_lane() {
        let (mut tree, _) = tree19();
        let root = tree.root();
        line(&mut tree, root, Color::Black, 0, 0..19);
        let main: Vec<NodeId> = tree.main_line();
        // Off move 15, three moves; off move 3, two.
        let late = line(&mut tree, main[15], Color::Black, 5, 0..3);
        let early = line(&mut tree, main[3], Color::White, 6, 0..2);

        let graph = tree.branch_graph();
        assert_eq!(lane(&graph, late), Some(1));
        assert_eq!(lane(&graph, early), Some(1));
        assert_eq!(graph.lanes, 1);
    }

    /// A trunk drops down at its parent's depth through every lane on its way. A line that
    /// would put a move there, on another parent's trunk, goes below it instead.
    #[test]
    fn no_move_sits_on_another_parents_trunk() {
        let (mut tree, _) = tree19();
        let root = tree.root();
        line(&mut tree, root, Color::Black, 0, 0..12);
        let main: Vec<NodeId> = tree.main_line();
        // Two branches off move 7, in lanes 1 and 2: the trunk at depth 7 runs through both.
        let first = line(&mut tree, main[7], Color::White, 5, 0..2);
        let second = line(&mut tree, main[7], Color::White, 6, 0..2);
        // Off move 3, four moves: depths 4 to 7, ending on that trunk in lanes 1 and 2.
        let early = line(&mut tree, main[3], Color::White, 7, 0..4);

        let graph = tree.branch_graph();
        assert_eq!(
            (lane(&graph, first), lane(&graph, second)),
            (Some(1), Some(2))
        );
        assert_eq!(lane(&graph, early), Some(3));
        assert_drawable(&graph);
    }

    /// The other way round: a line that branches again would drop its own trunk through
    /// every lane below it. A short line fits in lane 1 here, but a move stands in lane 2 at
    /// the depth where it branches, so it goes past that move instead.
    #[test]
    fn no_trunk_crosses_a_move() {
        let (mut tree, _) = tree19();
        let root = tree.root();
        line(&mut tree, root, Color::Black, 0, 0..6);
        let main: Vec<NodeId> = tree.main_line();
        // Off move 4, one move: lane 1 at depth 5.
        line(&mut tree, main[4], Color::White, 5, 0..1);
        // Off move 1, four moves: depths 2 to 5, so lane 2.
        let long = line(&mut tree, main[1], Color::White, 6, 0..4);
        // Off move 1 again, two moves, and a second answer to its first.
        let short_first = line(&mut tree, main[1], Color::White, 7, 0..1);
        line(&mut tree, short_first, Color::Black, 7, 1..2);
        let answer = line(&mut tree, short_first, Color::Black, 8, 0..1);

        let graph = tree.branch_graph();
        assert_eq!(lane(&graph, long), Some(2));
        assert_eq!(
            lane(&graph, short_first),
            Some(3),
            "not lane 1, above the long line's move"
        );
        assert_eq!(lane(&graph, answer), Some(4));
        assert_drawable(&graph);
    }

    /// What every frontend draws must be drawable: one node per cell, and no trunk — down
    /// from a parent at its depth to its furthest child's lane — through another node.
    fn assert_drawable(graph: &BranchGraph) {
        let mut cells = std::collections::HashSet::new();
        for node in &graph.nodes {
            assert!(
                cells.insert((node.depth, node.lane)),
                "two nodes at {node:?}"
            );
        }
        for (slot, parent) in graph.nodes.iter().enumerate() {
            let furthest = graph
                .nodes
                .iter()
                .filter(|n| n.parent == Some(slot))
                .map(|n| n.lane)
                .max();
            for lane in parent.lane + 1..=furthest.unwrap_or(parent.lane) {
                assert!(
                    !cells.contains(&(parent.depth, lane)),
                    "the trunk of {parent:?} crosses a node in lane {lane}"
                );
            }
        }
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
