//! The settings dialog: themes, and adding accounts.

use super::*;

#[gpui_kit::test]
fn settings_change_the_theme(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    open_settings(cx, window);
    cx.update_window(window, |_, window, cx| {
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
    let mail = TestMail::without_account();
    let (window, mailbox) = open(cx, &mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| mailbox.read(cx).adding_account.is_some())
    });

    let port = mail.server().port.to_string();
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
    open_settings(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click(("settings-section", 1u64), cx);
        window.render_frame(cx);
        window.click("settings-add-account", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let port = mail.server().port.to_string();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for (id, text) in [
            ("email", "alice+work@example.com"),
            ("password", "pw"),
            ("username", "alice"),
            ("imap-host", "127.0.0.1"),
            ("imap-port", port.as_str()),
            ("smtp-host", "127.0.0.1"),
        ] {
            window.click(id, cx);
            window.input(text, cx);
        }
        window.within("imap-security").click(2usize, cx);
        window.click("sign-in", cx);
    })
    .unwrap();
    // GPUI's tasks run only when parked, so signing in is still under way here.
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // The account may be stored by now; leaving would only skip its first sync.
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
