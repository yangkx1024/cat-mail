//! Starting a message from the mailbox: a new one, a reply, a forward, a draft opened
//! again. These run in the mailbox's view; the window they open is `crate::composer`,
//! whose view is on a thread of its own.

use catmail_frontend::composer::{ComposeFrom, new_message_account};
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::prelude::*;

use crate::engine::client;
use crate::mailbox::list;
use crate::mailbox::sidebar::{SelectedPlace, with_accounts};
use crate::shell::{self, on_main};
use crate::tasks::in_view;
use crate::toast::report;

/// Opens a composer window for `from`, from the mailbox. The draft is made (or read)
/// first; failures are reported in the mailbox.
pub fn open(from: ComposeFrom) {
    in_view(async move {
        if on_main(move || shell::begin_opening(from)).await != Some(true) {
            return;
        }
        let opened = from.start(&client()).await;
        if let Err(e) = &opened {
            report("Couldn't start the message", e);
        }
        let _ = on_main(move || shell::opened(from, opened.ok())).await;
    });
}

/// A new message, from the account of the place shown (or else the first).
pub fn new_message() {
    let place = global::<SelectedPlace>().with_untracked(|p| p.0);
    let account = with_accounts(|accounts| new_message_account(place, accounts));
    if let Some(account) = account {
        open(ComposeFrom::New(account));
    }
}

/// A reply to the message open, to its sender or to everyone it went to.
pub fn reply(all: bool) {
    if let Some(message) = untracked(list::selected_id) {
        open(ComposeFrom::Reply { message, all });
    }
}

/// A forward of the message open: quoting it, or with `as_attachment` attaching it
/// whole.
pub fn forward(as_attachment: bool) {
    if let Some(message) = untracked(list::selected_id) {
        open(ComposeFrom::Forward {
            message,
            as_attachment,
        });
    }
}

/// The ways to forward the message open, in a menu.
pub fn choose_forward() {
    let items = vec![
        MenuItem::new("Forward").on_select(|| forward(false)),
        MenuItem::new("Forward as attachment").on_select(|| forward(true)),
    ];
    // Nothing waits on it closing.
    drop(popup_menu(items));
}

/// The keys that start a message.
pub fn bind_keys() {
    on_key("mod+n", |_| new_message());
    on_key("mod+r", |_| reply(false));
    on_key("mod+shift+r", |_| reply(true));
    on_key("mod+shift+f", |_| forward(false));
}
