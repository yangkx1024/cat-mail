//! The update's button in the sidebar, as the Flatpak portal's monitor would drive it.

use catmail_frontend::system::update::{Failure, Update};

use super::{TestMail, wait_until};
use crate::system::update;

#[test]
fn the_sidebar_installs_an_update_then_restarts_into_it() {
    let mail = TestMail::start();
    let (desktop, mut main) = mail.open();
    let app = &mut main.app;
    app.wait_for("Weekend plans").unwrap();
    assert!(!app.screen().shows("Install update"));

    update::said(Some(Update::Available));
    app.wait_for("Install update").unwrap();
    update::said(Some(Update::Installing(Some(40))));
    app.wait_for("Updating 40%").unwrap();

    // One that only the software center can install: said, and the button goes.
    update::said(Some(Update::Failed(Failure::Elsewhere)));
    app.wait_for(&Failure::Elsewhere.to_string()).unwrap();
    wait_until("the button is gone", || {
        let screen = app.screen();
        !screen.shows("Updating") && !screen.shows("Install update")
    });

    // Installed by the software center meanwhile: restarting quits, to start it.
    update::said(Some(Update::Installed));
    app.wait_for("Restart to update").unwrap();
    app.press("Restart to update").unwrap();
    desktop.wait_quit();
    update::said(None);
}
