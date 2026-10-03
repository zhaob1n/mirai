// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Draws a dialog's content from a texture while libadwaita scales the dialog.
//!
//! A floating sheet opens and closes by scaling from 0.8 (`adw-floating-sheet.c`). GSK's
//! GPU renderer draws text under that scale at the scale itself and keys its glyph cache
//! by it, so each frame of the animation rasterised every glyph of the dialog again
//! (`RENDERING.md` §8). While the sheet scales, [`SheetTexture`] renders its child at the
//! surface's own scale, whose glyphs are cached, and hands GSK that texture to scale
//! instead. Ptyxis does the same for its tab overview.
//!
//! An opening dialog renders the texture again every frame. What it shows is what the user
//! is about to read, and its content changes as it opens: scrollbars fade in, the Fox picker
//! fills its rows. Each frame's texture is laid on the surface's pixels where that frame
//! puts it, and once the spring is in its tail — back from its overshoot, with what scale
//! remains moving the dialog's edges by under [`TAIL_PX`] — it is drawn at scale 1 about
//! the dialog's centre. Resampled under a scale of 0.998, text is softer than live text,
//! and it sharpened only as libadwaita snapped the spring to rest: a pop just as the motion
//! ended. A closing dialog keeps the texture it began with. Its scrollbars and focus rings
//! still fade as it goes, and rendering them again cost a millisecond or more a frame for
//! nothing.
//!
//! The sheet reports neither the start of an animation nor its end, but the transform the
//! content is drawn under shows both, and a tick callback follows it while it moves. An
//! open starts scaled. A close starts at rest, when libadwaita emits `AdwDialog::closed`,
//! and its first frame may still be at rest; the watch redraws every frame until the shrink
//! shows, so the paint that first scales takes the texture rather than the live text. Rest
//! is scale 1, or any scale that holds for [`HOLD`] frames, so a sheet that came to rest
//! scaled stops the watch too. A bottom sheet slides and a reduced-motion sheet fades:
//! neither scales, and both are drawn live.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

/// Frames a close may take to start shrinking before the sheet is taken not to scale.
const SETTLE: u8 = 3;
/// Frames a scale other than 1 must hold to count as rest. The watch reads each frame's
/// scale a frame late, so the frame an open starts on reads as held once, and a present
/// whose spring's first step is zero holds a second.
const HOLD: u8 = 4;
/// Device pixels the scale left in an open may move the dialog's edges by, once the spring
/// has turned back from its overshoot, for its texture to be drawn at scale 1. The content
/// is then off the sheet by that much at most, and less as the spring settles: libadwaita's
/// undershoot is under a tenth of its overshoot. Latched on the way up, a dialog would ride
/// the overshoot, 1.7 % of its size, off the sheet.
const TAIL_PX: f32 = 3.0;

/// What the sheet is doing, as far as the transform it draws the widget under shows.
enum State {
    /// Holding at `scale`, 1 at rest: the child is drawn live.
    Still { scale: f32 },
    /// Opening, a texture rendered each frame.
    Opening(Track, Stage),
    /// `closed` was emitted at `scale`, and the sheet has not moved from it in `frames`.
    CloseStarting { scale: f32, frames: u8 },
    /// Closing, drawn from the texture it began with.
    Closing(Track, gdk::Texture, graphene::Rect),
}

impl Default for State {
    fn default() -> Self {
        State::Still { scale: 1.0 }
    }
}

/// The sheet's scale as the watch last read it, and how many frames it has held.
#[derive(Clone, Copy)]
struct Track {
    scale: f32,
    held: u8,
}

impl Track {
    fn new(scale: f32) -> Self {
        Track { scale, held: 0 }
    }

    /// The track at `scale` a frame on, or `None` once the sheet is at rest.
    fn follow(self, scale: f32) -> Option<Self> {
        let held = if scale == self.scale {
            self.held + 1
        } else {
            0
        };
        (scale != 1.0 && held < HOLD).then_some(Track { scale, held })
    }
}

/// How far an open's spring has got.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Rising,
    /// Past the peak of its overshoot, heading back to scale 1.
    Turned,
    /// Near enough to rest to be drawn at scale 1, as it is until rest.
    Tail,
}

/// What a snapshot draws.
enum Draw {
    Live,
    /// The texture, under the sheet's scale.
    Scaled(gdk::Texture, graphene::Rect),
    /// The texture at scale 1 about the widget's centre, undoing the sheet's scale, given.
    Unscaled(gdk::Texture, graphene::Rect, f32),
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SheetTexture {
        pub(super) state: Cell<State>,
        /// Follows the sheet's scale while it moves.
        pub(super) watch: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SheetTexture {
        const NAME: &'static str = "MiraiSheetTexture";
        type Type = super::SheetTexture;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for SheetTexture {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SheetTexture {
        fn unmap(&self) {
            if let Some(watch) = self.watch.take() {
                watch.remove();
            }
            self.state.take();
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let scale = self.sheet_scale();
            let (state, draw) = self.draw(self.state.take(), scale);
            self.state.set(state);
            match draw {
                Draw::Live => self.parent_snapshot(snapshot),
                Draw::Scaled(texture, bounds) => snapshot.append_texture(&texture, &bounds),
                Draw::Unscaled(texture, bounds, scale) => {
                    let obj = self.obj();
                    let (x, y) = (obj.width() as f32 / 2.0, obj.height() as f32 / 2.0);
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(x, y));
                    snapshot.scale(1.0 / scale, 1.0 / scale);
                    snapshot.translate(&graphene::Point::new(-x, -y));
                    snapshot.append_texture(&texture, &bounds);
                    snapshot.restore();
                }
            }
        }
    }

    impl SheetTexture {
        pub(super) fn begin_close(&self) {
            let scale = self.sheet_scale();
            self.state.set(State::CloseStarting { scale, frames: 0 });
            self.watch();
        }

        /// The scale the widget is drawn under, which the sheet applies uniformly; 1 at
        /// rest, exactly, as the sheet's transform is then translations only.
        fn sheet_scale(&self) -> f32 {
            let obj = self.obj();
            obj.root()
                .and_then(|root| obj.compute_transform(&root))
                .map_or(1.0, |m| m.value(0, 0))
        }

        fn device_scale(&self) -> Option<f64> {
            Some(self.obj().native()?.surface()?.scale())
        }

        /// What to draw at `scale` from `state`, and the state after.
        fn draw(&self, state: State, scale: f32) -> (State, Draw) {
            if scale == 1.0 {
                return (state, Draw::Live);
            }
            match state {
                State::Still { scale: held } if held == scale => (state, Draw::Live),
                State::Still { .. } => {
                    self.watch();
                    let state = State::Opening(Track::new(scale), Stage::Rising);
                    (state, self.render(false, scale))
                }
                State::Opening(track, stage) => {
                    let stage = self.advance(stage, track.scale, scale);
                    let draw = self.render(stage == Stage::Tail, scale);
                    (State::Opening(track, stage), draw)
                }
                State::CloseStarting { .. } => match self.texture(false) {
                    Some((texture, bounds)) => {
                        let draw = Draw::Scaled(texture.clone(), bounds);
                        (State::Closing(Track::new(scale), texture, bounds), draw)
                    }
                    None => (state, Draw::Live),
                },
                State::Closing(_, ref texture, bounds) => {
                    let draw = Draw::Scaled(texture.clone(), bounds);
                    (state, draw)
                }
            }
        }

        /// The child rendered at the surface's scale onto the surface's pixel grid, drawn
        /// under the sheet's `scale` or, `unscaled`, at scale 1.
        fn render(&self, unscaled: bool, scale: f32) -> Draw {
            match self.texture(unscaled) {
                Some((texture, bounds)) if unscaled => Draw::Unscaled(texture, bounds, scale),
                Some((texture, bounds)) => Draw::Scaled(texture, bounds),
                None => Draw::Live,
            }
        }

        /// Where the widget's origin falls inside a device pixel this frame: under the
        /// sheet's transform, or, `unscaled`, at scale 1 about the widget's centre.
        fn phase(&self, unscaled: bool) -> Option<(f32, f32)> {
            let obj = self.obj();
            let native = obj.native()?;
            let scale = self.device_scale()?;
            let (dx, dy) = native.surface_transform();
            let anchor = if unscaled {
                graphene::Point::new(obj.width() as f32 / 2.0, obj.height() as f32 / 2.0)
            } else {
                graphene::Point::zero()
            };
            let at = obj.compute_point(&native, &anchor)?;
            let x = (f64::from(at.x()) + dx - f64::from(anchor.x())) * scale;
            let y = (f64::from(at.y()) + dy - f64::from(anchor.y())) * scale;
            Some(((x - x.floor()) as f32, (y - y.floor()) as f32))
        }

        /// The child rendered at the surface's scale, and the rectangle that lays its pixels
        /// on the surface's. The render is offset by the widget's [`phase`](Self::phase): GSK
        /// snaps each glyph's baseline to a whole pixel of whatever it renders to, so a
        /// texture rendered from a pixel corner and drawn half a pixel off put its text 0.3
        /// px high and blurred, and the text jumped when the dialog went back to live.
        fn texture(&self, unscaled: bool) -> Option<(gdk::Texture, graphene::Rect)> {
            let _t = crate::render_probe::Timer::new("sheet-texture");
            let obj = self.obj();
            let renderer = obj.native()?.renderer()?;
            let scale = self.device_scale()? as f32;
            let child = obj.first_child()?;
            let (x, y) = self.phase(unscaled)?;
            let shot = gtk::Snapshot::new();
            shot.translate(&graphene::Point::new(x, y));
            shot.scale(scale, scale);
            obj.snapshot_child(&child, &shot);
            let node = shot.to_node()?;
            let width = (obj.width() as f32 * scale + x).ceil();
            let height = (obj.height() as f32 * scale + y).ceil();
            let texture =
                renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, width, height)));
            let bounds = graphene::Rect::new(-x / scale, -y / scale, width / scale, height / scale);
            Some((texture, bounds))
        }

        fn watch(&self) {
            if self.watch.borrow().is_some() {
                return;
            }
            let watch = self.obj().add_tick_callback(|obj, _| obj.imp().follow());
            self.watch.replace(Some(watch));
        }

        /// One frame of the watch. It runs before layout, so the scale it reads is the last
        /// frame's: a change shows here a frame late and is drawn on the next.
        ///
        /// An open redraws every frame, so each frame's texture is rendered after that
        /// frame's layout, at its [`phase`](Self::phase): the sheet moves the content by a
        /// fraction of a pixel a frame, and the texture of the frame the spring comes to rest
        /// on must already lie on the surface's pixels.
        fn follow(&self) -> glib::ControlFlow {
            let scale = self.sheet_scale();
            let (state, redraw) = match self.state.take() {
                // The layout that first shrinks the sheet runs after this; only a pending
                // draw has that frame's paint take the texture.
                State::CloseStarting { scale, frames } if frames < SETTLE => {
                    let frames = frames + 1;
                    (State::CloseStarting { scale, frames }, true)
                }
                // A close that never shrank: the sheet does not scale.
                State::CloseStarting { scale, .. } => (State::Still { scale }, false),
                State::Opening(track, stage) => match track.follow(scale) {
                    Some(next) => (State::Opening(next, stage), true),
                    None => (State::Still { scale }, true),
                },
                State::Closing(track, texture, bounds) => match track.follow(scale) {
                    Some(next) => (State::Closing(next, texture, bounds), false),
                    // Taken back by a new present, or stopped.
                    None => (State::Still { scale }, true),
                },
                still @ State::Still { .. } => (still, false),
            };
            let moving = !matches!(state, State::Still { .. });
            self.state.set(state);
            if redraw {
                self.obj().queue_draw();
            }
            if moving {
                return glib::ControlFlow::Continue;
            }
            self.watch.take();
            glib::ControlFlow::Break
        }

        /// The stage an open at `scale` has reached from `stage`, `last` the frame before.
        /// The snapshot asks, with this frame's scale; the watch, before layout, has only
        /// stored the last frame's, and deciding there would draw the tail a frame late.
        fn advance(&self, stage: Stage, last: f32, scale: f32) -> Stage {
            match stage {
                Stage::Rising if last > 1.0 && scale < last => Stage::Turned,
                Stage::Turned => {
                    let obj = self.obj();
                    let reach = obj.width().max(obj.height()) as f32 / 2.0
                        * self.device_scale().unwrap_or(1.0) as f32;
                    if (scale - 1.0).abs() * reach < TAIL_PX {
                        Stage::Tail
                    } else {
                        stage
                    }
                }
                _ => stage,
            }
        }
    }
}

glib::wrapper! {
    pub struct SheetTexture(ObjectSubclass<imp::SheetTexture>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Puts `dialog`'s child under a [`SheetTexture`]. Call once, before the dialog is first
/// presented; libadwaita's own dialogs take it too, as their content is `AdwDialog:child`.
pub fn install(dialog: &impl IsA<adw::Dialog>) {
    let dialog = dialog.upcast_ref::<adw::Dialog>();
    if crate::render_probe::no_sheet_texture() {
        return;
    }
    let Some(child) = dialog.child() else {
        return;
    };
    let cache: SheetTexture = glib::Object::new();
    dialog.set_child(None::<&gtk::Widget>);
    child.set_parent(&cache);
    dialog.set_child(Some(&cache));
    dialog.connect_closed(glib::clone!(
        #[weak]
        cache,
        move |_| cache.imp().begin_close()
    ));
}
