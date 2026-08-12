// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Drawing primitives shared by the custom widgets, and the one rule they exist to enforce:
//! **do not hand GSK a path for a shape a quad can draw.**
//!
//! GSK rasterises a `fill` or `stroke` node by evaluating the path's curves across the whole
//! bounding box of the node, so cost grows as *area × segments* — and it is redone on every
//! frame that damages the widget, because there is no rasterisation cache to hit. A board's
//! grid (38 segments spanning the board) and its stones (one circle each) measured 30 ms a
//! frame on an empty board and 120 ms on a full one, which is what made every animation and
//! every step through a game record drop frames. Colour nodes, border nodes and rounded
//! clips are quads the renderer batches; the same scenes now paint in 2–8 ms.
//!
//! Paths are still right for genuinely curved or diagonal ink — the win-rate curve, the move
//! tree's edges, the triangle and cross marks — as long as the node's bounds stay small or
//! its segment count stays low. `docs/dev/RENDERING.md` carries the measurements.

use gtk::gdk;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;

/// Source-over composite of `fg` onto an opaque `bg`.
///
/// Pre-blending translucent ink against a known background lets it be drawn as opaque
/// rectangles: two translucent rectangles that cross would blend twice and leave a darker dot
/// at the intersection, which a single stroked path would not.
pub fn over(fg: gdk::RGBA, bg: gdk::RGBA) -> gdk::RGBA {
    let a = fg.alpha();
    gdk::RGBA::new(
        fg.red() * a + bg.red() * (1.0 - a),
        fg.green() * a + bg.green() * (1.0 - a),
        fg.blue() * a + bg.blue() * (1.0 - a),
        1.0,
    )
}

/// A filled disc of radius `r`, as a colour node inside a rounded clip.
#[inline]
pub fn fill_disc(snapshot: &gtk::Snapshot, cx: f32, cy: f32, r: f32, color: &gdk::RGBA) {
    let bounds = graphene::Rect::new(cx - r, cy - r, r * 2.0, r * 2.0);
    snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, r));
    snapshot.append_color(color, &bounds);
    snapshot.pop();
}

/// A circular outline of `width`, centred on radius `r`, as a border node.
#[inline]
pub fn stroke_disc(
    snapshot: &gtk::Snapshot,
    cx: f32,
    cy: f32,
    r: f32,
    width: f32,
    color: &gdk::RGBA,
) {
    let outer = r + width * 0.5;
    let bounds = graphene::Rect::new(cx - outer, cy - outer, outer * 2.0, outer * 2.0);
    snapshot.append_border(
        &gsk::RoundedRect::from_rect(bounds, outer),
        &[width; 4],
        &[*color; 4],
    );
}

/// A rectangular outline of `width`, centred on `rect`'s edges, as a border node.
#[inline]
pub fn stroke_rect(snapshot: &gtk::Snapshot, rect: &graphene::Rect, width: f32, color: &gdk::RGBA) {
    let half = width * 0.5;
    let outer = graphene::Rect::new(
        rect.x() - half,
        rect.y() - half,
        rect.width() + width,
        rect.height() + width,
    );
    snapshot.append_border(
        &gsk::RoundedRect::from_rect(outer, 0.0),
        &[width; 4],
        &[*color; 4],
    );
}

/// A horizontal line of `width`, centred on `y`, as a colour node.
#[inline]
pub fn hline(snapshot: &gtk::Snapshot, x0: f32, x1: f32, y: f32, width: f32, color: &gdk::RGBA) {
    snapshot.append_color(
        color,
        &graphene::Rect::new(x0, y - width * 0.5, x1 - x0, width),
    );
}

/// A vertical line of `width`, centred on `x`, as a colour node.
#[inline]
pub fn vline(snapshot: &gtk::Snapshot, x: f32, y0: f32, y1: f32, width: f32, color: &gdk::RGBA) {
    snapshot.append_color(
        color,
        &graphene::Rect::new(x - width * 0.5, y0, width, y1 - y0),
    );
}
