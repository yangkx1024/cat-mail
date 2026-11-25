//! Reading: opening a message from the list, and the commands on it.

use nsh_testing::DialogAnswer;

use super::{TestMail, WAIT, open_message, wait_until};

#[test]
fn opens_a_message_from_the_list_and_marks_it_read() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;
    app.wait_for("3 unread").unwrap();

    open_message(app, "Lunch on Friday");
    assert!(
        app.screen()
            .matches("id:reader >> text:To alice@example.com")
            .unwrap()
    );
    app.wait_for("2 unread").unwrap();
    wait_until("the server has it read", || {
        let mut state = mail.imap.state();
        state
            .inbox()
            .messages
            .iter()
            .find(|m| String::from_utf8_lossy(&m.raw).contains("Lunch on Friday"))
            .is_some_and(|m| m.flags.iter().any(|f| f == "\\Seen"))
    });
}

#[test]
fn a_message_still_downloading_shows_its_header_and_that_it_loads() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;
    let gate = mail.imap.state().hold_body_fetches();

    app.press("Quarterly report").unwrap();
    // As the list has it, while the body is on its way.
    app.wait_for_within("id:reader >> text:Loading the message…", WAIT)
        .unwrap();
    assert!(
        app.screen()
            .matches("id:reader >> text:Quarterly report")
            .unwrap()
    );
    gate.add_permits(100);
    app.wait_for_gone("Loading the message…", WAIT).unwrap();
    assert!(
        app.screen()
            .matches("id:reader >> text:Quarterly report")
            .unwrap()
    );
}

#[test]
fn archiving_opens_the_next_message() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;

    open_message(app, "Quarterly report");
    app.press("label:Archive").unwrap();
    app.wait_for_gone("Quarterly report", WAIT).unwrap();
    app.wait_for_within("id:reader >> text:Lunch on Friday", WAIT)
        .unwrap();
    wait_until("the server archived it", || {
        mail.on_server("Archive") == ["Quarterly report"]
    });
}

#[test]
fn deleting_from_trash_asks_first() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;

    open_message(app, "Weekend plans");
    app.press("label:Delete").unwrap();
    app.wait_for_gone("Weekend plans", WAIT).unwrap();
    wait_until("the server has it in Trash", || {
        mail.on_server("Trash") == ["Weekend plans"]
    });

    app.press("Trash").unwrap();
    app.wait_for_within("Weekend plans", WAIT).unwrap();
    open_message(app, "Weekend plans");
    app.on_dialog(|dialog| {
        assert!(
            dialog
                .title
                .as_deref()
                .unwrap_or_default()
                .contains("Delete"),
            "{dialog:?}"
        );
        DialogAnswer::Press("Cancel".into())
    });
    app.press("label:Delete").unwrap();
    assert!(app.shows("Weekend plans"));
    assert_eq!(mail.on_server("Trash"), ["Weekend plans"]);

    app.on_dialog(|dialog| {
        let button = dialog.buttons.last().expect("a button").text.clone();
        DialogAnswer::Press(button)
    });
    app.press("label:Delete").unwrap();
    app.wait_for_gone("Weekend plans", WAIT).unwrap();
    wait_until("the server deleted it", || {
        mail.on_server("Trash").is_empty()
    });
}

#[test]
fn the_move_to_picker_closes_from_its_button() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;

    open_message(app, "Lunch on Friday");
    app.press_key("mod+shift+m").unwrap();
    app.wait_for_within("text:Move to", WAIT).unwrap();
    app.press("Close").unwrap();
    app.wait_for_gone("text:Move to", WAIT).unwrap();
    // Nothing moved.
    assert!(app.screen().matches("Lunch on Friday").unwrap());
}
