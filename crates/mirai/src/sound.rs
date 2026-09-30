// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

//! Stone sounds: a click for a move, and a click followed by stones dropped into the lid
//! for a capture.
//!
//! What sounds and what it sounds like are [`mirai_client::sound`], shared with the
//! HarmonyOS client. This is the playback: each clip is rendered once, wrapped as an
//! in-memory WAV and played through a `GtkMediaFile`, so there is no audio dependency beyond
//! GTK's own GStreamer backend.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::LazyLock;

use gtk::prelude::*;
use gtk::{gio, glib};
use mirai_client::sound::{CLIPS, FAMILIES, VOICES, Visit, family, render, stone_sound, wav};

use crate::app::{AppState, NodeRef, TreeEpoch};

/// Every clip at full level, rendered once per process and shared by every window: 60 ms in
/// a release build, 120 ms in debug. The window forces it on the runtime's blocking pool
/// ([`render_clips`]) before anything on the GTK thread touches it.
static CLIP_SAMPLES: LazyLock<[Vec<i16>; CLIPS]> = LazyLock::new(|| std::array::from_fn(render));

pub fn render_clips() {
    LazyLock::force(&CLIP_SAMPLES);
}

/// Turns the 0–100 volume setting into a gain. Cubic, like PulseAudio's volume percentage:
/// a straight line crowds everything audible into the top of the slider, where 50% would
/// be only 6 dB down.
fn gain(volume: u8) -> f32 {
    (f32::from(volume.min(100)) / 100.0).powi(3)
}

/// One window's stone sounds.
///
/// Nothing plays until the clips are rendered ([`Self::prepare`]): a move made in the first
/// 60–120 ms of a window stays silent rather than wait for them on the GTK thread.
pub struct StoneSounds {
    /// Where the cursor was last seen, in the record of that epoch. An edit that changes
    /// this node's position in place (a setup stone) leaves the visit stale until the cursor
    /// moves; at worst that sounds one refused move.
    last: Cell<(TreeEpoch, Visit)>,
    voices: Rc<Voices>,
    /// Set by [`Self::prepare`]: the clips are rendered and may be built from.
    rendered: Cell<bool>,
    /// The voice each family plays next.
    next_voice: Cell<[usize; FAMILIES]>,
}

/// Every stream plays once. Rewinding a finished one with `seek(0)` and playing it again
/// loses the first 10–25 ms on a real output device (a null sink does not show it): that is
/// the whole attack of a click, so every sound after a voice's first came out several times
/// quieter. Each voice therefore keeps a prepared stream in reserve, plays it, and a new one
/// is opened at idle. Opening costs 12–20 ms, so it never lands in the frame that draws a
/// move, and streams are opened one per idle rather than all eight (~100 ms) in one.
///
/// The volume is baked into the samples, never set on the stream: `MediaStream::set_volume`
/// becomes the PipeWire stream volume, which the session manager remembers per application
/// and restores on every later stream — one test at 50% left every later mirai sound at
/// 0.2% gain, and the setting would have been the system mixer's mirai slider.
#[derive(Default)]
struct Voices {
    /// The volume `wavs` were built for; 0 until the first build.
    volume: Cell<u8>,
    wavs: RefCell<Vec<glib::Bytes>>,
    ready: RefCell<[Option<gtk::MediaFile>; CLIPS]>,
    /// Kept alive until the voice plays again; dropping a stream stops it.
    playing: RefCell<[Option<gtk::MediaFile>; CLIPS]>,
    refill_queued: Cell<bool>,
    /// The pending rebuild after a volume change, taken by the callback when it runs.
    rebuild: RefCell<Option<glib::SourceId>>,
}

impl Voices {
    /// Rebuilds the WAVs at `volume`, dropping streams opened at the old one, and queues
    /// the new ones: 9 ms for all eight clips in a debug build, paid when the volume slider
    /// rests, never by a move. Only once the clips are rendered.
    fn build(self: &Rc<Self>, volume: u8) {
        self.volume.set(volume);
        let gain = gain(volume);
        *self.wavs.borrow_mut() = CLIP_SAMPLES
            .iter()
            .map(|clip| {
                let scaled: Vec<i16> = clip.iter().map(|&s| (f32::from(s) * gain) as i16).collect();
                glib::Bytes::from_owned(wav(&scaled))
            })
            .collect();
        *self.ready.borrow_mut() = Default::default();
        self.queue_refill();
    }

    /// Opens missing streams, one per idle.
    fn queue_refill(self: &Rc<Self>) {
        if self.refill_queued.replace(true) {
            return;
        }
        let voices: Weak<Voices> = Rc::downgrade(self);
        glib::idle_add_local(move || {
            let Some(voices) = voices.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if voices.open_one() {
                glib::ControlFlow::Continue
            } else {
                voices.refill_queued.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    /// Opens the first missing stream; true while more remain.
    fn open_one(&self) -> bool {
        let wavs = self.wavs.borrow();
        let mut ready = self.ready.borrow_mut();
        let mut missing = ready
            .iter_mut()
            .zip(wavs.iter())
            .filter(|(slot, _)| slot.is_none());
        if let Some((slot, wav)) = missing.next() {
            *slot = Some(open(wav));
        }
        missing.next().is_some()
    }

    /// Plays a voice. Its stream is normally ready; only a move in the moment after a build
    /// opens one on the spot.
    fn play(self: &Rc<Self>, index: usize) {
        let stream = self.ready.borrow_mut()[index].take();
        let stream = stream.unwrap_or_else(|| open(&self.wavs.borrow()[index]));
        stream.play();
        self.playing.borrow_mut()[index] = Some(stream);
        self.queue_refill();
    }

    /// Rebuilds at `volume` 250 ms after the last call, so dragging the slider does not
    /// rebuild at every step.
    fn rebuild_later(self: &Rc<Self>, volume: u8) {
        self.cancel_rebuild();
        let voices: Weak<Voices> = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(voices) = voices.upgrade() {
                voices.rebuild.take();
                voices.build(volume);
            }
        });
        *self.rebuild.borrow_mut() = Some(id);
    }

    fn cancel_rebuild(&self) {
        if let Some(pending) = self.rebuild.take() {
            pending.remove();
        }
    }
}

fn open(wav: &glib::Bytes) -> gtk::MediaFile {
    gtk::MediaFile::for_input_stream(&gio::MemoryInputStream::from_bytes(wav))
}

impl StoneSounds {
    /// Starts from the window's current cursor, so that the first move played sounds.
    pub fn new(state: &AppState) -> StoneSounds {
        StoneSounds {
            last: Cell::new(cursor_visit(state)),
            voices: Rc::default(),
            rendered: Cell::new(false),
            next_voice: Cell::new([0; FAMILIES]),
        }
    }

    /// Called once [`render_clips`] has finished: builds the clips at the set volume and
    /// opens their streams, unless muted.
    pub fn prepare(&self, state: &AppState) {
        self.rendered.set(true);
        let volume = state.config().ui.stone_volume;
        if volume > 0 {
            self.voices.build(volume);
        }
    }

    /// Called on `Change::StoneVolume`. Muting takes effect at once; any other level from
    /// the next move after the slider rests. Before [`Self::prepare`] there is nothing to
    /// do: it reads the setting itself.
    pub fn volume_changed(&self, state: &AppState) {
        let volume = state.config().ui.stone_volume;
        if volume == 0 || volume == self.voices.volume.get() {
            self.voices.cancel_rebuild();
        } else if self.rendered.get() {
            self.voices.rebuild_later(volume);
        }
    }

    /// Called first on every `Change::Cursor`; plays the step's sound, if it has one.
    pub fn cursor_moved(&self, state: &AppState) {
        let from = self.last_visit(state);
        let (epoch, to) = cursor_visit(state);
        self.last.set((epoch, to));
        // Muted, or not built yet (still rendering, or started muted and just unmuted).
        if state.config().ui.stone_volume == 0 || self.voices.volume.get() == 0 {
            return;
        }
        let Some(sound) = from.and_then(|from| stone_sound(&state.tree(), from, to)) else {
            return;
        };
        let family = family(sound);
        let mut next = self.next_voice.get();
        let voice = next[family];
        next[family] = (voice + 1) % VOICES;
        self.next_voice.set(next);
        self.voices.play(family * VOICES + voice);
    }

    /// Called when the tree changes. A move reaches here before its cursor change and must
    /// keep the node it was played from; only a record that no longer holds that node
    /// resets it, so the next step is judged against the new record.
    pub fn tree_changed(&self, state: &AppState) {
        if self.last_visit(state).is_none() {
            self.last.set(cursor_visit(state));
        }
    }

    fn last_visit(&self, state: &AppState) -> Option<Visit> {
        let (epoch, visit) = self.last.get();
        state
            .resolve_node(NodeRef {
                epoch,
                id: visit.id(),
            })
            .map(|_| visit)
    }
}

fn cursor_visit(state: &AppState) -> (TreeEpoch, Visit) {
    let (epoch, id) = (state.tree_epoch(), state.cursor());
    (epoch, state.with_tree_cached(|tree| Visit::at(tree, id)))
}
