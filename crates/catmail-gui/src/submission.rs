//! A form's submission, shared by every form (the sign-in page, the dialogs that add a
//! sending address or name a folder), the dialog a form opens in, and the one-field
//! dialog that asks for a name.

use std::future::Future;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div, rems,
};

use catmail_frontend::login::Submission;

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

/// A form's submission (`Submission`: whether one is running, and why the last one
/// failed), and its task.
pub struct Submit {
    state: Submission,
    _task: Task<()>,
}

impl Submit {
    /// `what` submitting does, to say what failed: "sign in".
    pub fn new(what: &'static str) -> Self {
        Self {
            state: Submission::new(what),
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
        cx.notify();
        let Some(request) = self.state.begin(request) else {
            return;
        };
        self._task = cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| {
                if let Some(value) = submit(this).state.ended(result) {
                    done(value, window, cx);
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Why the last submission failed, under the form.
    pub fn error(&self, id: &'static str, cx: &App) -> Option<impl IntoElement + use<>> {
        self.state.error().map(|error| {
            let error = SharedString::from(error.to_owned());
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
        self.state.busy()
    }

    /// The form's Cancel button: off while a submission runs.
    pub fn cancel_button(&self, cancel: Button) -> Button {
        cancel.rounded_full().disabled(self.busy())
    }

    /// The button that submits the form. Running, it spins, as Sync does: `loading` alone
    /// only dims a button without an icon.
    pub fn submit_button(&self, submit: Button) -> Button {
        submit
            .primary()
            .rounded_full()
            .loading(self.busy())
            .when(self.busy(), |submit| submit.icon(Spinner::new()))
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

/// The element ids of a [`NameDialog`].
#[derive(Debug, Clone, Copy)]
pub struct NameIds {
    pub field: &'static str,
    pub cancel: &'static str,
    pub ok: &'static str,
    pub error: &'static str,
}

/// A dialog that asks for a name: a folder's, or the one an address sends as. Its one
/// field holds `name`, selected, so that what is typed replaces it.
pub struct NameDialog {
    pub ids: NameIds,
    pub title: SharedString,
    pub name: String,
    /// Shown while the field is empty.
    pub placeholder: Option<&'static str>,
    /// The button that submits the name.
    pub ok: &'static str,
    /// What submitting does, for the error: "rename it".
    pub what: &'static str,
}

impl NameDialog {
    /// Opens it over the window. Each submission hands the name, as typed, to `request`;
    /// the dialog stays open until one succeeds, and says why one didn't.
    pub fn open<F, Fut, R>(self, request: F, window: &mut Window, cx: &mut App)
    where
        F: Fn(String) -> Fut + 'static,
        Fut: Future<Output = catmail_core::Result<R>> + 'static,
        R: 'static,
    {
        let Self {
            ids,
            title,
            name,
            placeholder,
            ok,
            what,
        } = self;
        let form = cx.new(|cx| NameForm {
            name: cx.new(|cx| {
                let input = InputState::new(window, cx).default_value(name);
                match placeholder {
                    Some(placeholder) => input.placeholder(placeholder),
                    None => input,
                }
            }),
            ids,
            request,
            submit: Submit::new(what),
        });
        open_dialog(
            form.clone(),
            title,
            24.,
            (ids.cancel, ids.ok),
            ok,
            window,
            cx,
        );
        // Only once the dialog is open: it gives focus back to what had it when it closes.
        form.update(cx, |form, cx| {
            form.name.update(cx, |name, cx| {
                name.focus(window, cx);
                name.select_all(window, cx);
            })
        });
    }
}

struct NameForm<F> {
    name: Entity<InputState>,
    ids: NameIds,
    request: F,
    submit: Submit,
}

impl<F, Fut, R> DialogForm for NameForm<F>
where
    F: Fn(String) -> Fut + 'static,
    Fut: Future<Output = catmail_core::Result<R>> + 'static,
    R: 'static,
{
    fn submit(&self) -> &Submit {
        &self.submit
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().to_string();
        // The view shows the change once it reads it again, for the engine's event.
        // Nothing can open over this dialog meanwhile, so it is the one closed.
        self.submit.start(
            |this: &mut Self| &mut this.submit,
            Ok((self.request)(name)),
            |_, window, cx| window.close_dialog(cx),
            window,
            cx,
        );
    }
}

impl<F: 'static> Render for NameForm<F> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(Input::new(&self.name).id(self.ids.field))
            .children(self.submit.error(self.ids.error, cx))
    }
}
