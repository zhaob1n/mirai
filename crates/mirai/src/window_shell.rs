// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, gio, glib};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/window.blp")]
    pub struct MiraiWindow {
        /// The sole owner of this window's mutable Rust state.
        pub ui: RefCell<Option<crate::window::Ui>>,
        /// Makes shutdown idempotent across `close-request` and `dispose`.
        pub closing: Cell<bool>,
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub live_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub engine_menu: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub engine_content: TemplateChild<adw::ButtonContent>,
        #[template_child]
        pub sidebar_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub clock_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub clock_black: TemplateChild<gtk::Label>,
        #[template_child]
        pub clock_white: TemplateChild<gtk::Label>,
        #[template_child]
        pub split: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub banner_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub board_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub editor_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub editor_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub editor_toolbar: TemplateChild<adw::Bin>,
        #[template_child]
        pub play_tool: TemplateChild<adw::Toggle>,
        #[template_child]
        pub play_tool_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub stone_tools: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub mark_tools: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub board_menu_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub content: TemplateChild<gtk::Paned>,
        #[template_child]
        pub sidebar_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub play_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub play_controls: TemplateChild<gtk::Box>,
        #[template_child]
        pub undo_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub pass_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub resign_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub move_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub move_position: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MiraiWindow {
        const NAME: &'static str = "MiraiWindow";

        type Type = super::MiraiWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MiraiWindow {
        fn dispose(&self) {
            self.obj().shutdown();
        }
    }
    impl WidgetImpl for MiraiWindow {}
    impl WindowImpl for MiraiWindow {}
    impl ApplicationWindowImpl for MiraiWindow {}
    impl AdwApplicationWindowImpl for MiraiWindow {}
}

glib::wrapper! {
    pub struct MiraiWindow(ObjectSubclass<imp::MiraiWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native,
            gtk::Root, gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

impl MiraiWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    pub(crate) fn install_ui(&self, ui: crate::window::Ui) {
        let old = self.imp().ui.replace(Some(ui));
        debug_assert!(old.is_none(), "window state installed twice");
    }

    pub(crate) fn with_ui<R>(&self, f: impl FnOnce(&crate::window::Ui) -> R) -> Option<R> {
        self.imp().ui.borrow().as_ref().map(f)
    }

    pub(crate) fn take_ui(&self) -> Option<crate::window::Ui> {
        self.imp().ui.borrow_mut().take()
    }

    pub(crate) fn begin_shutdown(&self) -> bool {
        !self.imp().closing.replace(true)
    }

    pub fn toasts(&self) -> adw::ToastOverlay {
        self.imp().toasts.get()
    }

    pub fn title_widget(&self) -> adw::WindowTitle {
        self.imp().title.get()
    }

    pub fn live_toggle(&self) -> gtk::ToggleButton {
        self.imp().live_toggle.get()
    }

    pub fn engine_menu(&self) -> gtk::MenuButton {
        self.imp().engine_menu.get()
    }

    pub fn engine_content(&self) -> adw::ButtonContent {
        self.imp().engine_content.get()
    }

    pub fn sidebar_toggle(&self) -> gtk::ToggleButton {
        self.imp().sidebar_toggle.get()
    }

    pub fn split(&self) -> adw::OverlaySplitView {
        self.imp().split.get()
    }

    pub fn banner_slot(&self) -> gtk::Box {
        self.imp().banner_slot.get()
    }

    pub(crate) fn header_bar(&self) -> adw::HeaderBar {
        self.imp().header_bar.get()
    }

    pub(crate) fn board_view(&self) -> adw::ToolbarView {
        self.imp().board_view.get()
    }

    pub(crate) fn editor_revealer(&self) -> gtk::Revealer {
        self.imp().editor_revealer.get()
    }

    pub(crate) fn editor_toggle(&self) -> gtk::ToggleButton {
        self.imp().editor_toggle.get()
    }

    pub(crate) fn editor_toolbar(&self) -> adw::Bin {
        self.imp().editor_toolbar.get()
    }

    pub(crate) fn play_tool(&self) -> adw::Toggle {
        self.imp().play_tool.get()
    }

    pub(crate) fn play_tool_icon(&self) -> gtk::Image {
        self.imp().play_tool_icon.get()
    }

    pub(crate) fn stone_tools(&self) -> adw::ToggleGroup {
        self.imp().stone_tools.get()
    }

    pub(crate) fn mark_tools(&self) -> adw::ToggleGroup {
        self.imp().mark_tools.get()
    }

    pub(crate) fn board_menu_button(&self) -> gtk::Button {
        self.imp().board_menu_button.get()
    }

    pub fn content_paned(&self) -> gtk::Paned {
        self.imp().content.get()
    }

    pub fn sidebar_stack(&self) -> adw::ViewStack {
        self.imp().sidebar_stack.get()
    }

    pub fn clock_box(&self) -> gtk::Box {
        self.imp().clock_box.get()
    }

    pub fn clock_black(&self) -> gtk::Label {
        self.imp().clock_black.get()
    }

    pub fn clock_white(&self) -> gtk::Label {
        self.imp().clock_white.get()
    }

    pub fn play_bar(&self) -> gtk::Box {
        self.imp().play_bar.get()
    }

    pub fn play_controls(&self) -> gtk::Box {
        self.imp().play_controls.get()
    }

    pub fn undo_button(&self) -> gtk::Button {
        self.imp().undo_button.get()
    }

    pub fn pass_button(&self) -> gtk::Button {
        self.imp().pass_button.get()
    }

    pub fn resign_button(&self) -> gtk::Button {
        self.imp().resign_button.get()
    }

    pub fn move_scale(&self) -> gtk::Scale {
        self.imp().move_scale.get()
    }

    pub fn move_position(&self) -> gtk::Label {
        self.imp().move_position.get()
    }
}
