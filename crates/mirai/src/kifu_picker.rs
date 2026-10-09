// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The download dialog's fixed hierarchy, and the pages of records, searches and players it
//! shows.
//!
//! A server answers with up to 200 records. The dialog shows them [`PAGE`] at a time, as
//! rows of an `AdwPreferencesGroup` that are built once, with the dialog, and refilled in
//! place; a row opens its record when activated, through the `picker.open-record` action. A
//! list view cannot do as well: `GtkListView` keeps 200 rows alive whatever its height, and
//! any list view drops its rows while unrooted and rebinds them all, synchronously, when
//! presented again (`docs/dev/RENDERING.md` §8). A page of ten is also what a reader scans.
//! The recent searches are paged the same way, and so are the players a Yike nickname may
//! name, so no list needs the dialog scrolled, and the toolbar's one pager turns whichever
//! of them is on screen.
//!
//! Ten rows of CJK names still take 7–8 ms to shape and measure, more than a 144 Hz frame.
//! A page therefore changes [`ROWS_PER_FRAME`] rows a frame, and the dialog is presented
//! with its rows hidden — presenting measures whatever is visible, synchronously, and a
//! dialog put back on screen shapes all its text anew — to show them the same way.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;

use mirai_client::kifu::{Player, Record, Server};

use crate::i18n;

type OpenHandler = Box<dyn Fn(crate::kifu::DownloadedGame)>;

/// What a search is found again by: the same words on another server are another search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SearchKey {
    pub(crate) server: Server,
    /// What was typed, trimmed.
    pub(crate) query: String,
}

/// Records, or recent searches, a page shows.
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

    #[derive(Default)]
    pub struct KifuPickerDialog {
        pub(super) widgets: KifuPickerWidgets,
        /// The [`PAGE`] rows of `result_group`, in order.
        pub(super) slots: OnceCell<Vec<adw::ActionRow>>,
        /// The record each row shows. Rows a page turn has yet to refill still show the
        /// last page's, for a frame or two, and a click there must open what it shows.
        pub(super) slot_games: RefCell<Vec<Option<Record>>>,
        pub(super) games: RefCell<Vec<Record>>,
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
        /// Rows of `recent_group`, at most [`PAGE`], built as the history first needs them
        /// and refilled in place, with the search each one shows. Like the records' rows,
        /// one a page turn has yet to refill still shows, and acts on, the last page's.
        pub(super) recent: RefCell<Vec<(adw::ActionRow, gtk::Button, RecentText)>>,
        /// Every saved search, most recent first.
        pub(super) recent_entries: RefCell<Vec<RecentText>>,
        /// What the recent list is filtered by, lowercase.
        pub(super) recent_needle: RefCell<String>,
        /// The indices in `recent_entries` of the searches the filter keeps: what the
        /// recent list pages through.
        pub(super) recent_matches: RefCell<Vec<usize>>,
        /// The recent page on screen, from 0, most recent first.
        pub(super) recent_page_index: Cell<usize>,
        /// The next row of the recent page to fill; [`PAGE`] once all are.
        pub(super) next_recent: Cell<usize>,
        /// The history revision `recent_entries` holds, and the server whose searches they
        /// are.
        pub(super) recent_stamp: Cell<Option<(u64, Server)>>,
        /// Rows of `players_group`, at most [`PAGE`], built as a list of players first needs
        /// them and refilled in place.
        pub(super) player_rows: RefCell<Vec<adw::ActionRow>>,
        /// The players a search named, and how each reads.
        pub(super) players: RefCell<Vec<(Player, RecordText)>>,
        /// The players' page on screen, from 0.
        pub(super) players_page_index: Cell<usize>,
        /// The next row of the players' page to fill; [`PAGE`] once all are.
        pub(super) next_player: Cell<usize>,
        /// The search whose players the players' page holds.
        pub(super) players_key: RefCell<SearchKey>,
        /// The search whose records the results page holds.
        pub(super) shown_key: RefCell<SearchKey>,
        /// The search the status page reports on — a failure, or an account with no public
        /// games — kept on screen while the entry still reads it.
        pub(super) status_key: RefCell<SearchKey>,
        /// What the entry read when a request began. The entry is read-only meanwhile, but
        /// its clear icon still empties it; the text is put back.
        pub(super) busy_text: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for KifuPickerDialog {
        const NAME: &'static str = "MiraiKifuPickerDialog";

        type Type = super::KifuPickerDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for KifuPickerDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let dialog = self.obj();
            dialog.set_title(&i18n::gettext("Download Game Record"));
            dialog.set_content_width(640);
            dialog.set_content_height(860);
            dialog.set_child(Some(&self.widgets.toolbar));
            self.obj().build_slots();
            // The pager is the toolbar's bottom bar, up only with a page of records to turn.
            let weak = self.obj().downgrade();
            self.widgets.stack.connect_visible_child_notify(move |_| {
                if let Some(dialog) = weak.upgrade() {
                    dialog.sync_pager();
                }
            });
        }

        fn dispose(&self) {
            if let Some(task) = self.task.borrow_mut().take() {
                task.abort();
            }
            self.on_open.borrow_mut().take();
        }
    }
    impl WidgetImpl for KifuPickerDialog {}
    impl AdwDialogImpl for KifuPickerDialog {}
}

glib::wrapper! {
    pub struct KifuPickerDialog(ObjectSubclass<imp::KifuPickerDialog>)
        @extends gtk::Widget, adw::Dialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::ShortcutManager;
}

impl KifuPickerDialog {
    pub fn new() -> Self {
        let dialog = glib::Object::new();
        crate::widgets::sheet_texture::install(&dialog);
        dialog
    }

    /// Builds the page's rows, hidden until a page fills them. Called once, from
    /// `constructed`. Row `i` activates `picker.open-record` with `i`.
    fn build_slots(&self) {
        let imp = self.imp();
        let slots: Vec<adw::ActionRow> = (0..PAGE)
            .map(|i| {
                let row = adw::ActionRow::builder()
                    .activatable(true)
                    // Nicknames are plain text; one with `<` in it is not markup.
                    .use_markup(false)
                    .title_lines(1)
                    .subtitle_lines(1)
                    .visible(false)
                    .build();
                // The target first: an action that takes a parameter refuses a row that
                // names it without one, with a warning.
                row.set_action_target_value(Some(&(i as u32).to_variant()));
                row.set_action_name(Some("picker.open-record"));
                imp.widgets.result_group.add(&row);
                row
            })
            .collect();
        let _ = imp.slots.set(slots);
        imp.slot_games.replace(vec![None; PAGE]);
    }

    pub fn widgets(&self) -> &KifuPickerWidgets {
        &self.imp().widgets
    }

    pub(crate) fn install_handler(&self, handler: impl Fn(crate::kifu::DownloadedGame) + 'static) {
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
        self.replace_games(SearchKey::default(), Vec::new(), Vec::new());
    }

    /// Installs `games`, the records the search `key` found, with `texts` how they read, and
    /// shows the first page.
    pub(crate) fn replace_games(&self, key: SearchKey, games: Vec<Record>, texts: Vec<RecordText>) {
        let imp = self.imp();
        imp.shown_key.replace(key);
        *imp.games.borrow_mut() = games;
        *imp.texts.borrow_mut() = texts;
        self.show_page(0);
    }

    /// The search whose records the results page holds; an empty query when it holds none.
    pub(crate) fn shown_key(&self) -> SearchKey {
        self.imp().shown_key.borrow().clone()
    }

    fn page_count(&self) -> usize {
        self.imp().texts.borrow().len().div_ceil(PAGE)
    }

    /// Shows page `page` of the records. The first rows change at once, the rest over the
    /// next frames.
    fn show_page(&self, page: usize) {
        let imp = self.imp();
        let page = page.min(self.page_count().saturating_sub(1));
        imp.page.set(page);
        imp.next_slot.set(0);
        self.fill_rows();
        self.feed();
        self.sync_pager();
        // A page shorter than the window may still be scrolled down from the last one.
        imp.widgets.results_page.scroll_to_top();
    }

    /// Turns the page of whichever list is on screen, toward the older entries or the newer.
    pub(crate) fn turn_page(&self, older: bool) {
        let imp = self.imp();
        let turn = |page: usize| {
            if older {
                page + 1
            } else {
                page.saturating_sub(1)
            }
        };
        if self.showing(imp.widgets.recent_page.upcast_ref()) {
            self.show_recent_page(turn(imp.recent_page_index.get()));
        } else if self.showing(imp.widgets.players_page.upcast_ref()) {
            self.show_players_page(turn(imp.players_page_index.get()));
        } else {
            self.show_page(turn(imp.page.get()));
        }
    }

    fn showing(&self, page: &gtk::Widget) -> bool {
        self.imp().widgets.stack.visible_child().as_ref() == Some(page)
    }

    /// Reveals the pager while the records, the recent searches or the players are on screen
    /// and there is a page of them to turn to, and says where it is.
    fn sync_pager(&self) {
        enum List {
            Records,
            Searches,
            Players,
        }
        let imp = self.imp();
        let (list, page, count) = if self.showing(imp.widgets.results_page.upcast_ref()) {
            (List::Records, imp.page.get(), imp.texts.borrow().len())
        } else if self.showing(imp.widgets.recent_page.upcast_ref()) {
            (
                List::Searches,
                imp.recent_page_index.get(),
                imp.recent_matches.borrow().len(),
            )
        } else if self.showing(imp.widgets.players_page.upcast_ref()) {
            (
                List::Players,
                imp.players_page_index.get(),
                imp.players.borrow().len(),
            )
        } else {
            imp.widgets.toolbar.set_reveal_bottom_bars(false);
            return;
        };
        let pages = count.div_ceil(PAGE);
        imp.widgets.newer_button.set_sensitive(page > 0);
        imp.widgets.older_button.set_sensitive(page + 1 < pages);
        let first = page * PAGE;
        let (first, last, count) = (
            (first + 1).to_string(),
            (first + PAGE).min(count).to_string(),
            count.to_string(),
        );
        let args = [("first", &*first), ("last", &*last), ("count", &*count)];
        // One message per list, so a language that counts with a measure word can use each
        // list's own.
        let range = match list {
            // Translators: which records of how many the page shows, such as "11–20 of 200".
            List::Records => i18n::pgettext_f("records", "{first}–{last} of {count}", &args),
            // Translators: which saved searches of how many the page shows, such as
            // "11–20 of 20".
            List::Searches => i18n::pgettext_f("searches", "{first}–{last} of {count}", &args),
            // Translators: which of the players a search matched the page shows, such as
            // "11–20 of 21".
            List::Players => i18n::pgettext_f("players", "{first}–{last} of {count}", &args),
        };
        imp.widgets.page_label.set_label(&range);
        imp.widgets.toolbar.set_reveal_bottom_bars(pages > 1);
    }

    /// Hides the rows, to come back a few a frame once the dialog is on screen, and the
    /// pager with them until a page shows. Called before each presentation, while hiding
    /// them costs nothing.
    pub(crate) fn refill(&self) {
        let imp = self.imp();
        for row in self.slots() {
            row.set_visible(false);
        }
        for (row, _, _) in imp.recent.borrow().iter() {
            row.set_visible(false);
        }
        for row in imp.player_rows.borrow().iter() {
            row.set_visible(false);
        }
        imp.next_slot.set(0);
        imp.next_recent.set(0);
        imp.next_player.set(0);
        self.feed();
        self.sync_pager();
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

    /// Fills the rows left, of the page, of the recent list and of the players, on the
    /// dialog's frame clock — so not before it is mapped.
    fn feed(&self) {
        let imp = self.imp();
        let pending = imp.next_slot.get() < PAGE
            || imp.next_recent.get() < PAGE
            || imp.next_player.get() < PAGE;
        if imp.feed.borrow().is_some() || !pending {
            return;
        }
        let id = self.add_tick_callback(|dialog, _| {
            // All, every frame: `|`, not `||`.
            if dialog.fill_rows() | dialog.fill_recent() | dialog.fill_players() {
                return glib::ControlFlow::Continue;
            }
            // Returning `Break` removes the callback; dropping the id would not.
            dialog.imp().feed.take();
            glib::ControlFlow::Break
        });
        imp.feed.replace(Some(id));
    }

    /// The record row `slot` shows.
    pub(crate) fn game_at(&self, slot: usize) -> Option<Record> {
        self.imp().slot_games.borrow().get(slot).cloned().flatten()
    }

    pub(crate) fn has_games(&self) -> bool {
        !self.imp().games.borrow().is_empty()
    }

    /// Shows `players`, the people the search `key` may mean, with how each reads, from
    /// their first page. A row opens its player's records through `picker.open-player`
    /// with the player's index.
    pub(crate) fn set_players(&self, key: SearchKey, players: Vec<(Player, RecordText)>) {
        let imp = self.imp();
        imp.players_key.replace(key);
        imp.players.replace(players);
        self.show_players_page(0);
    }

    pub(crate) fn has_players(&self) -> bool {
        !self.imp().players.borrow().is_empty()
    }

    /// The search whose players the players' page holds.
    pub(crate) fn players_key(&self) -> SearchKey {
        self.imp().players_key.borrow().clone()
    }

    /// The player at `index` in the list.
    pub(crate) fn player_at(&self, index: usize) -> Option<Player> {
        let players = self.imp().players.borrow();
        players.get(index).map(|(player, _)| player.clone())
    }

    /// Shows page `page` of the players. The first rows change at once, the rest over the
    /// next frames.
    fn show_players_page(&self, page: usize) {
        let imp = self.imp();
        let pages = imp.players.borrow().len().div_ceil(PAGE);
        imp.players_page_index
            .set(page.min(pages.saturating_sub(1)));
        imp.next_player.set(0);
        self.fill_players();
        self.feed();
        self.sync_pager();
        imp.widgets.players_page.scroll_to_top();
    }

    /// Fills the players' page's next [`ROWS_PER_FRAME`] rows, building those that do not
    /// exist yet, and hides the rows the page has no player for; false once all are filled.
    fn fill_players(&self) -> bool {
        let imp = self.imp();
        let from = imp.next_player.get();
        if from >= PAGE {
            return false;
        }
        let to = (from + ROWS_PER_FRAME).min(PAGE);
        let first = imp.players_page_index.get() * PAGE;
        let players = imp.players.borrow();
        let mut rows = imp.player_rows.borrow_mut();
        for i in from..to {
            let Some((_, text)) = players.get(first + i) else {
                if let Some(row) = rows.get(i) {
                    row.set_visible(false);
                }
                continue;
            };
            // A page fills its rows in order from the first, so the row a player needs is
            // at most the next one to build.
            if rows.len() == i {
                let row = adw::ActionRow::builder()
                    .activatable(true)
                    .use_markup(false)
                    .title_lines(1)
                    .subtitle_lines(1)
                    .build();
                // The target first: an action that takes a parameter refuses a row that
                // names it without one, with a warning.
                row.set_action_target_value(Some(&((first + i) as u32).to_variant()));
                row.set_action_name(Some("picker.open-player"));
                imp.widgets.players_group.add(&row);
                rows.push(row);
            }
            let row = &rows[i];
            row.set_title(&text.title);
            row.set_subtitle(&text.subtitle);
            row.set_action_target_value(Some(&((first + i) as u32).to_variant()));
            row.set_visible(true);
        }
        imp.next_player.set(to);
        to < PAGE
    }

    pub(crate) fn abort_task(&self) {
        if let Some(task) = self.imp().task.take() {
            task.abort();
        }
    }

    pub(crate) fn replace_task(&self, task: glib::JoinHandle<()>) {
        if let Some(old) = self.imp().task.replace(Some(task)) {
            old.abort();
        }
    }

    pub(crate) fn open_game(&self, game: crate::kifu::DownloadedGame) {
        if let Some(handler) = self.imp().on_open.borrow().as_ref() {
            handler(game);
        }
    }

    /// The history revision and the server the recent list shows, if it has been filled.
    pub(crate) fn recent_stamp(&self) -> Option<(u64, Server)> {
        self.imp().recent_stamp.get()
    }

    /// Shows `entries` as the recent searches, from the page already on screen if it still
    /// has some. A row opens its search through the dialog's `picker.open-saved` action, and
    /// its button forgets it through `picker.forget`.
    pub(crate) fn set_recent(&self, stamp: (u64, Server), entries: Vec<RecentText>) {
        let imp = self.imp();
        imp.recent_entries.replace(entries);
        imp.recent_stamp.set(Some(stamp));
        self.match_recent();
        self.show_recent_page(imp.recent_page_index.get());
    }

    /// Finds again the searches the needle keeps.
    fn match_recent(&self) {
        let imp = self.imp();
        let needle = imp.recent_needle.borrow();
        let matches = imp
            .recent_entries
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.matches(&needle))
            .map(|(i, _)| i)
            .collect();
        imp.recent_matches.replace(matches);
    }

    /// Shows page `page` of the recent searches the filter keeps. The first rows change at
    /// once, the rest over the next frames.
    fn show_recent_page(&self, page: usize) {
        let imp = self.imp();
        let pages = imp.recent_matches.borrow().len().div_ceil(PAGE);
        imp.recent_page_index.set(page.min(pages.saturating_sub(1)));
        imp.next_recent.set(0);
        self.fill_recent();
        self.feed();
        self.sync_pager();
        imp.widgets.recent_page.scroll_to_top();
    }

    /// Fills the recent page's next [`ROWS_PER_FRAME`] rows; false once all are filled.
    fn fill_recent(&self) -> bool {
        let imp = self.imp();
        let from = imp.next_recent.get();
        if from >= PAGE {
            return false;
        }
        let to = (from + ROWS_PER_FRAME).min(PAGE);
        self.put_recent(from, to);
        imp.next_recent.set(to);
        to < PAGE
    }

    /// Puts the recent page's searches in rows `from..to`, building the rows that do not
    /// exist yet, and hides the rows the page has no search for.
    fn put_recent(&self, from: usize, to: usize) {
        let imp = self.imp();
        let entries = imp.recent_entries.borrow();
        let matches = imp.recent_matches.borrow();
        let first = imp.recent_page_index.get() * PAGE;
        let mut recent = imp.recent.borrow_mut();
        for i in from..to {
            let Some(entry) = matches.get(first + i).map(|&m| &entries[m]) else {
                if let Some((row, _, _)) = recent.get(i) {
                    row.set_visible(false);
                }
                continue;
            };
            // A page fills its rows in order from the first, so the row a search needs is
            // at most the next one to build.
            if recent.len() == i {
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
                // The targets first: an action that takes a string refuses a widget that
                // names it without one, with a warning.
                let query = entry.query.to_variant();
                row.set_action_target_value(Some(&query));
                row.set_action_name(Some("picker.open-saved"));
                forget.set_action_target_value(Some(&query));
                forget.set_action_name(Some("picker.forget"));
                row.add_suffix(&forget);
                imp.widgets.recent_group.add(&row);
                recent.push((row, forget, RecentText::default()));
            }
            let (row, forget, shown) = &mut recent[i];
            if shown != entry {
                row.set_title(&entry.title);
                row.set_subtitle(&entry.subtitle);
                let query = entry.query.to_variant();
                row.set_action_target_value(Some(&query));
                forget.set_action_target_value(Some(&query));
                *shown = entry.clone();
            }
            row.set_visible(true);
        }
    }

    /// Filters the recent searches by `text`, from their first page if that changes what
    /// the filter keeps, and says how many it keeps.
    pub(crate) fn filter_recent(&self, text: &str) -> usize {
        let imp = self.imp();
        let needle = text.trim().to_lowercase();
        if *imp.recent_needle.borrow() != needle {
            imp.recent_needle.replace(needle);
            self.match_recent();
            self.show_recent_page(0);
        }
        imp.recent_matches.borrow().len()
    }

    pub(crate) fn status_key(&self) -> SearchKey {
        self.imp().status_key.borrow().clone()
    }

    pub(crate) fn set_status_key(&self, key: SearchKey) {
        self.imp().status_key.replace(key);
    }

    pub(crate) fn busy_text(&self) -> String {
        self.imp().busy_text.borrow().clone()
    }

    pub(crate) fn set_busy_text(&self, text: String) {
        self.imp().busy_text.replace(text);
    }
}

impl Default for KifuPickerDialog {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed controls built once with the dialog; names are harness targets.
pub struct KifuPickerWidgets {
    toolbar: adw::ToolbarView,
    recent_group: adw::PreferencesGroup,
    page_label: gtk::Label,
    pub server_group: adw::ToggleGroup,
    pub entry: gtk::SearchEntry,
    pub search_button: gtk::Button,
    pub caption: gtk::Label,
    pub banner: adw::Banner,
    pub stack: adw::ViewStack,
    pub status_page: adw::StatusPage,
    pub loading_page: adw::StatusPage,
    pub recent_page: adw::PreferencesPage,
    pub results_page: adw::PreferencesPage,
    pub result_group: adw::PreferencesGroup,
    pub players_page: adw::PreferencesPage,
    pub players_group: adw::PreferencesGroup,
    pub refresh_button: gtk::Button,
    pub newer_button: gtk::Button,
    pub older_button: gtk::Button,
}

impl Default for KifuPickerWidgets {
    fn default() -> Self {
        // The server occupies the title's room so all ten records still fit underneath.
        // The dialog title continues to name the dialog to a screen reader.
        let server_group = adw::ToggleGroup::builder().name("server_group").build();
        server_group
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label(&i18n::gettext("Server"))]);
        for (name, label) in [
            ("fox", i18n::pgettext("server", "Fox")),
            ("eweiqi", i18n::pgettext("server", "eWeiqi")),
            ("yike", i18n::pgettext("server", "Yike")),
        ] {
            server_group.add(adw::Toggle::builder().name(name).label(label).build());
        }
        let header = adw::HeaderBar::builder()
            .title_widget(&server_group)
            .build();
        let entry = gtk::SearchEntry::builder()
            .name("entry")
            .placeholder_text(i18n::gettext("Exact Fox nickname or numeric UID"))
            .hexpand(true)
            .build();
        let search_button = gtk::Button::builder()
            .name("search_button")
            .label(i18n::pgettext("verb", "Search"))
            .sensitive(false)
            .build();
        let search_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        search_line.append(&entry);
        search_line.append(&search_button);
        let caption = gtk::Label::builder()
            .name("caption")
            .label(i18n::gettext(
                "Public records only · Fox exposes at most the latest 200 games",
            ))
            .xalign(0.0)
            .wrap(true)
            .margin_top(6)
            .margin_bottom(6)
            .css_classes(["dim-label", "caption"])
            .build();
        // Clamp and inset like the PreferencesPages below, keeping the search aligned.
        let search = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .build();
        search.append(&search_line);
        search.append(&caption);
        let clamp = adw::Clamp::builder().child(&search).build();
        let banner = adw::Banner::builder()
            .name("banner")
            .title(i18n::gettext("Request failed"))
            .revealed(false)
            .build();
        let status_page = adw::StatusPage::builder()
            .name("status_page")
            .icon_name("system-search-symbolic")
            .title(i18n::gettext("Find a Fox Player"))
            .description(i18n::gettext(
                "Enter an exact nickname or UID to browse their recent public games.",
            ))
            .build();
        let loading_page = adw::StatusPage::builder().name("loading_page").build();
        loading_page.set_paintable(Some(&adw::SpinnerPaintable::new(Some(&loading_page))));
        let recent_group = adw::PreferencesGroup::builder()
            .name("recent_group")
            .title(i18n::gettext("Recent Searches"))
            .build();
        let recent_page = adw::PreferencesPage::builder()
            .title(i18n::gettext("Recent Searches"))
            .build();
        // PreferencesPage:name identifies a preferences page, not its GtkWidget.
        recent_page.set_widget_name("recent_page");
        recent_page.add(&recent_group);
        let refresh_button = gtk::Button::builder()
            .name("refresh_button")
            .icon_name("view-refresh-symbolic")
            .tooltip_text(i18n::gettext("Search Again"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let result_group = adw::PreferencesGroup::builder()
            .name("result_group")
            .title(i18n::gettext("Recent Games"))
            .header_suffix(&refresh_button)
            .build();
        let results_page = adw::PreferencesPage::builder()
            .title(i18n::gettext("Recent Games"))
            .build();
        results_page.set_widget_name("results_page");
        results_page.add(&result_group);
        let players_group = adw::PreferencesGroup::builder()
            .name("players_group")
            .title(i18n::gettext("Several Players Match"))
            .description(i18n::gettext(
                "Choose whose games to browse, or search an account number.",
            ))
            .build();
        let players_page = adw::PreferencesPage::builder()
            .title(i18n::gettext("Matching Players"))
            .build();
        players_page.set_widget_name("players_page");
        players_page.add(&players_group);
        let stack = adw::ViewStack::builder()
            .name("stack")
            .enable_transitions(true)
            .vexpand(true)
            .build();
        for page in [
            status_page.upcast_ref::<gtk::Widget>(),
            loading_page.upcast_ref(),
            recent_page.upcast_ref(),
            results_page.upcast_ref(),
            players_page.upcast_ref(),
        ] {
            // Pages are selected by widget identity, not by ViewStackPage names.
            stack.add(page);
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&clamp);
        content.append(&banner);
        content.append(&stack);
        let newer_button = gtk::Button::builder()
            .name("newer_button")
            .icon_name("go-previous-symbolic")
            .tooltip_text(i18n::gettext("Previous Page"))
            .build();
        // A live region announces which entries a page turn puts on screen.
        let page_label = gtk::Label::builder()
            .name("page_label")
            .accessible_role(gtk::AccessibleRole::Status)
            .css_classes(["dim-label", "numeric"])
            .build();
        let older_button = gtk::Button::builder()
            .name("older_button")
            .icon_name("go-next-symbolic")
            .tooltip_text(i18n::gettext("Next Page"))
            .build();
        let pager = gtk::CenterBox::builder()
            .name("pager")
            .css_classes(["toolbar"])
            .start_widget(&newer_button)
            .center_widget(&page_label)
            .end_widget(&older_button)
            .build();
        let toolbar = adw::ToolbarView::builder()
            .name("toolbar")
            .content(&content)
            .reveal_bottom_bars(false)
            .build();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&pager);
        Self {
            toolbar,
            server_group,
            entry,
            search_button,
            caption,
            banner,
            stack,
            status_page,
            loading_page,
            recent_page,
            recent_group,
            results_page,
            result_group,
            players_page,
            players_group,
            refresh_button,
            newer_button,
            page_label,
            older_button,
        }
    }
}
