//! The mailbox's commands: on the selected message (its toolbar), the messages checked
//! (the list's toolbar), what the keys act on (those checked, or else the selected one)
//! or a row's; writing, syncing and settings.

use catmail_core::{AccountId, DeleteMode, Flags, FolderId, MessageId, MessageSummary};
use catmail_frontend::commands::{self, Deletion, MessageCommand, what};
use catmail_frontend::folders::AccountFolders;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Context, Window};

use crate::composer::{self, ComposeFrom};
use crate::layout;

use super::message_commands::Applies;
use super::*;
use crate::settings::dialog as settings_dialog;

/// Runs an engine call, and tells the user if it fails: "Couldn't {what}: …".
pub(crate) fn report_failure<T: 'static>(
    what: &'static str,
    call: impl Future<Output = catmail_core::Result<T>> + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            if let Err(e) = call.await {
                cx.update(|window, cx| {
                    window
                        .push_notification(Notification::error(format!("Couldn't {what}: {e}")), cx)
                })
                .ok();
            }
        })
        .detach();
}

impl Mailbox {
    /// Syncs every account now. The engine's background sync does it: progress and
    /// failures show as the accounts' status.
    pub(super) fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        report_failure("sync", async move { client.sync_now().await }, window, cx);
    }

    /// Whether an account is syncing every folder, as the engine says.
    pub(crate) fn syncing(&self) -> bool {
        self.accounts
            .iter()
            .flatten()
            .any(|entry| entry.status.syncing)
    }

    /// Changes the server refused are undone locally by the engine; this says so, once
    /// per change, and forgets them. Coalesced like reading.
    pub(super) fn report_refused_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.checking_refused.start() {
            return;
        }
        let client = self.client.clone();
        self._refused = cx.spawn_in(window, async move |this, cx| {
            let refused = client.dismiss_settled(None).await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                for op in &refused {
                    window
                        .push_notification(Notification::warning(commands::refused_change(op)), cx);
                }
                if this.checking_refused.finish() {
                    this.report_refused_changes(window, cx);
                }
            })
            .ok();
        });
    }

    // Commands on messages: the selected one, those checked, or what the keys act on.

    pub(super) fn selected_message(&self, cx: &gpui_kit::App) -> Option<MessageSummary> {
        self.messages
            .read(cx)
            .delegate()
            .selected_message()
            .cloned()
    }

    /// The selected message, as the messages a command acts on.
    pub(super) fn selected(&self, cx: &gpui_kit::App) -> Vec<MessageSummary> {
        self.selected_message(cx).into_iter().collect()
    }

    /// The messages checked in the list, top to bottom.
    pub(super) fn checked(&self, cx: &gpui_kit::App) -> Vec<MessageSummary> {
        self.messages
            .read(cx)
            .delegate()
            .checked_messages()
            .cloned()
            .collect()
    }

    /// What a key acts on: the messages checked, or else the selected one.
    pub(super) fn keyed(&self, cx: &gpui_kit::App) -> Vec<MessageSummary> {
        let checked = self.checked(cx);
        if checked.is_empty() {
            self.selected(cx)
        } else {
            checked
        }
    }

    /// What the commands do to the messages, for their buttons.
    pub(super) fn applies<'a, I>(&'a self, messages: I) -> Applies
    where
        I: IntoIterator<Item = &'a MessageSummary>,
        I::IntoIter: Clone,
    {
        Applies::of(messages, |account| self.account_folders(account))
    }

    /// Runs `command` on the messages: from a key, a toolbar or a row.
    pub(super) fn run(
        &mut self,
        command: MessageCommand,
        messages: Vec<MessageSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            MessageCommand::Archive => self.archive(&messages, window, cx),
            MessageCommand::Delete => self.delete(&messages, window, cx),
            MessageCommand::Move => self.move_to_folder(messages, window, cx),
            MessageCommand::ToggleRead => self.toggle(&messages, Flags::SEEN, window, cx),
            MessageCommand::ToggleFlag => self.toggle(&messages, Flags::FLAGGED, window, cx),
        }
    }

    pub(super) fn set_flags(
        &mut self,
        ids: Vec<MessageId>,
        add: Flags,
        remove: Flags,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        report_failure(
            "change the message",
            async move { client.set_flags(ids, add, remove).await },
            window,
            cx,
        );
    }

    /// Clears the flag on the messages if they all have it, or else sets it on them all.
    fn toggle(
        &mut self,
        messages: &[MessageSummary],
        flag: Flags,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if messages.is_empty() {
            return;
        }
        let (add, remove) = commands::toggled(messages, flag);
        let ids = messages.iter().map(|m| m.id).collect();
        self.set_flags(ids, add, remove, window, cx);
    }

    /// An account's folders, which the commands go by.
    pub(crate) fn account_folders(&self, account: AccountId) -> Option<&AccountFolders> {
        self.account(account).map(|entry| &entry.folders)
    }

    /// Archives those of the messages Archive does anything to.
    fn archive(
        &mut self,
        messages: &[MessageSummary],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = commands::archivable(messages, |account| self.account_folders(account));
        if ids.is_empty() {
            return;
        }
        let what = what(ids.len(), "archive the message", "archive the messages");
        self.uncheck(&ids, cx);
        self.change(ids.clone(), what, window, cx, move |client| async move {
            client.archive_messages(ids).await
        });
    }

    /// Moves those of the messages, all of one account, that moving to `to` changes. If
    /// the selected one goes, the next one is selected.
    pub(super) fn move_to(
        &mut self,
        messages: &[MessageSummary],
        to: FolderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((account, shown_in)) = self.move_source(messages, cx) else {
            return;
        };
        let ids = commands::movable(&account.folders, messages, shown_in, to);
        if ids.is_empty() {
            return;
        }
        let what = what(ids.len(), "move the message", "move the messages");
        self.uncheck(&ids, cx);
        self.change(ids.clone(), what, window, cx, move |client| async move {
            client.move_messages(ids, shown_in, to).await
        });
    }

    /// The account of the messages, which "Move to" lists the folders of, and the folder
    /// they are looked at in; none if there are none or they are of different accounts.
    fn move_source(
        &self,
        messages: &[MessageSummary],
        cx: &gpui_kit::App,
    ) -> Option<(&AccountEntry, Option<FolderId>)> {
        let account = self.account(commands::one_account(messages)?)?;
        Some((account, self.shown_in(&messages[0], cx)))
    }

    /// Unchecks the messages moved or deleted: those still listed (on Gmail, one moved
    /// to a label from All Mail) have been dealt with.
    fn uncheck(&self, ids: &[MessageId], cx: &mut Context<Self>) {
        self.messages
            .update(cx, |list, cx| message_list::uncheck(list, ids, cx));
    }

    /// The folder the listed message is looked at in: on Gmail, a label it leaves when
    /// moved. In All Inboxes, its account's INBOX; none for search results.
    fn shown_in(&self, message: &MessageSummary, cx: &gpui_kit::App) -> Option<FolderId> {
        if self.is_searching(cx) {
            return None;
        }
        self.shown_folders()
            .find(|f| f.account_id == message.account_id)
            .map(|f| f.id)
    }

    /// Changes the messages with `call`, which may take them out of the list: if the
    /// selected one goes, the next one is selected. Whether it goes is the store's to say (on Gmail, one moved
    /// to a label from All Mail stays): once the change is made, the list is read again,
    /// and a message still in it stays selected (see `message_list::removal_settled`).
    fn change<F: Future<Output = catmail_core::Result<Vec<AccountId>>> + 'static>(
        &mut self,
        ids: Vec<MessageId>,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        call: impl FnOnce(catmail_core::MailClient) -> F,
    ) {
        // Only the selected message's removal is waited for: another's (a row's own
        // command) must not take the place of one pending for it.
        let removal = self.messages.update(cx, |list, _| {
            let list = list.delegate_mut();
            let selected = list.selected_message().map(|m| m.id);
            selected
                .filter(|id| ids.contains(id))
                .map(|id| list.expect_removal(id))
        });
        let call = call(self.client.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(e) = &result {
                    window.push_notification(
                        Notification::error(format!("Couldn't {what}: {e}")),
                        cx,
                    );
                }
                if let Some(removal) = removal {
                    this.messages.update(cx, |list, cx| {
                        message_list::removal_settled(list, removal, result.is_ok(), window, cx)
                    });
                }
            })
            .ok();
        })
        .detach();
    }

    /// Each to Trash, or for good from Trash or without one, as the engine will
    /// ([`DeleteOutcome`]): one change. A message deleted for good can't be brought back,
    /// so that asks first, once for them all.
    fn delete(&mut self, messages: &[MessageSummary], window: &mut Window, cx: &mut Context<Self>) {
        let Deletion { ids, mode, confirm } =
            commands::deletion(messages, |account| self.account_folders(account));
        let Some(confirm) = confirm else {
            return self.delete_now(ids, mode, window, cx);
        };
        let this = cx.entity().downgrade();
        layout::confirm_danger(
            confirm.title,
            confirm.description,
            "Delete",
            window,
            cx,
            move |window, cx| {
                let ids = ids.clone();
                this.update(cx, |this, cx| this.delete_now(ids, mode, window, cx))
                    .ok();
            },
        );
    }

    fn delete_now(
        &mut self,
        ids: Vec<MessageId>,
        mode: DeleteMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        let what = what(ids.len(), "delete the message", "delete the messages");
        self.uncheck(&ids, cx);
        self.change(ids.clone(), what, window, cx, move |client| async move {
            client.delete_messages(ids, mode).await
        });
    }

    /// Asks where to move the messages, all of one account: to the folders moving any of
    /// them to changes.
    fn move_to_folder(
        &mut self,
        messages: Vec<MessageSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((account, shown_in)) = self.move_source(&messages, cx) else {
            return;
        };
        let folders = commands::move_targets(&account.folders, &messages, shown_in);
        let this = cx.entity().downgrade();
        folder_picker::open(folders, window, cx, move |folder, window, cx| {
            this.update(cx, |this, cx| this.move_to(&messages, folder, window, cx))
                .ok();
        });
    }

    /// Whether the selected message has labels to edit: it is in a Gmail account.
    pub(crate) fn can_edit_labels(&self, cx: &gpui_kit::App) -> bool {
        self.selected_message(cx)
            .is_some_and(|message| self.labels_account(&message).is_some())
    }

    /// The message's account, if it has labels: on Gmail.
    fn labels_account(&self, message: &MessageSummary) -> Option<&AccountEntry> {
        self.account(message.account_id).filter(|a| a.has_labels())
    }

    /// Opens "Labels" for the selected message, which changes them as they are picked.
    /// It shows the message's labels as they are, and closes once the message is no
    /// longer the one selected ([`sync_labels_dialog`](Self::sync_labels_dialog)).
    pub(crate) fn edit_labels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.selected_message(cx) else {
            return;
        };
        let Some(account) = self.labels_account(&message) else {
            return;
        };
        let labels = account.folders.user_labels();
        let checked = message.labels.iter().copied().collect();
        let this = cx.entity().downgrade();
        let id = message.id;
        let list =
            folder_picker::open_labels(labels, checked, window, cx, move |label, window, cx| {
                this.update(cx, |this, cx| this.toggle_label(id, label, window, cx))
                    .ok();
            });
        self.labels_dialog = Some(LabelsDialog { message: id, list });
    }

    /// Keeps "Labels" to the message it was opened for: its checks follow the message's
    /// labels as the list has them (a change refused and undone, or made elsewhere, shows
    /// there too), and it closes once the message is no longer selected.
    pub(super) fn sync_labels_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.labels_dialog else {
            return;
        };
        let Some(list) = dialog.list.upgrade() else {
            self.labels_dialog = None; // Closed.
            return;
        };
        match self.selected_message(cx).filter(|m| m.id == dialog.message) {
            Some(message) => list.update(cx, |list, cx| {
                if list
                    .delegate_mut()
                    .set_checked(message.labels.into_iter().collect())
                {
                    cx.notify();
                }
            }),
            None => {
                self.labels_dialog = None;
                window.close_dialog(cx);
            }
        }
    }

    /// Adds `label` to the message, the selected one, or takes it off, as it has it now.
    fn toggle_label(
        &mut self,
        id: MessageId,
        label: FolderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.selected_message(cx).filter(|m| m.id == id) else {
            return;
        };
        let (add, remove) = commands::label_change(&message, label);
        // Off the label it is listed in, it leaves the list: on to the next one.
        self.change(
            vec![id],
            "change the labels",
            window,
            cx,
            move |client| async move { client.label_messages(vec![id], add, remove).await },
        );
    }

    pub(crate) fn compose(
        &mut self,
        from: ComposeFrom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        composer::open(self.client.clone(), from, window, cx);
    }

    pub(crate) fn new_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.accounts.is_none() {
            self.compose_once_loaded = true;
            return;
        }
        let accounts = self.accounts.as_deref().unwrap_or_default();
        if let Some(account) = composer::new_message_account(self.place, accounts) {
            self.compose(ComposeFrom::New(account), window, cx);
        }
    }

    pub(crate) fn reply(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.selected_message(cx) {
            self.compose(
                ComposeFrom::Reply {
                    message: message.id,
                    all,
                },
                window,
                cx,
            );
        }
    }

    pub(crate) fn forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.selected_message(cx) {
            self.compose(ComposeFrom::Forward(message.id), window, cx);
        }
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        settings_dialog::open(self.client.clone(), cx.entity().downgrade(), window, cx);
    }
}
