// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};

use crate::i18n::{gettext, pgettext};

mod imp {

    use super::*;

    #[derive(Default)]
    pub struct MiraiWindow {
        /// The sole owner of this window's mutable Rust state.
        pub ui: RefCell<Option<crate::window::Ui>>,
        pub(super) widgets: WindowWidgets,
        /// Makes shutdown idempotent across close-request and dispose.
        pub closing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MiraiWindow {
        const NAME: &'static str = "MiraiWindow";
        type Type = super::MiraiWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for MiraiWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let window = self.obj();
            window.set_title(Some("mirai"));
            window.set_default_size(1280, 860);
            window.set_content(Some(&self.widgets.toasts));
        }

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

    pub(crate) fn widgets(&self) -> &WindowWidgets {
        &self.imp().widgets
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
}

pub(crate) struct WindowWidgets {
    pub toasts: adw::ToastOverlay,
    pub title: adw::WindowTitle,
    pub header_bar: adw::HeaderBar,
    pub live_toggle: gtk::ToggleButton,
    pub engine_menu: gtk::MenuButton,
    pub engine_content: adw::ButtonContent,
    pub sidebar_toggle: gtk::ToggleButton,
    pub clock_box: gtk::Box,
    pub clock_black: gtk::Label,
    pub clock_white: gtk::Label,
    pub split: adw::OverlaySplitView,
    pub batch_banner: adw::Banner,
    pub board_view: adw::ToolbarView,
    pub nav: gtk::Box,
    pub editor_revealer: gtk::Revealer,
    pub editor_toggle: gtk::ToggleButton,
    pub editor_toolbar: adw::Bin,
    pub play_tool: adw::Toggle,
    pub play_tool_icon: gtk::Image,
    pub stone_tools: adw::ToggleGroup,
    pub mark_tools: adw::ToggleGroup,
    pub content: gtk::Paned,
    pub analysis_stack: adw::ViewStack,
    pub tree_scroller: gtk::ScrolledWindow,
    pub comment: gtk::TextView,
    pub play_bar: gtk::Box,
    pub play_controls: gtk::Box,
    pub undo_button: gtk::Button,
    pub pass_button: gtk::Button,
    pub retry_button: gtk::Button,
    pub resign_button: gtk::Button,
    pub move_scale: gtk::Scale,
    pub move_position: gtk::Label,
}

impl Default for WindowWidgets {
    fn default() -> Self {
        let title = adw::WindowTitle::builder()
            .title(gettext("Untitled"))
            .name("title")
            .build();
        let header_bar = adw::HeaderBar::builder()
            .title_widget(&title)
            .name("header_bar")
            .build();
        let open = adw::SplitButton::builder()
            .icon_name("folder-download-symbolic")
            .tooltip_text(gettext("Download a Public Game Record (Ctrl+Shift+O)"))
            .dropdown_tooltip(gettext("Open a File or Paste SGF"))
            .action_name("win.download-record")
            .menu_model(&open_menu())
            .build();
        open.upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label(&gettext(
                "Download Game Record",
            ))]);
        header_bar.pack_start(&open);
        header_bar.pack_start(&icon_button(
            "document-new-symbolic",
            "win.new-game",
            &gettext("New Game (Ctrl+N)"),
            &gettext("New Game…"),
        ));
        header_bar.pack_start(&gtk::Separator::builder().css_classes(["spacer"]).build());
        let engine_content = adw::ButtonContent::builder()
            .icon_name("applications-engineering-symbolic")
            .label(gettext("No Engine"))
            .can_shrink(true)
            .name("engine_content")
            .build();
        let engine_menu = gtk::MenuButton::builder()
            .child(&engine_content)
            .tooltip_text(gettext("Analysis Engine"))
            .name("engine_menu")
            .build();
        engine_menu.update_property(&[gtk::accessible::Property::Label(&gettext(
            "Analysis Engine",
        ))]);
        header_bar.pack_start(&engine_menu);
        let live_toggle = gtk::ToggleButton::builder()
            .icon_name("media-playback-start-symbolic")
            .action_name("win.toggle-analysis")
            .tooltip_text(gettext("Start Live Analysis (Space)"))
            .name("live_toggle")
            .build();
        live_toggle.update_property(&[gtk::accessible::Property::Label(&gettext("Live Analysis"))]);
        header_bar.pack_start(&live_toggle);
        let main_menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text(gettext("Main Menu"))
            .menu_model(&primary_menu())
            .build();
        main_menu.update_property(&[gtk::accessible::Property::Label(&gettext("Main Menu"))]);
        header_bar.pack_end(&main_menu);
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .action_name("win.toggle-sidebar")
            .tooltip_text(gettext("Hide Sidebar (F9)"))
            .name("sidebar_toggle")
            .build();
        sidebar_toggle.update_property(&[gtk::accessible::Property::Label(&gettext("Sidebar"))]);
        header_bar.pack_end(&sidebar_toggle);

        let tools = adw::WrapBox::builder()
            .child_spacing(12)
            .line_spacing(6)
            .line_homogeneous(true)
            .hexpand(true)
            .build();
        let history = gtk::Box::builder().valign(gtk::Align::Center).build();
        history.update_property(&[gtk::accessible::Property::Label(&gettext("Edit History"))]);
        history.append(&icon_button(
            "edit-undo-symbolic",
            "win.undo",
            &gettext("Undo the Last Edit (Ctrl+Z)"),
            &gettext("Undo"),
        ));
        history.append(&icon_button(
            "edit-redo-symbolic",
            "win.redo",
            &gettext("Redo the Last Edit (Ctrl+Shift+Z)"),
            &gettext("Redo"),
        ));
        tools.append(&history);
        // Switching the side is separate from selecting the Play tool.
        let switch = icon_button(
            "mirai-swap-symbolic",
            "win.switch-to-play",
            &gettext("Switch Side to Play (T)"),
            &gettext("Switch Side to Play"),
        );
        switch.set_valign(gtk::Align::Center);
        switch.add_css_class("flat");
        tools.append(&switch);
        let stone_tools = adw::ToggleGroup::builder()
            .valign(gtk::Align::Center)
            .name("stone_tools")
            .build();
        stone_tools
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label(&gettext("Stone Tools"))]);
        let play_tool_icon = gtk::Image::builder()
            .icon_name("mirai-play-black")
            .pixel_size(24)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .name("play_tool_icon")
            .build();
        let play_tool = adw::Toggle::builder()
            .name("play")
            .label(pgettext("verb", "Play"))
            .child(&play_tool_icon)
            .tooltip(gettext(
                "Play — Left-Click to Play, Right-Click to Take Back",
            ))
            .build();
        stone_tools.add(play_tool.clone());
        stone_tools.add(tool_toggle("setup-black", "mirai-stone-black", &gettext("Setup Black"), Some(&gettext("Setup Black — Left-Click Black, Right-Click White; Matching Color Removes the Stone"))));
        stone_tools.add(tool_toggle("setup-white", "mirai-stone-white", &gettext("Setup White"), Some(&gettext("Setup White — Left-Click White, Right-Click Black; Matching Color Removes the Stone"))));
        tools.append(&stone_tools);
        let mark_tools = adw::ToggleGroup::builder()
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .name("mark_tools")
            .build();
        mark_tools
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label(&gettext("Marks"))]);
        for (name, icon, label) in [
            (
                "triangle",
                "mirai-mark-triangle-symbolic",
                pgettext("mark", "Triangle"),
            ),
            (
                "square",
                "mirai-mark-square-symbolic",
                pgettext("mark", "Square"),
            ),
            (
                "circle",
                "mirai-mark-circle-symbolic",
                pgettext("mark", "Circle"),
            ),
            (
                "cross",
                "mirai-mark-cross-symbolic",
                pgettext("mark", "Cross"),
            ),
            (
                "label",
                "mirai-mark-label-symbolic",
                pgettext("mark", "Label"),
            ),
        ] {
            mark_tools.add(tool_toggle(name, icon, &label, Some(&label)));
        }
        mark_tools.add(tool_toggle(
            "erase-mark",
            "mirai-eraser-symbolic",
            &gettext("Erase Mark"),
            None,
        ));
        tools.append(&mark_tools);
        let editor_toolbar = adw::Bin::builder()
            .child(&tools)
            .css_classes(["toolbar"])
            .name("editor_toolbar")
            .build();
        let editor_revealer = gtk::Revealer::builder()
            .child(&editor_toolbar)
            .reveal_child(true)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .name("editor_revealer")
            .build();
        let batch_banner = adw::Banner::builder()
            .button_label(gettext("Cancel"))
            .revealed(false)
            .name("batch_banner")
            .build();
        let banner_slot = adw::Bin::builder()
            .child(&batch_banner)
            .name("banner_slot")
            .build();
        let content = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .resize_start_child(true)
            .resize_end_child(false)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .hexpand(true)
            .vexpand(true)
            .name("content")
            .build();
        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .name("content_box")
            .build();
        content_box.append(&banner_slot);
        content_box.append(&editor_revealer);
        content_box.append(&content);
        let board_view = adw::ToolbarView::builder()
            .content(&content_box)
            .name("board_view")
            .build();

        let nav = gtk::Box::builder()
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .name("nav")
            .build();
        for (icon, action, tooltip, label) in [
            (
                "go-first-symbolic",
                "win.first",
                gettext("First Move (Home)"),
                gettext("First Move"),
            ),
            (
                "go-previous-symbolic",
                "win.prev",
                gettext("Previous Move (Left)"),
                gettext("Previous Move"),
            ),
            (
                "go-next-symbolic",
                "win.next",
                gettext("Next Move (Right)"),
                gettext("Next Move"),
            ),
            (
                "go-last-symbolic",
                "win.last",
                gettext("Last Move (End)"),
                gettext("Last Move"),
            ),
        ] {
            nav.append(&icon_button(icon, action, &tooltip, &label));
        }
        let branches = gtk::Box::builder().css_classes(["linked"]).build();
        branches.append(&icon_button(
            "go-up-symbolic",
            "win.branch-prev",
            &gettext("Previous Variation (Up)"),
            &gettext("Previous Variation"),
        ));
        branches.append(&icon_button(
            "go-down-symbolic",
            "win.branch-next",
            &gettext("Next Variation (Down)"),
            &gettext("Next Variation"),
        ));
        nav.append(&branches);
        let move_scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .draw_value(false)
            .round_digits(0)
            .digits(0)
            .adjustment(
                &gtk::Adjustment::builder()
                    .lower(0.0)
                    .upper(1.0)
                    .value(0.0)
                    .build(),
            )
            .name("move_scale")
            .build();
        move_scale.update_property(&[gtk::accessible::Property::Label(&gettext("Move Number"))]);
        nav.append(&move_scale);
        let move_position = gtk::Label::builder()
            .width_chars(16)
            .max_width_chars(16)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["mirai-position"])
            .name("move_position")
            .build();
        nav.append(&move_position);
        let editor_toggle = gtk::ToggleButton::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text(gettext("Editing Tools"))
            .action_name("win.toggle-editor")
            .name("editor_toggle")
            .build();
        editor_toggle
            .update_property(&[gtk::accessible::Property::Label(&gettext("Editing Tools"))]);
        nav.append(&editor_toggle);
        let clock_black = gtk::Label::builder()
            .label("–:––")
            .css_classes(["mirai-clock"])
            .name("clock_black")
            .build();
        let clock_white = gtk::Label::builder()
            .label("–:––")
            .css_classes(["mirai-clock"])
            .name("clock_white")
            .build();
        // Clocks belong beside play controls, not in the variable-width header.
        let clock_box = gtk::Box::builder()
            .visible(false)
            .spacing(4)
            .name("clock_box")
            .build();
        clock_box.append(&clock_black);
        clock_box.append(&clock_white);
        let undo_button = gtk::Button::builder()
            .label(gettext("Undo"))
            .action_name("win.undo")
            .tooltip_text(gettext("Undo the Last Turn (Ctrl+Z)"))
            .name("undo_button")
            .build();
        let pass_button = gtk::Button::builder()
            .label(pgettext("verb", "Pass"))
            .action_name("win.pass")
            .tooltip_text(gettext("Pass This Turn (P)"))
            .name("pass_button")
            .build();
        let retry_button = gtk::Button::builder()
            .label(gettext("Retry"))
            .action_name("win.retry-ai")
            .tooltip_text(gettext("Ask the Engine to Move Again"))
            .visible(false)
            .css_classes(["suggested-action"])
            .name("retry_button")
            .build();
        let resign_button = gtk::Button::builder()
            .label(gettext("Resign"))
            .action_name("win.resign")
            .tooltip_text(gettext("Resign the Current Game"))
            .css_classes(["destructive-action"])
            .name("resign_button")
            .build();
        let play_controls = gtk::Box::builder()
            .visible(false)
            .homogeneous(true)
            .spacing(6)
            .name("play_controls")
            .build();
        for button in [&undo_button, &pass_button, &retry_button, &resign_button] {
            play_controls.append(button);
        }
        let play_bar = gtk::Box::builder()
            .visible(false)
            .spacing(12)
            .halign(gtk::Align::Center)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .name("play_bar")
            .build();
        play_bar.append(&clock_box);
        play_bar.append(&play_controls);
        let board_bars = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["toolbar"])
            .name("board_bars")
            .build();
        board_bars.append(&nav);
        board_bars.append(&play_bar);
        board_view.add_bottom_bar(&board_bars);

        let preferences = gtk::Button::builder()
            .label(gettext("Preferences"))
            .action_name("win.preferences")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("application-x-executable-symbolic")
            .title(gettext("No Engine Configured"))
            .description(gettext(
                "Add a local KataGo or a remote mirai-server in Preferences.",
            ))
            .child(&preferences)
            .build();
        let analysis_stack = adw::ViewStack::builder()
            .vexpand(true)
            .name("analysis_stack")
            .build();
        analysis_stack.add_named(&empty, Some("empty"));
        let tree_scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .name("tree_scroller")
            .build();
        let comment = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .buffer(&gtk::TextBuffer::builder().enable_undo(true).build())
            .name("comment")
            .build();
        let comment_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&comment)
            .build();
        let sidebar_stack = adw::ViewStack::builder().name("sidebar_stack").build();
        sidebar_stack.add_titled_with_icon(
            &analysis_stack,
            Some("analysis"),
            &gettext("Analysis"),
            "view-list-symbolic",
        );
        sidebar_stack.add_titled_with_icon(
            &tree_scroller,
            Some("moves"),
            &gettext("Moves"),
            "view-grid-symbolic",
        );
        // Translators: the sidebar tab for the note on this position, not the verb.
        sidebar_stack.add_titled_with_icon(
            &comment_scroll,
            Some("comment"),
            &pgettext("noun", "Comment"),
            "text-editor-symbolic",
        );
        let sidebar_header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .title_widget(
                &adw::InlineViewSwitcher::builder()
                    .stack(&sidebar_stack)
                    .build(),
            )
            .build();
        let sidebar_view = adw::ToolbarView::builder()
            .content(&sidebar_stack)
            .name("sidebar_view")
            .build();
        sidebar_view.add_top_bar(&sidebar_header);
        let split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .show_sidebar(true)
            .content(&board_view)
            .sidebar(&sidebar_view)
            .name("split")
            .build();
        let toolbar = adw::ToolbarView::builder()
            .content(&split)
            .name("toolbar")
            .build();
        toolbar.add_top_bar(&header_bar);
        let toasts = adw::ToastOverlay::new();
        toasts.set_widget_name("toasts");
        toasts.set_child(Some(&toolbar));
        Self {
            toasts,
            title,
            header_bar,
            live_toggle,
            engine_menu,
            engine_content,
            sidebar_toggle,
            clock_box,
            clock_black,
            clock_white,
            split,
            batch_banner,
            board_view,
            nav,
            editor_revealer,
            editor_toggle,
            editor_toolbar,
            play_tool,
            play_tool_icon,
            stone_tools,
            mark_tools,
            content,
            analysis_stack,
            tree_scroller,
            comment,
            play_bar,
            play_controls,
            undo_button,
            pass_button,
            retry_button,
            resign_button,
            move_scale,
            move_position,
        }
    }
}

fn icon_button(icon: &str, action: &str, tooltip: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .action_name(action)
        .tooltip_text(tooltip)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn tool_toggle(name: &str, icon: &str, label: &str, tooltip: Option<&str>) -> adw::Toggle {
    let image = gtk::Image::builder()
        .icon_name(icon)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    let toggle = adw::Toggle::builder()
        .name(name)
        .label(label)
        .child(&image)
        .build();
    if let Some(tooltip) = tooltip {
        toggle.set_tooltip(tooltip);
    }
    toggle
}

fn menu_item(menu: &gio::Menu, label: &str, action: &str, accel: Option<&str>) {
    let item = gio::MenuItem::new(Some(label), Some(action));
    // View-scoped shortcuts are not application accelerators, so supply their hints.
    if let Some(accel) = accel {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    menu.append_item(&item);
}

fn open_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    menu_item(&section, &gettext("_Open File…"), "win.open", None);
    menu_item(
        &section,
        &gettext("_Paste SGF"),
        "win.paste-sgf",
        Some("<Control>v"),
    );
    menu.append_section(None, &section);
    menu
}

fn primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let files = gio::Menu::new();
    menu_item(&files, &gettext("_Clear Board"), "win.clear-board", None);
    menu_item(&files, &gettext("_Save"), "win.save", None);
    menu_item(&files, &gettext("Save _As…"), "win.save-as", None);
    menu_item(
        &files,
        &gettext("_Copy SGF"),
        "win.copy-sgf",
        Some("<Control>c"),
    );
    menu.append_section(None, &files);
    let analysis = gio::Menu::new();
    menu_item(
        &analysis,
        &gettext("Analyse _Game"),
        "win.analyse-game",
        Some("<Control>a"),
    );
    menu_item(&analysis, &gettext("_Estimate Score"), "win.score", None);
    menu.append_section(None, &analysis);
    let view = gio::Menu::new();
    let panels = gio::Menu::new();
    menu_item(&panels, &gettext("_Sidebar"), "win.toggle-sidebar", None);
    menu_item(
        &panels,
        &gettext("_Win-Rate Graph"),
        "win.toggle-graph",
        Some("g"),
    );
    menu_item(
        &panels,
        &gettext("_Editing Tools"),
        "win.toggle-editor",
        None,
    );
    menu_item(
        &panels,
        &gettext("_Loss and Prior Columns"),
        "win.toggle-candidate-details",
        None,
    );
    view.append_section(None, &panels);
    let labels = gio::Menu::new();
    menu_item(
        &labels,
        &gettext("_Coordinates"),
        "win.toggle-coords",
        Some("c"),
    );
    menu_item(
        &labels,
        &gettext("_Move Numbers"),
        "win.toggle-move-numbers",
        Some("n"),
    );
    view.append_section(None, &labels);
    let overlays = gio::Menu::new();
    menu_item(
        &overlays,
        &gettext("_Ownership Overlay"),
        "win.toggle-ownership",
        Some("o"),
    );
    menu_item(
        &overlays,
        &gettext("_Policy Overlay"),
        "win.toggle-policy",
        Some("y"),
    );
    view.append_section(None, &overlays);
    menu.append_submenu(Some(&gettext("_View")), &view);
    let application = gio::Menu::new();
    menu_item(
        &application,
        &gettext("_Preferences"),
        "win.preferences",
        None,
    );
    menu_item(
        &application,
        &gettext("_Keyboard Shortcuts"),
        "win.shortcuts",
        None,
    );
    // Translators: mirai is the application name; do not translate it.
    menu_item(&application, &gettext("_About mirai"), "win.about", None);
    menu.append_section(None, &application);
    menu
}
