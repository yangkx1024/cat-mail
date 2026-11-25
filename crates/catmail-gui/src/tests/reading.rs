//! Reading: opening a message from the list, and the commands on it.

use nsh_testing::DialogAnswer;

use catmail_frontend::mailbox::Destination;

use super::{TestMail, WAIT, older_subject, open_message, wait_until};
use crate::shell;

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
fn escape_pressed_twice_still_lays_the_list_out_alone() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;
    app.wait_for("3 unread").unwrap();
    let alone = app.screen().layout("text:Quarterly report").unwrap();

    open_message(app, "Lunch on Friday");
    assert_ne!(app.screen().layout("text:Quarterly report").unwrap(), alone);
    // The second selects nothing again while the pane slides out.
    app.press_keys(&["escape", "escape"]).unwrap();
    app.wait(std::time::Duration::from_secs(1)).unwrap();
    assert_eq!(app.screen().layout("text:Quarterly report").unwrap(), alone);
}

#[test]
fn a_notification_opening_a_message_far_down_the_list_scrolls_to_it() {
    let mail = TestMail::start_with_older(60);
    let (desktop, mut main) = mail.open();
    let oldest = older_subject(0);
    let (account, message) = mail.inbox_message(&oldest);
    let in_list = format!("id:list >> text:{oldest}");
    // Too far down for its row to be made.
    assert!(!main.app.screen().matches(&in_list).unwrap());

    desktop.on_main(move || {
        let to = Destination::Inbox {
            account,
            message: Some(message),
        };
        shell::show_at(Some(to), None);
    });
    let app = &mut main.app;
    app.wait_for_within(&format!("id:reader >> text:{oldest}"), WAIT)
        .unwrap();
    app.wait(std::time::Duration::from_secs(1)).unwrap();
    let row = app.screen().layout(&in_list).unwrap();
    assert!(
        row.as_ref().is_some_and(|row| !row.hidden && !row.clipped),
        "{row:?}"
    );
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
