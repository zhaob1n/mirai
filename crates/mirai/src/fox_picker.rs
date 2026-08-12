// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};
type OpenHandler = Box<dyn Fn(crate::fox::DownloadedGame)>;

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
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub loading_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub results_page: TemplateChild<gtk::Box>,
        #[template_child]
        pub result_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub result_list: TemplateChild<gtk::ListBox>,
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

    pub(crate) fn clear_games(&self) {
        self.imp().games.borrow_mut().clear();
    }

    pub(crate) fn replace_games(&self, games: Vec<crate::fox::FoxGame>) {
        *self.imp().games.borrow_mut() = games;
    }

    pub(crate) fn game(&self, index: usize) -> Option<crate::fox::FoxGame> {
        self.imp().games.borrow().get(index).cloned()
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
    pub stack: gtk::Stack,
    pub status_page: adw::StatusPage,
    pub loading_page: adw::StatusPage,
    pub results_page: gtk::Box,
    pub result_label: gtk::Label,
    pub result_list: gtk::ListBox,
}
