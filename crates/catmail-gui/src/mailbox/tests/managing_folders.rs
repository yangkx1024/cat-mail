//! Folders made, renamed and deleted from the sidebar: made on the server at once, the
//! sidebar following the engine's events.

use catmail_core::{Folder, FolderId};
use catmail_frontend::mailbox::FolderMenuEntry as MenuEntry;

use super::*;
use crate::mailbox::folder_commands;

/// The account's folders the sidebar shows.
fn sidebar_folders<'a>(mailbox: &Entity<Mailbox>, cx: &'a App) -> Vec<&'a Folder> {
    let account = &mailbox.read(cx).accounts.as_ref().unwrap()[0];
    account
        .folders
        .nodes()
        .filter_map(|node| node.folder.as_ref())
        .collect()
}

fn folder_named(mailbox: &Entity<Mailbox>, name: &str, cx: &App) -> Option<FolderId> {
    sidebar_folders(mailbox, cx)
        .into_iter()
        .find(|f| f.name == name)
        .map(|f| f.id)
}

/// Types `name` over what the open dialog's field holds (it opens selected), and
/// presses Enter.
fn enter_name(cx: &mut TestAppContext, window: AnyWindowHandle, name: &str) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.input(name, cx);
        window.press("enter", cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn folders_are_made_renamed_and_deleted_from_the_sidebar(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);

    // The address's menu makes a folder at the top.
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click(super::sidebar::account_heading(mail.account()), cx);
    })
    .unwrap();
    frame(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.within("popup-menu").click(0usize, cx)
    })
    .unwrap();
    assert!(dialog_open(cx, window));
    enter_name(cx, window, "Clients");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| folder_named(&mailbox, "Clients", cx).is_some())
    });
    assert!(!dialog_open(cx, window));
    assert!(mail.server_mailboxes().contains(&"Clients".to_owned()));

    // F2 renames the folder selected, not one the engine treats as special; a name
    // taken keeps the dialog open, saying why.
    let rename = |cx: &mut TestAppContext, folder| {
        select_folder(cx, window, &mailbox, folder);
        cx.update_window(window, |_, window, cx| window.press("f2", cx))
            .unwrap();
        dialog_open(cx, window)
    };
    assert!(!rename(cx, mail.folder_id("INBOX")));
    let clients = cx.read(|cx| folder_named(&mailbox, "Clients", cx).unwrap());
    assert!(rename(cx, clients));
    enter_name(cx, window, "Archive");
    wait_until(|| shown(cx, window, "folder-error"));
    assert!(dialog_open(cx, window));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(folder_commands::NAME, cx);
        window.press("secondary-a", cx);
    })
    .unwrap();
    enter_name(cx, window, "Customers");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| folder_named(&mailbox, "Customers", cx) == Some(clients))
    });
    assert!(!dialog_open(cx, window));
    cx.read(|cx| assert_eq!(mailbox.read(cx).place, Some(Place::Folder(clients))));

    // Delete asks first; the mailbox leaves the folder that went.
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| mailbox.delete_folder(clients, window, cx))
    })
    .unwrap();
    click(cx, window, crate::layout::CONFIRM);
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| folder_named(&mailbox, "Customers", cx).is_none())
    });
    cx.read(|cx| {
        let mut names: Vec<&str> = sidebar_folders(&mailbox, cx)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, ["Archive", "INBOX", "Sent", "Trash"]);
        assert_ne!(mailbox.read(cx).place, Some(Place::Folder(clients)));
    });
    assert!(!mail.server_mailboxes().contains(&"Customers".to_owned()));
}

#[gpui_kit::test]
fn new_makes_a_folder_in_what_was_right_clicked(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    let (inbox, archive) = (mail.folder_id("INBOX"), mail.folder_id("Archive"));
    let entries = |cx: &mut TestAppContext, folder| {
        cx.read(|cx| {
            let entry = mailbox.read(cx).account(mail.account()).unwrap();
            entry.folder_menu_entries(folder)
        })
    };

    // The address's makes one at the top; INBOX has none, a special folder no Rename.
    assert_eq!(entries(cx, None), [MenuEntry::New(None)]);
    assert_eq!(entries(cx, Some(inbox)), []);
    assert_eq!(entries(cx, Some(archive)), [MenuEntry::New(Some(archive))]);
    cx.update_window(window, |_, window, cx| {
        mailbox.update(cx, |mailbox, cx| {
            mailbox.new_folder(mail.account(), Some(archive), window, cx)
        })
    })
    .unwrap();
    assert!(dialog_open(cx, window));
    enter_name(cx, window, "2026");
    wait_until(|| {
        cx.run_until_parked();
        cx.read(|cx| folder_named(&mailbox, "Archive/2026", cx).is_some())
    });
    let made = cx.read(|cx| folder_named(&mailbox, "Archive/2026", cx).unwrap());
    assert!(mail.server_mailboxes().contains(&"Archive/2026".to_owned()));
    assert_eq!(
        entries(cx, Some(made)),
        [
            MenuEntry::New(Some(made)),
            MenuEntry::Rename(made),
            MenuEntry::Delete(made)
        ]
    );
}
