//! Composer windows: replying, and what closing leaves behind.

use super::*;

use gpui_kit::{ExternalPaths, FileDropEvent, PlatformInput, point};

#[gpui_kit::test]
fn replies_from_a_composer_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "你好，世界");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("reply", cx);
    })
    .unwrap();

    // The reply starts in the body, above the quote.
    let composer = composer_window(cx, window);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.input("收到，谢谢！", cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    wait_until(|| mail.sent().len() == 1);
    let sent = &mail.sent()[0];
    assert!(
        sent.contains("Subject: =?utf-8?") || sent.contains("Subject: Re:"),
        "{sent}"
    );
    assert!(sent.contains("In-Reply-To:"), "{sent}");
    // The key that sends adds no line: "收到，谢谢！\n\nOn …" in base64.
    assert!(sent.contains("5pS25Yiw77yM6LCi6LCi77yBCgpPbi"), "{sent}");
    assert!(
        pollster::block_on(mail.client.list_drafts(mail.account()))
            .unwrap()
            .is_empty()
    );
}

#[gpui_kit::test]
fn a_message_goes_from_the_address_chosen(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    // The same server and login, for another address.
    pollster::block_on(mail.client.add_identity(
        account,
        catmail_core::NewIdentity {
            email: "alice@example.org".into(),
            display_name: Some("Alice".into()),
            username: "alice".into(),
            smtp: mail.smtp_server(),
        },
        "pw".into(),
    ))
    .unwrap();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click(cx, window, "new-message");
    let composer = composer_window(cx, window);

    // The second choice: the account's own address comes first.
    click(cx, composer, "composer-from");
    frame(cx, composer);
    cx.update_window(composer, |_, window, cx| {
        window.press("down", cx);
        window.render_frame(cx);
        window.press("enter", cx);
    })
    .unwrap();
    // Sending saves it first, with the choice.
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.click("composer-to", cx);
        window.input("bob@example.com", cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    wait_until(|| mail.sent().len() == 1);
    assert!(
        mail.sent()[0].contains("From: \"Alice\" <alice@example.org>")
            || mail.sent()[0].contains("From: Alice <alice@example.org>"),
        "{}",
        mail.sent()[0]
    );
}

#[gpui_kit::test]
fn an_open_message_follows_its_address_being_replaced(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click(cx, window, "new-message");
    let composer = composer_window(cx, window);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.click("composer-to", cx);
        window.input("bob@example.com", cx);
    })
    .unwrap();

    // Meanwhile, in Settings: another address, made the default, and the first one gone.
    let identities = || pollster::block_on(mail.client.list_identities(account)).unwrap();
    let first = identities()[0].id;
    let other = pollster::block_on(mail.client.add_identity(
        account,
        catmail_core::NewIdentity {
            email: "alice@example.org".into(),
            display_name: None,
            username: "alice".into(),
            smtp: mail.smtp_server(),
        },
        "pw".into(),
    ))
    .unwrap();
    pollster::block_on(mail.client.set_default_identity(other.id)).unwrap();
    pollster::block_on(mail.client.remove_identity(first)).unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            mailbox.read(cx).accounts.as_deref().unwrap_or_default()[0]
                .identities
                .iter()
                .map(|i| i.id)
                .eq([other.id])
        })
    });
    cx.run_until_parked();

    // It saves, and goes, from the address left.
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    wait_until(|| mail.sent().len() == 1);
    assert!(
        mail.sent()[0].contains("From: <alice@example.org>"),
        "{}",
        mail.sent()[0]
    );
}

#[gpui_kit::test]
fn an_untouched_new_message_leaves_no_draft(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("new-message", cx);
    })
    .unwrap();
    let composer = composer_window(cx, window);
    assert_eq!(
        pollster::block_on(mail.client.list_drafts(mail.account()))
            .unwrap()
            .len(),
        1
    );
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    wait_until(|| {
        cx.run_until_parked();
        pollster::block_on(mail.client.list_drafts(mail.account()))
            .unwrap()
            .is_empty()
    });
}

#[gpui_kit::test]
fn a_closed_composer_keeps_what_was_written(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("new-message", cx);
    })
    .unwrap();
    let composer = composer_window(cx, window);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.input("bob@example.com", cx);
        window.click("composer-subject", cx);
        window.input("Lunch", cx);
    })
    .unwrap();
    // An address that doesn't parse keeps the window open, so it isn't lost; also after
    // an autosave, which keeps the field's last good value.
    cx.update_window(composer, |_, window, cx| {
        window.click("composer-to", cx);
        window.input(", carol@", cx);
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.update_window(composer, |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("backspace", cx);
        window.press("escape", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    let drafts = pollster::block_on(mail.client.list_drafts(mail.account())).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].content.subject, "Lunch");
    assert_eq!(drafts[0].content.to[0].email, "bob@example.com");

    // The drafts are listed under Drafts.
    cx.update(|cx| {
        mailbox.update(cx, |_, cx| cx.notify());
    });
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| {
            let account = mail.account();
            mailbox.select_place(Some(Place::Drafts(account)), window, cx)
        })
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| mailbox.read(cx).messages.read(cx).delegate().rows().len() == 1)
    });
}

/// Opens a reply to "你好，世界" and returns its window.
fn reply_window(cx: &mut TestAppContext, mail: &TestMail) -> (AnyWindowHandle, AnyWindowHandle) {
    let (window, mailbox) = open(cx, mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "你好，世界");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("reply", cx);
    })
    .unwrap();
    (window, composer_window(cx, window))
}

#[gpui_kit::test]
fn an_untouched_reply_still_sends(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (_, composer) = reply_window(cx, &mail);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.click("send", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    wait_until(|| mail.sent().len() == 1);
}

#[gpui_kit::test]
fn an_attachment_dropped_just_before_sending_goes_with_it(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("minutes.txt");
    // Big enough that storing it takes longer than sending would.
    std::fs::write(&file, "minutes of the meeting\n".repeat(400_000)).unwrap();
    let (_, composer) = reply_window(cx, &mail);

    // Dropped and sent at once: the send waits for the attachment.
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        let position = point(px(300.), px(400.));
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![file].into()),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Submit { position }),
            cx,
        );
        window.press("secondary-enter", cx);
    })
    .unwrap();
    wait_for_one_window(cx);
    wait_until(|| mail.sent().len() == 1);
    assert!(mail.sent()[0].contains("minutes.txt"), "{}", mail.sent()[0]);
}

#[gpui_kit::test]
fn nothing_can_be_typed_while_the_message_goes(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("minutes.txt");
    // The send waits for it, long enough to type.
    std::fs::write(&file, "minutes of the meeting\n".repeat(400_000)).unwrap();
    let (_, composer) = reply_window(cx, &mail);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.input("Sent", cx);
        let position = point(px(300.), px(400.));
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![file].into()),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Submit { position }),
            cx,
        );
        window.press("secondary-enter", cx);
        window.render_frame(cx);
        // What the send took is all there is: this would be lost with the window.
        window.input(" and lost", cx);
    })
    .unwrap();
    let texts = cx.read(crate::composer::texts);
    assert!(texts[0].starts_with("Sent\n"), "{texts:?}");
    wait_for_one_window(cx);
    wait_until(|| mail.sent().len() == 1);
}

#[gpui_kit::test]
fn an_attachment_that_fails_stops_the_send(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    // A folder can't be attached.
    let dir = tempfile::tempdir().unwrap();
    let (_, composer) = reply_window(cx, &mail);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        let position = point(px(300.), px(400.));
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![dir.path().to_path_buf()].into()),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Submit { position }),
            cx,
        );
        window.press("secondary-enter", cx);
    })
    .unwrap();
    // The window stays, with the reason, and nothing goes out: the send is called off
    // when the reason is shown.
    let reason = Some("Not sent: check the attachments first.".to_owned());
    wait_until(|| {
        cx.run_until_parked();
        cx.read(crate::composer::errors) == [reason.clone()]
    });
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
    assert!(mail.sent().is_empty());
}

#[gpui_kit::test]
fn quitting_saves_what_the_composers_hold(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (_, composer) = reply_window(cx, &mail);
    cx.update_window(composer, |_, window, cx| {
        window.render_frame(cx);
        window.input("Not saved yet", cx);
    })
    .unwrap();
    // Before the autosave.
    cx.update(crate::composer::quit);
    wait_until(|| {
        cx.run_until_parked();
        pollster::block_on(mail.client.list_drafts(mail.account()))
            .unwrap()
            .first()
            .is_some_and(|d| d.content.text.starts_with("Not saved yet"))
    });
}

#[gpui_kit::test]
fn a_draft_opened_twice_at_once_gets_one_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let draft = pollster::block_on(
        mail.client
            .create_draft(mail.account(), catmail_core::DraftContent::default()),
    )
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        for _ in 0..2 {
            let client = mail.client.clone();
            crate::composer::open(client, ComposeFrom::Draft(draft.id), window, cx);
        }
    })
    .unwrap();
    // The second found the first on its way, and did nothing.
    composer_window(cx, window);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
}

#[gpui_kit::test]
fn a_reply_opened_from_the_drafts_before_its_window_is_up_gets_one_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let message = cx.read(|cx| id_of(&mailbox, "你好，世界", cx));
    let reply = ComposeFrom::Reply {
        message,
        all: false,
    };
    cx.update_window(window, |_, window, cx| {
        crate::composer::open(mail.client.clone(), reply, window, cx)
    })
    .unwrap();
    // The reply's draft is there before its window: the account is read after it, and
    // the window comes up only when GPUI runs again.
    let mut draft = None;
    wait_until(|| {
        cx.executor().tick();
        draft = pollster::block_on(mail.client.list_drafts(mail.account()))
            .unwrap()
            .first()
            .map(|d| d.id);
        draft.is_some()
    });
    cx.update_window(window, |_, window, cx| {
        let from = ComposeFrom::Draft(draft.unwrap());
        crate::composer::open(mail.client.clone(), from, window, cx)
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(crate::composer::opened)
    });
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
}
