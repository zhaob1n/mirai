// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Loads the fonts CJK text falls back to, at idle, before any of it is shown.
//!
//! Pango builds a fallback fontset the first time a string needs one for a font
//! description: fontconfig sorts the installed fonts for it, and the CJK font it settles on
//! is opened and shaped with for the first time. With a Latin-script interface the first
//! player name from Fox paid all of that for every text style at once, inside one layout:
//! 60–130 ms of a frame. Pango's font map belongs to the thread that uses it, so the work
//! cannot move to a worker. It runs on the GTK thread instead, one text style at a time at
//! low priority, and only while no window is painting. A CJK interface has loaded part of
//! this for its own text, and those steps are cheaper.
//!
//! What stays is per glyph: shaping and rasterising each character the first time it is
//! drawn.

use std::cell::RefCell;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib};

/// Han, Hangul and kana: Fox's players write their names in all three.
const SAMPLE: &str = "围棋 囲碁 바둑 いご";
/// Long enough for the window and its first record to be on screen.
const START: Duration = Duration::from_secs(2);
/// A window that painted within this long is animating; a step would take its frames.
const STILL_US: i64 = 50_000;
const RETRY: Duration = Duration::from_millis(100);
/// libadwaita's text styles, which are the font descriptions a fontset is built for: body
/// text, bold (`.heading`, window titles), `smaller` (row and window subtitles) and
/// `.caption`.
const STEPS: usize = 4;

/// The warm-up's progress, and the windows it keeps out of the way of.
#[derive(Default)]
struct Warmup {
    /// The next text style to warm; [`STEPS`] once all are.
    next: usize,
    /// Whether a step is scheduled.
    running: bool,
    /// When any window last painted. `gdk_frame_clock_get_frame_time` cannot say: between
    /// frames it answers with a time close to now. A frame that only ticked callbacks — the
    /// frame probe ticks every frame — has no paint phase and does not count.
    painted_at: i64,
    /// Every open window's frame clock, with the handler that stamps `painted_at`.
    clocks: Vec<(glib::WeakRef<gdk::FrameClock>, glib::SignalHandlerId)>,
}

thread_local!(static WARMUP: RefCell<Warmup> = RefCell::default());

/// Has the warm-up wait for `window` to be still as well, and starts it if it is not
/// running. Fonts belong to the GTK thread, so it runs once a process, unless every
/// window closes before it is done; the next window to open then carries it on.
pub(crate) fn schedule(window: &impl IsA<gtk::Widget>) {
    let Some(clock) = window.frame_clock() else {
        return;
    };
    let start = WARMUP.with_borrow_mut(|warmup| {
        if warmup.next >= STEPS {
            return false;
        }
        let handler = clock.connect_paint(|_| {
            WARMUP.with_borrow_mut(|warmup| warmup.painted_at = glib::monotonic_time());
        });
        warmup.clocks.push((clock.downgrade(), handler));
        !std::mem::replace(&mut warmup.running, true)
    });
    if start {
        glib::timeout_add_local_once(START, step);
    }
}

enum Next {
    /// A window painted lately: an animation's frames come first.
    Wait,
    Warm(usize),
    /// No window is left to show the text.
    Stop,
}

fn step() {
    glib::idle_add_local_full(glib::Priority::LOW, || {
        let next = WARMUP.with_borrow_mut(|warmup| {
            // A closed window's clock goes, and its handler with it.
            warmup.clocks.retain(|(clock, _)| clock.upgrade().is_some());
            if warmup.clocks.is_empty() {
                warmup.running = false;
                Next::Stop
            } else if glib::monotonic_time() - warmup.painted_at < STILL_US {
                Next::Wait
            } else {
                Next::Warm(warmup.next)
            }
        });
        match next {
            Next::Wait => {
                glib::timeout_add_local_once(RETRY, step);
            }
            Next::Warm(style) => {
                warm(style);
                if finish_step() {
                    step();
                }
            }
            Next::Stop => {}
        }
        glib::ControlFlow::Break
    });
}

/// Records a style as warm; true while styles remain. Once none do, stops watching paints.
fn finish_step() -> bool {
    WARMUP.with_borrow_mut(|warmup| {
        warmup.next += 1;
        if warmup.next < STEPS {
            return true;
        }
        warmup.running = false;
        for (clock, handler) in warmup.clocks.drain(..) {
            if let Some(clock) = clock.upgrade() {
                clock.disconnect(handler);
            }
        }
        false
    })
}

/// Measures [`SAMPLE`] in text style `step`, on a widget that is never shown: measuring
/// lays the text out, which is what builds the fontset and loads its fonts.
fn warm(step: usize) {
    let _t = crate::render_probe::Timer::new("font-warmup");
    let widget: gtk::Widget = match step {
        0 => gtk::Label::new(Some(SAMPLE)).upcast(),
        1 => label(&["heading"]),
        2 => label(&["caption"]),
        _ => adw::ActionRow::builder().subtitle(SAMPLE).build().upcast(),
    };
    let (_, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    widget.measure(gtk::Orientation::Vertical, natural);
}

fn label(classes: &[&str]) -> gtk::Widget {
    gtk::Label::builder()
        .label(SAMPLE)
        .css_classes(classes)
        .build()
        .upcast()
}
