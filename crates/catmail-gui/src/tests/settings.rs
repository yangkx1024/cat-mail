//! The settings, the sign-in page, and running on without a window.

use catmail_frontend::settings::{Appearance, DEFAULT_TEXT_SIZE};

use super::{TestMail, WAIT, wait_until};
use crate::shell;

#[test]
fn settings_change_the_appearance() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;
    app.press_key("mod+,").unwrap();
    app.press("Appearance").unwrap();
    app.press("label:Dark").unwrap();
    wait_until("the settings are dark", || {
        shell::settings().appearance == Appearance::Dark
    });
    app.press("label:Light").unwrap();
    wait_until("the settings are light", || {
        shell::settings().appearance == Appearance::Light
    });
}

#[test]
fn the_text_size_keys_step_it_and_put_it_back() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    main.app.press_key("mod+=").unwrap();
    wait_until("the text is larger", || {
        shell::settings().text_size > DEFAULT_TEXT_SIZE
    });
    main.app.press_key("mod+0").unwrap();
    wait_until("the text is as it was", || {
        shell::settings().text_size == DEFAULT_TEXT_SIZE
    });
}

#[test]
fn runs_on_without_a_window_until_told_not_to() {
    let mail = TestMail::start();
    let (desktop, main) = mail.open();
    desktop.close(shell::APP_NAME);
    main.wait_closed(WAIT).unwrap();
    // Running on: the tray, say, opens the window again.
    desktop.on_main(|| shell::show_at(None, None));
    let mut main = desktop.window(shell::APP_NAME);
    main.app.wait_for_within("Weekend plans", WAIT).unwrap();

    main.app.press_key("mod+,").unwrap();
    main.app
        .press("Keep running after the window is closed")
        .unwrap();
    wait_until("keeping running is off", || !shell::settings().keep_running);
    desktop.close(shell::APP_NAME);
    desktop.wait_quit(WAIT).unwrap();
}

#[test]
fn signs_in_from_the_window() {
    let mail = TestMail::without_account();
    let (_desktop, mut main) = mail.open_window();
    let app = &mut main.app;
    app.wait_for_within("Add an account", WAIT).unwrap();
    app.type_text("label:Email", "alice@example.com").unwrap();
    app.type_text("label:Password", "pw").unwrap();
    app.type_text("label:Username", "alice").unwrap();
    for (n, port) in [mail.imap.port(), mail.smtp.port()].into_iter().enumerate() {
        app.type_text(&format!("label:Server >> nth={n}"), "127.0.0.1")
            .unwrap();
        app.type_text(&format!("label:Port >> nth={n}"), &port.to_string())
            .unwrap();
        app.press(&format!("label:None >> nth={n}")).unwrap();
    }
    app.press("Sign in").unwrap();
    // Background sync brings the account's messages.
    app.wait_for_within("Weekend plans", WAIT).unwrap();
    assert!(app.shows("alice@example.com"));
}
