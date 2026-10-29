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

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("open-in-browser").is_some());
    })
    .unwrap();

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
fn shows_html_as_a_page_and_text_without_one(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let painted = |cx: &mut TestAppContext| {
        frame(cx, window);
        cx.read(|cx| mailbox.read(cx).reader.read(cx).painted_strips(cx))
    };
    // Blitz lays the HTML out on its thread and paints the strip on screen, though the
    // message has a text part too.
    click_message(cx, window, &mailbox, "月度报表");
    wait_until(|| painted(cx).is_some_and(|strips| strips > 0));

    // A message with only text needs no page.
    click_message(cx, window, &mailbox, "你好，世界");
    assert_eq!(painted(cx), None);
}

#[gpui_kit::test]
fn shows_html_dark_in_a_dark_theme(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    mail.deliver_raw_and_sync(
        b"From: a@example.com\r\nTo: catmail@example.com\r\nSubject: Plain HTML\r\n\
          Date: Mon, 19 Oct 2026 10:00:00 +0800\r\nMIME-Version: 1.0\r\n\
          Content-Type: text/html; charset=utf-8\r\n\r\n<p>No colors of its own</p>\r\n",
    );
    mail.deliver_raw_and_sync(
        b"From: a@example.com\r\nTo: catmail@example.com\r\nSubject: White HTML\r\n\
          Date: Mon, 19 Oct 2026 11:00:00 +0800\r\nMIME-Version: 1.0\r\n\
          Content-Type: text/html; charset=utf-8\r\n\r\n\
          <body style=\"margin:0\"><table width=\"100%\" bgcolor=\"#ffffff\"><tr>\
          <td style=\"color:#333333;height:40px\">Colored for white</td></tr></table></body>\r\n",
    );
    let (window, mailbox) = open(cx, &mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            find(&mailbox, "Plain HTML", cx).is_some() && find(&mailbox, "White HTML", cx).is_some()
        })
    });
    let pixel = |cx: &mut TestAppContext| {
        frame(cx, window);
        cx.read(|cx| mailbox.read(cx).reader.read(cx).page_pixel(4, 4, cx))
    };

    // A message with no colors of its own takes the dark theme's background.
    set_appearance(cx, crate::settings::Appearance::Dark);
    click_message(cx, window, &mailbox, "Plain HTML");
    let background = cx.read(|cx| crate::theme::colors::rgb(cx.theme().background));
    let dark = [background.b, background.g, background.r, 255];
    assert_ne!(dark, [255; 4]);
    wait_until(|| pixel(cx) == Some(dark));

    // One colored for a white page has its white moved to the theme's background (to
    // within rounding: it goes through another color space).
    click_message(cx, window, &mailbox, "White HTML");
    let near = |pixel: [u8; 4]| pixel.iter().zip(dark).all(|(&a, b)| a.abs_diff(b) <= 1);
    wait_until(|| pixel(cx).is_some_and(near));

    // The page follows the theme when it changes.
    set_appearance(cx, crate::settings::Appearance::Light);
    wait_until(|| pixel(cx) == Some([255; 4]));
}

#[gpui_kit::test]
fn shows_remote_images_as_they_come_when_asked(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let images = mail.serve_images();
    let (red_png, slow_png) = (images.url("/red.png"), images.url("/slow.png"));
    mail.deliver_raw_and_sync(
        format!(
            "From: a@example.com\r\nTo: catmail@example.com\r\nSubject: Remote image\r\n\
             Date: Mon, 19 Oct 2026 10:00:00 +0800\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n\
             <img src=\"{red_png}\" width=\"40\" height=\"40\" style=\"display:block\">\
             <img src=\"{slow_png}\" width=\"40\" height=\"40\" style=\"display:block\">\r\n"
        )
        .as_bytes(),
    );
    let (window, mailbox) = open(cx, &mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| find(&mailbox, "Remote image", cx).is_some())
    });
    // Inside each image, past the page's margin: one under the other, at 8..48 and
    // 48..88 CSS px.
    let scale = cx
        .update_window(window, |_, window, _| window.scale_factor())
        .unwrap();
    let at = |css: f32| (css * scale) as u32;
    let pixels = |cx: &mut TestAppContext| {
        frame(cx, window);
        cx.read(|cx| {
            let reader = mailbox.read(cx).reader.read(cx);
            let pixel = |y| reader.page_pixel(at(20.), at(y), cx);
            (pixel(20.), pixel(70.))
        })
    };
    let has_button = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("show-remote-images").is_some()
        })
        .unwrap()
    };
    let (white, red) = (Some([255; 4]), Some([0, 0, 255, 255]));

    set_appearance(cx, crate::settings::Appearance::Light);
    // HTML only, so shown as HTML; the images' places stay blank.
    click_message(cx, window, &mailbox, "Remote image");
    wait_until(|| pixels(cx) == (white, white));
    assert_eq!(images.requests(), 0, "nothing loads on its own");

    // Asked for, each image shows as it comes; the button stays until the last has.
    click(cx, window, "show-remote-images");
    wait_until(|| pixels(cx) == (red, white));
    assert!(has_button(cx));
    images.release();
    wait_until(|| pixels(cx) == (red, red));
    wait_until(|| !has_button(cx));
    assert_eq!(images.requests(), 2);

    // Shown once, they show again from the cache, unasked and with no request.
    click_message(cx, window, &mailbox, "月度报表");
    click_message(cx, window, &mailbox, "Remote image");
    wait_until(|| pixels(cx) == (red, red));
    assert!(!has_button(cx));
    assert_eq!(images.requests(), 2);
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
fn an_earlier_removal_settling_leaves_a_later_one_waiting(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let hello = click_message(cx, window, &mailbox, "你好，世界");
    // Archived, and the rows read again to see whether it left; meanwhile, before that
    // read is back, the user deletes it.
    cx.update_window(window, |_, window, cx| {
        let list = mailbox.read(cx).messages.clone();
        list.update(cx, |list, cx| {
            let archived = list.delegate_mut().expect_removal(hello);
            message_list::removal_settled(list, archived, true, window, cx);
            list.delegate_mut().expect_removal(hello);
        });
    })
    .unwrap();
    // That read still has it, which says nothing of the deletion: still waited for.
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| !mailbox.read(cx).messages.read(cx).delegate().is_loading())
    });
    cx.read(|cx| {
        let list = mailbox.read(cx).messages.read(cx).delegate();
        assert!(list.waits_for_removal());
        assert!(find(&mailbox, "你好，世界", cx).is_some());
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
fn choosing_a_message_gives_up_revealing_one_still_to_come(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| !mailbox.read(cx).messages.read(cx).delegate().is_loading())
    });
    let chosen = cx.read(|cx| listed(&mailbox, cx)[0].id);
    // Stored, and not listed yet: the window hasn't run since.
    mail.deliver_and_sync("Lunch?");
    let inbox = pollster::block_on(mail.client.find_folder(mail.account(), "INBOX"))
        .unwrap()
        .unwrap();
    let stored = pollster::block_on(mail.client.list_messages(inbox.id, 100, None)).unwrap();
    let lunch = stored
        .iter()
        .find(|m| m.subject.as_deref() == Some("Lunch?"))
        .unwrap()
        .id;
    assert!(cx.read(|cx| find(&mailbox, "Lunch?", cx).is_none()));

    // A notification for it is clicked, then another message before the list has it.
    cx.update_window(window, |_, window, cx| {
        let list = mailbox.read(cx).messages.clone();
        list.update(cx, |list, cx| message_list::reveal(list, lunch, window, cx));
        window.render_frame(cx);
        window.click(("message", chosen.0 as u64), cx);
    })
    .unwrap();
    // The reader shows a message once its body has loaded.
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            find(&mailbox, "Lunch?", cx).is_some() && shown_in_reader(&mailbox, cx).is_some()
        })
    });
    cx.read(|cx| {
        let list = mailbox.read(cx).messages.read(cx);
        assert_eq!(list.delegate().selected_message().unwrap().id, chosen);
        assert_eq!(shown_in_reader(&mailbox, cx), Some(chosen));
    });
}

#[gpui_kit::test]
fn a_message_revealed_during_a_read_from_before_it_came_opens_after(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| !mailbox.read(cx).messages.read(cx).delegate().is_loading())
    });
    // A read starts, and the engine makes it while the window waits to hear back.
    cx.update_window(window, |_, window, cx| {
        let list = mailbox.read(cx).messages.clone();
        list.update(cx, |list, cx| message_list::refresh(list, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(100));
    // Then the mail arrives, and its notification is clicked before that read is in.
    mail.deliver_and_sync("Lunch?");
    let inbox = pollster::block_on(mail.client.find_folder(mail.account(), "INBOX"))
        .unwrap()
        .unwrap();
    let stored = pollster::block_on(mail.client.list_messages(inbox.id, 100, None)).unwrap();
    let lunch = stored
        .iter()
        .find(|m| m.subject.as_deref() == Some("Lunch?"))
        .unwrap()
        .id;
    cx.update_window(window, |_, window, cx| {
        let list = mailbox.read(cx).messages.clone();
        list.update(cx, |list, cx| message_list::reveal(list, lunch, window, cx));
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| shown_in_reader(&mailbox, cx) == Some(lunch))
    });
    cx.read(|cx| {
        let list = mailbox.read(cx).messages.read(cx);
        assert_eq!(list.delegate().selected_message().unwrap().id, lunch);
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
fn a_change_refused_before_the_window_opened_is_reported(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let client = &mail.client;
    pollster::block_on(async {
        let inbox = client
            .find_folder(mail.account(), "INBOX")
            .await
            .unwrap()
            .unwrap();
        let messages = client.list_messages(inbox.id, 1, None).await.unwrap();
        let ids = messages.iter().map(|m| m.id).collect();
        client
            .set_flags(ids, Flags::FLAGGED, Flags::empty())
            .await
            .unwrap();
    });
    mail.refuse_changes("[CANNOT] Permission denied");
    let report = pollster::block_on(client.replay_pending(mail.account())).unwrap();
    assert!(report.failed > 0);

    let (_, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    wait_until(|| {
        cx.run_until_parked();
        pollster::block_on(client.list_pending(None))
            .unwrap()
            .is_empty()
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
            .create_draft(
                mail.account(),
                None,
                catmail_core::DraftContent::default(),
                Vec::new(),
            )
            .await
            .unwrap();
        for n in 0..=catmail_core::EVENT_CAPACITY {
            let content = catmail_core::DraftContent {
                subject: format!("Draft {n}"),
                ..Default::default()
            };
            client
                .update_draft(draft.id, draft.identity, content)
                .await
                .unwrap();
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
