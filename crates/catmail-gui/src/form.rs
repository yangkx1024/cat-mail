//! What the forms and dialogs share: the dialog's look, its buttons, a labelled field, a
//! form's submission (`catmail_frontend::login::Submission`), and the one-field dialog that
//! asks for a name (a folder's, or the one an address sends as).

use std::rc::Rc;

use catmail_frontend::commands::Confirm;
use catmail_frontend::login::Submission;
use futures_util::future::LocalBoxFuture;
use nsh_base::{alert_dialog, button, dialog, theme};
use nsh_core::ModalAnimation;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::tasks::in_view;
use crate::theme::SelectableText as _;

/// A dialog `width` points wide over the window, dimming it; `cancel` is what Escape or
/// a press beside it does.
pub fn modal(
    width: f32,
    cancel: impl Fn() + 'static,
    content: impl IntoElement,
) -> impl IntoElement {
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
                        .p(px(24.0))
                        .gap(px(16.0))
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(move || theme().border)
                        .bg(move || theme().popover)
                        .child(content),
                ),
        )
}

/// A dialog's title.
pub fn title(text_: impl Into<String>) -> Text {
    text(text_.into())
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(move || theme().popover_foreground)
}

/// A dialog's or form's button, round: filled for the one that does what it is for.
/// Off while `busy`; the filled one shows it by dimming.
pub fn form_button(
    label: &'static str,
    primary: bool,
    busy: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .disabled(busy)
        .on_press(press)
        .h(px(32.0))
        .px(px(16.0))
        .rounded(px(16.0))
        .justify_center()
        .opacity(move || if primary && busy() { 0.6_f32 } else { 1.0 })
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
                .text_color(move || {
                    if primary {
                        theme().primary_foreground
                    } else {
                        theme().foreground
                    }
                }),
        )
}

/// Cancel and the button that submits, at the end of a row.
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

/// A text field's look.
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

/// A field with its label above it, and what it is for under it, if that needs saying.
pub fn field(label: &'static str, help: Option<&'static str>, input: impl IntoElement) -> View {
    view()
        .gap(px(6.0))
        .child(
            text(label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().foreground),
        )
        .child(input)
        .children(help.map(|help| {
            text(help)
                .text_xs()
                .text_color(move || theme().muted_foreground)
                .into_element()
        }))
}

/// Why the last submission failed, under the form.
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
/// the form doesn't stop it, hence Cancel being off meanwhile.
pub fn submit<T: 'static>(
    submission: Signal<Submission>,
    request: Result<LocalBoxFuture<'static, catmail_core::Result<T>>, &'static str>,
    done: impl FnOnce(T) + 'static,
) {
    let Some(request) = change(submission, |s| s.begin(request)) else {
        return;
    };
    in_view(async move {
        let result = request.await;
        if let Some(value) = change(submission, |s| s.ended(result)) {
            done(value);
        }
    });
}

/// Changes the signal's value with `f`, and gives what `f` did.
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

// --- The name dialog ----------------------------------------------------------------

/// What a name is asked for.
#[derive(Clone)]
pub struct NameRequest {
    pub title: String,
    /// What the field starts with, selected, so that what is typed replaces it.
    pub name: String,
    /// Shown while the field is empty.
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
