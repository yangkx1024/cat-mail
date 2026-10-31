//! The window's commands and their keys. A toolbar button, a menu item and a key run
//! the same action, so the three can't disagree; the view that owns the command handles
//! it.

use gpui_kit::base::input::SelectAll;
use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate, NoAction};

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
        EditLabels,
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
    let bindings = [
        key("secondary-q", Quit, None, cx),
        key("secondary-=", TextLarger, None, cx),
        key("secondary-+", TextLarger, None, cx),
        key("secondary--", TextSmaller, None, cx),
        key("secondary-0", TextReset, None, cx),
        key("secondary-n", NewMessage, mailbox, cx),
        key("secondary-r", Reply, mailbox, cx),
        key("secondary-shift-r", ReplyAll, mailbox, cx),
        key("secondary-shift-f", Forward, mailbox, cx),
        key("secondary-shift-u", ToggleRead, mailbox, cx),
        key("secondary-shift-l", ToggleFlag, mailbox, cx),
        key("secondary-shift-m", MoveToFolder, mailbox, cx),
        key("secondary-shift-n", Sync, mailbox, cx),
        key("f5", Sync, mailbox, cx),
        key("secondary-f", FocusSearch, mailbox, cx),
        key("secondary-,", OpenSettings, mailbox, cx),
        key("e", Archive, list, cx),
        // Gmail's key for it.
        key("l", EditLabels, list, cx),
        key("delete", Delete, list, cx),
        key("backspace", Delete, list, cx),
        key("secondary-a", SelectAll, Some(READER_PAGE), cx),
        key("secondary-enter", Send, composer, cx),
        // Before the fields see it: the text would take it as a new line, then send.
        key("secondary-enter", Send, Some("Composer > Input"), cx),
        key("escape", CloseComposer, composer, cx),
        key("secondary-w", CloseComposer, composer, cx),
        key("secondary-shift-a", Attach, composer, cx),
        // A dialog binds Enter to its Confirm, which would take it from a focused button
        // (Cancel, say): unbound here, the button sees it as a click.
        key("enter", NoAction, Some(DIALOG_BUTTON), cx),
    ];
    cx.bind_keys(bindings);
}

/// A binding for this OS: only `load` reads `secondary-`, as Cmd on macOS and Ctrl
/// elsewhere.
fn key(keys: &str, action: impl Action, context: Option<&str>, cx: &App) -> KeyBinding {
    let context = context.map(|c| KeyBindingContextPredicate::parse(c).unwrap().into());
    KeyBinding::load(keys, Box::new(action), context, false, None, cx).unwrap()
}
