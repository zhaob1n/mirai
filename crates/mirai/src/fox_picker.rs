// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The download dialog's fixed hierarchy, and the pages of records it shows.
//!
//! Fox answers with up to 200 records. The dialog shows them [`PAGE`] at a time in a boxed
//! list whose rows are built once, with the dialog, and refilled in place. A list view
//! cannot do as well: `GtkListView` keeps 200 rows alive whatever its height, and any list
//! view drops its rows while unrooted and rebinds them all, synchronously, when presented
//! again (`docs/dev/RENDERING.md` §8). A page of ten is also what a reader scans anyway.
//!
//! Ten rows of CJK names still take 7–8 ms to shape and measure, more than a 144 Hz frame.
//! A page therefore changes [`ROWS_PER_FRAME`] rows a frame, and the dialog is presented
//! with its rows hidden — presenting measures whatever is visible, synchronously, and a
//! dialog put back on screen shapes all its text anew — to show them the same way.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};

use crate::i18n;

type OpenHandler = Box<dyn Fn(crate::fox::DownloadedGame)>;

/// Records a page shows.
pub(crate) const PAGE: usize = 10;
/// Rows filled per frame. A row of CJK names costs up to a millisecond to shape and
/// measure; four keep a page inside a 144 Hz frame.
const ROWS_PER_FRAME: usize = 4;

/// How one record reads in the list: both players, then date, size, length and result.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordText {
    pub(crate) title: String,
    pub(crate) subtitle: String,
}

/// One saved search as the recent list shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecentText {
    /// What was typed: the search's key, and what the row's button forgets.
    pub(crate) query: String,
    /// The player.
    pub(crate) title: String,
    /// How many records, and when they were saved.
    pub(crate) subtitle: String,
}

impl RecentText {
    /// Whether typing `needle` (already lowercase) should keep this search in view: a
    /// substring of what was typed for it or of the player's name, in any case.
    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.query.to_lowercase().contains(needle)
            || self.title.to_lowercase().contains(needle)
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
        pub recent_page: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub recent_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub results_page: TemplateChild<gtk::Box>,
        #[template_child]
        pub result_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub saved_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub refresh_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub result_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub result_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub pager: TemplateChild<gtk::CenterBox>,
        #[template_child]
        pub newer_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub page_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub older_button: TemplateChild<gtk::Button>,
        /// The [`PAGE`] rows of `result_list`, in order.
        pub(super) slots: OnceCell<Vec<adw::ActionRow>>,
        /// The record each row shows. Rows a page turn has yet to refill still show the
        /// last page's, for a frame or two, and a click there must open what it shows.
        pub(super) slot_games: RefCell<Vec<Option<crate::fox::FoxGame>>>,
        pub(super) games: RefCell<Vec<crate::fox::FoxGame>>,
        /// How each record in `games` reads.
        pub(super) texts: RefCell<Vec<RecordText>>,
        /// The page on screen, from 0, newest first.
        pub(super) page: Cell<usize>,
        /// The next row of the page to fill; [`PAGE`] once all are.
        pub(super) next_slot: Cell<usize>,
        pub(super) feed: RefCell<Option<gtk::TickCallbackId>>,
        /// Presented and not yet closing.
        pub(super) shown: Cell<bool>,
        pub(super) busy: Cell<bool>,
        pub(super) task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) on_open: RefCell<Option<OpenHandler>>,
        /// The application runtime: the saved searches are read and written on its blocking
        /// pool (INV-11).
        pub(super) runtime: OnceCell<tokio::runtime::Handle>,
        /// Rows of `recent_list`, built as the history first needs them and refilled in
        /// place, with the search each one shows.
        pub(super) recent: RefCell<Vec<(adw::ActionRow, gtk::Button, RecentText)>>,
        /// The searches the recent list is to show, most recent first.
        pub(super) recent_entries: RefCell<Vec<RecentText>>,
        /// The next of `recent_entries` to put in a row; its length once all are.
        pub(super) next_recent: Cell<usize>,
        /// What the recent list is filtered by, lowercase.
        pub(super) recent_needle: RefCell<String>,
        /// The history revision `recent_entries` holds.
        pub(super) recent_revision: Cell<Option<u64>>,
        /// The query whose records the results page holds.
        pub(super) shown_query: RefCell<String>,
        /// The query the status page reports on — a failure, or an account with no public
        /// games — kept on screen while the entry still reads it.
        pub(super) status_query: RefCell<String>,
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
            self.obj().build_slots();
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

    /// Builds the page's rows, hidden until a page fills them. Called once, from
    /// `constructed`.
    fn build_slots(&self) {
        let imp = self.imp();
        let slots: Vec<adw::ActionRow> = (0..PAGE)
            .map(|_| {
                let row = adw::ActionRow::builder()
                    .activatable(true)
                    // Nicknames are plain text; one with `<` in it is not markup.
                    .use_markup(false)
                    .title_lines(1)
                    .subtitle_lines(1)
                    .visible(false)
                    .build();
                imp.result_list.append(&row);
                row
            })
            .collect();
        let _ = imp.slots.set(slots);
        imp.slot_games.replace(vec![None; PAGE]);
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
            recent_page: imp.recent_page.get(),
            recent_list: imp.recent_list.get(),
            results_page: imp.results_page.get(),
            result_label: imp.result_label.get(),
            saved_label: imp.saved_label.get(),
            refresh_button: imp.refresh_button.get(),
            result_list: imp.result_list.get(),
            newer_button: imp.newer_button.get(),
            older_button: imp.older_button.get(),
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

    fn slots(&self) -> &[adw::ActionRow] {
        self.imp().slots.get().expect("the rows are built")
    }

    pub(crate) fn clear_games(&self) {
        self.replace_games(String::new(), Vec::new(), Vec::new());
    }

    /// Installs `games`, the records `query` found, with `texts` how they read; shows the
    /// first page with its first record selected.
    pub(crate) fn replace_games(
        &self,
        query: String,
        games: Vec<crate::fox::FoxGame>,
        texts: Vec<RecordText>,
    ) {
        let imp = self.imp();
        imp.shown_query.replace(query);
        *imp.games.borrow_mut() = games;
        *imp.texts.borrow_mut() = texts;
        self.show_page(0);
    }

    /// The query whose records the results page holds; empty when it holds none.
    pub(crate) fn shown_query(&self) -> String {
        self.imp().shown_query.borrow().clone()
    }

    pub(crate) fn page_count(&self) -> usize {
        self.imp().texts.borrow().len().div_ceil(PAGE)
    }

    pub(crate) fn page(&self) -> usize {
        self.imp().page.get()
    }

    /// Shows page `page` with its first record selected, and says where it is. The first
    /// rows change at once, the rest over the next frames.
    pub(crate) fn show_page(&self, page: usize) {
        let imp = self.imp();
        let pages = self.page_count();
        let page = page.min(pages.saturating_sub(1));
        imp.page.set(page);
        imp.next_slot.set(0);
        self.fill_rows();
        let count = imp.texts.borrow().len();
        match self.slots().first().filter(|_| count > 0) {
            Some(row) => imp.result_list.select_row(Some(row)),
            None => imp.result_list.unselect_all(),
        }
        self.feed();

        imp.pager.set_visible(pages > 1);
        imp.newer_button.set_sensitive(page > 0);
        imp.older_button.set_sensitive(page + 1 < pages);
        let first = page * PAGE;
        let last = (first + PAGE).min(count);
        // Translators: which records of how many the page shows, such as "11–20 of 200".
        imp.page_label.set_label(&i18n::gettext_f(
            "{first}–{last} of {count}",
            &[
                ("first", &(first + 1).to_string()),
                ("last", &last.to_string()),
                ("count", &count.to_string()),
            ],
        ));
        // A page shorter than the window may still be scrolled down from the last one.
        imp.result_scroll.vadjustment().set_value(0.0);
    }

    /// Hides the rows, to come back a few a frame once the dialog is on screen. Called
    /// before each presentation, while hiding them costs nothing; the selection stays.
    pub(crate) fn refill(&self) {
        for row in self.slots() {
            row.set_visible(false);
        }
        self.imp().next_slot.set(0);
        self.feed();
    }

    /// Fills the page's next [`ROWS_PER_FRAME`] rows; false once all are filled.
    fn fill_rows(&self) -> bool {
        let imp = self.imp();
        let from = imp.next_slot.get();
        if from >= PAGE {
            return false;
        }
        let texts = imp.texts.borrow();
        let games = imp.games.borrow();
        let mut slot_games = imp.slot_games.borrow_mut();
        let first = imp.page.get() * PAGE;
        let to = (from + ROWS_PER_FRAME).min(PAGE);
        for (i, row) in self.slots().iter().enumerate().take(to).skip(from) {
            match texts.get(first + i) {
                Some(text) => {
                    // Both setters return early on the text the row already shows.
                    row.set_title(&text.title);
                    row.set_subtitle(&text.subtitle);
                    row.set_visible(true);
                }
                None => row.set_visible(false),
            }
            slot_games[i] = games.get(first + i).cloned();
        }
        imp.next_slot.set(to);
        to < PAGE
    }

    /// Fills the rows left, of the page and of the recent list, on the dialog's frame
    /// clock — so not before it is mapped.
    fn feed(&self) {
        let imp = self.imp();
        let pending =
            imp.next_slot.get() < PAGE || imp.next_recent.get() < imp.recent_entries.borrow().len();
        if imp.feed.borrow().is_some() || !pending {
            return;
        }
        let id = self.add_tick_callback(|dialog, _| {
            // Both, every frame: `|`, not `||`.
            if dialog.fill_rows() | dialog.fill_recent() {
                return glib::ControlFlow::Continue;
            }
            // Returning `Break` removes the callback; dropping the id would not.
            dialog.imp().feed.take();
            glib::ControlFlow::Break
        });
        imp.feed.replace(Some(id));
    }

    /// The record behind the selected row, if any is selected.
    pub(crate) fn selected_game(&self) -> Option<crate::fox::FoxGame> {
        let index = self.imp().result_list.selected_row()?.index();
        self.game_at(usize::try_from(index).ok()?)
    }

    /// The record row `slot` shows.
    pub(crate) fn game_at(&self, slot: usize) -> Option<crate::fox::FoxGame> {
        self.imp().slot_games.borrow().get(slot).cloned().flatten()
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.imp().result_list.selected_row().is_some()
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
        self.imp()
            .result_list
            .connect_selected_rows_changed(move |_| f());
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

    /// The history revision the recent list shows, if it has been filled.
    pub(crate) fn recent_revision(&self) -> Option<u64> {
        self.imp().recent_revision.get()
    }

    /// Shows `entries` as the recent searches. Rows already built are refilled at once, so
    /// none goes on showing a search that has moved or gone; the rows the list has never
    /// needed are built [`ROWS_PER_FRAME`] a frame, as building one costs more than
    /// refilling it. Each row's button forgets its search through the dialog's
    /// `picker.forget` action.
    pub(crate) fn set_recent(&self, revision: u64, entries: Vec<RecentText>) {
        let imp = self.imp();
        let built = imp.recent.borrow().len();
        for (row, _, _) in imp.recent.borrow().iter().skip(entries.len()) {
            row.set_visible(false);
        }
        let refill = built.min(entries.len());
        imp.recent_entries.replace(entries);
        imp.recent_revision.set(Some(revision));
        self.put_recent(0, refill);
        imp.next_recent.set(refill);
        self.fill_recent();
        self.feed();
    }

    /// Builds the next [`ROWS_PER_FRAME`] recent rows; false once all are built.
    fn fill_recent(&self) -> bool {
        let imp = self.imp();
        let len = imp.recent_entries.borrow().len();
        let from = imp.next_recent.get();
        if from >= len {
            return false;
        }
        let to = (from + ROWS_PER_FRAME).min(len);
        self.put_recent(from, to);
        imp.next_recent.set(to);
        to < len
    }

    /// Puts searches `from..to` in their rows, building the rows that do not exist yet,
    /// and shows those the filter keeps.
    fn put_recent(&self, from: usize, to: usize) {
        let imp = self.imp();
        let entries = imp.recent_entries.borrow();
        let needle = imp.recent_needle.borrow();
        let mut recent = imp.recent.borrow_mut();
        for (i, entry) in entries.iter().enumerate().take(to).skip(from) {
            if recent.len() <= i {
                let row = adw::ActionRow::builder()
                    .activatable(true)
                    .use_markup(false)
                    .title_lines(1)
                    .subtitle_lines(1)
                    .build();
                let forget = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text(i18n::gettext("Remove From History"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                // The target first: an action that takes a string refuses a button that
                // names it without one, with a warning.
                forget.set_action_target_value(Some(&entry.query.to_variant()));
                forget.set_action_name(Some("picker.forget"));
                row.add_suffix(&forget);
                imp.recent_list.append(&row);
                recent.push((row, forget, RecentText::default()));
            }
            let (row, forget, shown) = &mut recent[i];
            if shown != entry {
                row.set_title(&entry.title);
                row.set_subtitle(&entry.subtitle);
                forget.set_action_target_value(Some(&entry.query.to_variant()));
                *shown = entry.clone();
            }
            row.set_visible(entry.matches(&needle));
        }
    }

    /// Shows the recent searches that `text` matches, and says how many there are —
    /// counting those not yet in a row.
    pub(crate) fn filter_recent(&self, text: &str) -> usize {
        let imp = self.imp();
        let needle = text.trim().to_lowercase();
        let filled = imp.next_recent.get();
        for (row, _, shown) in imp.recent.borrow().iter().take(filled) {
            row.set_visible(shown.matches(&needle));
        }
        let count = imp
            .recent_entries
            .borrow()
            .iter()
            .filter(|entry| entry.matches(&needle))
            .count();
        imp.recent_needle.replace(needle);
        count
    }

    /// The search behind row `index` of the recent list.
    pub(crate) fn recent_query(&self, index: usize) -> Option<String> {
        let imp = self.imp();
        let recent = imp.recent.borrow();
        recent
            .get(index)
            .filter(|_| index < imp.recent_entries.borrow().len())
            .map(|(_, _, shown)| shown.query.clone())
    }

    pub(crate) fn status_query(&self) -> String {
        self.imp().status_query.borrow().clone()
    }

    pub(crate) fn set_status_query(&self, query: String) {
        self.imp().status_query.replace(query);
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
    pub stack: adw::ViewStack,
    pub status_page: adw::StatusPage,
    pub loading_page: adw::StatusPage,
    pub recent_page: gtk::ScrolledWindow,
    pub recent_list: gtk::ListBox,
    pub results_page: gtk::Box,
    pub result_label: gtk::Label,
    pub saved_label: gtk::Label,
    pub refresh_button: gtk::Button,
    pub result_list: gtk::ListBox,
    pub newer_button: gtk::Button,
    pub older_button: gtk::Button,
}
