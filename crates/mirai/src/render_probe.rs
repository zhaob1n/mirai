// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Render-performance probe: frame timings and scene ablations, driven by environment
//! variables and printed to stderr in a form `tools/perf/frame-stats.py` aggregates.
//!
//! This exists because the symptom — "folding the sidebar drops frames" — is invisible to
//! the test suite and to any screenshot. It answers three questions in order:
//!
//! 1. *How bad is it?* `frame-dt` lines carry the frame clock's own cadence, so `16.67`
//!    means a 60 Hz frame was hit and `50.00` means two were missed.
//! 2. *Which phase costs the time?* `frame-phases` splits one frame into GTK's phases:
//!    `update` ticks animations, `layout` measures and allocates, `paint` snapshots the
//!    widget tree and hands the resulting render node to GSK.
//! 3. *Which part of the scene costs it?* [`Timer`] brackets one drawing pass, and the
//!    `MIRAI_NO_*` flags take a whole widget out.
//!
//! Every entry point is behind `cfg!(debug_assertions)`, so a release build folds the module
//! away without a single `#[cfg]` at the call sites; a debug build is still inert unless the
//! variable is set.
//!
//! ```text
//! MIRAI_FRAMES=1     frame-dt, frame-phases and Timer lines
//! MIRAI_COLLAPSED=1  force the split view collapsed, so the sidebar overlays the content
//!                    instead of resizing it
//! MIRAI_NO_LABEL_DEFER=1
//!                    paint the board's text on every frame, however the allocation moved
//! MIRAI_NO_BOARD=1   take BoardView out of the paned
//! MIRAI_NO_GRAPH=1   take WinrateGraph out of the paned
//! MIRAI_SPIN=1       redraw an unchanging scene every frame
//! MIRAI_DUMP_NODE=p  write the window's render node for gtk4-rendernode-tool
//! ```
//!
//! Recipe — the measurement this module was written for:
//!
//! ```sh
//! MIRAI_FRAMES=1 MIRAI_HARNESS="wait:2000,action:win.toggle-sidebar,wait:700,\
//! action:win.toggle-sidebar,wait:700,quit" ./target/debug/mirai game.sgf 2>frames.log
//! tools/perf/frame-stats.py frames.log
//! ```

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Instant;

/// True when `var` is set to anything at all.
pub fn flag(var: &str) -> bool {
    std::env::var_os(var).is_some()
}

/// Whether the frame probes are on. Read from the environment once: `Timer` sits inside
/// `snapshot()`, and a `getenv` per drawing pass per frame is exactly the kind of cost this
/// module exists to find.
fn frames() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| flag("MIRAI_FRAMES"));
    cfg!(debug_assertions) && *ON
}

/// Prints `<label> <milliseconds>` when it is dropped, if `MIRAI_FRAMES` is set.
///
/// `let _t = render_probe::Timer::new("build-static");` times the rest of the scope.
pub struct Timer {
    label: &'static str,
    start: Instant,
}

impl Timer {
    pub fn new(label: &'static str) -> Option<Self> {
        frames().then(|| Self {
            label,
            start: Instant::now(),
        })
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        eprintln!(
            "{} {:.3}",
            self.label,
            self.start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Prints `<label> <value>`, for a scalar the frame statistics should follow — the board's
/// `cell`, say, which is what decides whether a glyph is a cache hit.
pub fn trace(label: &'static str, value: f32) {
    if frames() {
        eprintln!("{label} {value:.3}");
    }
}

/// Whether `BoardView` may defer its labels while the board is being resized.
///
/// `MIRAI_NO_LABEL_DEFER=1` paints them on every frame instead, which is the ablation that
/// measures what the deferral is worth (`RENDERING.md` §6).
pub fn label_defer() -> bool {
    static OFF: LazyLock<bool> = LazyLock::new(|| flag("MIRAI_NO_LABEL_DEFER"));
    !(cfg!(debug_assertions) && *OFF)
}

/// Attaches the frame-clock probes and applies the window-level ablations.
///
/// The tick callback is what keeps the clock running while nothing animates: an idle GTK
/// application produces no frames at all, and without a baseline the animation's frame
/// intervals mean nothing.
///
/// There is deliberately no size knob. mirai is developed under a tiling compositor, which
/// decides the window's size and ignores `set_default_size`; resizing for a sweep is
/// `niri msg action set-window-width/-height`, from outside the process.
pub fn install(window: &crate::window_shell::MiraiWindow) {
    if !cfg!(debug_assertions) {
        return;
    }
    if flag("MIRAI_COLLAPSED") {
        window.split().set_collapsed(true);
    }
    if flag("MIRAI_NO_BOARD") {
        window.content_paned().set_start_child(gtk::Widget::NONE);
    }
    if flag("MIRAI_NO_GRAPH") {
        window.content_paned().set_end_child(gtk::Widget::NONE);
    }
    if let Some(path) = std::env::var_os("MIRAI_DUMP_NODE") {
        // Two seconds in, so the board has its final allocation and the SGF has loaded.
        let window = window.clone();
        glib::spawn_future_local(async move {
            glib::timeout_future(std::time::Duration::from_millis(2000)).await;
            let path = path.to_string_lossy().to_string();
            match dump_node(window.upcast_ref(), &path) {
                Ok(()) => eprintln!("probe: wrote {path}"),
                Err(e) => eprintln!("probe: node dump failed: {e}"),
            }
        });
    }
    if !frames() {
        return;
    }

    // `MIRAI_SPIN` redraws an unchanging scene every frame. It separates two very different
    // costs: rasterising the same render nodes again (cheap if GSK caches by node) from
    // rasterising freshly built nodes, which is what a resize or a stone move produces.
    let spin = flag("MIRAI_SPIN").then(|| window.content_paned().start_child());
    let last = Cell::new(0i64);
    window.add_tick_callback(move |widget, clock| {
        let now = clock.frame_time();
        let prev = last.replace(now);
        if prev != 0 {
            eprintln!("frame-dt {:.2}", (now - prev) as f64 / 1000.0);
        }
        if let Some(child) = &spin {
            match child {
                Some(child) => child.queue_draw(),
                None => widget.queue_draw(),
            }
        }
        glib::ControlFlow::Continue
    });

    let Some(clock) = window.frame_clock() else {
        return;
    };
    let marks: Rc<RefCell<Vec<(&'static str, Instant)>>> = Rc::default();
    for phase in ["before-paint", "update", "layout", "paint"] {
        let marks = marks.clone();
        clock.connect_local(phase, false, move |_| {
            marks.borrow_mut().push((phase, Instant::now()));
            None
        });
    }
    // Connected `after` so it runs once every other handler for the phase is done, which is
    // what makes `paint` cover the whole snapshot-and-render pass.
    clock.connect_local("after-paint", true, move |_| {
        let mut spans = marks.borrow_mut();
        spans.push(("after-paint", Instant::now()));
        if let (Some(first), Some(last)) = (spans.first(), spans.last()) {
            let total = (last.1 - first.1).as_secs_f64() * 1000.0;
            let detail: Vec<String> = spans
                .windows(2)
                .map(|w| format!("{}={:.2}", w[1].0, (w[1].1 - w[0].1).as_secs_f64() * 1000.0))
                .collect();
            eprintln!("frame-phases total={total:.2} {}", detail.join(" "));
        }
        spans.clear();
        None
    });
}

/// Writes the render node of the whole window to `path`, for `gtk4-rendernode-tool`.
///
/// `gtk4-rendernode-tool benchmark --renderer=vulkan <path>` then measures the same scene
/// outside the application, which is how the cost of the board's fill and stroke nodes was
/// separated from everything else GTK does per frame.
pub fn dump_node(window: &gtk::Widget, path: &str) -> Result<(), String> {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        snapshot.upcast_ref::<gdk::Snapshot>(),
        window.width() as f64,
        window.height() as f64,
    );
    let node = snapshot.to_node().ok_or("nothing was drawn")?;
    node.write_to_file(path).map_err(|e| e.to_string())
}
