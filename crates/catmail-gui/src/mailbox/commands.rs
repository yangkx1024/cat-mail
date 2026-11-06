//! The commands on messages: the same icon buttons over the message open, by the list's
//! heading for the messages checked and on a row while hovered; what they do, which ones
//! ask first, and "Move to" and "Labels" (their dialog is `picker.rs`). What they apply
//! to and what they say are `catmail_frontend::commands`'.

use catmail_core::{AccountId, Flags, FolderId, MessageId, MessageSummary};
use catmail_frontend::commands::{self, Applies, Deletion, MessageCommand, what};
use catmail_frontend::folders::AccountFolders;
use catmail_frontend::mailbox::AccountEntry;

use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, separator};
use crate::engine::client;
use crate::form::confirmed;
use crate::icons::Icon;
use crate::mailbox::list;
use crate::mailbox::picker::{Picker, Picking, open_picker};
use crate::mailbox::sidebar::{self, Accounts, with_account, with_accounts};
use crate::tasks::in_view;
use crate::toast::{Level, notify, report};

/// An account's folders, among `accounts`.
pub fn folders_in<'a>(
    accounts: &'a [AccountEntry],
) -> impl Fn(AccountId) -> Option<&'a AccountFolders> + 'a {
    move |id| {
        accounts
            .iter()
            .find(|e| e.account.id == id)
            .map(|e| &e.folders)
    }
}

/// What the commands do to the messages, for their buttons. Tracked: the buttons follow
/// the accounts' folders.
pub fn applies(messages: &[MessageSummary]) -> Applies {
    global::<Accounts>().with(|a| {
        let accounts = a.0.as_deref().unwrap_or_default();
        Applies::of(messages, folders_in(accounts))
    })
}

/// Runs `command` on the messages: from a key, a toolbar or a row.
pub fn run(command: MessageCommand, messages: Vec<MessageSummary>) {
    if messages.is_empty() {
        return;
    }
    match command {
        MessageCommand::Archive => archive(&messages),
        MessageCommand::Delete => delete(&messages),
        MessageCommand::Move => move_to_folder(messages),
        MessageCommand::ToggleRead => toggle(&messages, Flags::SEEN),
        MessageCommand::ToggleFlag => toggle(&messages, Flags::FLAGGED),
    }
}

pub fn set_flags(ids: Vec<MessageId>, add: Flags, remove: Flags) {
    in_view(async move {
        if let Err(e) = client().set_flags(ids, add, remove).await {
            report("Couldn't change the message", e);
        }
    });
}

fn toggle(messages: &[MessageSummary], flag: Flags) {
    let (add, remove) = commands::toggled(messages, flag);
    set_flags(messages.iter().map(|m| m.id).collect(), add, remove);
}

fn archive(messages: &[MessageSummary]) {
    let ids = with_accounts(|accounts| commands::archivable(messages, folders_in(accounts)));
    if ids.is_empty() {
        return;
    }
    let what = what(ids.len(), "archive the message", "archive the messages");
    list::uncheck(&ids);
    let call = {
        let ids = ids.clone();
        async move { client().archive_messages(ids).await }
    };
    change(ids, what, call);
}

/// The folder the listed message is looked at in: on Gmail, a label it leaves when moved.
/// In All Inboxes, its account's INBOX; none for search results.
fn shown_in(message: &MessageSummary) -> Option<FolderId> {
    if list::is_searching() {
        return None;
    }
    let shown = sidebar::shown_folders();
    with_accounts(|accounts| {
        let folders = folders_in(accounts)(message.account_id)?;
        shown.into_iter().find(|&id| folders.get(id).is_some())
    })
}

/// Moves those of the messages, all of one account, that moving to `to` changes.
pub fn move_to(messages: &[MessageSummary], to: FolderId) {
    let Some(first) = messages.first() else {
        return;
    };
    let shown_in = shown_in(first);
    let ids = with_accounts(|accounts| {
        let account = commands::one_account(messages)?;
        let folders = folders_in(accounts)(account)?;
        Some(commands::movable(folders, messages, shown_in, to))
    })
    .unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let what = what(ids.len(), "move the message", "move the messages");
    list::uncheck(&ids);
    let call = {
        let ids = ids.clone();
        async move { client().move_messages(ids, shown_in, to).await }
    };
    change(ids, what, call);
}

/// Changes the messages with `call`, which may take them out of the list: if the
/// selected one goes, the next one is selected (`list::expect_removal`).
fn change(
    ids: Vec<MessageId>,
    what: &'static str,
    call: impl Future<Output = catmail_core::Result<Vec<AccountId>>> + 'static,
) {
    // Only the selected message's removal is waited for: another's (a row's own command)
    // must not take the place of one pending for it.
    let removal = list::selected_message()
        .map(|m| m.id)
        .filter(|id| ids.contains(id))
        .map(list::expect_removal);
    in_view(async move {
        let result = call.await;
        if let Err(e) = &result {
            report(&format!("Couldn't {what}"), e);
        }
        if let Some(removal) = removal {
            list::removal_settled(removal, result.is_ok());
        }
    });
}

/// Each to Trash, or for good from Trash or without one, as the engine will: one change.
/// A message deleted for good can't be brought back, so that asks first.
fn delete(messages: &[MessageSummary]) {
    let Deletion { ids, mode, confirm } =
        with_accounts(|accounts| commands::deletion(messages, folders_in(accounts)));
    if ids.is_empty() {
        return;
    }
    let delete_now = move || {
        let what = what(ids.len(), "delete the message", "delete the messages");
        list::uncheck(&ids);
        let call = {
            let ids = ids.clone();
            async move { client().delete_messages(ids, mode).await }
        };
        change(ids, what, call);
    };
    let Some(confirm) = confirm else {
        return delete_now();
    };
    let answer = confirmed(confirm);
    in_view(async move {
        if answer.await {
            delete_now();
        }
    });
}

/// Asks where to move the messages, all of one account: to the folders moving any of
/// them to changes.
fn move_to_folder(messages: Vec<MessageSummary>) {
    let Some(first) = messages.first() else {
        return;
    };
    let shown_in = shown_in(first);
    let folders = with_accounts(|accounts| {
        let account = commands::one_account(&messages)?;
        let folders = folders_in(accounts)(account)?;
        Some(commands::move_targets(folders, &messages, shown_in))
    });
    let Some(folders) = folders else {
        return;
    };
    open_picker(Picker {
        kind: Picking::Move(messages),
        folders,
    });
}

/// The message's account, if it has labels to edit: on Gmail.
pub fn can_edit_labels(message: &MessageSummary) -> bool {
    global::<Accounts>().with(|a| {
        a.0.iter()
            .flatten()
            .any(|e| e.account.id == message.account_id && e.has_labels())
    })
}

/// Opens "Labels" for the selected message, which changes them as they are picked. Its
/// checks are the message's labels as the list has them, and it closes once the message
/// is no longer the one selected.
pub fn edit_labels() {
    let Some(message) = list::selected_message() else {
        return;
    };
    let labels = with_account(message.account_id, |e| {
        e.has_labels().then(|| e.folders.user_labels())
    })
    .flatten();
    let Some(labels) = labels else {
        return;
    };
    open_picker(Picker {
        kind: Picking::Labels(message.id),
        folders: labels,
    });
}

/// Adds `label` to the message, the selected one, or takes it off, as it has it now.
/// Off the label it is listed in, it leaves the list: on to the next one.
pub fn toggle_label(id: MessageId, label: FolderId) {
    let Some(message) = list::selected_message().filter(|m| m.id == id) else {
        return;
    };
    let (add, remove) = commands::label_change(&message, label);
    let call = async move { client().label_messages(vec![id], add, remove).await };
    change(vec![id], "change the labels", call);
}

/// The server refused changes, which the engine undid: says so, once per change, and
/// forgets them. One check at a time, and one more if asked meanwhile.
pub fn report_refused_changes() {
    if !global::<RefusedCheck>().with_untracked(|c| c.0.borrow_mut().start()) {
        return;
    }
    in_view(async {
        let refused = client().dismiss_settled(None).await.unwrap_or_default();
        for op in &refused {
            notify(Level::Warning, commands::refused_change(op));
        }
        if global::<RefusedCheck>().with_untracked(|c| c.0.borrow_mut().finish()) {
            report_refused_changes();
        }
    });
}

#[derive(Clone, Default)]
struct RefusedCheck(std::rc::Rc<std::cell::RefCell<catmail_frontend::refresh::Coalesce>>);

// --- Buttons ------------------------------------------------------------------------

/// A command's icon, for messages that all have `flags`: a toggle shows what it does.
fn command_icon(command: MessageCommand, flags: Flags) -> Icon {
    match command {
        MessageCommand::Archive => Icon::ArchiveBox,
        MessageCommand::Delete => Icon::Trash,
        MessageCommand::Move => Icon::FolderArrowRight,
        MessageCommand::ToggleRead if flags.contains(Flags::SEEN) => Icon::Envelope,
        MessageCommand::ToggleRead => Icon::EnvelopeOpen,
        MessageCommand::ToggleFlag if flags.contains(Flags::FLAGGED) => Icon::FlagSolid,
        MessageCommand::ToggleFlag => Icon::Flag,
    }
}

/// The commands' buttons, in their two groups: where the messages are, and their marks.
/// `messages` gives those they act on; the buttons follow what the commands do to them.
pub fn buttons(
    messages: impl Fn() -> Vec<MessageSummary> + Copy + 'static,
    size: Size,
) -> impl IntoElement {
    let applies = computed(move || applies(&messages()));
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(2.0))
        .child(group(&MessageCommand::PLACE, messages, applies, size))
        .child(separator(size))
        .child(group(&MessageCommand::MARKS, messages, applies, size))
}

/// The buttons of one group of `commands`, as [`buttons`] has them: a toolbar puts
/// buttons of its own between the groups. `applies` is what the commands do to `messages`
/// ([`applies`]), one for a toolbar's groups.
pub fn group(
    commands: &[MessageCommand],
    messages: impl Fn() -> Vec<MessageSummary> + Copy + 'static,
    applies: Memo<Applies>,
    size: Size,
) -> View {
    let button = move |&command: &MessageCommand| {
        let flags = move || applies.with(|a| a.flags());
        icon_button(
            move || command_icon(command, flags()),
            move || command.label(flags()).to_owned(),
            move || applies.with(|a| a.enables(command)),
            size,
            move || run(command, untracked(messages)),
        )
    };
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(2.0))
        .children(commands.iter().map(button).collect::<Vec<_>>())
}
