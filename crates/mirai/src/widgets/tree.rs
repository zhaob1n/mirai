// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! `MoveTreeView` — the branch graph. Step 10.
//!
//! Nodes sit on a fixed grid: the row is the node's depth from the root and the column is
//! a *lane* handed out by a depth-first walk that keeps the main line on lane 0.
//!
//! Depth runs *down*. The widget lives in the sidebar — 340-520 px wide and as tall as the
//! window — so the axis a 250-move record is 5000 px long on has to be the panel's long
//! axis, and the wheel then scrolls it without a modifier. Lanes run across instead, where
//! a handful of variations fit in the width. The layout, including each parent's trunk
//! span, is cached by tree epoch and [`GameTree::structure_revision`].
//! `draw` culls edges, trunks and nodes to the scrolled viewport, so a long record does
//! not repaint nodes the panel cannot see. Deliberately *not* `GameTree::revision`: that
//! counts a stored analysis as a change too, so a running engine would invalidate this
//! ten times a second and every navigation step would rebuild.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use mirai_core::{Color, GameTree, NodeId};

use crate::app::{AppState, TreeEpoch};
use crate::widgets::paint::{fill_disc, hline, stroke_disc, vline, with_alpha};

/// Grid geometry: `CELL_W` spaces the lanes across, `CELL_H` the depths down. Equal, so a
/// branch elbow is a right angle and long games stay scannable.
const CELL_W: f32 = 20.0;
const CELL_H: f32 = 20.0;
const MARGIN: f32 = 10.0;
const RADIUS: f32 = 6.0;
/// Elbow ink. Translucent, so two rectangles that share a pixel blend twice.
const EDGE_WIDTH: f32 = 1.5;

/// A node placed on the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub id: NodeId,
    /// Depth from the root; the root is row 0.
    pub depth: u32,
    /// Branch lane; the main line is lane 0 and later variations sit to its right.
    pub lane: u32,
    /// Index into [`TreeLayout::nodes`] of this node's parent.
    pub parent: Option<usize>,
    pub mv: Option<(Color, mirai_core::Point)>,
}

/// The whole tree, placed.
#[derive(Debug, Default)]
pub(crate) struct TreeLayout {
    pub nodes: Vec<Placed>,
    pub index: HashMap<NodeId, usize>,
    /// Depth of the deepest node — the last row.
    pub depth: u32,
    /// Highest lane in use — the last column.
    pub lanes: u32,
    /// Horizontal trunk of each parent, `(left_x, right_x)` in widget coordinates,
    /// seeded with the parent's own x and widened to every child. `None` for a node
    /// with no children. A zero-width span is not drawn: the ink is translucent, so
    /// one rectangle per parent is the whole trunk.
    pub trunks: Vec<Option<(f32, f32)>>,
}

/// Places every node of `tree` on the grid.
///
/// The walk is depth-first in child order, and `children[0]` is the main line, so lane 0
/// is claimed by the main line before any variation can ask for it. A branch takes the
/// lowest lane at or right of its parent's that is still free from its depth onwards, which
/// lets a short variation share a lane with a later, disjoint one.
pub(crate) fn lay_out(tree: &GameTree) -> TreeLayout {
    let mut layout = TreeLayout::default();
    // `free[l]` is the first depth in lane `l` that nothing occupies yet.
    let mut free: Vec<u32> = Vec::new();
    // (node, depth, parent lane, parent slot)
    let mut stack: Vec<(NodeId, u32, u32, Option<usize>)> = vec![(tree.root(), 0, 0, None)];

    while let Some((id, depth, parent_lane, parent)) = stack.pop() {
        let mut lane = parent_lane;
        while (lane as usize) < free.len() && free[lane as usize] > depth {
            lane += 1;
        }
        while free.len() <= lane as usize {
            free.push(0);
        }
        free[lane as usize] = depth + 1;

        let slot = layout.nodes.len();
        layout.nodes.push(Placed {
            id,
            depth,
            lane,
            parent,
            mv: tree.node(id).mv,
        });
        layout.index.insert(id, slot);
        layout.depth = layout.depth.max(depth);
        layout.lanes = layout.lanes.max(lane);

        // Reversed, so `children[0]` is popped first and keeps the parent's lane.
        for &child in tree.children(id).iter().rev() {
            stack.push((child, depth + 1, lane, Some(slot)));
        }
    }
    layout.trunks = vec![None; layout.nodes.len()];
    for node in &layout.nodes {
        let Some(parent) = node.parent else { continue };
        let (px, _) = cell_xy(layout.nodes[parent].depth, layout.nodes[parent].lane);
        let (cx, _) = cell_xy(node.depth, node.lane);
        let span = layout.trunks[parent].get_or_insert((px, px));
        span.0 = span.0.min(cx);
        span.1 = span.1.max(cx);
    }
    layout
}

/// Revisions and node IDs are arena-local: adopting another record can keep the
/// same revision while replacing every move the view draws.
#[derive(Default)]
struct LayoutCache {
    source: Option<(TreeEpoch, u64)>,
    layout: TreeLayout,
}

impl LayoutCache {
    fn update(&mut self, tree: &GameTree, epoch: TreeEpoch) -> bool {
        let source = (epoch, tree.structure_revision());
        if self.source == Some(source) {
            return false;
        }
        self.layout = lay_out(tree);
        self.source = Some(source);
        true
    }
}

/// Centre of a grid cell in widget coordinates: lanes across, depth down.
#[inline]
fn cell_xy(depth: u32, lane: u32) -> (f32, f32) {
    (
        MARGIN + RADIUS + lane as f32 * CELL_W,
        MARGIN + RADIUS + depth as f32 * CELL_H,
    )
}

/// Where a child drop starts. The horizontal trunk is centred on the parent and
/// [`EDGE_WIDTH`] tall; a drop from the centre overlaps that rectangle and the
/// joint paints darker than either stroke.
#[inline]
fn drop_top(parent_y: f32) -> f32 {
    parent_y + EDGE_WIDTH * 0.5
}

/// A parent's trunk in x, from its own lane to its furthest child. Both ends reach the
/// outer edge of the drop there, not its centre line, so each elbow is a square corner
/// with no notch.
#[inline]
fn trunk_x(own_x: f32, furthest_child_x: f32) -> (f32, f32) {
    (
        own_x - EDGE_WIDTH * 0.5,
        furthest_child_x + EDGE_WIDTH * 0.5,
    )
}

/// Inclusive depth and lane indexes the scrolled viewport can see, padded by a cell.
#[derive(Clone, Copy)]
struct Viewport {
    depth_lo: u32,
    depth_hi: u32,
    lane_lo: u32,
    lane_hi: u32,
}

impl Viewport {
    fn covers_depth(&self, depth: u32) -> bool {
        depth >= self.depth_lo && depth <= self.depth_hi
    }

    fn covers_lane(&self, lane: u32) -> bool {
        lane >= self.lane_lo && lane <= self.lane_hi
    }

    fn overlaps_x(&self, span: (f32, f32)) -> bool {
        let (left, right) = span;
        let (x0, _) = cell_xy(0, self.lane_lo);
        let (x1, _) = cell_xy(0, self.lane_hi);
        right >= x0 && left <= x1
    }
}

fn axis_lo(start: f64, cell: f32) -> u32 {
    let origin = MARGIN + RADIUS;
    ((start as f32 - CELL_W - origin) / cell).floor().max(0.0) as u32
}

fn axis_hi(start: f64, page: f64, cell: f32) -> u32 {
    let origin = MARGIN + RADIUS;
    ((start as f32 + page as f32 + CELL_W - origin) / cell)
        .ceil()
        .max(0.0) as u32
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MoveTreeView {
        pub state: OnceCell<AppState>,
        pub(super) layout: RefCell<LayoutCache>,
        pub cursor: Cell<Option<NodeId>>,
        /// The node context menu. Its items name a raw `NodeId`, so the window closes it
        /// on every structural change before that id can go stale.
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MoveTreeView {
        const NAME: &'static str = "MiraiMoveTreeView";
        type Type = super::MoveTreeView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MoveTreeView {
        fn dispose(&self) {
            if let Some(popover) = self.popover.borrow_mut().take() {
                popover.unparent();
            }
        }
    }

    impl WidgetImpl for MoveTreeView {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }

        /// A popover parented to a custom widget is positioned by that widget's allocation.
        fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) {
            if let Some(popover) = self.popover.borrow().as_ref()
                && popover.is_visible()
            {
                popover.present();
            }
        }
    }
}

glib::wrapper! {
    pub struct MoveTreeView(ObjectSubclass<imp::MoveTreeView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MoveTreeView {
    pub fn new(state: &AppState) -> MoveTreeView {
        let this: MoveTreeView = glib::Object::new();
        this.imp()
            .state
            .set(state.clone())
            .expect("a fresh MoveTreeView cannot already hold a state");
        this.add_css_class("mirai-movetree");
        this.set_halign(gtk::Align::Start);
        this.set_valign(gtk::Align::Start);
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        {
            let weak = this.downgrade();
            click.connect_pressed(move |_, _, x, y| {
                if let Some(this) = weak.upgrade() {
                    this.click_at(x as f32, y as f32);
                }
            });
        }
        this.add_controller(click);

        let menu = gtk::GestureClick::new();
        menu.set_button(gdk::BUTTON_SECONDARY);
        {
            let weak = this.downgrade();
            menu.connect_pressed(move |_, _, x, y| {
                if let Some(this) = weak.upgrade() {
                    this.show_menu_at(x, y);
                }
            });
        }
        this.add_controller(menu);

        let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        popover.set_parent(&this);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        *this.imp().popover.borrow_mut() = Some(popover);

        this
    }

    /// The window state this view draws, held directly rather than fetched back through the
    /// window on every layout pass.
    pub fn state(&self) -> &AppState {
        self.imp()
            .state
            .get()
            .expect("MoveTreeView was built without a state")
    }

    /// Puts the view in `scroller` and redraws whenever that scroller's adjustments change.
    ///
    /// [`Self::draw`] culls to the scroller's page, but scrolling only moves this widget's
    /// allocation and GTK reuses its cached render node. Without a redraw on every
    /// adjustment change, nodes that scroll or resize into view stay blank.
    pub fn pack_into(&self, scroller: &gtk::ScrolledWindow) {
        scroller.set_child(Some(self));
        self.redraw_on(&scroller.hadjustment());
        self.redraw_on(&scroller.vadjustment());
        let view = self.downgrade();
        scroller.connect_hadjustment_notify(move |scroller| {
            if let Some(view) = view.upgrade() {
                view.redraw_on(&scroller.hadjustment());
            }
        });
        let view = self.downgrade();
        scroller.connect_vadjustment_notify(move |scroller| {
            if let Some(view) = view.upgrade() {
                view.redraw_on(&scroller.vadjustment());
            }
        });
    }

    /// Scrolling changes `value`; a resized sidebar changes `page-size`, which emits
    /// `changed`, not `value-changed`.
    fn redraw_on(&self, adjustment: &gtk::Adjustment) {
        let view = self.downgrade();
        adjustment.connect_value_changed(move |_| {
            if let Some(view) = view.upgrade() {
                view.queue_draw();
            }
        });
        let view = self.downgrade();
        adjustment.connect_changed(move |_| {
            if let Some(view) = view.upgrade() {
                view.queue_draw();
            }
        });
    }

    /// Rebuilds the cached layout if the tree changed, then redraws.
    pub(crate) fn refresh(&self) {
        self.imp().cursor.set(Some(self.state().cursor()));
        self.ensure_layout();
        self.queue_draw();
        self.scroll_to_cursor();
    }

    fn ensure_layout(&self) {
        let state = self.state();
        let mut cache = self.imp().layout.borrow_mut();
        if !cache.update(&state.tree(), state.tree_epoch()) {
            return;
        }
        let layout = &cache.layout;
        let w = (MARGIN * 2.0 + RADIUS * 2.0 + layout.lanes as f32 * CELL_W).ceil() as i32;
        let h = (MARGIN * 2.0 + RADIUS * 2.0 + layout.depth as f32 * CELL_H).ceil() as i32;
        drop(cache);
        // The ScrolledWindow pans over this; we never implement gtk::Scrollable.
        if self.width_request() != w || self.height_request() != h {
            self.set_size_request(w, h);
        }
    }

    fn node_at(&self, x: f32, y: f32) -> Option<NodeId> {
        self.ensure_layout();
        let cache = self.imp().layout.borrow();
        let layout = &cache.layout;
        layout
            .nodes
            .iter()
            .find(|n| {
                let (cx, cy) = cell_xy(n.depth, n.lane);
                (cx - x).abs() <= CELL_W * 0.5 && (cy - y).abs() <= CELL_H * 0.5
            })
            .map(|n| n.id)
    }

    fn click_at(&self, x: f32, y: f32) {
        crate::widgets::release_focus(self);
        if let Some(id) = self.node_at(x, y) {
            self.state().set_cursor(id);
        }
    }

    /// The harness's pointer: a press at the centre of grid cell `(depth, lane)`.
    #[cfg(debug_assertions)]
    pub(crate) fn press_cell(&self, button: u32, depth: u32, lane: u32) {
        let (x, y) = cell_xy(depth, lane);
        match button {
            gdk::BUTTON_SECONDARY => self.show_menu_at(x as f64, y as f64),
            _ => self.click_at(x, y),
        }
    }

    /// Opens the branch menu for the node under the pointer. Only what applies is offered:
    /// a node already on the main line cannot be promoted, and the root cannot be deleted.
    fn show_menu_at(&self, x: f64, y: f64) {
        let Some(id) = self.node_at(x as f32, y as f32) else {
            return;
        };
        let menu = gio::Menu::new();
        {
            let tree = self.state().tree();
            if !tree.main_line().contains(&id) {
                let item =
                    gio::MenuItem::new(Some(&crate::i18n::gettext("Set as Main Line")), None);
                item.set_action_and_target_value(
                    Some("win.promote-line-at"),
                    Some(&id.0.to_variant()),
                );
                menu.append_item(&item);
            }
            if tree.parent(id).is_some() {
                let item = gio::MenuItem::new(Some(&crate::i18n::gettext("Delete Branch")), None);
                item.set_action_and_target_value(
                    Some("win.delete-branch-at"),
                    Some(&id.0.to_variant()),
                );
                menu.append_item(&item);
            }
        }
        if menu.n_items() == 0 {
            return;
        }
        let Some(popover) = self.imp().popover.borrow().clone() else {
            return;
        };
        popover.set_menu_model(Some(&menu));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }

    pub(crate) fn close_menu(&self) {
        if let Some(popover) = self.imp().popover.borrow().as_ref() {
            popover.popdown();
        }
    }

    /// Nudges the enclosing `ScrolledWindow` so the current node stays visible.
    fn scroll_to_cursor(&self) {
        let Some(scroller) = self
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
        else {
            return;
        };
        self.ensure_layout();
        let cursor = self.state().cursor();
        let cache = self.imp().layout.borrow();
        let layout = &cache.layout;
        let Some(&slot) = layout.index.get(&cursor) else {
            return;
        };
        let node = layout.nodes[slot];
        drop(cache);
        let (cx, cy) = cell_xy(node.depth, node.lane);

        for (adj, pos, pad) in [
            (scroller.hadjustment(), cx, CELL_W * 1.5),
            (scroller.vadjustment(), cy, CELL_H * 1.5),
        ] {
            let page = adj.page_size();
            if page <= 0.0 {
                continue;
            }
            let value = adj.value();
            let lo = (pos - pad) as f64;
            let hi = (pos + pad) as f64;
            let target = if lo < value {
                lo
            } else if hi > value + page {
                hi - page
            } else {
                continue;
            };
            adj.set_value(target.clamp(adj.lower(), (adj.upper() - page).max(adj.lower())));
        }
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let _t = crate::render_probe::Timer::new("tree-snapshot");
        let cache = self.imp().layout.borrow();
        let layout = &cache.layout;
        if layout.nodes.is_empty() {
            return;
        }
        let cursor = self.imp().cursor.get();

        let fg = self.color();
        let accent = adw::StyleManager::default()
            .accent_color()
            .to_standalone_rgba(adw::StyleManager::default().is_dark());
        let edge_color = with_alpha(fg, 0.45);
        let outline = with_alpha(fg, 0.7);
        let black = gdk::RGBA::new(0.09, 0.09, 0.11, 1.0);
        let white = gdk::RGBA::new(0.94, 0.94, 0.95, 1.0);

        // Edges first: right-angle elbows, across at the parent then down. Every segment is
        // axis-aligned, so they are rectangles rather than a stroked path — and one horizontal
        // per parent rather than one per child, because the ink is translucent and overlapping
        // rectangles would blend twice where two children share a trunk. A drop starts at the
        // bottom of that stroke, not at the parent's centre, for the same reason. The drops
        // are one per child and never overlap: siblings always land in different lanes. Trunk
        // spans were computed in `lay_out`; this pass only draws what the viewport can see.
        let view = self.viewport();
        for node in &layout.nodes {
            let Some(parent) = node.parent else { continue };
            let p = layout.nodes[parent];
            if let Some(view) = view
                && (!view.covers_lane(node.lane)
                    || !(view.covers_depth(node.depth) || view.covers_depth(p.depth)))
            {
                continue;
            }
            let (_, py) = cell_xy(p.depth, p.lane);
            let (cx, cy) = cell_xy(node.depth, node.lane);
            vline(snapshot, cx, drop_top(py), cy, EDGE_WIDTH, &edge_color);
        }
        for (index, span) in layout.trunks.iter().enumerate() {
            let Some((left, right)) = *span else { continue };
            if right - left < 0.01 {
                continue;
            }
            let p = layout.nodes[index];
            if let Some(view) = view
                && (!view.covers_depth(p.depth) || !view.overlaps_x((left, right)))
            {
                continue;
            }
            let (_, py) = cell_xy(p.depth, p.lane);
            let (x0, x1) = trunk_x(left, right);
            hline(snapshot, x0, x1, py, EDGE_WIDTH, &edge_color);
        }

        // Nodes.
        for node in &layout.nodes {
            if let Some(view) = view
                && (!view.covers_depth(node.depth) || !view.covers_lane(node.lane))
            {
                continue;
            }
            let (cx, cy) = cell_xy(node.depth, node.lane);

            match node.mv {
                Some((color, mv)) => {
                    let fill = match color {
                        Color::Black => black,
                        Color::White => white,
                    };
                    fill_disc(snapshot, cx, cy, RADIUS, &fill);
                    stroke_disc(snapshot, cx, cy, RADIUS, 1.0, &outline);
                    if mv.is_pass() {
                        // A pass reads as a bar through the stone.
                        let ink = match color {
                            Color::Black => white,
                            Color::White => black,
                        };
                        hline(
                            snapshot,
                            cx - RADIUS * 0.55,
                            cx + RADIUS * 0.55,
                            cy,
                            2.0,
                            &ink,
                        );
                    }
                }
                // Root and pure setup nodes are hollow.
                None => {
                    fill_disc(snapshot, cx, cy, RADIUS, &with_alpha(fg, 0.08));
                    stroke_disc(snapshot, cx, cy, RADIUS, 1.5, &outline);
                }
            }

            if Some(node.id) == cursor {
                stroke_disc(snapshot, cx, cy, RADIUS + 2.5, 2.0, &accent);
            }
        }
    }

    /// Inclusive depth and lane range the scrolled viewport can see.
    ///
    /// The tree does not implement `gtk::Scrollable`; the `ScrolledWindow` from
    /// [`Self::pack_into`] pans over it, and these are the same adjustments
    /// [`Self::scroll_to_cursor`] writes. gtk-rs does not expose the snapshot clip.
    /// `None` means the page size is not known yet — draw everything rather than guess.
    fn viewport(&self) -> Option<Viewport> {
        let scroller = self
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())?;
        let horizontal = scroller.hadjustment();
        let vertical = scroller.vadjustment();
        let page_w = horizontal.page_size();
        let page_h = vertical.page_size();
        if page_w <= 0.0 || page_h <= 0.0 {
            return None;
        }
        // One cell past the page covers the cursor ring (radius 8.5, stroke 2) and a
        // drop whose other end sits just outside the clip.
        Some(Viewport {
            depth_lo: axis_lo(vertical.value(), CELL_H),
            depth_hi: axis_hi(vertical.value(), page_h, CELL_H),
            lane_lo: axis_lo(horizontal.value(), CELL_W),
            lane_hi: axis_hi(horizontal.value(), page_w, CELL_W),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::{GameInfo, Size};

    fn lane(layout: &TreeLayout, id: NodeId) -> Option<u32> {
        layout.index.get(&id).map(|&i| layout.nodes[i].lane)
    }

    fn tree19() -> (GameTree, Size) {
        let size = Size::square(19);
        (GameTree::new(GameInfo::new(size, Default::default())), size)
    }

    #[test]
    fn replacing_a_record_at_the_same_revision_replaces_its_layout() {
        use mirai_client::GameSession;

        let mut session = GameSession::blank();
        let records = [
            "(;FF[4]GM[1]SZ[19];B[dd];W[pp])",
            "(;FF[4]GM[1]SZ[19];W[dd];B[pp])",
        ];
        let mut cache = LayoutCache::default();
        for (record, color) in records.into_iter().zip([Color::Black, Color::White]) {
            let tree = GameSession::parse(record.as_bytes()).unwrap().remove(0);
            session.adopt(tree, None);
            cache.update(session.tree(), TreeEpoch(session.epoch()));
            assert_eq!(cache.layout.nodes[1].mv.unwrap().0, color);
        }
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

        let layout = lay_out(&tree);
        assert_eq!(layout.nodes.len(), 9);
        for id in [root, m1, m2, m3, m4, m5] {
            assert_eq!(lane(&layout, id), Some(0), "main line node {id:?}");
        }
        assert_eq!(lane(&layout, v1), Some(1));
        assert_eq!(lane(&layout, v2), Some(1));
        assert_eq!(lane(&layout, w1), Some(2));
        assert_eq!(layout.lanes, 2);
        assert_eq!(layout.depth, 5);
    }

    #[test]
    fn rows_follow_depth_and_parents_are_linked() {
        let (mut tree, size) = tree19();
        let root = tree.root();
        let a = tree.play(root, Color::Black, size.point(3, 3)).unwrap();
        let b = tree.play(a, Color::White, size.point(15, 15)).unwrap();

        let layout = lay_out(&tree);
        let slot = |id: NodeId| layout.nodes[layout.index[&id]];
        assert_eq!(slot(root).depth, 0);
        assert_eq!(slot(a).depth, 1);
        assert_eq!(slot(b).depth, 2);
        assert_eq!(slot(root).parent, None);
        assert_eq!(layout.nodes[slot(b).parent.unwrap()].id, a);
    }

    /// Siblings of the same parent never collide, and a variation never rises above the
    /// lane of the branch it hangs off.
    #[test]
    fn sibling_variations_get_distinct_lanes() {
        let (mut tree, size) = tree19();
        let p = |x: u8, y: u8| size.point(x, y);
        let root = tree.root();
        let main = tree.play(root, Color::Black, p(3, 3)).unwrap();
        let a = tree.add_variation(root, Color::Black, p(15, 15)).unwrap();
        let b = tree.add_variation(root, Color::Black, p(15, 3)).unwrap();

        let layout = lay_out(&tree);
        assert_eq!(lane(&layout, root), Some(0));
        assert_eq!(lane(&layout, main), Some(0));
        assert_eq!(lane(&layout, a), Some(1));
        assert_eq!(lane(&layout, b), Some(2));
    }

    /// The walk is iterative, so a long game must not blow the stack.
    #[test]
    fn deep_lines_do_not_recurse() {
        let (mut tree, size) = tree19();
        let mut cur = tree.root();
        // A long ladder of passes: legal under every ruleset and cheap to build.
        for i in 0..2000u32 {
            let color = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };
            cur = tree
                .add_variation(cur, color, mirai_core::Point::PASS)
                .unwrap();
        }
        let _ = size;
        let layout = lay_out(&tree);
        assert_eq!(layout.nodes.len(), 2001);
        assert_eq!(layout.depth, 2000);
        assert_eq!(layout.lanes, 0);
        assert_eq!(lane(&layout, cur), Some(0));
    }

    /// The panel is tall and narrow, so depth runs *down* the widget and lanes run across it.
    /// A transposed grid is the one bug in here that still draws a plausible-looking tree.
    #[test]
    fn depth_runs_down_and_lanes_run_across() {
        let origin = cell_xy(0, 0);
        assert_eq!(
            cell_xy(1, 0),
            (origin.0, origin.1 + CELL_H),
            "one move deeper is one row down"
        );
        assert_eq!(
            cell_xy(0, 1),
            (origin.0 + CELL_W, origin.1),
            "the next lane is one column across"
        );
    }

    /// Both elbows: the trunk and each drop share an edge but no pixel (the ink is
    /// translucent, so overlap blends twice), and together they fill the corners
    /// instead of leaving a notch.
    #[test]
    fn an_elbow_is_a_square_corner_without_overlap() {
        let (px, py) = cell_xy(3, 1);
        let (cx, cy) = cell_xy(4, 3);
        let half = EDGE_WIDTH * 0.5;
        let (left, right) = trunk_x(px, cx);
        let trunk = (left, right, py - half, py + half);
        let main_drop = (px - half, px + half, drop_top(py), cy);
        let branch_drop = (cx - half, cx + half, drop_top(py), cy);
        for drop in [main_drop, branch_drop] {
            assert_eq!(drop.2, trunk.3, "the drop starts where the trunk ends");
        }
        assert_eq!(
            trunk.0, main_drop.0,
            "the trunk reaches the main line's outer edge"
        );
        assert_eq!(
            trunk.1, branch_drop.1,
            "the trunk reaches the branch's outer edge"
        );
    }

    /// A parent's trunk is one rectangle from its own lane to its furthest child, including
    /// a lane no child sits in. Drawing one segment per adjacent pair would leave a gap, and
    /// overlapping the two would blend twice because the ink is translucent.
    #[test]
    fn a_trunk_spans_children_in_non_adjacent_lanes() {
        let (mut tree, size) = tree19();
        let p = |x: u8, y: u8| size.point(x, y);
        let root = tree.root();
        let main = tree.play(root, Color::Black, p(3, 3)).unwrap();
        // The first child's variation claims lane 1 at the next depth, which is still
        // occupied when the root's second child is placed — so that sibling skips it.
        tree.play(main, Color::White, p(4, 4)).unwrap();
        let variation = tree.add_variation(main, Color::Black, p(5, 5)).unwrap();
        let sibling = tree.add_variation(root, Color::Black, p(15, 15)).unwrap();

        let layout = lay_out(&tree);
        assert_eq!(lane(&layout, main), Some(0));
        assert_eq!(lane(&layout, variation), Some(1));
        assert_eq!(lane(&layout, sibling), Some(2), "the gap lane stays empty");

        let span = layout.trunks[layout.index[&root]].expect("the root has children");
        assert_eq!(span.0, cell_xy(0, 0).0);
        assert_eq!(
            span.1,
            cell_xy(0, 2).0,
            "the trunk crosses the unoccupied lane instead of stopping at each child"
        );

        // A straight continuation has a zero-width span, which draw skips.
        let (mut straight, size) = tree19();
        let a = straight
            .play(straight.root(), Color::Black, size.point(3, 3))
            .unwrap();
        straight.play(a, Color::White, size.point(15, 15)).unwrap();
        let straight = lay_out(&straight);
        let span = straight.trunks[straight.index[&straight.nodes[0].id]].unwrap();
        assert!(
            span.1 - span.0 < 0.01,
            "a child in the parent's lane is not a trunk"
        );
    }
}
