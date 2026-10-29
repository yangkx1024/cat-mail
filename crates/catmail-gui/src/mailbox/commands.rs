//! The mailbox's commands: on the selected message (the toolbar, the menus and the keys
//! all call these), writing, syncing and settings.

use catmail_core::{
    Account, AccountId, AccountMode, Flags, FolderId, MessageId, MessageSummary, OpKind, OpStatus,
    PendingOp,
};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Context, Window};

use crate::composer::{self, ComposeFrom};
use crate::layout;

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
    /// Syncs every account, unless a sync is already running.
    pub(crate) fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing > 0 {
            return;
        }
        let accounts: Vec<Account> = self
            .accounts
            .iter()
            .flatten()
            .map(|a| a.account.clone())
            .collect();
        self.sync_accounts(accounts, false, window, cx);
    }

    /// Syncs the accounts in turn; `initial` for accounts just added, whose first sync
    /// background sync is already running (see `MailClient::initial_sync`). Progress
    /// shows as it commits, through the engine's events; only failures are reported here.
    pub(super) fn sync_accounts(
        &mut self,
        accounts: Vec<Account>,
        initial: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.syncing += 1;
        cx.notify();
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut problems = Vec::new();
            for account in accounts {
                let synced = if initial {
                    client.initial_sync(account.id).await
                } else {
                    client.sync_account(account.id).await
                };
                match synced {
                    Ok(report) => {
                        let failed = report.folders.iter().filter(|f| f.error.is_some()).count();
                        if failed > 0 {
                            problems.push(Notification::warning(format!(
                                "{}: {failed} folder(s) couldn't be synced",
                                account.email
                            )));
                        }
                    }
                    Err(e) => problems.push(Notification::error(format!(
                        "Couldn't sync {}: {e}",
                        account.email
                    ))),
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.syncing -= 1;
                for problem in problems {
                    window.push_notification(problem, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                    window.push_notification(Notification::warning(refused_change(op)), cx);
                }
                if this.checking_refused.finish() {
                    this.report_refused_changes(window, cx);
                }
            })
            .ok();
        });
    }

    // Commands on the selected message.

    pub(super) fn selected_message(&self, cx: &gpui_kit::App) -> Option<MessageSummary> {
        self.messages
            .read(cx)
            .delegate()
            .selected_message()
            .cloned()
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

    /// Sets the flag on the selected message, or clears it if it is set.
    pub(crate) fn toggle(&mut self, flag: Flags, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.selected_message(cx) else {
            return;
        };
        let (add, remove) = if message.flags.contains(flag) {
            (Flags::empty(), flag)
        } else {
            (flag, Flags::empty())
        };
        self.set_flags(vec![message.id], add, remove, window, cx);
    }

    /// Whether Archive does anything to `message`: its account has an Archive folder it
    /// isn't in, or on Gmail it is in INBOX and All Mail.
    pub(crate) fn can_archive(&self, message: &MessageSummary) -> bool {
        self.account(message.account_id)
            .is_some_and(|a| a.folders.can_archive(message))
    }

    pub(crate) fn archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.selected_message(cx) else {
            return;
        };
        if !self.can_archive(&message) {
            return;
        }
        self.change(
            message.id,
            "archive the message",
            window,
            cx,
            move |client| async move { client.archive_messages(vec![message.id]).await },
        );
    }

    /// Moves the selected message, and selects the next one once it has gone, if it goes.
    pub(crate) fn move_selected(
        &mut self,
        to: FolderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.selected_message(cx) else {
            return;
        };
        let shown_in = self.shown_in(cx);
        self.change(
            message.id,
            "move the message",
            window,
            cx,
            move |client| async move { client.move_messages(vec![message.id], shown_in, to).await },
        );
    }

    /// The folder the listed messages are looked at in: on Gmail, a label they leave when
    /// moved. None for search results.
    fn shown_in(&self, cx: &gpui_kit::App) -> Option<FolderId> {
        self.messages.read(cx).delegate().shown_folder()
    }

    /// Changes the message with `call`, which may take it out of the list: if it goes,
    /// the next one is selected. Whether it goes is the store's to say (on Gmail, one moved
    /// to a label from All Mail stays): once the change is made, the list is read again,
    /// and a message still in it stays selected (see `message_list::removal_settled`).
    fn change<F: Future<Output = catmail_core::Result<Vec<AccountId>>> + 'static>(
        &mut self,
        id: MessageId,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        call: impl FnOnce(catmail_core::MailClient) -> F,
    ) {
        let removal = self
            .messages
            .update(cx, |list, _| list.delegate_mut().expect_removal(id));
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
                this.messages.update(cx, |list, cx| {
                    message_list::removal_settled(list, removal, result.is_ok(), window, cx)
                });
            })
            .ok();
        })
        .detach();
    }

    /// To Trash, or for good from Trash or without one, as the engine will
    /// ([`DeleteOutcome`](catmail_core::DeleteOutcome)). A message deleted for good
    /// can't be brought back, so that asks first.
    pub(crate) fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.selected_message(cx) else {
            return;
        };
        let Some(account) = self.account(message.account_id) else {
            return;
        };
        if !account
            .folders
            .delete_outcome(message.folder_id)
            .is_permanent()
        {
            self.delete_selected(message.id, false, window, cx);
            return;
        }
        let subject = crate::format::subject(message.subject.as_deref());
        let this = cx.entity().downgrade();
        layout::confirm_danger(
            format!("Delete “{subject}” for good?"),
            "It is removed from the server and can't be brought back.",
            "Delete",
            window,
            cx,
            move |window, cx| {
                this.update(cx, |this, cx| {
                    this.delete_selected(message.id, true, window, cx)
                })
                .ok();
            },
        );
    }

    fn delete_selected(
        &mut self,
        id: MessageId,
        permanently: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change(
            id,
            "delete the message",
            window,
            cx,
            move |client| async move { client.delete_messages(vec![id], permanently).await },
        );
    }

    pub(crate) fn move_to_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.selected_message(cx) else {
            return;
        };
        let Some(account) = self.account(message.account_id) else {
            return;
        };
        let shown_in = self.shown_in(cx);
        let folders = account
            .folders
            .paths()
            .into_iter()
            .filter(|(folder, _)| {
                account
                    .folders
                    .can_move(message.folder_id, shown_in, folder.id)
            })
            .map(|(folder, path)| (folder.id, path))
            .collect();
        let this = cx.entity().downgrade();
        folder_picker::open(folders, window, cx, move |folder, window, cx| {
            this.update(cx, |this, cx| this.move_selected(folder, window, cx))
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
        self.account(message.account_id)
            .filter(|a| a.account.mode == Some(AccountMode::Gmail))
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
        let (add, remove) = if message.labels.contains(&label) {
            (Vec::new(), vec![label])
        } else {
            (vec![label], Vec::new())
        };
        // Off the label it is listed in, it leaves the list: on to the next one.
        self.change(
            id,
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
        let account = self
            .current_account()
            .or_else(|| self.accounts.as_deref()?.first())
            .map(|a| a.account.id);
        if let Some(account) = account {
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

/// What to tell the user about a change the server didn't make.
fn refused_change(op: &PendingOp) -> String {
    let subject = op.subject.as_deref().unwrap_or("a message");
    match op.status {
        OpStatus::Conflict => {
            format!("A change to “{subject}” was dropped: the folder changed on the server first.")
        }
        _ => {
            let verb = match op.kind {
                OpKind::Flags { .. } | OpKind::Label { .. } => "change",
                OpKind::Move { .. } => "move",
                OpKind::Delete => "delete",
            };
            format!(
                "The server didn't {verb} “{subject}”, so it is as it was. {}",
                op.last_error.as_deref().unwrap_or_default()
            )
            .trim_end()
            .to_owned()
        }
    }
}
