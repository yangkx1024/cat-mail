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
        let listed = listed(&mailbox, cx);
        let at = listed.iter().position(|m| m.id == report).unwrap();
        listed[at + 1].id
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
fn switches_the_html_between_engines(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "十月产品更新：新功能一览");
    let painted = |cx: &mut TestAppContext| {
        frame(cx, window);
        cx.read(|cx| mailbox.read(cx).reader.read(cx).painted_strips(cx))
    };
    // Blitz lays the newsletter out on its thread and paints the strip on screen.
    click(cx, window, "format-html");
    wait_until(|| painted(cx).is_some_and(|strips| strips > 0));

    // The text view's HTML needs no page, and stays chosen for the next message.
    click(cx, window, "engine-kit");
    assert_eq!(painted(cx), None);
    click_message(cx, window, &mailbox, "月度报表");
    assert_eq!(painted(cx), None);

    // Back to Blitz; Text drops the page.
    click(cx, window, "engine-blitz");
    wait_until(|| painted(cx).is_some_and(|strips| strips > 0));
    click(cx, window, "format-text");
    assert_eq!(painted(cx), None);
}

#[gpui_kit::test]
fn shows_html_dark_when_the_message_allows_it(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    mail.deliver_raw_and_sync(
        b"From: a@example.com\r\nTo: catmail@example.com\r\nSubject: Plain HTML\r\n\
          Date: Mon, 19 Oct 2026 10:00:00 +0800\r\nMIME-Version: 1.0\r\n\
          Content-Type: text/html; charset=utf-8\r\n\r\n<p>No colors of its own</p>\r\n",
    );
    let (window, mailbox) = open(cx, &mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "Plain HTML", cx).is_some())
    });
    let pixel = |cx: &mut TestAppContext| {
        frame(cx, window);
        cx.read(|cx| mailbox.read(cx).reader.read(cx).page_pixel(4, 4, cx))
    };

    // A message with no colors of its own takes the dark theme's background.
    set_appearance(cx, crate::settings::Appearance::Dark);
    click_message(cx, window, &mailbox, "Plain HTML");
    click(cx, window, "format-html");
    let background = cx.read(|cx| crate::theme::colors::rgb(cx.theme().background));
    let dark = [background.b, background.g, background.r, 255];
    assert_ne!(dark, [255; 4]);
    wait_until(|| pixel(cx) == Some(dark));

    // The page follows the theme when it changes.
    set_appearance(cx, crate::settings::Appearance::Light);
    wait_until(|| pixel(cx) == Some([255; 4]));
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
        window.click(crate::layout::CONFIRM, cx);
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
fn deleting_from_trash_asks_first_whichever_trash_it_is(cx: &mut TestAppContext) {
    // Two Trash folders: the engine moves to the first stored, "Trash", while the
    // sidebar lists "Old/Bin" first. Deleting from either is for good.
    let mail = TestMail::start_with_folders(&[("Trash", "\\Trash"), ("Old/Bin", "\\Trash")]);
    mail.deliver("Trash", "Binned");
    pollster::block_on(mail.client.sync_account(mail.account())).unwrap();
    let trash = pollster::block_on(mail.client.find_folder(mail.account(), "Trash"))
        .unwrap()
        .unwrap()
        .id;
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| {
            mailbox.select_place(Some(Place::Folder(trash)), window, cx)
        })
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx) == ["Binned"])
    });
    click_message(cx, window, &mailbox, "Binned");

    cx.update_window(window, |_, window, cx| window.press("delete", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(crate::layout::CONFIRM).is_some(),
            "no confirmation"
        );
    })
    .unwrap();
    cx.read(|cx| assert!(find(&mailbox, "Binned", cx).is_some()));
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
fn a_removal_still_waiting_doesnt_move_on_from_another_message(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    // A move of this one, not done yet when the user goes on to another.
    let report = click_message(cx, window, &mailbox, "月度报表");
    cx.update(|cx| {
        let list = mailbox.read(cx).messages.clone();
        list.update(cx, |list, _| list.delegate_mut().expect_removal(report));
    });
    // The second fixture, UID 2.
    click_message(cx, window, &mailbox, "你好，世界");

    // Gone from the server: the reader clears, as for any message, rather than opening
    // the next one as if the user had moved this one.
    mail.expunge_and_sync(2);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx).len() == 5)
    });
    cx.read(|cx| {
        assert_eq!(mailbox.read(cx).messages.read(cx).selected_index(), None);
        assert_eq!(shown_in_reader(&mailbox, cx), None);
    });
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
        assert_eq!(list.delegate().selected_message().unwrap().id, hello);
        assert_eq!(shown_in_reader(&mailbox, cx), Some(hello));
    });
}

#[gpui_kit::test]
fn opening_a_folder_syncs_it(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    // Synced as the test began: every open counts as late enough.
    cx.update(|cx| mailbox.update(cx, |m, _| m.sync_on_open_after = Duration::ZERO));
    let archive = pollster::block_on(mail.client.find_folder(mail.account(), "Archive"))
        .unwrap()
        .unwrap()
        .id;
    // Filed by another client; only INBOX is watched between full syncs.
    mail.deliver("Archive", "Filed elsewhere");

    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| {
            mailbox.select_place(Some(Place::Folder(archive)), window, cx)
        })
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx) == ["Filed elsewhere"])
    });
}

#[gpui_kit::test]
fn a_change_the_server_refuses_is_undone_and_reported(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (_, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let id = cx.read(|cx| id_of(&mailbox, "Quarterly report", cx));
    let flagged = |cx: &mut TestAppContext| {
        cx.read(|cx| find(&mailbox, "Quarterly report", cx).unwrap().flags)
            .contains(Flags::FLAGGED)
    };
    pollster::block_on(
        mail.client
            .set_flags(vec![id], Flags::FLAGGED, Flags::empty()),
    )
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        flagged(cx)
    });

    mail.refuse_changes("[CANNOT] Permission denied");
    let report = pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert_eq!(report.failed, 1);
    // Told once, and so forgotten: nothing is left to report again.
    wait_until(|| {
        cx.run_until_parked();
        pollster::block_on(mail.client.list_pending(None))
            .unwrap()
            .is_empty()
    });
    wait_until(|| {
        cx.run_until_parked();
        !flagged(cx)
    });
}

#[gpui_kit::test]
fn events_missed_meanwhile_read_everything_again(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (_, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);

    // While the window isn't looking: a new message, then more events than its receiver
    // keeps, none of them about INBOX. The one saying INBOX changed is lost.
    mail.deliver_and_sync("Missed");
    let client = &mail.client;
    pollster::block_on(async {
        let draft = client
            .create_draft(mail.account(), catmail_core::DraftContent::default())
            .await
            .unwrap();
        for n in 0..=catmail_core::EVENT_CAPACITY {
            let content = catmail_core::DraftContent {
                subject: format!("Draft {n}"),
                ..Default::default()
            };
            client.update_draft(draft.id, content).await.unwrap();
        }
    });
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "Missed", cx).is_some())
    });
}

#[gpui_kit::test]
fn reading_again_what_hasnt_changed_redraws_nothing(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let list = cx.read(|cx| mailbox.read(cx).messages.clone());
    let redraws = std::rc::Rc::new(std::cell::Cell::new(0));
    let _observing = cx.update(|cx| {
        let (mine, lists) = (redraws.clone(), redraws.clone());
        [
            cx.observe(&mailbox, move |_, _| mine.set(mine.get() + 1)),
            cx.observe(&list, move |_, _| lists.set(lists.get() + 1)),
        ]
    });

    // As after missed events: the sidebar and the list are read again, and both start
    // right away.
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| mailbox.on_engine_event(None, window, cx))
    })
    .unwrap();
    cx.read(|cx| {
        assert!(mailbox.read(cx).reading_folders.is_running());
        assert!(list.read(cx).delegate().is_loading());
    });
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            !mailbox.read(cx).reading_folders.is_running() && !list.read(cx).delegate().is_loading()
        })
    });
    assert_eq!(redraws.get(), 0);
}
