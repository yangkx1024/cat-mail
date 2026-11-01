//! A form's submission, shared by every form (the sign-in page, the dialogs that add a
//! sending address or name a folder), and the dialog a form opens in.

use std::future::Future;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex};
use gpui_kit::{
    App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, rems,
};

use crate::actions::DIALOG_BUTTON;
use crate::layout;

/// Enter in any of the fields submits the form, as in a native login form. A form in a
/// dialog has the dialog's Confirm instead.
pub fn submit_on_enter<'a, T: 'static>(
    fields: impl IntoIterator<Item = &'a Entity<InputState>>,
    submit: fn(&mut T, &mut Window, &mut Context<T>),
    window: &mut Window,
    cx: &mut Context<T>,
) -> Vec<Subscription> {
    fields
        .into_iter()
        .map(|field| {
            cx.subscribe_in(
                field,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        submit(this, window, cx);
                    }
                },
            )
        })
        .collect()
}

/// A form's submission: whether one is running, and why the last one failed.
pub struct Submit {
    busy: bool,
    error: Option<SharedString>,
    /// What submitting does, for the error: "Couldn't sign in".
    what: &'static str,
    _task: Task<()>,
}

impl Submit {
    /// `what` submitting does, to say what failed: "sign in".
    pub fn new(what: &'static str) -> Self {
        Self {
            busy: false,
            error: None,
            what,
            _task: Task::ready(()),
        }
    }

    /// Runs `request`, unless one is already running, and hands its result to `done`. A
    /// problem with the values (`Err`) or the request's failure shows as the error;
    /// `submit` finds this state in the form.
    pub fn start<T: 'static, R: 'static>(
        &mut self,
        submit: fn(&mut T) -> &mut Submit,
        request: Result<impl Future<Output = catmail_core::Result<R>> + 'static, &'static str>,
        done: impl FnOnce(R, &mut Window, &mut Context<T>) + 'static,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        if self.busy {
            return;
        }
        cx.notify();
        let request = match request {
            Ok(request) => request,
            Err(problem) => {
                self.error = Some(problem.into());
                return;
            }
        };
        self.busy = true;
        self.error = None;
        self._task = cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                submit(this).busy = false;
                match result {
                    Ok(value) => done(value, window, cx),
                    Err(e) => {
                        let submit = submit(this);
                        submit.error = Some(format!("Couldn't {}: {e}", submit.what).into());
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Why the last submission failed, under the form.
    pub fn error(&self, id: &'static str, cx: &App) -> Option<impl IntoElement + use<>> {
        self.error.clone().map(|error| {
            div()
                .id(id)
                .role(Role::Alert)
                .test_support()
                .aria_label(error.clone())
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error)
        })
    }

    /// Whether a submission is running. Leaving the form then would only skip what
    /// follows: the engine finishes the request whatever the window does (its task
    /// isn't cancelled with the form's).
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// The form's Cancel button: off while a submission runs.
    pub fn cancel_button(&self, cancel: Button) -> Button {
        cancel.disabled(self.busy)
    }

    /// The button that submits the form.
    pub fn submit_button(&self, submit: Button) -> Button {
        submit.primary().loading(self.busy)
    }

    /// The buttons under the form: Cancel, if it has one, and the one that submits it.
    pub fn actions(&self, cancel: Option<Button>, submit: Button) -> impl IntoElement + use<> {
        h_flex()
            .justify_end()
            .gap_2()
            .children(cancel.map(|cancel| self.cancel_button(cancel)))
            .child(self.submit_button(submit))
    }
}

/// A form shown in a dialog of its own, which its submission closes.
pub trait DialogForm: Render {
    fn submit(&self) -> &Submit;

    /// Submits the form, closing the dialog once that is done.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>);
}

/// Opens `form` in a dialog titled `title`, `width` rems wide, over the window. Enter (the
/// dialog's Confirm) or the `ok` button commits it; Escape or Cancel close it, unless a
/// submission is running. `ids` are the element ids of Cancel and `ok`.
pub fn open_dialog<F: DialogForm>(
    form: Entity<F>,
    title: SharedString,
    width: f32,
    (cancel_id, ok_id): (&'static str, &'static str),
    ok: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    let width = rems(width).to_pixels(window.rem_size());
    layout::open_dialog(window, cx, move |dialog, _, cx| {
        let submit = form.read(cx).submit();
        // The buttons act through the dialog's own Cancel and Confirm, as Escape and Enter
        // do. On the buttons themselves, not a wrapper: focused, they take Enter and Space.
        let cancel_button = submit.cancel_button(
            Button::new(cancel_id)
                .label("Cancel")
                .key_context(DIALOG_BUTTON)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        );
        let ok_button = submit.submit_button(
            Button::new(ok_id)
                .label(ok)
                .key_context(DIALOG_BUTTON)
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
                }),
        );
        let (commit, cancel) = (form.clone(), form.clone());
        dialog
            .title(title.clone())
            .w(width)
            .centered()
            .on_ok(move |_, window, cx| {
                commit.update(cx, |form, cx| form.commit(window, cx));
                false
            })
            .on_cancel(move |_, _, cx| !cancel.read(cx).submit().busy())
            // The dialog's body clips to its content: room for a field's focus ring (3px
            // outside its border) at the top and bottom, which a form ending in a field
            // would otherwise lose.
            .child(div().py_1().child(form.clone()))
            .footer(DialogFooter::new().child(cancel_button).child(ok_button))
    });
}
