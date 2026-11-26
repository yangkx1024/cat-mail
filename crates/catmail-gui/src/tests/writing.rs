//! Writing: the composer's windows, sending, and keeping what was written.

use super::{TestMail, WAIT, compose, open_message, parse, wait_until};

#[test]
fn sends_a_new_message() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    let mut composer = compose(&desktop, &mut main);
    let app = &mut composer.app;
    app.type_text("label:To", "bob@example.com").unwrap();
    app.type_text("label:Subject", "Hello Bob").unwrap();
    app.type_text("label:Message", "See you at noon.").unwrap();
    app.press("Send").unwrap();
    composer.wait_closed();

    wait_until("the message is delivered", || !mail.delivered().is_empty());
    let sent = &mail.delivered()[0];
    let sent = parse(sent);
    assert_eq!(sent.subject(), Some("Hello Bob"));
    let to = sent.to().and_then(|to| to.first()?.address());
    assert_eq!(to, Some("bob@example.com"));
    let text = sent.body_text(0).unwrap_or_default();
    assert!(text.contains("See you at noon."), "{text}");
    wait_until("a copy is in Sent", || {
        mail.on_server("Sent") == ["Hello Bob"]
    });
}

#[test]
fn replies_to_the_message_open() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    open_message(&mut main.app, "Lunch on Friday");
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

    app.type_text("label:Message", "Noon works.").unwrap();
    app.press("Send").unwrap();
    composer.wait_closed();

    wait_until("the reply is delivered", || !mail.delivered().is_empty());
    let sent = &mail.delivered()[0];
    let sent = parse(sent);
    assert_eq!(sent.subject(), Some("Re: Lunch on Friday"));
    let in_reply_to = sent.in_reply_to().and_then(|ids| ids.get(0));
    assert_eq!(in_reply_to, Some("0@example.com"));
    let text = sent.body_text(0).unwrap_or_default();
    assert!(text.contains("Noon works."), "{text}");
}

#[test]
fn a_closed_composer_keeps_what_was_written() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    let mut composer = compose(&desktop, &mut main);
    composer.app.type_text("label:Subject", "Ideas").unwrap();
    composer
        .app
        .type_text("label:Message", "A few thoughts.")
        .unwrap();

    desktop.close("New message");
    composer.wait_closed();
    main.app.press("Drafts").unwrap();
    main.app.wait_for_within("Ideas", WAIT).unwrap();
    assert!(mail.delivered().is_empty());

    // Opened again from the drafts, it holds what was written.
    main.app.press("Ideas").unwrap();
    // The draft's pane slides in as the selection changes, not at once.
    main.app.wait_for_within("label:Edit", WAIT).unwrap();
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
        let mut composer = compose(&desktop, &mut main);
        composer.app.type_text("label:Subject", subject).unwrap();
        composers.push(composer);
    }

    desktop.quit();
    for composer in &composers {
        composer.wait_closed();
    }
    main.wait_closed();
    desktop.wait_quit();

    let mut saved: Vec<String> = mail
        .drafts_after_quitting()
        .into_iter()
        .map(|d| d.subject)
        .collect();
    saved.sort();
    assert_eq!(saved, ["First", "Second"]);
}
