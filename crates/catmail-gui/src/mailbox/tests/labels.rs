//! Gmail's labels: listed apart from the folders, shown on their messages, changed from
//! the "Labels" dialog.

use catmail_core::FolderId;

use super::*;

fn open_gmail(cx: &mut TestAppContext, mail: &TestMail) -> (AnyWindowHandle, Entity<Mailbox>) {
    let (window, mailbox) = open(cx, mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx) == ["Plan"])
    });
    (window, mailbox)
}

fn folder_id(mail: &TestMail, name: &str) -> FolderId {
    pollster::block_on(mail.client.find_folder(mail.account(), name))
        .unwrap()
        .unwrap()
        .id
}

fn select_folder(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    mailbox: &Entity<Mailbox>,
    folder: FolderId,
) {
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| {
            mailbox.select_place(Some(Place::Folder(folder)), window, cx)
        })
    })
    .unwrap();
}

/// The names of the labels the reader shows.
fn reader_labels(mailbox: &Entity<Mailbox>, cx: &App) -> Vec<String> {
    let reader = mailbox.read(cx).reader.read(cx);
    reader.labels().iter().map(ToString::to_string).collect()
}

/// Whether "Labels", open, checks `label`.
fn checked(mailbox: &Entity<Mailbox>, label: FolderId, cx: &App) -> Option<bool> {
    let list = mailbox.read(cx).labels_dialog.as_ref()?.list.upgrade()?;
    Some(list.read(cx).delegate().checked()?.contains(&label))
}

fn dialog_open(cx: &mut TestAppContext, window: AnyWindowHandle) -> bool {
    cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

/// Presses L in the list and picks `label` in the dialog it opens.
fn toggle_label(cx: &mut TestAppContext, window: AnyWindowHandle, label: FolderId) {
    cx.update_window(window, |_, window, cx| window.press("l", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("folder-picker", label.0 as u64), cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn labels_are_listed_apart_and_shown_on_their_messages(cx: &mut TestAppContext) {
    let mail = TestMail::start_gmail();
    let (window, mailbox) = open_gmail(cx, &mail);
    cx.read(|cx| {
        let account = &mailbox.read(cx).accounts.as_ref().unwrap()[0];
        let labels: Vec<_> = account
            .folders
            .labels
            .iter()
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(labels, ["Personal", "Work"]);
        // `[Gmail]` holds nothing, once its folders are among those with a role.
        assert!(account.folders.others.is_empty());
    });

    // The inbox is where it is listed: Work is what it is about.
    click_message(cx, window, &mailbox, "Plan");
    cx.read(|cx| assert_eq!(reader_labels(&mailbox, cx), ["Work"]));
    assert!(shown(cx, window, "labels"));

    // In Work, every row has Work: the rows leave it out, the reader doesn't.
    select_folder(cx, window, &mailbox, folder_id(&mail, "Work"));
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx) == ["Plan", "Trip"])
    });
    click_message(cx, window, &mailbox, "Trip");
    cx.read(|cx| {
        assert_eq!(reader_labels(&mailbox, cx), ["Personal", "Work"]);
        let list = mailbox.read(cx).messages.read(cx).delegate();
        let trip = find(&mailbox, "Trip", cx).unwrap();
        let work = Some(folder_id(&mail, "Work"));
        assert_eq!(list.label_names(&trip, work), ["Personal"]);
    });
}

#[gpui_kit::test]
fn labels_are_changed_from_the_labels_dialog(cx: &mut TestAppContext) {
    let mail = TestMail::start_gmail();
    let (window, mailbox) = open_gmail(cx, &mail);
    let (personal, work) = (folder_id(&mail, "Personal"), folder_id(&mail, "Work"));
    click_message(cx, window, &mailbox, "Plan");

    // Each pick is made at once, and shows on the message as it is.
    toggle_label(cx, window, personal);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| reader_labels(&mailbox, cx) == ["Personal", "Work"])
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("folder-picker", work.0 as u64), cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| reader_labels(&mailbox, cx) == ["Personal"])
    });

    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    assert_eq!(mail.server_labels("Plan"), ["\\Inbox", "Personal"]);
}

#[gpui_kit::test]
fn taking_off_the_label_listed_opens_the_next_message(cx: &mut TestAppContext) {
    let mail = TestMail::start_gmail();
    let (window, mailbox) = open_gmail(cx, &mail);
    let work = folder_id(&mail, "Work");
    select_folder(cx, window, &mailbox, work);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx) == ["Plan", "Trip"])
    });
    click_message(cx, window, &mailbox, "Plan");
    let trip = cx.read(|cx| id_of(&mailbox, "Trip", cx));

    toggle_label(cx, window, work);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            subjects(&mailbox, cx) == ["Trip"] && shown_in_reader(&mailbox, cx) == Some(trip)
        })
    });
    // Its labels were Plan's: it goes with it, rather than change Trip's.
    assert!(!dialog_open(cx, window));
    cx.read(|cx| assert!(mailbox.read(cx).labels_dialog.is_none()));
}

#[gpui_kit::test]
fn a_refused_label_change_is_unchecked_again(cx: &mut TestAppContext) {
    let mail = TestMail::start_gmail();
    let (window, mailbox) = open_gmail(cx, &mail);
    let (personal, work) = (folder_id(&mail, "Personal"), folder_id(&mail, "Work"));
    click_message(cx, window, &mailbox, "Plan");
    mail.refuse_changes("no such label");

    toggle_label(cx, window, personal);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| checked(&mailbox, personal, cx) == Some(true))
    });

    // Refused, and undone: the dialog, still open, shows it as the reader does.
    pollster::block_on(mail.client.replay_pending(mail.account())).unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| {
            checked(&mailbox, personal, cx) == Some(false)
                && checked(&mailbox, work, cx) == Some(true)
                && reader_labels(&mailbox, cx) == ["Work"]
        })
    });
    assert!(dialog_open(cx, window));
}

#[gpui_kit::test]
fn with_all_mail_hidden_labels_are_folders_and_the_user_is_told(cx: &mut TestAppContext) {
    let mail = TestMail::start_gmail();
    mail.hide_all_mail_and_sync();
    let (window, mailbox) = open(cx, &mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| subjects(&mailbox, cx) == ["Plan"])
    });
    cx.read(|cx| {
        let account = &mailbox.read(cx).accounts.as_ref().unwrap()[0];
        assert!(account.account.all_mail_hidden());
        assert!(account.folders.labels.is_empty());
        let others: Vec<_> = account
            .folders
            .others
            .iter()
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(others, ["Personal", "Work"]);
    });
    assert!(shown(
        cx,
        window,
        super::sidebar::account_note(mail.account())
    ));
    click_message(cx, window, &mailbox, "Plan");
    assert!(!shown(cx, window, "labels"), "no labels to edit");
}
