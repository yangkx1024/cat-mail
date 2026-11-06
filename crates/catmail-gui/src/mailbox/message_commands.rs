//! The commands on messages as icon buttons, the same wherever they show: over the
//! message open, by the list's heading for the messages checked, and on a row while
//! hovered.

use std::rc::Rc;

use catmail_core::Flags;
pub use catmail_frontend::commands::{Applies, MessageCommand};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::{Action, App, ElementId, Window};

use crate::actions::{self, *};
use crate::assets::Hero;

/// A command's icon, for messages that all have `flags`: a toggle shows what it does.
fn icon(command: MessageCommand, flags: Flags) -> Hero {
    match command {
        MessageCommand::Archive => Hero::ArchiveBox,
        MessageCommand::Delete => Hero::Trash,
        MessageCommand::Move => Hero::FolderArrowRight,
        MessageCommand::ToggleRead if flags.contains(Flags::SEEN) => Hero::Envelope,
        MessageCommand::ToggleRead => Hero::EnvelopeOpen,
        MessageCommand::ToggleFlag if flags.contains(Flags::FLAGGED) => Hero::FlagSolid,
        MessageCommand::ToggleFlag => Hero::Flag,
    }
}

/// A command's element name and its action, whose key the tooltip says, in its context.
fn naming(command: MessageCommand) -> (&'static str, &'static dyn Action, &'static str) {
    match command {
        MessageCommand::Archive => ("archive", &Archive, actions::MESSAGE_LIST),
        MessageCommand::Delete => ("delete", &Delete, actions::MESSAGE_LIST),
        MessageCommand::Move => ("move", &MoveToFolder, actions::MAILBOX),
        MessageCommand::ToggleRead => ("toggle-read", &ToggleRead, actions::MAILBOX),
        MessageCommand::ToggleFlag => ("toggle-flag", &ToggleFlag, actions::MAILBOX),
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
    let flags = applies.flags();
    let button = |command: MessageCommand| {
        let on_command = on_command.clone();
        let (name, action, context) = naming(command);
        let label = command.label(flags);
        let button = Button::new(id(name)).ghost().icon(icon(command, flags));
        let button = if keys {
            button.tooltip_with_action(label, action, Some(context))
        } else {
            button.tooltip(label)
        };
        button
            .disabled(!applies.enables(command))
            .on_click(move |_, window, cx| on_command(&command, window, cx))
    };
    Buttons {
        place: MessageCommand::PLACE.into_iter().map(button).collect(),
        marks: MessageCommand::MARKS.into_iter().map(button).collect(),
    }
}
