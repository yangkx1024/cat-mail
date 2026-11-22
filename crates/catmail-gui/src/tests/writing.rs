//! Writing: the composer's windows, sending, and keeping what was written.

use super::{TestMail, WAIT, header, wait_until};

#[test]
fn sends_a_new_message() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    main.app.press("Compose").unwrap();
    let mut composer = desktop.window("New message");
    let app = &mut composer.app;
    app.wait_for_within("label:To", WAIT).unwrap();

    app.type_text("label:To", "bob@example.com").unwrap();
    app.type_text("label:Subject", "Hello Bob").unwrap();
    app.type_text("id:message", "See you at noon.").unwrap();
    app.press("Send").unwrap();
    composer.wait_closed(WAIT).unwrap();

    wait_until("the message is delivered", || !mail.delivered().is_empty());
    let sent = &mail.delivered()[0];
    assert_eq!(header(sent, "Subject").as_deref(), Some("Hello Bob"));
    assert_eq!(header(sent, "To").as_deref(), Some("<bob@example.com>"));
    assert!(sent.contains("See you at noon."), "{sent}");
    wait_until("a copy is in Sent", || {
        mail.on_server("Sent") == ["Hello Bob"]
    });
}

#[test]
fn replies_to_the_message_open() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    main.app.press("Lunch on Friday").unwrap();
    main.app
        .wait_for_within("id:reader >> text:Lunch on Friday", WAIT)
        .unwrap();
    main.app.press("label:Reply").unwrap();
    let mut composer = desktop.window("Re: Lunch on Friday");
    let app = &mut composer.app;
    app.wait_for_within("label:To", WAIT).unwrap();
    let to = app
        .screen()
        .input_text("label:To")
        .unwrap()
        .map(str::to_owned);
    assert!(
        to.as_deref()
            .unwrap_or_default()
            .contains("bob@example.com"),
        "{to:?}"
    );

    app.type_text("id:message", "Noon works.").unwrap();
    app.press("Send").unwrap();
    composer.wait_closed(WAIT).unwrap();

    wait_until("the reply is delivered", || !mail.delivered().is_empty());
    let sent = &mail.delivered()[0];
    assert_eq!(
        header(sent, "Subject").as_deref(),
        Some("Re: Lunch on Friday")
    );
    assert_eq!(
        header(sent, "In-Reply-To").as_deref(),
        Some("<0@example.com>")
    );
    assert!(sent.contains("Noon works."), "{sent}");
}

#[test]
fn a_closed_composer_keeps_what_was_written() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    main.app.press("Compose").unwrap();
    let mut composer = desktop.window("New message");
    composer.app.wait_for_within("label:Subject", WAIT).unwrap();
    composer.app.type_text("label:Subject", "Ideas").unwrap();
    composer
        .app
        .type_text("id:message", "A few thoughts.")
        .unwrap();

    desktop.close("New message");
    composer.wait_closed(WAIT).unwrap();
    main.app.press("Drafts").unwrap();
    main.app.wait_for_within("Ideas", WAIT).unwrap();
    assert!(mail.delivered().is_empty());

    // Opened again from the drafts, it holds what was written.
    main.app.press("Ideas").unwrap();
    main.app.press("Edit").unwrap();
    let mut again = desktop.window("Ideas");
    again.app.wait_for_within("A few thoughts.", WAIT).unwrap();
    assert_eq!(
        again.app.screen().input_text("label:Subject").unwrap(),
        Some("Ideas")
    );
}

#[test]
fn quitting_saves_what_every_composer_holds() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    let mut composers = Vec::new();
    for subject in ["First", "Second"] {
        main.app.press("Compose").unwrap();
        let mut composer = desktop.window("New message");
        composer.app.wait_for_within("label:Subject", WAIT).unwrap();
        composer.app.type_text("label:Subject", subject).unwrap();
        composers.push(composer);
    }

    desktop.quit();
    for composer in &composers {
        composer.wait_closed(WAIT).unwrap();
    }
    main.wait_closed(WAIT).unwrap();
    desktop.wait_quit(WAIT).unwrap();

    let mut saved: Vec<String> = mail
        .drafts_after_quitting()
        .into_iter()
        .map(|d| d.subject)
        .collect();
    saved.sort();
    assert_eq!(saved, ["First", "Second"]);
}
