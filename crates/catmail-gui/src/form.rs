//! What the forms and dialogs share: the dialog's look, its buttons, a labelled field, a
//! fact by its name, a form's submission (`catmail_frontend::login::Submission`), and the
//! one-field dialog that asks for a name (a folder's, or the one an address sends as).

use std::rc::Rc;

use catmail_frontend::confirm::Confirm;
use catmail_frontend::login::Submission;
use futures_util::future::LocalBoxFuture;
use nsh_base::{alert_dialog, button, dialog, theme};
use nsh_core::ModalAnimation;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::buttons::spinner;
use crate::icons::{Icon, icon};
use crate::tasks::in_view;
use crate::theme::SelectableText as _;

pub const PADDING: f32 = 24.0;

/// How many dialogs are open over the window: [`modal`]'s, and the picker's.
#[derive(Clone, Copy, Default)]
struct OpenDialogs(u32);

/// Counts the dialog being built as open over the window until it is gone.
pub fn count_as_open() {
    let open = global::<OpenDialogs>();
    open.update(|o| o.0 += 1);
    cleanup(move || open.update(|o| o.0 -= 1));
}

/// Binds the keys `bind` binds while no dialog is open over the window. The window's
/// keys come before a dialog's and the focus isn't kept in it: bound all along, the
/// list's would act on the messages behind a dialog, and Escape wouldn't reach it.
pub fn keys_without_dialog(bind: impl Fn() + 'static) {
    effect(move || {
        if global::<OpenDialogs>().with(|o| o.0 == 0) {
            untracked(&bind);
        }
    });
}

/// A dialog `width` points wide over the window, dimming it; `cancel` is what Escape
/// does (a press on the dimmed window does nothing: nsh leaves a backdrop the app draws
/// to the app).
pub fn modal(
    width: f32,
    cancel: impl Fn() + 'static,
    content: impl IntoElement,
) -> impl IntoElement {
    count_as_open();
    dialog()
        .animation(ModalAnimation::Fade)
        .on_cancel(cancel)
        .child(
            view()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(hsla(0.0, 0.0, 0.0, 0.4))
                .child(
                    view()
                        .w(px(width))
                        .p(px(PADDING))
                        .gap(px(16.0))
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(move || theme().border)
                        .bg(move || theme().popover)
                        .child(content),
                ),
        )
}

pub fn title(text_: impl Into<String>) -> Text {
    text(text_.into())
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(move || theme().popover_foreground)
}

/// A dialog's or form's button, round: filled for the one that does what it is for.
/// Off while `busy`; the filled one shows macOS's spinner in place of its label then, as
/// wide as before.
pub fn form_button(
    label: &'static str,
    primary: bool,
    busy: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> impl IntoElement {
    let spinning = move || primary && busy();
    button()
        .disabled(busy)
        .on_press(press)
        .h(px(32.0))
        .px(px(16.0))
        .rounded(px(16.0))
        .justify_center()
        .border_1()
        .border_color(move || {
            if primary {
                theme().primary
            } else {
                theme().border
            }
        })
        .bg(move || {
            if primary {
                theme().primary
            } else {
                AdaptiveColor::TRANSPARENT
            }
        })
        .child(
            text(label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                // Still there, hidden, while the spinner shows: it keeps the width.
                .opacity(move || if spinning() { 0.0_f32 } else { 1.0 })
                .text_color(move || {
                    if primary {
                        theme().primary_foreground
                    } else {
                        theme().foreground
                    }
                }),
        )
        .child(when(spinning, move || {
            view()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .child(spinner(14.0).text_color(move || theme().primary_foreground))
        }))
}

pub fn actions(
    cancel: Option<Rc<dyn Fn()>>,
    ok: &'static str,
    busy: impl Fn() -> bool + Copy + 'static,
    submit: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .flex_row()
        .justify_end()
        .gap(px(8.0))
        .children(
            cancel
                .map(|cancel| form_button("Cancel", false, busy, move || cancel()).into_element()),
        )
        .child(form_button(ok, true, busy, submit))
}

pub fn text_input() -> Input {
    input()
        .h(px(34.0))
        .px(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(move || theme().input)
        .text_sm()
        .text_color(move || theme().foreground)
}

/// A choice shown as what is chosen, opening a menu of the others (`popup_menu`) when
/// pressed. `label` names it for screen readers.
pub fn menu_field(
    label: &'static str,
    chosen: impl Fn() -> String + 'static,
    open: impl Fn() + 'static,
) -> View {
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(30.0))
        .px(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(move || theme().input)
        .label(label)
        .on_press(open)
        .child(
            text(chosen)
                .flex_1()
                .number_of_lines(1)
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(icon(|| Icon::ChevronDown, 14.0).text_color(move || theme().muted_foreground))
}

/// A field with its label above it, which names the input for screen readers too, and
/// what it is for under it, if that needs saying.
pub fn field(label: &'static str, help: Option<&'static str>, input: Input) -> View {
    view()
        .gap(px(6.0))
        .child(
            text(label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().foreground),
        )
        .child(input.label(label))
        .children(help.map(|help| {
            text(help)
                .text_xs()
                .text_color(move || theme().muted_foreground)
                .into_element()
        }))
}

/// A fact shown by its name, the name in a column of its own: "To", the addresses (of a
/// draft, an outbox entry, a forwarded message).
pub fn fact(name: &'static str, value: String) -> View {
    view()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .child(
            text(name)
                .w(px(64.0))
                .flex_shrink_0()
                .text_sm()
                .text_color(move || theme().muted_foreground),
        )
        .child(
            text(value)
                .flex_1()
                .text_sm()
                .selectable_text()
                .text_color(move || theme().foreground),
        )
}

pub fn error(submission: Signal<Submission>) -> impl IntoElement {
    let message = move || submission.with(|s| s.error().map(str::to_owned));
    when(
        move || message().is_some(),
        move || {
            text(move || message().unwrap_or_default())
                .text_sm()
                .selectable_text()
                .text_color(move || theme().destructive)
        },
    )
}

/// Runs a form's request, unless one is running (or the values are wrong, which the
/// form then says), and `done` with what it gave. The request is the engine's: leaving
/// the form doesn't stop it, hence Cancel being off meanwhile. A form that can go anyway
/// (a section of Settings, closed) is gone when the request ends: nothing is shown, and
/// `done` doesn't run.
pub fn submit<T: 'static, E: std::fmt::Display + 'static>(
    submission: Signal<Submission>,
    request: Result<LocalBoxFuture<'static, Result<T, E>>, &'static str>,
    done: impl FnOnce(T) + 'static,
) {
    let Some(request) = change(submission, |s| s.begin(request)) else {
        return;
    };
    in_view(async move {
        let result = request.await;
        // Its signals went with the form.
        if submission.try_get().is_none() {
            return;
        }
        if let Some(value) = change(submission, |s| s.ended(result)) {
            done(value);
        }
    });
}

fn change<T: Clone + 'static, R>(signal: Signal<T>, f: impl FnOnce(&mut T) -> R) -> R {
    let mut out = None;
    signal.update(|value| out = Some(f(value)));
    out.expect("`update` runs `f`")
}

/// Asks `confirm` about what can't be undone, at once; the future says whether the user
/// went on.
pub fn confirmed(confirm: Confirm) -> impl Future<Output = bool> {
    let answer = alert_dialog()
        .title(confirm.title)
        .description(confirm.description)
        .cancel("cancel", "Cancel")
        .destructive("go-on", confirm.action)
        .show();
    async move {
        answer
            .await
            .is_ok_and(|r| r.action.as_deref() == Some("go-on"))
    }
}

/// Asks first, then does `go`: what can't be undone.
pub fn confirm_then(confirm: Confirm, go: impl FnOnce() + 'static) {
    let answer = confirmed(confirm);
    in_view(async move {
        if answer.await {
            go();
        }
    });
}

#[derive(Clone)]
pub struct NameRequest {
    pub title: String,
    /// What the field starts with, selected, so that what is typed replaces it.
    pub name: String,
    pub placeholder: Option<&'static str>,
    /// The button that submits the name.
    pub ok: &'static str,
    /// What submitting does, for the error: "rename it".
    pub what: &'static str,
    /// Whether no name will do: an address goes alone without one, a folder can't.
    pub may_be_empty: bool,
    /// The change, given the name as typed.
    pub request: Rc<dyn Fn(String) -> LocalBoxFuture<'static, catmail_core::Result<()>>>,
}

/// The name dialog, while it is open.
#[derive(Clone, Default)]
struct NameDialog(Option<NameRequest>);

pub fn ask_name(request: NameRequest) {
    global::<NameDialog>().set(NameDialog(Some(request)));
}

pub fn asking_name() -> bool {
    global::<NameDialog>().with_untracked(|n| n.0.is_some())
}

fn close_name_dialog() {
    global::<NameDialog>().set(NameDialog(None));
}

/// The name dialog, while one is asked for: it stays open until the engine has made the
/// change (on the server, which it does at once), and says why if it couldn't.
pub fn name_dialog() -> impl IntoElement {
    dynamic(move || match global::<NameDialog>().get().0 {
        Some(request) => name_form(request).into_element(),
        None => view().into_element(),
    })
}

fn name_form(request: NameRequest) -> impl IntoElement {
    let value = signal(request.name.clone());
    let submission = signal(Submission::new(request.what));
    let busy = move || submission.with(Submission::busy);
    let (may_be_empty, ask) = (request.may_be_empty, request.request.clone());
    let submit_ = move || {
        let name = value.get_untracked();
        if !may_be_empty && name.trim().is_empty() {
            return;
        }
        submit(submission, Ok(ask(name)), |()| close_name_dialog());
    };
    let submit_ = Rc::new(submit_);
    let on_enter = submit_.clone();
    let cancel = move || {
        if !untracked(busy) {
            close_name_dialog();
        }
    };
    let length = request.name.chars().count() as u32;
    modal(
        400.0,
        cancel,
        view()
            .gap(px(12.0))
            .child(title(request.title.clone()))
            .child(
                text_input()
                    // The title names it for screen readers, as it does on screen.
                    .label(request.title.clone())
                    .default_value(request.name.clone())
                    .placeholder(request.placeholder.unwrap_or_default().to_owned())
                    .selected_range((0, length))
                    .auto_focus(true)
                    .on_change(move |name: &str| value.set(name.to_owned()))
                    .on_submit(move |_: &str| on_enter()),
            )
            .child(error(submission))
            .child(actions(
                Some(Rc::new(cancel)),
                request.ok,
                busy,
                move || submit_(),
            )),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use nsh_core::element::{Runtime, create_root};

    use super::*;

    thread_local! {
        static BOUND: Cell<u32> = const { Cell::new(0) };
    }

    /// Stands in for `on_key`: bound until its owner is cleaned up.
    fn bind() {
        BOUND.with(|b| b.set(b.get() + 1));
        cleanup(|| BOUND.with(|b| b.set(b.get() - 1)));
    }

    fn bound() -> bool {
        BOUND.with(|b| b.get()) > 0
    }

    #[test]
    fn keys_unbind_while_any_dialog_is_open() {
        let runtime = Runtime::default();
        let _entered = runtime.enter();
        let (_window, ()) = create_root(|| keys_without_dialog(bind));
        assert!(bound());

        let (settings, ()) = create_root(count_as_open);
        assert!(!bound(), "bound behind the dialog");
        let (picker, ()) = create_root(count_as_open);
        picker.dispose();
        assert!(!bound(), "bound again with a dialog still open");

        settings.dispose();
        assert!(bound(), "not bound again once the dialogs closed");
        assert_eq!(BOUND.with(|b| b.get()), 1);
    }
}
