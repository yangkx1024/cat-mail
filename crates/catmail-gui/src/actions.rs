//! The window's commands and their keys. A toolbar button, a menu item and a key run
//! the same action, so the three can't disagree; the view that owns the command handles
//! it.

use gpui_kit::{App, KeyBinding};

gpui_kit::actions!(
    catmail,
    [
        Quit,
        NewMessage,
        Reply,
        ReplyAll,
        Forward,
        Archive,
        Delete,
        ToggleRead,
        ToggleFlag,
        MoveToFolder,
        OpenInBrowser,
        Sync,
        FocusSearch,
        OpenSettings,
        TextLarger,
        TextSmaller,
        TextReset,
    ]
);

gpui_kit::actions!(composer, [Send, Attach, CloseComposer]);

/// The main window's key context.
pub const MAILBOX: &str = "Mailbox";
/// The message list's: single keys work here, where they can't be text being typed.
pub const MESSAGE_LIST: &str = "MessageList";
/// A composer window's.
pub const COMPOSER: &str = "Composer";

pub fn bind_keys(cx: &mut App) {
    let mailbox = Some(MAILBOX);
    let list = Some(MESSAGE_LIST);
    let composer = Some(COMPOSER);
    cx.bind_keys([
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-=", TextLarger, None),
        KeyBinding::new("secondary-+", TextLarger, None),
        KeyBinding::new("secondary--", TextSmaller, None),
        KeyBinding::new("secondary-0", TextReset, None),
        KeyBinding::new("secondary-n", NewMessage, mailbox),
        KeyBinding::new("secondary-r", Reply, mailbox),
        KeyBinding::new("secondary-shift-r", ReplyAll, mailbox),
        KeyBinding::new("secondary-shift-f", Forward, mailbox),
        KeyBinding::new("secondary-shift-u", ToggleRead, mailbox),
        KeyBinding::new("secondary-shift-l", ToggleFlag, mailbox),
        KeyBinding::new("secondary-shift-m", MoveToFolder, mailbox),
        KeyBinding::new("secondary-shift-n", Sync, mailbox),
        KeyBinding::new("f5", Sync, mailbox),
        KeyBinding::new("secondary-f", FocusSearch, mailbox),
        KeyBinding::new("secondary-,", OpenSettings, mailbox),
        KeyBinding::new("e", Archive, list),
        KeyBinding::new("delete", Delete, list),
        KeyBinding::new("backspace", Delete, list),
        KeyBinding::new("secondary-enter", Send, composer),
        // Before the fields see it: the text would take it as a new line, then send.
        KeyBinding::new("secondary-enter", Send, Some("Composer > Input")),
        KeyBinding::new("escape", CloseComposer, composer),
        KeyBinding::new("secondary-w", CloseComposer, composer),
        KeyBinding::new("secondary-shift-a", Attach, composer),
    ]);
}
