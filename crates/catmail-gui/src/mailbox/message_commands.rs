//! The commands on messages as icon buttons, the same wherever they show: over the
//! message open, by the list's heading for the messages checked, and on a row while
//! hovered.

use std::rc::Rc;

use catmail_core::{AccountId, Flags, MessageSummary};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::{Action, App, ElementId, Window};

use super::folders::AccountFolders;
use crate::actions::{self, *};
use crate::assets::Hero;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageCommand {
    Archive,
    Delete,
    Move,
    ToggleRead,
    ToggleFlag,
}

/// The icon and name of the command that marks a message read or unread, by its flags.
fn read_toggle(flags: Flags) -> (Hero, &'static str) {
    if flags.contains(Flags::SEEN) {
        (Hero::Envelope, "Mark as unread")
    } else {
        (Hero::EnvelopeOpen, "Mark as read")
    }
}

/// The icon and name of the command that flags a message or removes its flag.
fn flag_toggle(flags: Flags) -> (Hero, &'static str) {
    if flags.contains(Flags::FLAGGED) {
        (Hero::FlagSolid, "Remove flag")
    } else {
        (Hero::Flag, "Flag")
    }
}

/// The flags the messages all have: what a toggle takes off them all; one any of them
/// lacks it sets on all.
pub(super) fn common_flags<'a>(messages: impl IntoIterator<Item = &'a MessageSummary>) -> Flags {
    messages
        .into_iter()
        .map(|m| m.flags)
        .reduce(Flags::intersection)
        .unwrap_or_default()
}

/// The account the messages are all of: "Move to" lists its folders. None if there are
/// none, or they are of more than one.
pub(super) fn one_account<'a>(
    messages: impl IntoIterator<Item = &'a MessageSummary>,
) -> Option<AccountId> {
    let mut messages = messages.into_iter();
    let account = messages.next()?.account_id;
    messages.all(|m| m.account_id == account).then_some(account)
}

/// What the commands do to some messages, which their buttons show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applies {
    /// The flags they all have: the toggles take those off.
    flags: Flags,
    /// Archive changes one of them at least.
    archive: bool,
    /// They are of one account, whose folders "Move to" lists.
    move_to: bool,
}

impl Applies {
    /// `folders` gives an account's folders.
    pub fn of<'a, I>(messages: I, folders: impl Fn(AccountId) -> Option<&'a AccountFolders>) -> Self
    where
        I: IntoIterator<Item = &'a MessageSummary>,
        I::IntoIter: Clone,
    {
        let messages = messages.into_iter();
        let archive = messages
            .clone()
            .any(|m| folders(m.account_id).is_some_and(|f| f.can_archive(m)));
        let move_to = one_account(messages.clone()).is_some_and(|a| folders(a).is_some());
        Self {
            flags: common_flags(messages),
            archive,
            move_to,
        }
    }
}

/// The buttons, in their two groups: where the messages are, and their marks.
pub struct Buttons {
    pub place: Vec<Button>,
    pub marks: Vec<Button>,
}

/// The buttons of the commands, as `applies` says, running `on_command`. `id` names each
/// button's element by the command's name. With `keys`, the tooltips say the keys too:
/// not on a row, which the keys don't act on.
pub fn buttons(
    applies: Applies,
    id: impl Fn(&'static str) -> ElementId,
    keys: bool,
    on_command: impl Fn(&MessageCommand, &mut Window, &mut App) + 'static,
) -> Buttons {
    let on_command = Rc::new(on_command);
    let (read_icon, read_label) = read_toggle(applies.flags);
    let (flag_icon, flag_label) = flag_toggle(applies.flags);
    let (mailbox, list) = (actions::MAILBOX, actions::MESSAGE_LIST);
    let button = |command, name, icon, label, action: &dyn Action, context| {
        let on_command = on_command.clone();
        let button = Button::new(id(name)).ghost().icon(icon);
        let button = if keys {
            button.tooltip_with_action(label, action, Some(context))
        } else {
            button.tooltip(label)
        };
        button.on_click(move |_, window, cx| on_command(&command, window, cx))
    };
    Buttons {
        place: vec![
            button(
                MessageCommand::Archive,
                "archive",
                Hero::ArchiveBox,
                "Archive",
                &Archive,
                list,
            )
            .disabled(!applies.archive),
            button(
                MessageCommand::Delete,
                "delete",
                Hero::Trash,
                "Delete",
                &Delete,
                list,
            ),
            button(
                MessageCommand::Move,
                "move",
                Hero::FolderArrowRight,
                "Move to",
                &MoveToFolder,
                mailbox,
            )
            .disabled(!applies.move_to),
        ],
        marks: vec![
            button(
                MessageCommand::ToggleRead,
                "toggle-read",
                read_icon,
                read_label,
                &ToggleRead,
                mailbox,
            ),
            button(
                MessageCommand::ToggleFlag,
                "toggle-flag",
                flag_icon,
                flag_label,
                &ToggleFlag,
                mailbox,
            ),
        ],
    }
}
