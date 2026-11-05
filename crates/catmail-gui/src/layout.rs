//! Layout pieces the panes and windows share, so they line up and look alike.

use std::rc::Rc;

use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, Dialog, DialogEntrance, DialogFooter};
use gpui_kit::component::sidebar::SidebarMenuItem;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    AnyWindowHandle, App, Bounds, ClickEvent, Context, Div, ElementId, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Rems,
    Render, RenderOnce, SharedString, Size, Stateful, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions,
    div, point, prelude::FluentBuilder as _, px, rems,
};

use crate::actions::DIALOG_BUTTON;
use crate::assets::Hero;
use crate::format;
use crate::theme::colors::ThemeColors as _;

/// A row of a sidebar menu, the mailbox's or the settings'. Selected, its label and its
/// icon, filled, take the primary color, and the label keeps its weight: GPUI Kit sets a
/// selected row in medium, which widens it.
pub fn sidebar_row(
    label: impl Into<SharedString>,
    icon: Hero,
    selected: bool,
    cx: &App,
) -> SidebarMenuItem {
    let item = SidebarMenuItem::new(label).active(selected);
    if !selected {
        return item.icon(Icon::new(icon));
    }
    let primary = cx.theme().primary;
    item.icon(Icon::new(icon.solid()).text_color(primary))
        .label_style(
            StyleRefinement::default()
                .font_weight(FontWeight::NORMAL)
                .text_color(primary),
        )
}

/// What a window has at its top on macOS; elsewhere it has the system's title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// No title bar: the content goes up to the top, the window's buttons over the
    /// sidebar's corner, and the row they sit in, the bars at the top of the panes, is
    /// where the window is moved from ([`movable`]). The window places its buttons with
    /// [`place_window_buttons`].
    Unified,
    /// GPUI Kit's `TitleBar`, in the theme's colors: the system's would match the
    /// theme's light or dark at best (`App::set_window_appearance`), not its colors.
    /// The window renders it at its top.
    TitleBar,
}

/// A window centered on the screen. Its geometry is in device-independent pixels, not
/// the rem scale. Its title shows in the Window menu, Mission Control and to screen
/// readers, also where the window draws its own title bar.
pub fn window_options(
    title: impl Into<SharedString>,
    size: Size<Pixels>,
    min_size: Size<Pixels>,
    frame: Frame,
    cx: &App,
) -> WindowOptions {
    WindowOptions {
        titlebar: Some(titlebar(title.into(), frame, cx)),
        // The window moves itself (`movable`, `TitleBar`): AppKit would also take a
        // double click there, and hold every click back to see whether it is one.
        app_owns_titlebar_drag: cfg!(target_os = "macos"),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: Some(min_size),
        // Wayland reads it when it creates the window.
        app_id: Some(crate::APP_ID.into()),
        ..Default::default()
    }
}

fn titlebar(title: SharedString, frame: Frame, cx: &App) -> TitlebarOptions {
    let title = Some(title);
    if !cfg!(target_os = "macos") {
        return TitlebarOptions {
            title,
            ..Default::default()
        };
    }
    match frame {
        Frame::Unified => TitlebarOptions {
            title,
            appears_transparent: true,
            // The window's rem; `place_window_buttons` moves them when it changes.
            traffic_light_position: Some(window_buttons_at(cx.theme().font_size)),
        },
        Frame::TitleBar => TitlebarOptions {
            title,
            ..TitleBar::title_bar_options()
        },
    }
}

/// How tall the bars at the top of a window are, [`toolbar`] and the list's search row:
/// the row the window's buttons sit in, in a [`Frame::Unified`] window.
pub fn bar_height() -> Rems {
    rems(3.)
}

/// Whether a [`Frame::Unified`] window's buttons are over its top-left corner, which
/// then leaves them room: on macOS, unless the window is full screen, which hides them.
pub fn has_window_buttons(window: &Window) -> bool {
    cfg!(target_os = "macos") && !window.is_fullscreen()
}

/// Where the window's buttons go in a [`Frame::Unified`] window, at the text size
/// `rem`: in the middle of the top bars' row, as far from the left edge as from the
/// top, so they follow the bars as the text size changes them.
fn window_buttons_at(rem: Pixels) -> Point<Pixels> {
    // AppKit's buttons (their frames, a little taller than the circles).
    const BUTTON_HEIGHT: Pixels = px(16.);
    let inset = ((bar_height().to_pixels(rem) - BUTTON_HEIGHT) / 2.).floor();
    point(inset, inset)
}

/// Keeps a [`Frame::Unified`] window's buttons in the middle of its top row, which
/// grows with the text size; `placed` is where they went last, so AppKit is asked only
/// when that changes. Called as the window renders.
pub fn place_window_buttons(placed: &mut Option<Point<Pixels>>, window: &mut Window) {
    #[cfg(target_os = "macos")]
    {
        let at = window_buttons_at(window.rem_size());
        if *placed != Some(at) {
            window.set_traffic_light_position(at);
            *placed = Some(at);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (placed, window);
}

/// `content` in a [`Frame::Unified`] window, which has no title bar: behind it, across
/// the top, the row the window is moved from. Whatever is in front there and takes the
/// press (a button, a field, a checkbox: they prevent the default) keeps it; the rest
/// of the row moves the window. It goes inside any focusable element the content is in:
/// that takes the press for its focus, after what is in it.
pub fn movable(content: impl IntoElement, window: &Window) -> Div {
    div()
        .relative()
        .size_full()
        // First, so behind: what is in front gets the press first.
        .when(has_window_buttons(window), |this| this.child(MoveRow))
        .child(content)
}

/// Pressing it and moving moves the window, and a double click does what the system's
/// setting says a double click on a title bar does. See [`movable`].
#[derive(IntoElement)]
struct MoveRow;

impl RenderOnce for MoveRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Pressed, until it moves: then the window moves, as GPUI Kit's `TitleBar` does.
        // Nothing renders from it, so changing it doesn't notify.
        let pressed = window.use_state(cx, |_, _| false);
        let set = |pressed: &Entity<bool>, value: bool, cx: &mut App| {
            pressed.update(cx, |pressed, _| *pressed = value)
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(bar_height())
            .on_mouse_down(MouseButton::Left, {
                let pressed = pressed.clone();
                move |event, window, cx| {
                    // What is in front ran first, and took the press.
                    if window.default_prevented() {
                        return;
                    }
                    if event.click_count == 2 {
                        window.titlebar_double_click();
                    }
                    set(&pressed, event.click_count == 1, cx);
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let pressed = pressed.clone();
                move |_, _, cx| set(&pressed, false, cx)
            })
            // A press let go off the row is over by the next move here, without the button:
            // the row hears only what happens over it.
            .on_mouse_move(move |event, window, cx| {
                if *pressed.read(cx) {
                    set(&pressed, false, cx);
                    if event.pressed_button == Some(MouseButton::Left) {
                        window.start_window_move();
                    }
                }
            })
    }
}

/// Opens a window of the app. Its app id (Wayland) or `WM_CLASS` (X11) is the name of
/// the desktop file, which is how a dock or a task switcher finds the window's icon and
/// name.
pub fn open_window<V: Render>(
    options: WindowOptions,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<(AnyWindowHandle, Entity<V>)> {
    gpui_kit::open_window(options, cx, |window, cx| {
        // X11 doesn't read `WindowOptions::app_id`.
        window.set_app_id(crate::APP_ID);
        build(window, cx)
    })
}

/// The bar of commands at the top of a pane or window; the same height whatever it
/// holds, [`bar_height`]: in a [`Frame::Unified`] window, the row the window moves from.
pub fn toolbar(cx: &App) -> Div {
    command_bar(cx).border_b_1()
}

/// A [`toolbar`] at the bottom of a window instead.
pub fn bottom_bar(cx: &App) -> Div {
    command_bar(cx).border_t_1()
}

fn command_bar(cx: &App) -> Div {
    h_flex()
        .flex_none()
        .h(bar_height())
        .px_3()
        .gap_1()
        .border_color(cx.theme().border)
}

/// What a pane or list shows when it has nothing to show: a quiet line in its middle.
pub fn empty(text: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .size_full()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// A page of the right pane, `content` starting with its [`page_title`]: it scrolls as a
/// whole, so a long header doesn't squeeze the text, and spans the pane, whose width the
/// user sets. The caller sizes it.
pub fn page(id: impl Into<ElementId>, content: Div) -> Stateful<Div> {
    div()
        .id(id)
        .overflow_y_scroll()
        .child(content.px_6().pt_5().pb_8())
}

pub fn page_title(title: impl Into<SharedString>) -> Div {
    div()
        .text_xl()
        .font_weight(FontWeight::SEMIBOLD)
        .child(title.into())
}

/// A name and its value, the names lined up in a column.
pub fn labelled(name: impl IntoElement, value: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w_16()
                .flex_none()
                .text_color(cx.theme().muted_foreground)
                .child(name),
        )
        .child(div().flex_1().min_w_0().child(value))
}

/// A sender's avatar: the first letter of `name`, on the color `email` picks, so a
/// sender keeps theirs wherever it shows. The caller sizes it.
pub fn avatar(name: &str, email: &str, cx: &App) -> Div {
    let (bg, fg) = cx.theme().avatar(email);
    let initial = name.chars().take(1).collect::<String>().to_uppercase();
    circle(bg, fg)
        .font_weight(FontWeight::SEMIBOLD)
        .child(initial)
}

/// An [`avatar`]'s shape, for what stands in its place: its content centered.
pub fn circle(bg: Hsla, fg: Hsla) -> Div {
    div()
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .text_color(fg)
}

/// A short status beside a name, on the background and in the text color given.
pub fn badge(text: impl Into<SharedString>, (bg, fg): (Hsla, Hsla), cx: &App) -> Div {
    div()
        .px_1p5()
        .rounded(cx.theme().radius)
        .text_xs()
        .bg(bg)
        .text_color(fg)
        .child(text.into())
}

/// An attachment of a draft: its name and size.
pub fn attachment_chip(filename: impl Into<SharedString>, size: u64, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_1p5()
        .h_7()
        .px_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .text_sm()
        .child(Icon::new(Hero::PaperClip).xsmall().text_color(muted))
        .child(filename.into())
        .child(div().text_xs().text_color(muted).child(format::size(size)))
}

/// How every dialog comes and goes: settling from just above its place as it fades in,
/// and back out the same way. GPUI Kit's default slides down from the window's top, and
/// leaving retraces that whole distance in a few frames, so a dialog seems to vanish.
const DIALOG_ENTRANCE: DialogEntrance = DialogEntrance::FadeSlide;

/// Opens a dialog built by `build`, with the motion every dialog shares.
pub fn open_dialog(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        build(dialog.entrance(DIALOG_ENTRANCE), window, cx)
    });
}

/// What a context menu's item runs: `f` on `view`, once the menu has closed. A dialog
/// opened from the item gives the focus back, when it closes, to what had it when it
/// opened; run at once, that is the menu, gone by then, and the window's keys would do
/// nothing. GPUI Kit hands the focus back before an item that dispatches an action, not
/// before one with a click handler.
pub fn after_menu<V: 'static>(
    view: WeakEntity<V>,
    f: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let f = Rc::new(f);
    move |_, window, cx| {
        let (view, f) = (view.clone(), f.clone());
        window.defer(cx, move |window, cx| {
            view.update(cx, |view, cx| f(view, window, cx)).ok();
        });
    }
}

/// The element ids of the buttons that confirm and cancel [`confirm_danger`].
pub const CONFIRM: &str = "confirm-ok";
pub const CANCEL: &str = "confirm-cancel";

/// Asks before something that can't be undone: `title` asks the question, `description`
/// says what follows, and the red `ok` button does it with `on_ok`.
pub fn confirm_danger(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    ok: &'static str,
    window: &mut Window,
    cx: &mut App,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    let (title, description) = (title.into(), description.into());
    let on_ok = Rc::new(on_ok);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = on_ok.clone();
        // Buttons of our own, not GPUI Kit's: in the `DIALOG_BUTTON` context, Enter on a
        // focused Cancel cancels instead of being the dialog's Confirm.
        let cancel = Button::new(CANCEL)
            .label("Cancel")
            .key_context(DIALOG_BUTTON)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx));
        let confirm = Button::new(CONFIRM)
            .label(ok)
            .danger()
            .key_context(DIALOG_BUTTON)
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
            });
        alert
            .entrance(DIALOG_ENTRANCE)
            .title(title.clone())
            .centered()
            .description(description.clone())
            .footer(DialogFooter::new().child(cancel).child(confirm))
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}
