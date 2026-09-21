// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The download dialog's fixed hierarchy, and the model behind its result list.
//!
//! Fox answers with up to 200 records. They live in a `GListModel` behind a
//! `GtkListView`, so only the rows on screen are widgets: building all 200 as
//! `AdwActionRow`s cost ~270 ms of layout in the frame that showed the dialog.

use std::cell::{Cell, OnceCell, RefCell};

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{CompositeTemplate, gio, glib};
type OpenHandler = Box<dyn Fn(crate::fox::DownloadedGame)>;

mod row_imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::FoxRow)]
    pub struct FoxRow {
        /// Both players with their ranks.
        #[property(get, set)]
        pub title: RefCell<String>,
        /// Date, board size, length and result.
        #[property(get, set)]
        pub subtitle: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FoxRow {
        const NAME: &'static str = "MiraiFoxRow";
        type Type = super::FoxRow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for FoxRow {}
}

glib::wrapper! {
    /// One record as the list shows it. The record itself stays in `games`.
    pub struct FoxRow(ObjectSubclass<row_imp::FoxRow>);
}

impl FoxRow {
    pub(crate) fn new(title: &str, subtitle: &str) -> FoxRow {
        glib::Object::builder()
            .property("title", title)
            .property("subtitle", subtitle)
            .build()
    }
}

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(file = "src/fox_picker.blp")]
    pub struct FoxPickerDialog {
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub open_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub search_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub loading_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub results_page: TemplateChild<gtk::Box>,
        #[template_child]
        pub result_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub result_list: TemplateChild<gtk::ListView>,
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) selection: OnceCell<gtk::SingleSelection>,
        pub(super) games: RefCell<Vec<crate::fox::FoxGame>>,
        pub(super) busy: Cell<bool>,
        pub(super) task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) on_open: RefCell<Option<OpenHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FoxPickerDialog {
        const NAME: &'static str = "MiraiFoxPickerDialog";

        type Type = super::FoxPickerDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for FoxPickerDialog {
        fn constructed(&self) {
            self.parent_constructed();
            if let Some(paintable) = self.loading_page.paintable()
                && let Ok(spinner) = paintable.downcast::<adw::SpinnerPaintable>()
            {
                spinner.set_widget(Some(self.loading_page.upcast_ref::<gtk::Widget>()));
            }
            self.obj().install_model();
        }

        fn dispose(&self) {
            if let Some(task) = self.task.borrow_mut().take() {
                task.abort();
            }
            self.on_open.borrow_mut().take();
        }
    }
    impl WidgetImpl for FoxPickerDialog {}
    impl AdwDialogImpl for FoxPickerDialog {}
}

glib::wrapper! {
    pub struct FoxPickerDialog(ObjectSubclass<imp::FoxPickerDialog>)
        @extends gtk::Widget, adw::Dialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::ShortcutManager;
}

impl FoxPickerDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Gives the result list its model and its row factory. Called once, from
    /// `constructed`, so every accessor below can rely on both being there.
    fn install_model(&self) {
        let imp = self.imp();
        let store = gio::ListStore::new::<FoxRow>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        selection.set_selected(gtk::INVALID_LIST_POSITION);
        imp.result_list.set_model(Some(&selection));
        imp.result_list.set_factory(Some(&row_factory()));
        let _ = imp.store.set(store);
        let _ = imp.selection.set(selection);
    }

    pub fn widgets(&self) -> FoxPickerWidgets {
        let imp = self.imp();
        FoxPickerWidgets {
            cancel_button: imp.cancel_button.get(),
            open_button: imp.open_button.get(),
            entry: imp.entry.get(),
            search_button: imp.search_button.get(),
            banner: imp.banner.get(),
            stack: imp.stack.get(),
            status_page: imp.status_page.get(),
            loading_page: imp.loading_page.get(),
            results_page: imp.results_page.get(),
            result_label: imp.result_label.get(),
            result_list: imp.result_list.get(),
        }
    }

    pub(crate) fn install_handler(&self, handler: impl Fn(crate::fox::DownloadedGame) + 'static) {
        *self.imp().on_open.borrow_mut() = Some(Box::new(handler));
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.imp().busy.get()
    }

    pub(crate) fn set_busy_flag(&self, busy: bool) {
        self.imp().busy.set(busy);
    }

    fn store(&self) -> &gio::ListStore {
        self.imp().store.get().expect("the model is installed")
    }

    fn selection(&self) -> &gtk::SingleSelection {
        self.imp().selection.get().expect("the model is installed")
    }

    pub(crate) fn clear_games(&self) {
        self.imp().games.borrow_mut().clear();
        self.store().remove_all();
    }

    /// Installs `games` as the list, `rows` being how they read on screen.
    pub(crate) fn replace_games(&self, games: Vec<crate::fox::FoxGame>, rows: &[FoxRow]) {
        *self.imp().games.borrow_mut() = games;
        let store = self.store();
        store.splice(0, store.n_items(), rows);
        self.selection().set_selected(if rows.is_empty() {
            gtk::INVALID_LIST_POSITION
        } else {
            0
        });
    }

    /// The record behind the selected row, if any is selected.
    pub(crate) fn selected_game(&self) -> Option<crate::fox::FoxGame> {
        let selected = self.selection().selected();
        if selected == gtk::INVALID_LIST_POSITION {
            return None;
        }
        self.imp().games.borrow().get(selected as usize).cloned()
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection().selected() != gtk::INVALID_LIST_POSITION
    }

    pub(crate) fn has_games(&self) -> bool {
        !self.imp().games.borrow().is_empty()
    }

    pub(crate) fn abort_task(&self) {
        if let Some(task) = self.imp().task.take() {
            task.abort();
        }
    }

    pub(crate) fn connect_selection_changed(&self, f: impl Fn() + 'static) {
        self.selection().connect_selected_notify(move |_| f());
    }

    pub(crate) fn replace_task(&self, task: glib::JoinHandle<()>) {
        if let Some(old) = self.imp().task.replace(Some(task)) {
            old.abort();
        }
    }

    pub(crate) fn open_game(&self, game: crate::fox::DownloadedGame) {
        if let Some(handler) = self.imp().on_open.borrow().as_ref() {
            handler(game);
        }
    }
}

/// Two lines and a chevron, bound to the row's properties by expression so the
/// factory needs no bind handler of its own.
fn row_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        lines.append(&title);
        lines.append(&subtitle);

        let chevron = gtk::Image::from_icon_name("go-next-symbolic");
        chevron.add_css_class("dim-label");

        let row = gtk::Box::builder()
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(10)
            .margin_bottom(10)
            .build();
        row.append(&lines);
        row.append(&chevron);

        item.property_expression("item")
            .chain_property::<FoxRow>("title")
            .bind(&title, "label", gtk::Widget::NONE);
        item.property_expression("item")
            .chain_property::<FoxRow>("subtitle")
            .bind(&subtitle, "label", gtk::Widget::NONE);
        item.set_child(Some(&row));
    });
    factory
}

impl Default for FoxPickerDialog {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FoxPickerWidgets {
    pub cancel_button: gtk::Button,
    pub open_button: gtk::Button,
    pub entry: gtk::SearchEntry,
    pub search_button: gtk::Button,
    pub banner: adw::Banner,
    pub stack: adw::ViewStack,
    pub status_page: adw::StatusPage,
    pub loading_page: adw::StatusPage,
    pub results_page: gtk::Box,
    pub result_label: gtk::Label,
    pub result_list: gtk::ListView,
}
