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
