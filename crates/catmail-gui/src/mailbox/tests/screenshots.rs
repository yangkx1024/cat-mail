//! Offscreen renders for a person to look at.

use gpui_kit::test::TestEventExt as _;

use super::*;

/// Renders windows offscreen to PNG files, to look at the layout and the themes:
///
/// ```sh
/// CATMAIL_SCREENSHOTS=/tmp/shots cargo test -p catmail-gui -- --ignored screenshots
/// ```
///
/// Needs a GPU adapter, or Mesa's lavapipe.
#[test]
#[ignore = "writes images for a person to look at"]
fn screenshots() {
    let dir =
        PathBuf::from(std::env::var_os("CATMAIL_SCREENSHOTS").expect("set CATMAIL_SCREENSHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    let save = |cx: &mut HeadlessAppContext, window: AnyWindowHandle, name: &str| {
        // Frames in between: a page Blitz paints shows in the frame after its strips
        // arrive, and asks for them only when a frame measures it.
        for _ in 0..20 {
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let path = dir.join(format!("{name}.png"));
        cx.capture_screenshot(window).unwrap().save(&path).unwrap();
        println!("{}", path.display());
    };

    let empty = TestMail::without_account();
    let (mut cx, window, mailbox) = headless_window(&empty);
    wait_until(|| {
        cx.run_until_parked();
        cx.update(|cx| mailbox.read(cx).adding_account.is_some())
    });
    save(&mut cx, window, "sign-in");
    drop(cx);

    // Gmail: its labels apart from the folders, on the message, and in their dialog.
    let gmail = TestMail::start_gmail();
    let (mut cx, window, mailbox) = headless_window(&gmail);
    wait_until(|| {
        cx.run_until_parked();
        cx.update(|cx| subjects(&mailbox, cx) == ["Plan"])
    });
    let plan = cx.update(|cx| id_of(&mailbox, "Plan", cx));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        click_row(window, plan, cx);
    })
    .unwrap();
    wait_until(|| {
        cx.run_until_parked();
        cx.update(|cx| shown_in_reader(&mailbox, cx) == Some(plan))
    });
    save(&mut cx, window, "gmail");
    cx.update_window(window, |_, window, cx| {
        window.dispatch_action(Box::new(EditLabels), cx)
    })
    .unwrap();
    save(&mut cx, window, "gmail-labels");
    drop(cx);
    gmail.hide_all_mail_and_sync();
    let (mut cx, window, mailbox) = headless_window(&gmail);
    wait_until(|| {
        cx.run_until_parked();
        cx.update(|cx| subjects(&mailbox, cx) == ["Plan"])
    });
    save(&mut cx, window, "gmail-all-mail-hidden");
    drop(cx);

    let mail = TestMail::start();
    let (mut cx, window, mailbox) = headless_window(&mail);
    wait_until(|| {
        cx.run_until_parked();
        cx.update(|cx| subjects(&mailbox, cx).len() == 6)
    });

    // The list on one line, a row hovered.
    let id = cx.update(|cx| id_of(&mailbox, "月度报表", cx));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.hover(("message", id.0 as u64), cx);
    })
    .unwrap();
    save(&mut cx, window, "list-row-hovered");

    let shots = [
        ("text", "你好，世界"),
        ("html-inline-image", "月度报表"),
        ("html-newsletter", "十月产品更新：新功能一览"),
    ];
    for (name, subject) in shots {
        let id = cx.update(|cx| id_of(&mailbox, subject, cx));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            click_row(window, id, cx);
        })
        .unwrap();
        wait_until(|| {
            cx.run_until_parked();
            cx.update(|cx| shown_in_reader(&mailbox, cx) == Some(id))
        });
        save(&mut cx, window, name);
    }

    // Text selected on the newsletter's page, still open.
    cx.update_window(window, |_, window, cx| {
        window.drag(
            gpui_kit::point(px(760.), px(420.)),
            gpui_kit::point(px(1100.), px(560.)),
            cx,
        )
    })
    .unwrap();
    save(&mut cx, window, "html-newsletter-selected");
    // Its context menu.
    cx.update_window(window, |_, window, cx| {
        let position = gpui_kit::point(px(900.), px(480.));
        window.pointer_down(position, gpui_kit::MouseButton::Right, cx);
        window.pointer_up(position, gpui_kit::MouseButton::Right, cx);
    })
    .unwrap();
    save(&mut cx, window, "html-newsletter-context-menu");
    cx.update_window(window, |_, window, cx| window.press("escape", cx))
        .unwrap();

    // Every built-in theme, dark ones in the dark appearance.
    let themes: Vec<(String, bool)> = cx.update(|cx| {
        crate::theme::appearance::Themes::entries(cx)
            .iter()
            .map(|e| (e.name().to_owned(), e.is_dark()))
            .collect()
    });
    for (name, dark) in themes {
        cx.update(|cx| {
            Preferences::update(cx, |s| {
                s.appearance = if dark {
                    crate::settings::Appearance::Dark
                } else {
                    crate::settings::Appearance::Light
                };
                if dark {
                    s.dark_theme = name.clone();
                } else {
                    s.light_theme = name.clone();
                }
            });
        });
        save(
            &mut cx,
            window,
            &format!("theme-{}", name.replace(' ', "-")),
        );
    }

    // Settings, and a composer replying.
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenSettings), cx);
    })
    .unwrap();
    save(&mut cx, window, "settings");
    let account = mail.account();
    cx.update_window(window, |_, window, cx| {
        window.click(crate::settings::dialog::section_item("Accounts"), cx);
        window.render_frame(cx);
        window.click(("add-identity", account.0 as u64), cx);
    })
    .unwrap();
    save(&mut cx, window, "settings-add-identity");
    cx.update_window(window, |_, window, cx| {
        window.close_all_dialogs(cx);
        window.render_frame(cx);
        window.click("reply", cx);
    })
    .unwrap();
    let mut composer = None;
    wait_until(|| {
        cx.run_until_parked();
        composer = cx.update(|cx| cx.windows().into_iter().find(|w| *w != window));
        composer.is_some()
    });
    save(&mut cx, composer.unwrap(), "composer");
}

/// A window rendered offscreen by the GPU, so it can be captured.
fn headless_window(mail: &TestMail) -> (HeadlessAppContext, AnyWindowHandle, Entity<Mailbox>) {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(crate::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.allow_parking();
    let (window, mailbox) = cx.update(|cx| open_mailbox(cx, mail));
    (cx, window, mailbox)
}
