// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

//! Stone sounds: a click for a move, and a click followed by stones dropped into the lid
//! for a capture.
//!
//! The clips are synthesised, not shipped. A placement is [`stone_on_wood`]: damped stone
//! partials over damped board modes and a contact burst, soft-clipped. A capture is that
//! strike followed by stones dropped into the lid ([`clink`]). Each clip is rendered once, wrapped as an in-memory WAV and played through a
//! `GtkMediaFile`, so there is no audio dependency beyond GTK's own GStreamer backend.
//!
//! A sound is decided from what the cursor did, not from who moved it: stepping onto a
//! child that carries a move sounds exactly as playing that move does, whether the user,
//! the AI or the Right key put it there. Anything else — a jump, a pass, a setup node,
//! going back — is silent.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::LazyLock;

use gtk::prelude::*;
use gtk::{gio, glib};
use mirai_core::{GameTree, NodeId};

use crate::app::{AppState, NodeRef, TreeEpoch};

/// What a single step forward should sound like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoneSound {
    Place,
    /// A move that removed this many stones, suicide included.
    Capture(u32),
}

/// A node, with what its position says about the next step's sound. Kept for the node the
/// cursor leaves: by the time a new move's cursor change arrives, the tree's position cache
/// has already moved on to the child, and asking for the parent again replays the line from
/// the root.
#[derive(Clone, Copy, Debug)]
pub struct Visit {
    id: NodeId,
    zobrist: u64,
    /// Prisoners taken by both sides so far.
    removed: u32,
}

impl Visit {
    pub fn at(tree: &mut GameTree, id: NodeId) -> Visit {
        let board = &tree.position(id).board;
        Visit {
            id,
            zobrist: board.zobrist(),
            removed: board.captures.iter().map(|&c| u32::from(c)).sum(),
        }
    }
}

/// The sound for moving the cursor from `from` to `to`, if that step placed a stone.
pub fn stone_sound(tree: &GameTree, from: Visit, to: Visit) -> Option<StoneSound> {
    let node = tree.get(to.id)?;
    if node.parent != Some(from.id) || node.mv.is_none_or(|(_, p)| p.is_pass()) {
        return None;
    }
    // Setup stones on the same node change the board whether or not the move lands, so
    // the comparison below could not tell a refused move or a capture from the setup.
    if !node.setup.is_empty() {
        return None;
    }
    // A recorded move the board refused (on an occupied point, say) is replayed as a no-op:
    // the stones did not change.
    if to.zobrist == from.zobrist {
        return None;
    }
    Some(match to.removed.saturating_sub(from.removed) {
        0 => StoneSound::Place,
        n => StoneSound::Capture(n),
    })
}

/// Placement, then one family per size of capture clatter.
const FAMILIES: usize = 4;

fn family(sound: StoneSound) -> usize {
    match sound {
        StoneSound::Place => 0,
        StoneSound::Capture(1) => 1,
        StoneSound::Capture(2..=4) => 2,
        StoneSound::Capture(_) => 3,
    }
}

/// Every family has two voices, rendered from different seeds, and its sounds take turns
/// between them: stepping through a game does not sound like a metronome, and a capture's
/// clatter is not cut off by the next capture restarting the same stream.
const VOICES: usize = 2;
const CLIPS: usize = FAMILIES * VOICES;

/// Clip `index = family * VOICES + voice` as 16-bit PCM samples. Seeds are fixed, so every
/// run sounds the same.
fn render(index: usize) -> Vec<i16> {
    let (family, voice) = (index / VOICES, index % VOICES);
    let mut rng = Rng(0x5eed_0000 + (family * 16 + voice) as u64);
    let mut out = stone_on_wood(&mut rng);
    if family > 0 {
        // Stones dropped into the lid: more of them, and closer together, for a bigger
        // capture.
        let (drops, gap) = [(2, 0.055), (4, 0.045), (7, 0.032)][family - 1];
        let mut at = 0.21;
        let mut starts = Vec::with_capacity(drops);
        for _ in 0..drops {
            starts.push(at);
            at += gap * (0.6 + 0.8 * rng.unit());
        }
        out.resize(out.len().max(secs(at + 0.12)), 0.0);
        for (i, start) in starts.into_iter().enumerate() {
            let gain = if i == 0 { 0.5 } else { 0.25 + 0.2 * rng.unit() };
            clink(&mut out, secs(start), CLINK * gain, &mut rng);
        }
    }
    fade_out(&mut out);
    out.iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
        .collect()
}

const RATE: u32 = 48_000;

/// Peak of a placement, about -6 dBFS.
const PEAK: f32 = 0.47;

/// Scale of a lid drop against [`PEAK`]. Set by K-weighted loudness over 30 ms, not by peak:
/// a drop is so short that it peaks well above its loudness, and still sounds 2–4 dB under
/// the placement.
/// At 0.4 the drops sat 15–22 dB under and the capture was lost.
const CLINK: f32 = 2.3;

fn secs(s: f32) -> usize {
    (s * RATE as f32) as usize
}

/// A stone placed on a wooden board, normalised to [`PEAK`]; 0.34 s.
///
/// A port of the "standard" voicing of a Python prototype, the one of three candidates
/// picked by ear: a differenced-noise contact burst, four hard stone partials, four dry
/// board modes, a faint settling contact 16 ms later, all through `tanh(1.2 x)`. The phases
/// are the ones that prototype's seed (1002) drew, so the tone is the one that was chosen;
/// only the noise is this generator's, and white noise sounds the same whichever it is.
fn stone_on_wood(rng: &mut Rng) -> Vec<f32> {
    use std::f64::consts::TAU;

    /// Frequency in Hz, amplitude, decay time constant in seconds, phase.
    const STONE: [(f64, f64, f64, f64); 4] = [
        (1200.0, 0.47, 0.016, 0.870_169_519_063_864_1),
        (1950.0, 0.31, 0.011, 6.165_110_648_193_146),
        (3150.0, 0.16, 0.007, 0.917_952_928_565_037_6),
        (4650.0, 0.05, 0.005, 1.324_842_048_258_834),
    ];
    const BOARD: [(f64, f64, f64, f64); 4] = [
        (215.0, 0.26, 0.080, -0.043_046_907_681_917_324),
        (340.0, 0.19, 0.061, 0.138_363_683_547_996_39),
        (520.0, 0.12, 0.046, 0.072_919_760_439_849_27),
        (760.0, 0.06, 0.034, 0.174_606_063_914_891_3),
    ];
    let settle = secs(0.016);

    let mut last = 0.0;
    let mut out: Vec<f32> = (0..secs(0.34))
        .map(|n| {
            let t = n as f64 / f64::from(RATE);
            let white = rng.normal();
            let mut y = 0.18 * (white - last) * (-t / 0.0009).exp();
            last = white;
            for (f, a, tau, phase) in STONE.into_iter().chain(BOARD) {
                y += a * (TAU * f * t + phase).sin() * (-t / tau).exp();
            }
            if n >= settle {
                let t = (n - settle) as f64 / f64::from(RATE);
                y += 0.025 * rng.normal() * (-t / 0.001).exp();
            }
            (1.2 * y).tanh() as f32
        })
        .collect();
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    for s in &mut out {
        *s *= PEAK / peak;
    }
    out
}

/// One damped partial: frequency in Hz, decay time constant in seconds, amplitude.
type Partial = (f32, f32, f32);

/// A stone dropped onto others in the lid: bright and short, over a little of the lid's wood.
fn clink(out: &mut [f32], at: usize, gain: f32, rng: &mut Rng) {
    const PARTIALS: [Partial; 4] = [
        (520.0, 0.018, 0.22),
        (2300.0, 0.006, 0.24),
        (3700.0, 0.004, 0.15),
        (5000.0, 0.0025, 0.02),
    ];
    strike(out, at, gain, &PARTIALS, (0.0008, 0.12), rng);
}

/// Adds one impact at sample `at`. Every partial is detuned and reweighted a little per
/// impact, so no two strikes in a clip are identical. `noise` is the (time constant,
/// amplitude) of the contact burst, high-passed by differencing white noise.
fn strike(
    out: &mut [f32],
    at: usize,
    gain: f32,
    partials: &[Partial],
    noise: (f32, f32),
    rng: &mut Rng,
) {
    use std::f32::consts::TAU;

    let mut voices = [(0.0f32, 0.0f32, 0.0f32); 4];
    let voices = &mut voices[..partials.len()];
    for (voice, &(freq, tau, amp)) in voices.iter_mut().zip(partials) {
        let freq = freq * (1.0 + 0.04 * rng.signed());
        let amp = amp * (1.0 + 0.15 * rng.signed());
        // Per-sample phase step and decay factor.
        *voice = (
            TAU * freq / RATE as f32,
            (-1.0 / (tau * RATE as f32)).exp(),
            amp,
        );
    }
    let (noise_tau, noise_amp) = noise;
    let noise_decay = (-1.0 / (noise_tau * RATE as f32)).exp();
    // A 1 ms ramp: any sharper and the drop reads as a brittle click.
    let attack = secs(0.001).max(1) as f32;

    let mut env = [1.0f32; 4];
    let mut noise_env = 1.0;
    let mut last = 0.0;
    for (n, sample) in out.iter_mut().skip(at).enumerate() {
        let mut s = 0.0;
        for (e, &(step, decay, amp)) in env.iter_mut().zip(voices.iter()) {
            s += amp * *e * (step * n as f32).sin();
            *e *= decay;
        }
        let white = rng.signed();
        s += noise_amp * noise_env * (white - last);
        last = white;
        noise_env *= noise_decay;
        *sample += gain * s * (n as f32 / attack).min(1.0);
    }
}

/// A 5 ms ramp to silence, so a clip cut short by its buffer never ends on a step.
fn fade_out(out: &mut [f32]) {
    let len = secs(0.005).min(out.len());
    let tail = out.len() - len;
    for (i, s) in out[tail..].iter_mut().enumerate() {
        *s *= 1.0 - i as f32 / len as f32;
    }
}

/// SplitMix64; the clips only need repeatable variety.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in `[-1, 1)`.
    fn signed(&mut self) -> f32 {
        2.0 * self.unit() - 1.0
    }

    /// Standard normal, by Box–Muller.
    fn normal(&mut self) -> f64 {
        let u = 1.0 - (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        let v = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// A mono 16-bit PCM WAV file.
fn wav(samples: &[i16]) -> Vec<u8> {
    let data = u32::try_from(samples.len() * 2).expect("clips are short");
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

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
                id: visit.id,
            })
            .map(|_| visit)
    }
}

fn cursor_visit(state: &AppState) -> (TreeEpoch, Visit) {
    let (epoch, id) = (state.tree_epoch(), state.cursor());
    (epoch, state.with_tree_cached(|tree| Visit::at(tree, id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirai_core::{Color, GameInfo, Point, RuleSet, Size};

    fn step(t: &mut GameTree, from: NodeId, to: NodeId) -> Option<StoneSound> {
        let (from, to) = (Visit::at(t, from), Visit::at(t, to));
        stone_sound(t, from, to)
    }

    /// Only a single step onto a child that put a stone on the board makes a sound, and a
    /// capture is told apart from a placement by the stones it removed.
    #[test]
    fn only_a_single_step_onto_a_stone_sounds() {
        let size = Size::square(9);
        let p = |x, y| size.point(x, y);
        let mut t = GameTree::new(GameInfo::new(size, RuleSet::Chinese));
        let root = t.root();
        // White at the corner, Black surrounds it in two moves.
        let b1 = t.play(root, Color::Black, p(1, 0)).unwrap();
        let w1 = t.play(b1, Color::White, p(0, 0)).unwrap();
        let b2 = t.play(w1, Color::Black, p(0, 1)).unwrap();
        let w2 = t.play(b2, Color::White, Point::PASS).unwrap();

        assert_eq!(step(&mut t, root, b1), Some(StoneSound::Place));
        assert_eq!(step(&mut t, w1, b2), Some(StoneSound::Capture(1)));
        assert_eq!(step(&mut t, b2, w2), None, "a pass is silent");
        assert_eq!(step(&mut t, root, w1), None, "a jump is silent");
        assert_eq!(step(&mut t, b2, w1), None, "going back is silent");

        let setup = t.add_child(w2);
        t.set_setup_stone(setup, p(8, 8), Some(Color::Black));
        assert_eq!(step(&mut t, w2, setup), None, "setup stones are silent");

        // An SGF may record a move on an occupied point; replay ignores it.
        let refused = t.add_child(setup);
        t.node_mut(refused).mv = Some((Color::White, p(1, 0)));
        assert_eq!(
            step(&mut t, setup, refused),
            None,
            "a move the board refused placed nothing"
        );

        // `AB[dd]W[dd]`: the setup changes the board, the move then lands on its stone.
        let mixed = t.add_child(refused);
        t.set_setup_stone(mixed, p(3, 3), Some(Color::Black));
        t.node_mut(mixed).mv = Some((Color::White, p(3, 3)));
        assert_eq!(
            step(&mut t, refused, mixed),
            None,
            "a setup that comes with a move is silent"
        );
    }

    /// Not a test: writes every clip to `$MIRAI_SOUND_DIR` (default `/tmp/mirai-sounds`) for
    /// tuning by ear. `clip0`/`clip1` are the placement's two voices; `clip2`..`clip7` are
    /// captures of 1, 2–4 and 5+ stones, two voices each. See `docs/dev/TESTING.md`.
    #[test]
    #[ignore = "writes WAV files; run by hand when tuning"]
    fn render_clips() {
        let dir = std::env::var("MIRAI_SOUND_DIR").unwrap_or("/tmp/mirai-sounds".into());
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..CLIPS {
            std::fs::write(format!("{dir}/clip{i}.wav"), wav(&render(i))).unwrap();
        }
    }
}
