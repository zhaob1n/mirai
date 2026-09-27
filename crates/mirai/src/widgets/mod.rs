// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Custom `gtk::Widget` subclasses. Everything is drawn in `WidgetImpl::snapshot`
//! with `gsk` — no `gtk::DrawingArea`, no cairo.

pub mod board;
pub mod paint;
pub mod tree;
pub mod winrate;

pub use board::BoardView;
pub use tree::MoveTreeView;
pub use winrate::WinrateGraph;

use gtk::prelude::*;

/// Called when the board, move tree or graph is pressed. A comment or label field keeps
/// keyboard focus across a click on a widget that cannot take it, and would keep eating the
/// arrows and letters the window's shortcuts want. Clearing the window's focus hands the
/// keys back without making these views Tab stops that have no keys of their own — and the
/// move tree's scroller, once focused, would take Page Up/Down and Home/End for itself.
pub(crate) fn release_focus(widget: &impl IsA<gtk::Widget>) {
    if let Some(root) = widget.root() {
        root.set_focus(None::<&gtk::Widget>);
    }
}
