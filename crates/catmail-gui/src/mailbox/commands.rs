//! The commands on messages: the same icon buttons over the message open, by the list's
//! heading for the messages checked and on a row while hovered; what they do, which ones
//! ask first, and "Move to" and "Labels" (their dialog is `picker.rs`). What they apply
//! to, what they change and what they say are `catmail_frontend::commands`'; here they
//! are run.

use catmail_core::{Flags, FolderId, MessageId, MessageSummary};
use catmail_frontend::commands::{self, Applies, Change, Deletion, MessageCommand};

use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, separator};
use crate::engine::client;
use crate::form::confirmed;
use crate::icons::Icon;
use crate::mailbox::list;
use crate::mailbox::picker::{Picker, Picking, open_picker};
use crate::mailbox::sidebar::{Accounts, with_account, with_accounts};
use crate::tasks::in_view;
use crate::toast::{Level, notify, report};

/// What the commands do to the messages, for their buttons. Tracked: the buttons follow
/// the accounts' folders.
pub fn applies(messages: &[MessageSummary]) -> Applies {
    global::<Accounts>().with(|a| Applies::of(messages, a.list()))
}

/// Runs `command` on the messages: from a key, a toolbar or a row.
pub fn run(command: MessageCommand, messages: Vec<MessageSummary>) {
    if messages.is_empty() {
        return;
    }
    match command {
        MessageCommand::Archive => {
            run_change(with_accounts(|a| commands::archivable(&messages, a)));
        }
        MessageCommand::Delete => delete(&messages),
        MessageCommand::Move => move_to_folder(messages),
        MessageCommand::ToggleRead => run_change(commands::toggled(&messages, Flags::SEEN)),
        MessageCommand::ToggleFlag => run_change(commands::toggled(&messages, Flags::FLAGGED)),
    }
}

/// Makes `change`. If it may take the messages out of the list and the selected one goes,
/// the next one is selected (`ListModel::expect_removal`); those dealt with are no longer
/// checked.
pub fn run_change(change: Change) {
    if change.is_empty() {
        return;
    }
    if change.unchecks() {
        list::uncheck(&change.ids);
    }
    let removal = change
        .may_remove()
        .then(|| list::expect_removal(&change.ids))
        .flatten();
    in_view(async move {
        let result = change.run(&client()).await;
        if let Err(e) = &result {
            report(&change.failure(), e);
        }
        if let Some(removal) = removal {
            list::removal_settled(removal, result.is_ok());
        }
    });
}

/// Moves those of the messages, all of one account, that moving to `to` changes.
pub fn move_to(messages: &[MessageSummary], to: FolderId) {
    let source = list::source();
    run_change(with_accounts(|a| {
        commands::movable(messages, a, source.as_ref(), to)
    }));
}

/// Each to Trash, or for good from Trash or without one, as the engine will: one change.
/// A message deleted for good can't be brought back, so that asks first.
fn delete(messages: &[MessageSummary]) {
    let Deletion { change, confirm } = with_accounts(|a| commands::deletion(messages, a));
    // Something goes for good: there is something to delete.
    let Some(confirm) = confirm else {
        return run_change(change);
    };
    let answer = confirmed(confirm);
    in_view(async move {
        if answer.await {
            run_change(change);
        }
    });
}

/// Asks where to move the messages, all of one account: to the folders moving any of
/// them to changes.
fn move_to_folder(messages: Vec<MessageSummary>) {
    let source = list::source();
    let folders = with_accounts(|a| commands::move_targets(&messages, a, source.as_ref()));
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
    run_change(commands::label_change(&message, label));
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
