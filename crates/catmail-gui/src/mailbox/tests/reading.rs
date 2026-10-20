//! Reading, and changing what is read: the list, the reader, the commands on messages.

use super::*;

#[gpui_kit::test]
fn reads_a_message_through_the_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);

    // The inbox is selected on its own, newest first.
    wait_for_inbox(cx, &mailbox);
    cx.read(|cx| {
        assert_eq!(subjects(&mailbox, cx)[0], "十月产品更新：新功能一览");
        assert!(mailbox.read(cx).place.is_some());
    });

    // Opening it marks it read, here at once and on the server when the queue is
    // replayed.
    let report = click_message(cx, window, &mailbox, "月度报表");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "月度报表", cx).is_some_and(|m| m.flags.contains(Flags::SEEN)))
    });
    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert!(mail.server_flags("INBOX", 5).contains(&"\\Seen".to_owned()));

    // It has both a text and an HTML part: text first, HTML on request.
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("open-in-browser").is_some());
        window.click("format-html", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert!(mailbox.read(cx).reader.read(cx).prefers_html()));

    // The list keeps keyboard focus: the next message opens with the arrow key.
    let next = cx.read(|cx| {
        let list = mailbox.read(cx).messages.read(cx);
        let row = list.selected_index().unwrap().row;
        list.delegate().message(row + 1).unwrap().id
    });
    assert_ne!(next, report);
    cx.update_window(window, |_, window, cx| window.press("down", cx))
        .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| shown_in_reader(&mailbox, cx) == Some(next))
    });
}

#[gpui_kit::test]
fn archiving_opens_the_next_message(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let hello = click_message(cx, window, &mailbox, "你好，世界");
    let next = cx.read(|cx| {
        let list = listed(&mailbox, cx);
        let ix = list.iter().position(|m| m.id == hello).unwrap();
        list[ix + 1].id
    });

    // E, in the list.
    cx.update_window(window, |_, window, cx| window.press("e", cx))
        .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            find(&mailbox, "你好，世界", cx).is_none()
                && shown_in_reader(&mailbox, cx) == Some(next)
        })
    });
    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert_eq!(mail.server_uids("Archive").len(), 1);
    assert!(!mail.server_uids("INBOX").contains(&2));
}

#[gpui_kit::test]
fn deletes_for_good_without_a_trash_folder(cx: &mut TestAppContext) {
    let mail = TestMail::start_with_folders(&[("Archive", "\\Archive")]);
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "你好，世界");

    // Nowhere to move it to, so it is gone for good, once confirmed.
    cx.update_window(window, |_, window, cx| window.press("delete", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "你好，世界", cx).is_none())
    });
    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert!(!mail.server_uids("INBOX").contains(&2));
}

#[gpui_kit::test]
fn a_message_gone_from_the_server_leaves_the_reader(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    // The second fixture, UID 2.
    click_message(cx, window, &mailbox, "你好，世界");

    mail.expunge_and_sync(2);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx).len() == 5)
    });
    cx.read(|cx| assert_eq!(shown_in_reader(&mailbox, cx), None));
}

#[gpui_kit::test]
fn shows_what_a_sync_brings_without_losing_the_selection(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    // Opening a message would change the unread count this test watches.
    cx.update(|cx| Preferences::update(cx, |s| s.mark_read_on_open = false));
    wait_for_inbox(cx, &mailbox);
    // The body downloads on the engine's threads; a sync finishing first must not be
    // taken for the reader losing it.
    let hello = click_message(cx, window, &mailbox, "你好，世界");
    let unread = |cx: &App| {
        let mailbox = mailbox.read(cx);
        let Some(Place::Folder(folder)) = mailbox.place else {
            panic!("not a folder")
        };
        mailbox.accounts.as_deref().unwrap()[0]
            .folders
            .find(folder)
            .unwrap()
            .unread()
    };
    let unread_before = cx.read(unread);

    // Synced by the client, not by the window: only events tell it.
    mail.deliver_and_sync("Fresh news");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx).iter().any(|s| s == "Fresh news"))
    });
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| unread(cx) == unread_before + 1)
    });
    cx.read(|cx| {
        let list = mailbox.read(cx).messages.read(cx);
        let row = list.selected_index().unwrap().row;
        assert_eq!(list.delegate().message(row).unwrap().id, hello);
        assert_eq!(shown_in_reader(&mailbox, cx), Some(hello));
    });
}
