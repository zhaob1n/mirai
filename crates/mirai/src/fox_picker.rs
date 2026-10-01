// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The download dialog's fixed hierarchy, and the model behind its result list.
//!
//! Fox answers with up to 200 records, and every row a list widget keeps alive is a
//! widget to build, bind, measure and style. Two GTK facts decide how few that can be:
//!
//! - `GtkListView` keeps up to `GTK_LIST_VIEW_MAX_LIST_ITEMS` (200) rows around its anchor
//!   whatever its height, so it builds every Fox record: 130–220 ms of layout in the frame
//!   that showed the dialog, and 30–60 ms re-measuring them on every later open. A
//!   one-column `GtkGridView` keeps `GTK_GRID_VIEW_MAX_VISIBLE_ROWS` (30) plus three, so
//!   the list is a grid of one column. The price is its roles: assistive technologies
//!   meet a one-column grid, whose cells carry each record as their label.
//! - Both drop their factory while unrooted and rebind every live row, synchronously, when
//!   the dialog is presented again. So the store is emptied before each presentation and
//!   refilled a few rows per frame once the dialog is on screen ([`FEED_PER_FRAME`]); past
//!   the rows a grid keeps alive, the rest go in at once, since no widget is built for them.

use std::cell::{Cell, OnceCell, RefCell};

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{CompositeTemplate, gio, glib};
type OpenHandler = Box<dyn Fn(crate::fox::DownloadedGame)>;

/// Rows a one-column `GtkGridView` builds widgets for: 30 around the anchor, one more on
/// either side, and the anchor itself.
const LIVE_ROWS: usize = 33;
/// Rows bound per frame while the list fills. A row with a CJK title costs up to a
/// millisecond to bind and measure; four keep the fill inside a 144 Hz frame.
const FEED_PER_FRAME: usize = 4;

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
        pub result_list: TemplateChild<gtk::GridView>,
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) selection: OnceCell<gtk::SingleSelection>,
        pub(super) games: RefCell<Vec<crate::fox::FoxGame>>,
        /// How each record in `games` reads on screen. The store holds a prefix of these
        /// while it fills, all of them once it is full.
        pub(super) rows: RefCell<Vec<FoxRow>>,
        /// The row to select once the store holds it: the user's last choice.
        pub(super) wanted: Cell<Option<u32>>,
        /// Set while the dialog itself moves the selection — emptying the store, restoring
        /// the user's choice — so the notify does not record it as the user's.
        pub(super) ours: Cell<bool>,
        pub(super) feed: RefCell<Option<gtk::TickCallbackId>>,
        /// Presented and not yet closing.
        pub(super) shown: Cell<bool>,
        pub(super) busy: Cell<bool>,
        pub(super) task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) on_open: RefCell<Option<OpenHandler>>,
        /// The application runtime: the last-search file is read and written on its
        /// blocking pool (INV-11).
        pub(super) runtime: OnceCell<tokio::runtime::Handle>,
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
        let weak = self.downgrade();
        selection.connect_selected_notify(move |selection| {
            if let Some(dialog) = weak.upgrade()
                && !dialog.imp().ours.get()
            {
                let selected = selection.selected();
                dialog
                    .imp()
                    .wanted
                    .set((selected != gtk::INVALID_LIST_POSITION).then_some(selected));
            }
        });
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

    pub(crate) fn set_runtime(&self, runtime: tokio::runtime::Handle) {
        let _ = self.imp().runtime.set(runtime);
    }

    pub(crate) fn runtime(&self) -> &tokio::runtime::Handle {
        self.imp().runtime.get().expect("present sets the runtime")
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.imp().busy.get()
    }

    pub(crate) fn set_busy_flag(&self, busy: bool) {
        self.imp().busy.set(busy);
    }

    pub(crate) fn is_shown(&self) -> bool {
        self.imp().shown.get()
    }

    pub(crate) fn set_shown(&self, shown: bool) {
        self.imp().shown.set(shown);
    }

    fn store(&self) -> &gio::ListStore {
        self.imp().store.get().expect("the model is installed")
    }

    fn selection(&self) -> &gtk::SingleSelection {
        self.imp().selection.get().expect("the model is installed")
    }

    pub(crate) fn clear_games(&self) {
        self.replace_games(Vec::new(), Vec::new());
    }

    /// Installs `games` as the list, `rows` being how they read on screen, with the first
    /// selected.
    pub(crate) fn replace_games(&self, games: Vec<crate::fox::FoxGame>, rows: Vec<FoxRow>) {
        let imp = self.imp();
        imp.wanted.set((!rows.is_empty()).then_some(0));
        *imp.games.borrow_mut() = games;
        *imp.rows.borrow_mut() = rows;
        self.refill();
    }

    /// Empties the store and starts filling it again from `rows`, [`FEED_PER_FRAME`] at a
    /// time on the dialog's frame clock — so not before the dialog is mapped. Called
    /// before each presentation, while the list is unrooted and emptying it binds nothing.
    pub(crate) fn refill(&self) {
        let imp = self.imp();
        // Dropping an id does not remove its callback; a finished feed already returned
        // `Break`, which did.
        if let Some(id) = imp.feed.take() {
            id.remove();
        }
        imp.ours.set(true);
        self.store().remove_all();
        imp.ours.set(false);
        if imp.rows.borrow().is_empty() {
            return;
        }
        let id = self.add_tick_callback(|dialog, _| {
            if dialog.feed_rows() {
                return glib::ControlFlow::Continue;
            }
            dialog.imp().feed.take();
            glib::ControlFlow::Break
        });
        imp.feed.replace(Some(id));
    }

    /// Moves the next rows into the store; false once it holds all of them.
    fn feed_rows(&self) -> bool {
        let imp = self.imp();
        let store = self.store();
        let have = store.n_items() as usize;
        let next: Vec<FoxRow> = {
            let rows = imp.rows.borrow();
            let take = if have < LIVE_ROWS {
                FEED_PER_FRAME
            } else {
                rows.len()
            };
            rows.iter().skip(have).take(take).cloned().collect()
        };
        store.splice(have as u32, 0, &next);
        let n_items = store.n_items();
        // A row the user picked while the list filled is theirs to keep: `wanted` already
        // follows it.
        if let Some(wanted) = imp.wanted.get()
            && wanted < n_items
            && self.selection().selected() != wanted
        {
            imp.ours.set(true);
            self.selection().set_selected(wanted);
            imp.ours.set(false);
        }
        if n_items as usize == imp.rows.borrow().len() {
            // The choice survives the refill; bring it back into view if it was scrolled
            // off. A row already showing does not move.
            if let Some(wanted) = imp.wanted.get() {
                imp.result_list
                    .scroll_to(wanted, gtk::ListScrollFlags::NONE, None);
            }
            return false;
        }
        true
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

/// Two lines, bound to the row's properties by expression so the factory needs no bind
/// handler of its own. The item takes the same text as its accessible label and
/// description, so a row reads as one record rather than two loose labels.
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
        let row = gtk::Box::builder()
            .margin_start(12)
            .margin_end(12)
            .margin_top(10)
            .margin_bottom(10)
            .build();
        row.append(&lines);

        item.property_expression("item")
            .chain_property::<FoxRow>("title")
            .bind(&title, "label", gtk::Widget::NONE);
        item.property_expression("item")
            .chain_property::<FoxRow>("subtitle")
            .bind(&subtitle, "label", gtk::Widget::NONE);
        item.property_expression("item")
            .chain_property::<FoxRow>("title")
            .bind(item, "accessible-label", gtk::Widget::NONE);
        item.property_expression("item")
            .chain_property::<FoxRow>("subtitle")
            .bind(item, "accessible-description", gtk::Widget::NONE);
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
    pub result_list: gtk::GridView,
}
