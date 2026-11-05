//! The commands on messages, as the GUI's (`mailbox/commands.rs`, `message_commands.rs`):
//! the same icon buttons over the message open, by the list's heading for the messages
//! checked and on a row while hovered; what they do, which ones ask first, and "Move to"
//! and "Labels". What they apply to and what they say are `catmail_frontend::commands`'.

use std::collections::HashSet;
use std::rc::Rc;

use catmail_core::{AccountId, Flags, FolderId, MessageId, MessageSummary};
use catmail_frontend::commands::{self, Applies, Deletion, MessageCommand, what};
use catmail_frontend::folders::AccountFolders;
use catmail_frontend::mailbox::AccountEntry;

use nsh_base::{alert_dialog, theme};
use nsh_core::ModalAnimation;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox};
use crate::icons::{Icon, icon};
use crate::list;
use crate::sidebar::{self, Accounts};
use crate::toast::{Level, notify};

/// `f` with the accounts as the sidebar has them.
pub fn with_accounts<T>(f: impl FnOnce(&[AccountEntry]) -> T) -> T {
    global::<Accounts>().with_untracked(|a| f(a.0.as_deref().unwrap_or_default()))
}

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
    in_mailbox(async move {
        if let Err(e) = client().set_flags(ids, add, remove).await {
            notify(Level::Error, format!("Couldn't change the message: {e}"));
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
fn move_to(messages: &[MessageSummary], to: FolderId) {
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
    in_mailbox(async move {
        let result = call.await;
        if let Err(e) = &result {
            notify(Level::Error, format!("Couldn't {what}: {e}"));
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
    let answer = alert_dialog()
        .title(confirm.title)
        .description(confirm.description)
        .cancel("cancel", "Cancel")
        .destructive("delete", "Delete")
        .show();
    in_mailbox(async move {
        if answer
            .await
            .is_ok_and(|r| r.action.as_deref() == Some("delete"))
        {
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
    let labels = with_accounts(|accounts| {
        accounts
            .iter()
            .find(|e| e.account.id == message.account_id && e.has_labels())
            .map(|e| e.folders.user_labels())
    });
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
fn toggle_label(id: MessageId, label: FolderId) {
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
    in_mailbox(async {
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

/// How big the buttons are: a toolbar's, or a row's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Bar,
    Row,
}

impl Size {
    fn box_size(self) -> f32 {
        match self {
            Size::Bar => 30.0,
            Size::Row => 24.0,
        }
    }

    fn icon_size(self) -> f32 {
        match self {
            Size::Bar => 18.0,
            Size::Row => 15.0,
        }
    }
}

/// The commands' buttons, in their two groups: where the messages are, and their marks.
/// `messages` gives those they act on; the buttons follow what the commands do to them.
pub fn buttons(
    messages: impl Fn() -> Vec<MessageSummary> + Copy + 'static,
    size: Size,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(2.0))
        .child(group(&MessageCommand::PLACE, messages, size))
        .child(separator(size))
        .child(group(&MessageCommand::MARKS, messages, size))
}

/// The buttons of one group of `commands`, as [`buttons`] has them: a toolbar puts
/// buttons of its own between the groups.
pub fn group(
    commands: &[MessageCommand],
    messages: impl Fn() -> Vec<MessageSummary> + Copy + 'static,
    size: Size,
) -> View {
    let applies = computed(move || applies(&messages()));
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

/// Between groups of a toolbar's buttons.
pub fn separator(size: Size) -> View {
    view()
        .w(px(1.0))
        .h(px(size.box_size() - 12.0))
        .mx(px(4.0))
        .bg(move || theme().border)
}

/// An icon that does `press`; its name shows as a tooltip and is what screen readers
/// say. Disabled, it is dimmed and does nothing.
pub fn icon_button(
    which: impl Fn() -> Icon + 'static,
    name: impl Fn() -> String + Copy + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    size: Size,
    press: impl Fn() + 'static,
) -> View {
    let hovered = signal(false);
    view()
        .w(px(size.box_size()))
        .h(px(size.box_size()))
        .flex_shrink_0()
        .rounded(px(6.0))
        .items_center()
        .justify_center()
        .opacity(move || if enabled() { 1.0_f32 } else { 0.4 })
        .bg(move || {
            if hovered.get() && enabled() {
                theme().accent
            } else {
                AdaptiveColor::TRANSPARENT
            }
        })
        .label(move || SharedString::from(name()))
        .on_mouse_enter(move |_: &MouseEvent| hovered.set(true))
        .on_mouse_leave(move |_: &MouseEvent| hovered.set(false))
        .on_press(move || {
            if untracked(enabled) {
                press();
            }
        })
        .tooltip(tooltip(name))
        .child(icon(which, size.icon_size()).text_color(move || theme().foreground))
}

/// The button of the main thing to do where it is (Compose, Send, Edit): filled, round,
/// with an icon and a label.
pub fn primary_button(
    which: Icon,
    label: impl Fn() -> String + 'static,
    hint: impl Fn() -> String + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> View {
    let label = Rc::new(label);
    let name = label.clone();
    view()
        .flex_row()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .gap(px(8.0))
        .h(px(34.0))
        .px(px(18.0))
        .rounded(px(17.0))
        .bg(move || theme().primary)
        .opacity(move || if enabled() { 1.0_f32 } else { 0.5 })
        .label(move || SharedString::from(name()))
        .on_press(move || {
            if untracked(enabled) {
                press();
            }
        })
        .tooltip(tooltip(hint))
        .child(icon(move || which, 16.0).text_color(move || theme().primary_foreground))
        .child(
            text(move || label())
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().primary_foreground),
        )
}

/// A tooltip that says `text`.
pub fn tooltip(text_: impl Fn() -> String + 'static) -> AnyElement {
    view()
        .max_w(px(320.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(move || theme().popover)
        .border_1()
        .border_color(move || theme().border)
        .child(
            text(text_)
                .text_xs()
                .text_color(move || theme().popover_foreground),
        )
        .into_element()
}

// --- "Move to" and "Labels" ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Picking {
    /// Picks a folder to move these to, and closes.
    Move(Vec<MessageSummary>),
    /// Checks the message's labels and toggles one as it is picked, staying open.
    Labels(MessageId),
}

#[derive(Debug, Clone, PartialEq)]
struct Picker {
    kind: Picking,
    folders: Vec<(FolderId, String)>,
}

/// The picker open, if one is.
#[derive(Clone, Default)]
pub struct OpenPicker(Option<Picker>);

fn open_picker(picker: Picker) {
    global::<OpenPicker>().set(OpenPicker(Some(picker)));
}

fn close_picker() {
    global::<OpenPicker>().set(OpenPicker(None));
}

/// The picker, while one is open: filtered as you type, Enter or a click picks.
pub fn picker() -> impl IntoElement {
    dynamic(move || match global::<OpenPicker>().with(|p| p.0.clone()) {
        Some(picker) => picker_dialog(picker).into_element(),
        None => view().into_element(),
    })
}

fn picker_dialog(picker: Picker) -> impl IntoElement {
    let Picker { kind, folders } = picker;
    let labels = match &kind {
        Picking::Labels(id) => Some(*id),
        Picking::Move(_) => None,
    };
    if let Some(id) = labels {
        // Closes once the message isn't the one selected (in a label, taking it off the
        // label opens the next): it would change another.
        effect(move || {
            if list::selected_id() != Some(id) {
                untracked(close_picker);
            }
        });
    }
    // The message's labels as the list has them now: a change refused and undone, or made
    // elsewhere, shows too.
    let checked = computed(move || -> HashSet<FolderId> {
        match labels {
            Some(id) => list::message(id)
                .map(|m| m.labels.into_iter().collect())
                .unwrap_or_default(),
            None => HashSet::new(),
        }
    });
    let any = !folders.is_empty();
    let query = signal(String::new());
    let shown = computed(move || {
        query.with(|q| {
            folders
                .iter()
                .filter(|(_, name)| commands::picker_matches(name, q))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let highlighted = signal(0usize);
    let kind = std::rc::Rc::new(kind);
    let pick = {
        let kind = kind.clone();
        move |folder: FolderId| match &*kind {
            Picking::Move(messages) => {
                close_picker();
                move_to(messages, folder);
            }
            Picking::Labels(id) => toggle_label(*id, folder),
        }
    };
    let pick = std::rc::Rc::new(pick);
    let pick_highlighted = {
        let pick = pick.clone();
        move || {
            let folder = shown.with_untracked(|s| s.get(highlighted.get_untracked()).map(|f| f.0));
            if let Some(folder) = folder {
                pick(folder);
            }
        }
    };
    on_key("down", move |_| {
        let len = shown.with_untracked(Vec::len);
        highlighted.update(|h| *h = (*h + 1).min(len.saturating_sub(1)));
    });
    on_key("up", move |_| {
        highlighted.update(|h| *h = h.saturating_sub(1))
    });
    let (title, placeholder) = match labels {
        Some(_) => ("Labels", "Label"),
        None => ("Move to", "Folder"),
    };
    let row = move |entry: ReadSignal<(FolderId, String)>, ix: ReadSignal<usize>| {
        let pick = pick.clone();
        let is_checked = move || checked.with(|c| c.contains(&entry.get().0));
        let row_hovered = signal(false);
        view()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(32.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .bg(move || {
                if highlighted.get() == ix.get() || row_hovered.get() {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            })
            .label(move || {
                let name = entry.get().1;
                SharedString::from(match labels {
                    Some(_) if is_checked() => format!("{name}, checked"),
                    Some(_) => format!("{name}, not checked"),
                    None => name,
                })
            })
            .on_mouse_enter(move |_: &MouseEvent| row_hovered.set(true))
            .on_mouse_leave(move |_: &MouseEvent| row_hovered.set(false))
            .on_press(move || pick(entry.get_untracked().0))
            .child(
                icon(
                    move || {
                        if labels.is_some() {
                            Icon::Tag
                        } else {
                            Icon::Folder(catmail_frontend::folders::FolderIcon::Folder)
                        }
                    },
                    16.0,
                )
                .text_color(move || theme().muted_foreground),
            )
            .child(
                text(move || entry.get().1)
                    .number_of_lines(1)
                    .flex_1()
                    .text_sm()
                    .text_color(move || theme().popover_foreground),
            )
            .child(when(is_checked, || {
                icon(|| Icon::Check, 16.0).text_color(move || theme().primary)
            }))
    };
    nsh_base::dialog()
        .animation(ModalAnimation::Fade)
        .on_cancel(close_picker)
        .child(
            view()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(hsla(0.0, 0.0, 0.0, 0.4))
                .child(
                    view()
                        .w(px(416.0))
                        .h(px(420.0))
                        .p(px(16.0))
                        .gap(px(12.0))
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(move || theme().border)
                        .bg(move || theme().popover)
                        .child(
                            text(title)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(move || theme().popover_foreground),
                        )
                        .child(
                            input()
                                .placeholder(placeholder.to_owned())
                                .auto_focus(true)
                                .on_change(move |text: &str| {
                                    query.set(text.to_owned());
                                    highlighted.set(0);
                                })
                                .on_submit(move |_: &str| pick_highlighted())
                                .h(px(36.0))
                                .px(px(10.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(move || theme().input)
                                .text_sm()
                                .text_color(move || theme().popover_foreground),
                        )
                        .child(
                            scroll().flex_1().child(
                                view()
                                    .child(each(move || shown.get(), |(id, _)| *id, row))
                                    .child(when(
                                        move || shown.with(Vec::is_empty),
                                        move || {
                                            text(commands::picker_empty_text(labels.is_some(), any))
                                                .text_sm()
                                                .p(px(16.0))
                                                .text_color(move || theme().muted_foreground)
                                        },
                                    )),
                            ),
                        ),
                ),
        )
}
