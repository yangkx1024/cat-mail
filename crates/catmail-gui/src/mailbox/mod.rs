//! The main window: accounts and their folders, the list of what is selected, and the
//! pane that shows one item of it. Follows the engine's change events, so a sync
//! (started here or not), a sent message or a change made elsewhere shows up as it
//! commits. The commands on messages live in `commands.rs`: the toolbar, the menus and
//! the keys all call the same methods.

mod add_account;
mod commands;
mod draft;
mod folder_commands;
mod folder_picker;
mod message_commands;
mod message_list;
mod outbox;
mod panes;
mod sidebar;
#[cfg(test)]
mod tests;

pub(crate) use commands::report_failure;

use std::collections::HashMap;
use std::time::Duration;

use catmail_core::{
    AccountId, Event, Flags, Folder, FolderId, FolderRole, MailClient, MessageId, RecvError,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::list::{ListEvent, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Task, WeakEntity, Window, div,
};

use crate::actions::{self, *};
use crate::composer::{self, ComposeFrom};
use crate::layout;
use crate::reader::Reader;
use crate::settings::Preferences;
use add_account::{AddAccount, AddAccountEvent};
use catmail_frontend::folders;
pub use catmail_frontend::mailbox::{AccountEntry, Place};
use catmail_frontend::mailbox::{REFRESH_INTERVAL, SYNC_ON_OPEN_AFTER};
pub(crate) use catmail_frontend::refresh::{Coalesce, Refresh};
use catmail_frontend::refresh::{DraftCounts, Throttle};
use message_commands::MessageCommand;
use message_list::{MessageList, Row, RowCommand, Source};

/// How long typing in the search field pauses before it searches.
const SEARCH_DELAY: Duration = Duration::from_millis(200);

/// Where to take the mailbox: a place, or an account's INBOX and a message to open there
/// (what a notification was about), which the mailbox finds among its folders. Not All
/// Inboxes, even if shown: the message may be too far down to be loaded there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Place(Place),
    Inbox {
        account: AccountId,
        message: Option<MessageId>,
    },
}

/// "Labels", open for a message.
pub(crate) struct LabelsDialog {
    message: MessageId,
    /// Gone once the dialog is closed.
    pub(crate) list: WeakEntity<ListState<folder_picker::FolderPicker>>,
}

pub struct Mailbox {
    pub(crate) client: MailClient,
    /// `None` until the first load finishes.
    pub(crate) accounts: Option<Vec<AccountEntry>>,
    pub(crate) place: Option<Place>,
    /// Where to start instead of the first inbox, if it is still there, and the message
    /// to open there.
    reopen_at: Option<Destination>,
    /// A new message was asked for before the folders loaded, which say from which
    /// account: composed once they have.
    compose_once_loaded: bool,
    /// [`SYNC_ON_OPEN_AFTER`], except in tests that open folders synced just now.
    pub(crate) sync_on_open_after: Duration,
    /// [`REFRESH_INTERVAL`], except in tests, whose clock only moves when told to.
    pub(crate) refresh_interval: Duration,
    throttle: Throttle,
    /// Whether each account the user folded or unfolded in the sidebar is open; see
    /// [`Mailbox::account_open`] for the others.
    account_folds: HashMap<AccountId, bool>,
    /// "Labels", while it is open.
    pub(crate) labels_dialog: Option<LabelsDialog>,
    /// The sign-in form, shown instead of the mailbox while it is open.
    adding_account: Option<(Entity<AddAccount>, Subscription)>,
    pub(crate) search: Entity<InputState>,
    pub(crate) messages: Entity<ListState<MessageList>>,
    pub(crate) reader: Entity<Reader>,
    focus_handle: FocusHandle,
    pub(crate) reading_folders: Coalesce,
    draft_counts: DraftCounts,
    /// Looking for refused changes, coalesced like reading.
    checking_refused: Coalesce,
    _load_folders: Task<()>,
    _throttle: Task<()>,
    _refused: Task<()>,
    _search: Task<()>,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Mailbox {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Mailbox {
    pub fn new(client: MailClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let messages = cx.new(|cx| ListState::new(MessageList::new(client.clone()), window, cx));
        let focus_handle = cx.focus_handle();
        let reader = cx.new(|_| Reader::new(client.clone()));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscriptions = vec![
            cx.subscribe_in(&messages, window, Self::on_list_event),
            cx.subscribe_in(
                &messages,
                window,
                |this, _, row: &RowCommand, window, cx| {
                    this.run(row.command, vec![row.message.clone()], window, cx)
                },
            ),
            cx.subscribe_in(
                &search,
                window,
                |this, search, event, window, cx| match event {
                    InputEvent::Change => {
                        let text = search.read(cx).value().trim().to_owned();
                        // After a pause in typing, not for every key. Replacing the task
                        // cancels the previous wait.
                        this._search = cx.spawn_in(window, async move |this, cx| {
                            cx.background_executor().timer(SEARCH_DELAY).await;
                            this.update_in(cx, |this, window, cx| {
                                this.search_for(text, window, cx)
                            })
                            .ok();
                        });
                    }
                    InputEvent::PressEnter { .. } => {
                        this.messages.update(cx, |list, cx| list.focus(window, cx));
                    }
                    InputEvent::Focus => this.connect_for_search(cx),
                    InputEvent::Blur => {}
                },
            ),
            // A refresh may bring the open message with other labels, or another
            // message selected.
            cx.observe_in(&messages, window, |this, _, window, cx| {
                this.show_labels(cx);
                this.sync_labels_dialog(window, cx);
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                crate::theme::appearance::system_appearance_changed(window, cx);
            }),
        ];
        crate::theme::appearance::system_appearance_changed(window, cx);

        let mut events = client.subscribe();
        let events = cx.spawn_in(window, async move |this, cx| {
            loop {
                let event = events.recv().await;
                if event == Err(RecvError::Closed) {
                    break;
                }
                let updated = this.update_in(cx, |this, window, cx| {
                    this.on_engine_event(event.as_ref().ok(), window, cx)
                });
                if updated.is_err() {
                    break; // The window is gone.
                }
            }
        });

        let mut mailbox = Self {
            client,
            accounts: None,
            place: None,
            reopen_at: None,
            compose_once_loaded: false,
            sync_on_open_after: SYNC_ON_OPEN_AFTER,
            refresh_interval: REFRESH_INTERVAL,
            throttle: Throttle::default(),
            account_folds: HashMap::new(),
            labels_dialog: None,
            adding_account: None,
            search,
            messages,
            reader,
            focus_handle,
            reading_folders: Coalesce::default(),
            draft_counts: DraftCounts::default(),
            checking_refused: Coalesce::default(),
            _load_folders: Task::ready(()),
            _throttle: Task::ready(()),
            _refused: Task::ready(()),
            _search: Task::ready(()),
            _events: events,
            _subscriptions: subscriptions,
        };
        mailbox.load_folders(window, cx);
        // Left from before, or refused while no window was open.
        mailbox.report_refused_changes(window, cx);
        mailbox
    }

    /// Goes to `to` if it is still there, once the folders are loaded: from a new
    /// mailbox, where the main window was when it was last closed, or what a
    /// notification was about.
    pub fn go_to(&mut self, to: Destination, window: &mut Window, cx: &mut Context<Self>) {
        if self.accounts.is_none() {
            self.reopen_at = Some(to);
        } else if let Some((place, message)) = self.find(to) {
            self.show_at(place, message, window, cx);
        }
    }

    /// The place `to` is, if still there, and the message to open there.
    fn find(&self, to: Destination) -> Option<(Place, Option<MessageId>)> {
        let accounts = self.accounts.as_deref()?;
        match to {
            Destination::Place(place) => place.is_in(accounts).then_some((place, None)),
            Destination::Inbox { account, message } => {
                let inbox = self
                    .account(account)?
                    .folders
                    .with_role(FolderRole::Inbox)?;
                Some((Place::Folder(inbox.id), message))
            }
        }
    }

    /// Selects `place`, and opens `message` there once it is listed.
    fn show_at(
        &mut self,
        place: Place,
        message: Option<MessageId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_place(Some(place), window, cx);
        if let Some(message) = message {
            self.messages.update(cx, |list, cx| {
                message_list::reveal(list, message, window, cx)
            });
        }
    }

    fn on_list_event(
        &mut self,
        list: &Entity<ListState<MessageList>>,
        event: &ListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A row selected now, by the user or by the list itself, ends the wait for a
        // message to reveal: the user's choice wins over a notification clicked before.
        list.update(cx, |list, _| list.delegate_mut().forget_reveal());
        let (ix, confirmed) = match event {
            ListEvent::Select(ix) => (*ix, false),
            ListEvent::Confirm(ix) => (*ix, true),
            ListEvent::Cancel => {
                self.close_detail(window, cx);
                return;
            }
        };
        let Some(row) = list.read(cx).delegate().row_at(ix).cloned() else {
            return;
        };
        match row {
            Row::Message(message) => {
                self.reader
                    .update(cx, |reader, cx| reader.open(message.id, window, cx));
                self.show_labels(cx);
                if Preferences::get(cx).mark_read_on_open && !message.flags.contains(Flags::SEEN) {
                    self.set_flags(vec![message.id], Flags::SEEN, Flags::empty(), window, cx);
                }
            }
            Row::Draft(draft) => {
                self.reader.update(cx, |reader, cx| reader.clear(cx));
                if confirmed {
                    composer::open(
                        self.client.clone(),
                        ComposeFrom::Draft(draft.id),
                        window,
                        cx,
                    );
                }
            }
            Row::Outbox(_) => self.reader.update(cx, |reader, cx| reader.clear(cx)),
        }
        cx.notify();
    }

    /// `event` is `None` when some were missed: anything shown may be out of date.
    fn on_engine_event(
        &mut self,
        event: Option<&Event>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.is_none() {
            // Before reading the sidebar again, so the read isn't taken for an old one.
            self.draft_counts.missed();
        }
        let shown = self.messages.read(cx).delegate().source().cloned();
        let folders: Vec<_> = self.shown_folders().map(|f| f.id).collect();
        let refresh = self
            .throttle
            .ask(Refresh::after(event, shown.as_ref(), &folders));
        self.read_again(refresh, window, cx);
        match event {
            Some(&Event::AccountStatusChanged { account }) => {
                let status = self.client.account_status(account);
                if let Some(entry) = self.account_mut(account) {
                    entry.status = status;
                    cx.notify();
                }
            }
            Some(&Event::DraftsChanged { account }) => self.count_drafts(account, window, cx),
            Some(Event::ChangesRefused { .. }) | None => self.report_refused_changes(window, cx),
            Some(_) => {}
        }
    }

    /// Reads the sidebar or the list again, as [`Throttle::ask`] or
    /// [`Throttle::elapsed`] said, and waits out an interval after that.
    fn read_again(&mut self, refresh: Refresh, window: &mut Window, cx: &mut Context<Self>) {
        if refresh.is_empty() {
            return;
        }
        if refresh.sidebar {
            self.load_folders(window, cx);
        }
        if refresh.list {
            self.messages
                .update(cx, |list, cx| message_list::refresh(list, window, cx));
        }
        let interval = self.refresh_interval;
        self._throttle = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(interval).await;
            this.update_in(cx, |this, window, cx| {
                let held = this.throttle.elapsed();
                this.read_again(held, window, cx);
            })
            .ok();
        });
    }

    /// Shows the selected message's labels in the reader, as the list has them.
    fn show_labels(&self, cx: &mut Context<Self>) {
        let list = self.messages.read(cx).delegate();
        let labels = list
            .selected_message()
            .map(|message| list.label_names(message, None))
            .unwrap_or_default();
        self.reader
            .update(cx, |reader, cx| reader.set_labels(labels, cx));
    }

    fn account_mut(&mut self, id: AccountId) -> Option<&mut AccountEntry> {
        self.accounts
            .as_mut()?
            .iter_mut()
            .find(|a| a.account.id == id)
    }

    pub(crate) fn account(&self, id: AccountId) -> Option<&AccountEntry> {
        self.accounts.as_ref()?.iter().find(|a| a.account.id == id)
    }

    /// Each account's INBOX, as All Inboxes lists them.
    pub(crate) fn inboxes(&self) -> impl Iterator<Item = &Folder> {
        self.accounts
            .iter()
            .flatten()
            .filter_map(|a| a.folders.with_role(FolderRole::Inbox))
    }

    /// The folders whose messages the selected place lists: one, or for All Inboxes each
    /// account's INBOX; none for the drafts and the outbox.
    pub(crate) fn shown_folders(&self) -> Box<dyn Iterator<Item = &Folder> + '_> {
        match self.place {
            Some(Place::Inboxes) => Box::new(self.inboxes()),
            Some(Place::Folder(id)) => Box::new(
                self.accounts
                    .iter()
                    .flatten()
                    .find_map(|a| a.folders.find(id)?.selectable())
                    .into_iter(),
            ),
            _ => Box::new(std::iter::empty()),
        }
    }

    /// The account the selected place belongs to.
    pub(crate) fn current_account(&self) -> Option<&AccountEntry> {
        self.place?.account_in(self.accounts.as_deref()?)
    }

    /// Reads the accounts, their folders, drafts and outbox again (see [`Coalesce`]).
    fn load_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reading_folders.start() {
            return;
        }
        let draft_counts = self.draft_counts.clone();
        let client = self.client.clone();
        self._load_folders = cx.spawn_in(window, async move |this, cx| {
            let loaded = client.overview().await;
            this.update_in(cx, |this, window, cx| {
                let again = this.reading_folders.finish();
                match loaded {
                    Ok(accounts) => {
                        let accounts = accounts
                            .into_iter()
                            .map(|a| {
                                let id = a.account.id;
                                // Not stored: taken now, it is never older than its event.
                                let status = this.client.account_status(id);
                                let mut entry = AccountEntry::new(a, status);
                                if !this.draft_counts.read_is_latest(&draft_counts, id)
                                    && let Some(shown) = this.account(id)
                                {
                                    entry.drafts = shown.drafts;
                                }
                                entry
                            })
                            .collect();
                        this.set_accounts(accounts, window, cx)
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("Couldn't load folders: {e}")),
                        cx,
                    ),
                }
                if again {
                    this.load_folders(window, cx);
                }
            })
            .ok();
        });
    }

    /// Every pause in typing saves a draft: only the account's draft count can have
    /// changed, so only that is read again.
    fn count_drafts(&mut self, account: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let change = self.draft_counts.changed(account);
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let counted = client.count_drafts(account).await;
            this.update_in(cx, |this, window, cx| {
                if !this.draft_counts.is_latest(account, change) {
                    return; // A later count is on its way.
                }
                match counted {
                    Ok(drafts) => {
                        if let Some(entry) = this.account_mut(account)
                            && entry.drafts != drafts
                        {
                            entry.drafts = drafts;
                            cx.notify();
                        }
                    }
                    // No later count would put it right: the sidebar's read counts the
                    // drafts too, and says so if it fails as well.
                    Err(e) => {
                        tracing::warn!(%account, error = %e, "couldn't count the drafts");
                        this.load_folders(window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_accounts(
        &mut self,
        accounts: Vec<AccountEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.accounts.as_ref() == Some(&accounts) {
            // A sync's events often bring nothing the sidebar shows.
            return;
        }
        let first_load = self.accounts.is_none();
        let still_there = self.place.is_some_and(|place| place.is_in(&accounts));
        let shown: Vec<_> = self.shown_folders().map(|f| f.id).collect();
        let folders = accounts
            .iter()
            .map(|a| (a.account.id, a.folders.clone()))
            .collect();
        self.messages.update(cx, |list, cx| {
            if list.delegate_mut().set_folders(folders) {
                cx.notify();
            }
        });
        self.accounts = Some(accounts);
        if let Some((place, message)) = self.reopen_at.take().and_then(|to| self.find(to)) {
            self.show_at(place, message, window, cx);
        } else if !still_there {
            let start = Place::start(self.accounts.as_deref().unwrap_or_default());
            self.select_place(start, window, cx);
        } else if !self.shown_folders().map(|f| f.id).eq(shown) && !self.is_searching(cx) {
            // The list shows a folder more (All Inboxes, an account's first INBOX): its
            // messages' events may have come first, taken for a folder not shown.
            self.messages
                .update(cx, |list, cx| message_list::refresh(list, window, cx));
        }
        let no_accounts = self.accounts.as_ref().is_some_and(Vec::is_empty);
        if no_accounts && self.adding_account.is_none() {
            self.add_account(false, window, cx);
        } else if first_load {
            self.messages.update(cx, |list, cx| list.focus(window, cx));
        }
        if std::mem::take(&mut self.compose_once_loaded) {
            self.new_message(window, cx);
        }
        cx.notify();
    }

    /// Opens the sign-in form; `cancellable` is false when there is no mailbox to go back to.
    pub(crate) fn add_account(
        &mut self,
        cancellable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let form = cx.new(|cx| AddAccount::new(client, cancellable, window, cx));
        let subscription = cx.subscribe_in(&form, window, |this, _, event, window, cx| {
            this.adding_account = None;
            match event {
                // Background sync syncs it: its folders and messages appear through the
                // engine's events, and how that goes in its status.
                AddAccountEvent::Added => {
                    this.messages.update(cx, |list, cx| list.focus(window, cx));
                }
                AddAccountEvent::Cancelled => {}
            }
            cx.notify();
        });
        self.adding_account = Some((form, subscription));
        cx.notify();
    }

    pub(crate) fn select_place(
        &mut self,
        place: Option<Place>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.place == place && place.is_some() && !self.is_searching(cx) {
            return;
        }
        self.place = place;
        // Wherever it was taken (a notification, the last session), the selected row shows.
        let accounts = self.accounts.as_deref().unwrap_or_default();
        if let Some(entry) = place.and_then(|place| place.account_in(accounts)) {
            self.account_folds.insert(entry.account.id, true);
        }
        // Leaving a search for a folder: the folder's messages, not the results.
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.messages.update(cx, |list, cx| {
            message_list::show(list, place.map(Place::source), window, cx)
        });
        self.reader.update(cx, |reader, cx| reader.clear(cx));
        window.set_window_title(&self.title());
        for folder in self.shown_folders() {
            self.sync_on_open(folder.id, cx);
        }
        cx.notify();
    }

    /// Logs in to the servers ahead of a search, which asks them a moment after the local
    /// results show. Failures only go to the log: the search adds nothing then either.
    fn connect_for_search(&self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.background_spawn(async move {
            if let Err(e) = client.connect_for_search(None).await {
                tracing::warn!(error = %e, "couldn't connect ahead of a search");
            }
        })
        .detach();
    }

    /// What it brings shows through the engine's events. A server that can't be reached
    /// already shows in the sidebar, so a failure here only goes to the log.
    fn sync_on_open(&self, folder: FolderId, cx: &mut Context<Self>) {
        let (client, max_age) = (self.client.clone(), self.sync_on_open_after);
        cx.background_spawn(async move {
            let error = match client.sync_folder(folder, max_age).await {
                Ok(report) => report.and_then(|r| r.error).map(|e| e.message),
                Err(e) => Some(e.to_string()),
            };
            if let Some(error) = error {
                tracing::warn!(%folder, %error, "couldn't sync the opened folder");
            }
        })
        .detach();
    }

    fn title(&self) -> String {
        match self.place_label() {
            Some(label) => format!("{label} — catmail"),
            None => "catmail".into(),
        }
    }

    /// The selected place's name, as the sidebar shows it.
    pub(crate) fn place_label(&self) -> Option<String> {
        self.place?.label(self.accounts.as_deref()?)
    }

    pub(crate) fn is_searching(&self, cx: &gpui_kit::App) -> bool {
        matches!(
            self.messages.read(cx).delegate().source(),
            Some(Source::Search(_))
        )
    }

    fn search_for(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let source = if text.is_empty() {
            self.place.map(Place::source)
        } else {
            Some(Source::Search(text))
        };
        if self.messages.read(cx).delegate().source() == source.as_ref() {
            return;
        }
        self.messages
            .update(cx, |list, cx| message_list::show(list, source, window, cx));
        self.reader.update(cx, |reader, cx| reader.clear(cx));
        cx.notify();
    }
}

impl Render for Mailbox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if let Some((form, _)) = &self.adding_account {
            form.clone().into_any_element()
        } else {
            self.render_panes(window, cx).into_any_element()
        };
        div()
            .id("mailbox")
            .key_context(actions::MAILBOX)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NewMessage, window, cx| this.new_message(window, cx)))
            .on_action(cx.listener(|this, _: &Reply, window, cx| this.reply(false, window, cx)))
            .on_action(cx.listener(|this, _: &ReplyAll, window, cx| this.reply(true, window, cx)))
            .on_action(cx.listener(|this, _: &Forward, window, cx| this.forward(window, cx)))
            // On the messages checked, or else the selected one.
            .on_action(cx.listener(|this, _: &Archive, window, cx| {
                this.run(MessageCommand::Archive, this.keyed(cx), window, cx)
            }))
            .on_action(cx.listener(|this, _: &Delete, window, cx| {
                this.run(MessageCommand::Delete, this.keyed(cx), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRead, window, cx| {
                this.run(MessageCommand::ToggleRead, this.keyed(cx), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleFlag, window, cx| {
                this.run(MessageCommand::ToggleFlag, this.keyed(cx), window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveToFolder, window, cx| {
                this.run(MessageCommand::Move, this.keyed(cx), window, cx)
            }))
            .on_action(cx.listener(|this, _: &EditLabels, window, cx| this.edit_labels(window, cx)))
            // The keys' way to check a message: the avatar is the pointer's.
            .on_action(cx.listener(|this, _: &ToggleChecked, _, cx| {
                if let Some(message) = this.selected_message(cx) {
                    this.messages.update(cx, |list, cx| {
                        message_list::toggle_checked(list, message.id, cx)
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &RenameFolder, window, cx| {
                this.rename_selected_folder(window, cx)
            }))
            .on_action(cx.listener(|this, _: &Sync, window, cx| this.sync(window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search
                    .update(cx, |search, cx| search.focus(window, cx))
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // In the root, which takes the press for its focus after the row.
            .child(layout::movable(content, window))
    }
}
