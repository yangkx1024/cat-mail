//! Checking messages in the list, and the commands on all of them.

use super::*;
use catmail_core::DeleteMode;

/// The messages checked, top to bottom.
fn checked(mailbox: &Entity<Mailbox>, cx: &App) -> Vec<MessageId> {
    mailbox
        .read(cx)
        .checked(cx)
        .into_iter()
        .map(|m| m.id)
        .collect()
}

fn click_avatar(cx: &mut TestAppContext, window: AnyWindowHandle, id: MessageId) {
    click(cx, window, ("avatar", id.0 as u64));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn checks_messages_by_their_avatars_and_acts_on_them_all(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let (hello, report) = cx.read(|cx| {
        (
            id_of(&mailbox, "你好，世界", cx),
            id_of(&mailbox, "Quarterly report", cx),
        )
    });
    assert!(!shown(cx, window, "checked-archive"), "nothing checked");

    // Checked, not opened; the list's toolbar shows.
    click_avatar(cx, window, report);
    click_avatar(cx, window, hello);
    cx.read(|cx| {
        assert_eq!(checked(&mailbox, cx), [report, hello]);
        assert_eq!(shown_in_reader(&mailbox, cx), None);
    });
    assert!(shown(cx, window, "checked-archive"));

    // Clicked again, unchecked.
    click_avatar(cx, window, report);
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), [hello]));
    click_avatar(cx, window, report);

    // Flagged together, and still checked.
    click(cx, window, "checked-toggle-flag");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            [hello, report].iter().all(|&id| {
                listed(&mailbox, cx)
                    .iter()
                    .any(|m| m.id == id && m.flags.contains(Flags::FLAGGED))
            })
        })
    });
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), [report, hello]));

    // Archived together: gone, and nothing is checked.
    click(cx, window, "checked-archive");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            find(&mailbox, "你好，世界", cx).is_none()
                && find(&mailbox, "Quarterly report", cx).is_none()
        })
    });
    cx.read(|cx| assert!(checked(&mailbox, cx).is_empty()));
    assert!(!shown(cx, window, "checked-archive"));
    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert_eq!(mail.server_uids("Archive").len(), 2);
}

#[gpui_kit::test]
fn the_checkbox_checks_every_message_or_those_its_menu_says(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let all: Vec<_> = cx.read(|cx| listed(&mailbox, cx).iter().map(|m| m.id).collect());

    click(cx, window, "check-all");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), all));
    click(cx, window, "check-all");
    cx.run_until_parked();
    cx.read(|cx| assert!(checked(&mailbox, cx).is_empty()));

    // From the menu: "Unread".
    let unread: Vec<_> = cx.read(|cx| {
        listed(&mailbox, cx)
            .iter()
            .filter(|m| !m.flags.contains(Flags::SEEN))
            .map(|m| m.id)
            .collect()
    });
    assert!(!unread.is_empty() && unread.len() < all.len());
    click(cx, window, "check-menu");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(2usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), unread));

    // Marked read together: none of them is unread any more.
    click(cx, window, "checked-toggle-read");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            listed(&mailbox, cx)
                .iter()
                .all(|m| m.flags.contains(Flags::SEEN))
        })
    });

    // Another folder checks nothing.
    select_folder(cx, window, &mailbox, mail.folder_id("Archive"));
    cx.run_until_parked();
    cx.read(|cx| assert!(checked(&mailbox, cx).is_empty()));
}

#[gpui_kit::test]
fn keys_act_on_the_messages_checked_rather_than_the_one_open(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let open = click_message(cx, window, &mailbox, "你好，世界");
    let report = cx.read(|cx| id_of(&mailbox, "Quarterly report", cx));
    click_avatar(cx, window, report);
    cx.read(|cx| assert_eq!(shown_in_reader(&mailbox, cx), Some(open)));

    cx.update_window(window, |_, window, cx| window.press("delete", cx))
        .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "Quarterly report", cx).is_none())
    });
    cx.read(|cx| {
        assert!(find(&mailbox, "你好，世界", cx).is_some());
        assert_eq!(shown_in_reader(&mailbox, cx), Some(open));
    });
}

#[gpui_kit::test]
fn deleting_messages_checked_for_good_asks_once(cx: &mut TestAppContext) {
    let mail = TestMail::start_with_folders(&[("Archive", "\\Archive")]);
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let (hello, report) = cx.read(|cx| {
        (
            id_of(&mailbox, "你好，世界", cx),
            id_of(&mailbox, "Quarterly report", cx),
        )
    });
    click_avatar(cx, window, hello);
    click_avatar(cx, window, report);

    // No Trash: gone for good, once confirmed.
    click(cx, window, "checked-delete");
    assert!(dialog_open(cx, window));
    click(cx, window, crate::layout::CONFIRM);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            find(&mailbox, "你好，世界", cx).is_none()
                && find(&mailbox, "Quarterly report", cx).is_none()
        })
    });
    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert_eq!(mail.server_uids("INBOX").len(), 4);
}

#[gpui_kit::test]
fn a_message_checked_is_unchecked_once_it_is_gone(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let (hello, report) = cx.read(|cx| {
        (
            id_of(&mailbox, "你好，世界", cx),
            id_of(&mailbox, "Quarterly report", cx),
        )
    });
    click_avatar(cx, window, hello);
    click_avatar(cx, window, report);

    // Deleted elsewhere than the list.
    pollster::block_on(
        mail.client
            .delete_messages(vec![report], DeleteMode::ToTrash),
    )
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "Quarterly report", cx).is_none())
    });
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), [hello]));
}

#[gpui_kit::test]
fn search_results_gain_what_the_server_finds_in_bodies_not_downloaded(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    mail.deliver_raw_to_and_sync(
        "Archive",
        b"From: Bob <bob@example.com>\r\nSubject: Kept for later\r\n\r\nThe pelican numbers.\r\n",
    );
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let search_for = |text: &str, cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            mailbox.update(cx, |mailbox, cx| {
                mailbox.search_for(text.into(), window, cx)
            })
        })
        .unwrap();
        cx.run_until_parked();
    };
    // Not while the text is still changing: only once its results showed a while.
    search_for("peli", cx);
    cx.executor().advance_clock(Duration::from_millis(500));
    search_for("pelican", cx);
    assert!(mail.text_searches().is_empty());
    cx.executor().advance_clock(Duration::from_secs(1));
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "Kept for later", cx).is_some())
    });
    let searches = mail.text_searches();
    assert!(
        !searches.is_empty() && searches.iter().all(|s| s.contains("pelican")),
        "{searches:?}"
    );
}

#[gpui_kit::test]
fn deleting_from_search_results_trashes_some_and_deletes_the_binned_for_good(
    cx: &mut TestAppContext,
) {
    let mail = TestMail::start();
    mail.deliver("Trash", "Old report");
    pollster::block_on(mail.client.sync_account(mail.account())).unwrap();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| {
            mailbox.search_for("report".into(), window, cx)
        })
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            find(&mailbox, "Old report", cx).is_some()
                && find(&mailbox, "Quarterly report", cx).is_some()
        })
    });
    click(cx, window, "check-all");
    cx.run_until_parked();

    // The one in Trash asks first; once confirmed, it is gone and the other is in Trash.
    click(cx, window, "checked-delete");
    assert!(dialog_open(cx, window));
    click(cx, window, crate::layout::CONFIRM);
    wait_until(|| {
        cx.run_until_parked();
        mail.stored_subjects("Trash") == ["Quarterly report"]
    });
    assert!(
        !mail
            .stored_subjects("INBOX")
            .contains(&"Quarterly report".to_owned())
    );
}

#[gpui_kit::test]
fn removing_the_open_message_with_others_above_it_opens_the_next_one_left(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let ids: Vec<_> = cx.read(|cx| listed(&mailbox, cx).iter().map(|m| m.id).collect());
    // The second open, the first two checked: the third is next.
    let second = cx.read(|cx| find(&mailbox, "Quarterly report", cx).unwrap());
    assert_eq!(second.id, ids[1]);
    click_message(cx, window, &mailbox, "Quarterly report");
    click_avatar(cx, window, ids[0]);
    click_avatar(cx, window, ids[1]);

    click(cx, window, "checked-delete");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| listed(&mailbox, cx).len() == 4)
    });
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| shown_in_reader(&mailbox, cx) == Some(ids[2]))
    });
}

#[gpui_kit::test]
fn the_lists_keys_work_after_checking_with_the_checkbox_or_its_menu(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);

    // From the menu: "Read", then E archives them.
    let read = cx.read(|cx| {
        listed(&mailbox, cx)
            .iter()
            .filter(|m| m.flags.contains(Flags::SEEN))
            .count()
    });
    assert!(read > 0);
    click(cx, window, "check-menu");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(1usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.press("e", cx))
        .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| listed(&mailbox, cx).len() == 6 - read)
    });

    // The checkbox, then Delete.
    click(cx, window, "check-all");
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.press("delete", cx))
        .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| listed(&mailbox, cx).is_empty())
    });
}

#[gpui_kit::test]
fn x_checks_the_message_at_the_lists_cursor(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let report = click_message(cx, window, &mailbox, "Quarterly report");
    let press = |cx: &mut TestAppContext, key: &str| {
        cx.update_window(window, |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    };

    press(cx, "x");
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), [report]));
    // On to the next one, checked too; again, unchecked.
    press(cx, "down");
    let next = cx.read(|cx| mailbox.read(cx).selected_message(cx).unwrap().id);
    assert_ne!(next, report);
    press(cx, "x");
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), [report, next]));
    press(cx, "x");
    cx.read(|cx| assert_eq!(checked(&mailbox, cx), [report]));
}
