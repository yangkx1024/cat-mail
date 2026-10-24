//! The window's commands and their keys. A toolbar button, a menu item and a key run
//! the same action, so the three can't disagree; the view that owns the command handles
//! it.

use gpui_kit::base::input::SelectAll;
use gpui_kit::{App, KeyBinding, NoAction};

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
/// A message's page, laid out by an engine: keys for its text.
pub const READER_PAGE: &str = "ReaderPage";
/// A composer window's.
pub const COMPOSER: &str = "Composer";
/// A button of a dialog's own footer.
pub const DIALOG_BUTTON: &str = "DialogButton";

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
        KeyBinding::new("secondary-a", SelectAll, Some(READER_PAGE)),
        KeyBinding::new("secondary-enter", Send, composer),
        // Before the fields see it: the text would take it as a new line, then send.
        KeyBinding::new("secondary-enter", Send, Some("Composer > Input")),
        KeyBinding::new("escape", CloseComposer, composer),
        KeyBinding::new("secondary-w", CloseComposer, composer),
        KeyBinding::new("secondary-shift-a", Attach, composer),
        // A dialog binds Enter to its Confirm, which would take it from a focused button
        // (Cancel, say): unbound here, the button sees it as a click.
        KeyBinding::new("enter", NoAction, Some(DIALOG_BUTTON)),
    ]);
}
