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
//! content is drawn under shows both, and a tick callback follows it while it may move. An
//! open starts scaled. A close starts at rest, when libadwaita emits `AdwDialog::closed`,
//! and its first frame may still be at rest; the watch redraws every frame until the shrink
//! shows, so the paint that first scales takes the texture rather than the live text. A
//! bottom sheet slides and a reduced-motion sheet fades: neither scales, and both are drawn
//! live.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

/// Frames a close may take to start shrinking before the sheet is taken not to scale.
const SETTLE: u8 = 3;
/// Device pixels the scale left in an open may move the dialog's edges by, once the spring
/// has turned back from its overshoot, for its texture to be drawn at scale 1. The content
/// is then off the sheet by that much at most, and less as the spring settles: libadwaita's
/// undershoot is under a tenth of its overshoot. Latched on the way up, a dialog would ride
/// the overshoot, 1.7 % of its size, off the sheet.
const TAIL_PX: f32 = 3.0;

/// How far a close has got.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Close {
    #[default]
    No,
    /// `closed` was emitted, and the sheet has not shrunk in this many frames.
    Starting(u8),
    /// The sheet is shrinking, drawn from the texture it began with.
    Running,
}

/// How far an open's spring has got.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Open {
    #[default]
    Rising,
    /// Past the peak of its overshoot, heading back to scale 1.
    Turned,
    /// Near enough to rest to be drawn at scale 1, as it is until rest.
    Tail,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SheetTexture {
        pub(super) close: Cell<Close>,
        /// Whether the last snapshot drew a texture rather than the child.
        pub(super) textured: Cell<bool>,
        /// The texture a close draws throughout, with its bounds.
        pub(super) frozen: RefCell<Option<(gdk::Texture, graphene::Rect)>>,
        /// The sheet's scale at the last snapshot, 0 before the first.
        pub(super) scale: Cell<f32>,
        /// How far an open has got.
        pub(super) open: Cell<Open>,
        /// Follows the sheet's scale while it may be animating.
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
            self.rest();
            self.textured.set(false);
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let drawn = self.sheet_scale().and_then(|scale| {
                let tail = self.in_tail(scale);
                self.texture(tail)
                    .map(|texture| (texture, tail.then_some(scale)))
            });
            self.textured.set(drawn.is_some());
            let Some(((texture, bounds), tail)) = drawn else {
                self.parent_snapshot(snapshot);
                return;
            };
            match tail {
                Some(scale) => {
                    let obj = self.obj();
                    let (x, y) = (obj.width() as f32 / 2.0, obj.height() as f32 / 2.0);
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(x, y));
                    snapshot.scale(1.0 / scale, 1.0 / scale);
                    snapshot.translate(&graphene::Point::new(-x, -y));
                    snapshot.append_texture(&texture, &bounds);
                    snapshot.restore();
                }
                None => snapshot.append_texture(&texture, &bounds),
            }
            self.watch();
        }
    }

    impl SheetTexture {
        pub(super) fn begin_close(&self) {
            self.close.set(Close::Starting(0));
            self.watch();
        }

        /// The scale the widget is drawn under, unless that is 1. Exact: at rest the
        /// sheet's transform is translations only, and its scale is a product of ones.
        fn sheet_scale(&self) -> Option<f32> {
            let obj = self.obj();
            let m = obj.root().and_then(|root| obj.compute_transform(&root))?;
            (m.value(0, 0) != 1.0 || m.value(1, 1) != 1.0).then(|| m.value(0, 0))
        }

        fn device_scale(&self) -> Option<f64> {
            Some(self.obj().native()?.surface()?.scale())
        }

        /// Whether an open, now at `scale`, is in its tail.
        fn in_tail(&self, scale: f32) -> bool {
            let last = self.scale.replace(scale);
            if self.close.get() != Close::No {
                return false;
            }
            match self.open.get() {
                Open::Rising if last > 1.0 && scale < last => self.open.set(Open::Turned),
                Open::Turned => {
                    let obj = self.obj();
                    let reach = obj.width().max(obj.height()) as f32 / 2.0
                        * self.device_scale().unwrap_or(1.0) as f32;
                    if (scale - 1.0).abs() * reach < TAIL_PX {
                        self.open.set(Open::Tail);
                    }
                }
                _ => {}
            }
            self.open.get() == Open::Tail
        }

        fn texture(&self, tail: bool) -> Option<(gdk::Texture, graphene::Rect)> {
            if self.close.get() == Close::No {
                return self.render(tail);
            }
            self.close.set(Close::Running);
            if self.frozen.borrow().is_none() {
                let rendered = self.render(false);
                self.frozen.replace(rendered);
            }
            self.frozen.borrow().clone()
        }

        /// Where the widget's origin falls inside a device pixel this frame: under the
        /// sheet's transform, or, in an open's tail, at scale 1 about the widget's centre.
        fn phase(&self, tail: bool) -> Option<(f32, f32)> {
            let obj = self.obj();
            let native = obj.native()?;
            let scale = self.device_scale()?;
            let (dx, dy) = native.surface_transform();
            let anchor = if tail {
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
        fn render(&self, tail: bool) -> Option<(gdk::Texture, graphene::Rect)> {
            let _t = crate::render_probe::Timer::new("sheet-texture");
            let obj = self.obj();
            let renderer = obj.native()?.renderer()?;
            let scale = self.device_scale()? as f32;
            let child = obj.first_child()?;
            let (x, y) = self.phase(tail)?;
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

        /// One frame of the watch. It runs before layout, so the transform it reads is the
        /// last frame's: a change shows here a frame late and is drawn on the next.
        ///
        /// An open redraws every frame, so each frame's texture is rendered after that
        /// frame's layout, at its [`phase`](Self::phase): the sheet moves the content by a
        /// fraction of a pixel a frame, and the texture of the frame the spring comes to rest
        /// on must already lie on the surface's pixels.
        fn follow(&self) -> glib::ControlFlow {
            if let Close::Starting(frames) = self.close.get()
                && frames < SETTLE
            {
                // The layout that first shrinks the sheet runs after this; only a pending
                // draw has that frame's paint take the texture.
                self.close.set(Close::Starting(frames + 1));
                self.obj().queue_draw();
                return glib::ControlFlow::Continue;
            }
            let scaled = self.sheet_scale().is_some();
            if scaled != self.textured.get() || (scaled && self.close.get() == Close::No) {
                self.obj().queue_draw();
            }
            if scaled {
                return glib::ControlFlow::Continue;
            }
            // At rest: an open has ended, a close was taken back by a new present, or the
            // sheet does not scale at all.
            self.rest();
            self.watch.take();
            glib::ControlFlow::Break
        }

        fn rest(&self) {
            self.close.set(Close::No);
            self.frozen.take();
            self.scale.set(0.0);
            self.open.set(Open::Rising);
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
