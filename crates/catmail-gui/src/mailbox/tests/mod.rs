//! The mailbox window driven like a person would, against fake servers.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use catmail_core::{MessageId, MessageSummary};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, HeadlessAppContext, TestAppContext, Window, WindowBounds,
    WindowOptions, px, size,
};

use super::*;
use crate::test_support::{TestMail, wait_until};

fn window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            Default::default(),
            size(px(1280.), px(800.)),
        ))),
        focus: false,
        show: false,
        ..Default::default()
    }
}

/// Opens the mailbox in `cx`, which is a `TestAppContext` or a `HeadlessAppContext`,
/// set up as `main` sets up the app.
fn open_mailbox(cx: &mut App, mail: &TestMail) -> (AnyWindowHandle, Entity<Mailbox>) {
    crate::init_app(Preferences::load_nowhere(), cx);
    // Dialogs slide in, and hit testing follows them: on a slow machine a click meant
    // for a dialog's button lands where it was a frame ago. (Tests never read the
    // system's setting, so it is set here.)
    cx.set_reduce_motion(true);
    let client = mail.client.clone();
    gpui_kit::open_window(window_options(), cx, |window, cx| {
        cx.new(|cx| {
            let mut mailbox = Mailbox::new(client, window, cx);
            // Nothing waits for an interval: the test clock moves only when told to.
            mailbox.refresh_interval = Duration::ZERO;
            mailbox
        })
    })
    .unwrap()
}

fn open(cx: &mut TestAppContext, mail: &TestMail) -> (AnyWindowHandle, Entity<Mailbox>) {
    cx.executor().allow_parking();
    let opened = cx.update(|cx| open_mailbox(cx, mail));
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| opened.1.read(cx).accounts.is_some())
    });
    opened
}

/// The messages listed, top to bottom.
fn listed(mailbox: &Entity<Mailbox>, cx: &App) -> Vec<MessageSummary> {
    let list = mailbox.read(cx).messages.read(cx).delegate();
    list.rows()
        .iter()
        .filter_map(Row::message)
        .cloned()
        .collect()
}

fn subjects(mailbox: &Entity<Mailbox>, cx: &App) -> Vec<String> {
    listed(mailbox, cx)
        .into_iter()
        .filter_map(|m| m.subject)
        .collect()
}

fn find(mailbox: &Entity<Mailbox>, subject: &str, cx: &App) -> Option<MessageSummary> {
    listed(mailbox, cx)
        .into_iter()
        .find(|m| m.subject.as_deref() == Some(subject))
}

fn id_of(mailbox: &Entity<Mailbox>, subject: &str, cx: &App) -> MessageId {
    find(mailbox, subject, cx).unwrap().id
}

fn shown_in_reader(mailbox: &Entity<Mailbox>, cx: &App) -> Option<MessageId> {
    mailbox.read(cx).reader.read(cx).open_message_id()
}

fn wait_for_inbox(cx: &mut TestAppContext, mailbox: &Entity<Mailbox>) {
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(mailbox, cx).len() == 6)
    });
}

fn click_message(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    mailbox: &Entity<Mailbox>,
    subject: &str,
) -> MessageId {
    let id = cx.read(|cx| id_of(mailbox, subject, cx));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("message", id.0 as u64), cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| shown_in_reader(mailbox, cx) == Some(id))
    });
    id
}

/// The window that isn't the mailbox's, once there is one.
fn composer_window(cx: &mut TestAppContext, mailbox: AnyWindowHandle) -> AnyWindowHandle {
    let mut found = None;
    wait_until(|| {
        cx.run_until_parked();
        found = cx.update(|cx| cx.windows().into_iter().find(|w| *w != mailbox));
        found.is_some()
    });
    found.unwrap()
}

/// Opens Settings with its key, as dispatched actions run once the update is over.
fn open_settings(cx: &mut TestAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("secondary-,", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn wait_for_one_window(cx: &mut TestAppContext) {
    wait_until(|| {
        cx.run_until_parked();
        cx.update(|cx| cx.windows().len() == 1)
    });
}

mod reading;
mod screenshots;
mod settings;
mod writing;
