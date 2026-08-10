// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Custom `gtk::Widget` subclasses. Everything is drawn in `WidgetImpl::snapshot`
//! with `gsk` — no `gtk::DrawingArea`, no cairo.

pub mod board;
pub mod tree;
pub mod winrate;

pub use board::BoardView;
pub use tree::MoveTreeView;
pub use winrate::WinrateGraph;
