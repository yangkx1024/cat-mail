//! The folders: made, renamed and deleted from the sidebar's menus.

use nsh_testing::DialogAnswer;

use super::{TestMail, WAIT, wait_until};

#[test]
fn folders_are_made_renamed_and_deleted_from_the_sidebar() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;

    app.right_click("text:alice@example.com >> nth=0").unwrap();
    app.choose_menu_item("New folder").unwrap();
    app.type_text("label:New folder", "Projects").unwrap();
    app.press("Create").unwrap();
    app.wait_for_gone("label:New folder", WAIT).unwrap();
    app.wait_for_within("Projects", WAIT).unwrap();
    assert!(mail.has_folder("Projects"));

    app.right_click("Projects").unwrap();
    app.choose_menu_item("Rename").unwrap();
    let rename = "label:Rename “Projects”";
    app.clear(rename).unwrap();
    app.type_text(rename, "Clients").unwrap();
    app.submit(rename).unwrap();
    app.wait_for_within("Clients", WAIT).unwrap();
    app.wait_for_gone("Projects", WAIT).unwrap();
    assert!(mail.has_folder("Clients") && !mail.has_folder("Projects"));

    app.on_dialog(|_| DialogAnswer::Press("Delete".into()));
    app.right_click("Clients").unwrap();
    app.choose_menu_item("Delete").unwrap();
    app.wait_for_gone("Clients", WAIT).unwrap();
    wait_until("Clients is deleted on the server", || {
        !mail.has_folder("Clients")
    });
}

#[test]
fn accounts_and_folders_fold_and_unfold() {
    let mail = TestMail::start();
    let (_desktop, mut main) = mail.open();
    let app = &mut main.app;
    let new_folder = |app: &mut nsh_testing::Headless, on: &str, label: &str, name: &str| {
        app.right_click(on).unwrap();
        app.choose_menu_item("New folder").unwrap();
        app.type_text(label, name).unwrap();
        app.press("Create").unwrap();
        app.wait_for_gone(label, WAIT).unwrap();
        app.wait_for_within(name, WAIT).unwrap();
    };
    new_folder(
        app,
        "text:alice@example.com >> nth=0",
        "label:New folder",
        "Projects",
    );
    new_folder(app, "Projects", "label:New folder in “Projects”", "2026");

    app.press("label:Collapse Projects").unwrap();
    app.wait_for_gone("2026", WAIT).unwrap();
    app.press("label:Expand Projects").unwrap();
    app.wait_for_within("2026", WAIT).unwrap();

    // The account folds its places away, subfolders and all, and brings them back as
    // they were.
    app.press("text:alice@example.com >> nth=0").unwrap();
    app.wait_for_gone("Projects", WAIT).unwrap();
    app.wait_for_gone("2026", WAIT).unwrap();
    app.press("text:alice@example.com >> nth=0").unwrap();
    app.wait_for_within("Projects", WAIT).unwrap();
    app.wait_for_within("2026", WAIT).unwrap();

    // A double press on the folder's row folds it as well as selecting it.
    app.press("Projects").unwrap();
    app.press("Projects").unwrap();
    app.wait_for_gone("2026", WAIT).unwrap();
    app.press("label:Expand Projects").unwrap();
    app.wait_for_within("2026", WAIT).unwrap();

    // The account's chevron folds it as its row does (the selected folder's name stays,
    // over the list).
    app.press("label:Collapse alice@example.com").unwrap();
    app.wait_for_within("label:Expand alice@example.com", WAIT)
        .unwrap();
    app.wait_for_gone("2026", WAIT).unwrap();
    app.press("label:Expand alice@example.com").unwrap();
    app.wait_for_within("2026", WAIT).unwrap();
}
