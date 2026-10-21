//! The mailbox's commands: on the selected message (the toolbar, the menus and the keys
//! all call these), writing, syncing and settings.

use catmail_core::{Account, AccountId, Flags, FolderId, MessageId, OpKind, OpStatus, PendingOp};
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
        self.sync_accounts(accounts, window, cx);
    }

    /// Syncs the accounts in turn. Progress shows as it commits, through the engine's
    /// events; only failures are reported here.
    pub(super) fn sync_accounts(
        &mut self,
        accounts: Vec<Account>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.syncing += 1;
        cx.notify();
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut problems = Vec::new();
            for account in accounts {
                match client.sync_account(account.id).await {
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
    /// per change, and forgets them. Coalesced like reading (dismissing them sends
    /// another event).
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

    pub(super) fn selected_message(
        &self,
        cx: &gpui_kit::App,
    ) -> Option<catmail_core::MessageSummary> {
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

    /// The folder Archive moves the selected message to, if its account has one and the
    /// message isn't archived already.
    pub(crate) fn archive_folder(&self, cx: &gpui_kit::App) -> Option<FolderId> {
        let message = self.selected_message(cx)?;
        self.account(message.account_id)?
            .folders
            .archive_folder(message.folder_id)
    }

    pub(crate) fn archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.selected_message(cx)
            && self
                .account(message.account_id)
                .is_some_and(|a| a.folders.archive_folder(message.folder_id).is_some())
        {
            self.remove(
                message.id,
                "archive the message",
                window,
                cx,
                move |client| async move { client.archive_messages(vec![message.id]).await },
            );
        }
    }

    /// Moves the selected message, and selects the next one once it has gone.
    pub(crate) fn move_selected(
        &mut self,
        to: FolderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(message) = self.selected_message(cx) {
            self.remove(
                message.id,
                "move the message",
                window,
                cx,
                move |client| async move { client.move_messages(vec![message.id], to).await },
            );
        }
    }

    /// Takes the message out of the list with `call`, selecting the next one once it has
    /// gone.
    fn remove<F: Future<Output = catmail_core::Result<Vec<AccountId>>> + 'static>(
        &mut self,
        id: MessageId,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        call: impl FnOnce(catmail_core::MailClient) -> F,
    ) {
        self.messages
            .update(cx, |list, _| list.delegate_mut().expect_removal(id));
        report_failure(what, call(self.client.clone()), window, cx);
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
        self.remove(
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
        let folders = account
            .folders
            .paths()
            .into_iter()
            .filter(|(folder, _)| folder.id != message.folder_id)
            .map(|(folder, path)| (folder.id, path))
            .collect();
        let this = cx.entity().downgrade();
        move_to::open(folders, window, cx, move |folder, window, cx| {
            this.update(cx, |this, cx| this.move_selected(folder, window, cx))
                .ok();
        });
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
                OpKind::Flags { .. } => "change",
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
