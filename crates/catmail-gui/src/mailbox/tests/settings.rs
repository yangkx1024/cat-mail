//! The settings dialog: themes, and adding accounts.

use super::*;

use catmail_core::Identity;
use gpui_kit::{ScrollDelta, point, px};

#[gpui_kit::test]
fn settings_change_the_theme(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    open_settings(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click(crate::settings::dialog::section_item("Appearance"), cx);
        window.render_frame(cx);
        window.click("appearance-Dark", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let theme = Theme::global(cx);
        assert_eq!(theme.mode, ThemeMode::Dark);
        assert_eq!(theme.theme_name().as_ref(), crate::theme::DEFAULT_DARK);
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("appearance-Light", cx);
        window.render_frame(cx);
        window.click("theme-Catppuccin Latte", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let theme = Theme::global(cx);
        assert_eq!(theme.mode, ThemeMode::Light);
        assert_eq!(theme.theme_name().as_ref(), "Catppuccin Latte");
        assert_eq!(Preferences::get(cx).light_theme, "Catppuccin Latte");
    });
}

#[gpui_kit::test]
fn signs_in_from_the_window(cx: &mut TestAppContext) {
    // The app's background sync syncs the account once it is added.
    let mail = TestMail::without_account_syncing_in_background();
    let (window, mailbox) = open(cx, &mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| mailbox.read(cx).adding_account.is_some())
    });

    let port = mail.server().port.to_string();
    let smtp_port = mail.smtp_server().port.to_string();
    let fill = |window: &mut Window, cx: &mut App, id: &'static str, text: &str| {
        window.click(id, cx);
        window.press("secondary-a", cx);
        window.input(text, cx);
    };
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        fill(window, cx, "email", "alice@example.com");
        fill(window, cx, "password", "wrong");
        fill(window, cx, "username", "alice");
        fill(window, cx, "imap-host", "127.0.0.1");
        fill(window, cx, "imap-port", &port);
        window.within("imap-security").click(2usize, cx); // None: the fake server is plain
        fill(window, cx, "smtp-host", "127.0.0.1");
        fill(window, cx, "smtp-port", &smtp_port);
        window.within("smtp-security").click(2usize, cx);
        window.click("sign-in", cx);
    })
    .unwrap();

    // The server refuses: the form says so and nothing is stored.
    let mut error = None;
    wait_until(|| {
        cx.run_until_parked();
        error = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("sign-in-error")?.label().map(str::to_owned)
            })
            .unwrap();
        error.is_some()
    });
    assert!(error.unwrap().contains("check the username and password"));
    assert!(
        pollster::block_on(mail.client.list_accounts())
            .unwrap()
            .is_empty()
    );

    // Enter in a field signs in, too.
    cx.update_window(window, |_, window, cx| {
        fill(window, cx, "password", "pw");
        window.press("enter", cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| mailbox.read(cx).adding_account.is_none() && subjects(&mailbox, cx).len() == 6)
    });
    let accounts = pollster::block_on(mail.client.list_accounts()).unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].imap.security, catmail_core::Security::Plain);
}

#[gpui_kit::test]
fn cancel_is_off_once_signing_in_has_started(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);

    // Settings › Accounts › Add account.
    open_accounts(cx, window);
    click(cx, window, "settings-add-account");
    cx.run_until_parked();
    let port = mail.server().port.to_string();
    let smtp_port = mail.smtp_server().port.to_string();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for (id, text) in [
            ("email", "alice+work@example.com"),
            ("password", "pw"),
            ("username", "alice"),
            ("imap-host", "127.0.0.1"),
            ("imap-port", port.as_str()),
            ("smtp-host", "127.0.0.1"),
            ("smtp-port", smtp_port.as_str()),
        ] {
            window.click(id, cx);
            window.input(text, cx);
        }
        window.within("imap-security").click(2usize, cx);
        window.within("smtp-security").click(2usize, cx);
        window.click("sign-in", cx);
    })
    .unwrap();
    // GPUI's tasks run only when parked, so signing in is still under way here.
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // The account may be stored by now; leaving wouldn't undo that.
        window.click("cancel", cx);
    })
    .unwrap();
    cx.read(|cx| assert!(mailbox.read(cx).adding_account.is_some()));
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| mailbox.read(cx).adding_account.is_none())
    });
    assert_eq!(
        pollster::block_on(mail.client.list_accounts())
            .unwrap()
            .len(),
        2
    );
}

#[gpui_kit::test]
fn adds_a_sending_address_and_makes_it_the_default(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let identities = || pollster::block_on(mail.client.list_identities(account)).unwrap();

    // Settings › Accounts › Add sending address; Enter in a field adds it.
    open_accounts(cx, window);
    let port = mail.smtp_server().port.to_string();
    cx.update_window(window, |_, window, cx| {
        window.click(("add-identity", account.0 as u64), cx);
        window.render_frame(cx);
        let fill = |window: &mut Window, cx: &mut App, id: &'static str, text: &str| {
            window.click(id, cx);
            window.input(text, cx);
        };
        fill(window, cx, "identity-email", "alice@example.org");
        fill(window, cx, "identity-password", "pw");
        fill(window, cx, "identity-username", "alice");
        // The server's fields are below the fold.
        window.scroll(
            "identity-username",
            ScrollDelta::Pixels(point(px(0.), px(-1000.))),
            cx,
        );
        window.render_frame(cx);
        window.within("identity-smtp-security").click(2usize, cx);
        fill(window, cx, "identity-smtp-host", "127.0.0.1");
        fill(window, cx, "identity-smtp-port", &port);
        window.press("enter", cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        identities().len() == 2
    });
    let (own, identity) = (identities()[0].clone(), identities()[1].clone());
    assert_eq!(identity.email, "alice@example.org");

    // Back in the list, under its account; made the default, the first can go.
    let make_default = ("default-identity", identity.id.0 as u64);
    wait_until(|| shown(cx, window, make_default));
    cx.update_window(window, |_, window, cx| window.click(make_default, cx))
        .unwrap();
    let remove = ("remove-identity", own.id.0 as u64);
    wait_until(|| shown(cx, window, remove) && identities()[0].id == identity.id);
    cx.update_window(window, |_, window, cx| window.click(remove, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(crate::layout::CONFIRM, cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        identities()
            == [Identity {
                is_default: true,
                ..identity.clone()
            }]
    });
}

#[gpui_kit::test]
fn tells_whether_the_server_saves_sent_mail(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let own = || pollster::block_on(mail.client.list_identities(account)).unwrap()[0].clone();
    assert_eq!(own().saves_sent, None);

    // Not Gmail: guessed not to, until switched on; and off again.
    open_accounts(cx, window);
    let switch = ("saves-sent", own().id.0 as u64);
    for saves in [true, false] {
        wait_until(|| shown(cx, window, switch));
        cx.update_window(window, |_, window, cx| window.click(switch, cx))
            .unwrap();
        // Stored, and read back by the window: the switch shows it.
        wait_until(|| {
            cx.run_until_parked();
            own().saves_sent == Some(saves)
                && cx.read(|cx| {
                    let entry = mailbox.read(cx).account(account).unwrap();
                    entry.identities[0].saves_sent == Some(saves)
                })
        });
    }
}

#[gpui_kit::test]
fn clears_the_image_cache(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let images = mail.serve_images();
    let url = images.url("/red.png");
    let cached = |mail: &TestMail| {
        pollster::block_on(mail.client.cached_remote_images(vec![url.clone()]))
            .unwrap()
            .len()
    };
    pollster::block_on(async {
        let urls = vec![url.clone()];
        let mut fetching = mail.client.fetch_remote_images(urls).await.unwrap();
        while fetching.next().await.is_some() {}
    });
    assert_eq!(cached(&mail), 1);
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);

    // Settings › Reading › Clear image cache, once the dialog knows there is one.
    open_settings(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click(crate::settings::dialog::section_item("Reading"), cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        click(cx, window, "clear-image-cache");
        cx.run_until_parked();
        cached(&mail) == 0
    });
    assert_eq!(
        pollster::block_on(mail.client.image_cache_size()).unwrap(),
        0
    );
    assert_eq!(images.requests(), 1);
}

#[gpui_kit::test]
fn settings_open_on_general_where_keeping_running_can_be_turned_off(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    assert!(cx.read(|cx| Preferences::get(cx).keep_running));
    open_settings(cx, window);
    cx.update_window(window, |_, window, cx| window.click("keep-running", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!cx.read(|cx| Preferences::get(cx).keep_running));
}

#[gpui_kit::test]
fn escape_and_cancel_close_only_the_sending_address_form(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    open_accounts(cx, window);
    let closes: [fn(&mut Window, &mut App); 2] = [
        |window, cx| window.press("escape", cx),
        |window, cx| window.click("identity-cancel", cx),
    ];
    for close in closes {
        cx.update_window(window, |_, window, cx| {
            window.click(("add-identity", account.0 as u64), cx);
            window.render_frame(cx);
            window.click("identity-email", cx);
            window.input("alice@example.org", cx);
            close(window, cx);
        })
        .unwrap();
        assert!(!shown(cx, window, "identity-email"));
        assert!(shown(cx, window, "settings-add-account"));
    }
    assert_eq!(
        pollster::block_on(mail.client.list_identities(account))
            .unwrap()
            .len(),
        1
    );
}

#[gpui_kit::test]
fn enter_on_cancel_of_a_confirmation_removes_nothing(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    open_accounts(cx, window);
    let remove = ("remove-account", account.0 as u64);
    let confirm_with = |cx: &mut TestAppContext, button: &'static str| {
        cx.update_window(window, |_, window, cx| window.click(remove, cx))
            .unwrap();
        for _ in 0..10 {
            let focused = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.find(button).focused() == Some(true)
                })
                .unwrap();
            if focused {
                cx.update_window(window, |_, window, cx| window.press("enter", cx))
                    .unwrap();
                return;
            }
            cx.update_window(window, |_, window, cx| window.press("tab", cx))
                .unwrap();
            cx.run_until_parked();
        }
        panic!("Tab never reached {button}");
    };
    let accounts = || {
        pollster::block_on(mail.client.list_accounts())
            .unwrap()
            .len()
    };

    confirm_with(cx, crate::layout::CANCEL);
    assert!(!shown(cx, window, crate::layout::CANCEL));
    assert_eq!(accounts(), 1);

    confirm_with(cx, crate::layout::CONFIRM);
    wait_until(|| {
        cx.run_until_parked();
        accounts() == 0
    });
}

#[gpui_kit::test]
fn the_sending_address_form_works_from_the_keyboard(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let account = mail.account();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    open_accounts(cx, window);
    let open_form = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.click(("add-identity", account.0 as u64), cx);
            window.render_frame(cx);
            window.input("alice@example.org", cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    // Tab from the email field to the button.
    let tab_to = |cx: &mut TestAppContext, id: &'static str| {
        for _ in 0..30 {
            let focused = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.find(id).focused() == Some(true)
                })
                .unwrap();
            if focused {
                return;
            }
            cx.update_window(window, |_, window, cx| window.press("tab", cx))
                .unwrap();
            cx.run_until_parked();
        }
        panic!("Tab never reached {id}");
    };
    let press = |cx: &mut TestAppContext, key: &'static str| {
        cx.update_window(window, |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    };

    // Enter on Cancel cancels: nothing is added, and no error from trying.
    open_form(cx);
    tab_to(cx, "identity-cancel");
    press(cx, "enter");
    assert!(!shown(cx, window, "identity-email"));
    assert!(!shown(cx, window, "identity-error"));

    // Space on Add adds: here it fails, there being no server.
    open_form(cx);
    tab_to(cx, "identity-add");
    press(cx, "space");
    wait_until(|| shown(cx, window, "identity-error"));

    // Closed, the form gives focus back to Settings: Escape closes it next.
    press(cx, "escape");
    assert!(!shown(cx, window, "identity-email"));
    assert!(shown(cx, window, "settings-add-account"));
    press(cx, "escape");
    assert!(!shown(cx, window, "settings-add-account"));
    assert_eq!(
        pollster::block_on(mail.client.list_identities(account))
            .unwrap()
            .len(),
        1
    );
}

/// Asserts the dialog on top sits in the vertical middle of the window.
fn assert_centered(window: &mut Window) {
    let viewport = window.viewport_size();
    let dialog = window.find("surface").bounds();
    let (above, below) = (dialog.top(), viewport.height - dialog.bottom());
    assert!(
        (above - below).abs() <= px(1.),
        "{above:?} above, {below:?} below"
    );
}

#[gpui_kit::test]
fn dialogs_open_in_the_middle_of_the_window(cx: &mut TestAppContext) {
    let mail = TestMail::start_with_folders(&[("Archive", "\\Archive")]);
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    open_settings(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert_centered(window);
        window.close_all_dialogs(cx);
    })
    .unwrap();
    // A confirmation: with no Trash, deleting is for good.
    click_message(cx, window, &mailbox, "你好，世界");
    cx.update_window(window, |_, window, cx| window.press("delete", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_centered(window);
    })
    .unwrap();
}

#[gpui_kit::test]
fn dialogs_settle_in_place_and_take_their_time_to_leave(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    cx.update(|cx| cx.set_reduce_motion(false));
    let top = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("surface")
                .map(|surface| surface.bounds().top())
        })
        .unwrap()
    };
    let step = |ms, cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_millis(ms));
        top(cx)
    };

    cx.update_window(window, |_, window, cx| {
        crate::layout::confirm_danger("Delete?", "For good.", "Delete", window, cx, |_, _| {});
    })
    .unwrap();
    let opening = top(cx).unwrap();
    let settled = step(500, cx).unwrap();
    // A few pixels above its place, not from the window's top.
    assert!(
        opening < settled && settled - opening <= px(8.),
        "opened at {opening:?}, settled at {settled:?}"
    );

    cx.update_window(window, |_, window, cx| window.close_dialog(cx))
        .unwrap();
    let leaving = step(90, cx).unwrap();
    assert!(
        leaving < settled && settled - leaving <= px(8.),
        "halfway out at {leaving:?}, from {settled:?}"
    );
    assert_eq!(step(500, cx), None);
}
