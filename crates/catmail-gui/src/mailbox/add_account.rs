//! The sign-in form: an account's servers and password. The login is tried against both
//! servers before anything is stored, so a typo leaves no account (or keyring entry)
//! behind, as with `catmail account add`.

use catmail_core::{MailClient, Security, Verify};
use catmail_frontend::login::AccountValues;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::{
    App, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, rems,
};

use crate::login_form::{LoginFields, ServerFields};
use crate::submission::{self, Submit};

pub enum AddAccountEvent {
    /// The login worked and the account is stored; background sync syncs it.
    Added,
    Cancelled,
}

pub struct AddAccount {
    client: MailClient,
    login: LoginFields,
    imap: ServerFields,
    smtp: ServerFields,
    /// False for the first account: there is nothing to go back to.
    cancellable: bool,
    submit: Submit,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AddAccountEvent> for AddAccount {}

impl AddAccount {
    pub fn new(
        client: MailClient,
        cancellable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let login = LoginFields::new("", None, window, cx);
        login.focus(window, cx);
        let imap = ServerFields::new(
            "imap",
            "Incoming mail (IMAP)",
            "imap.example.com",
            Security::default_imap_port,
            window,
            cx,
        );
        let smtp = ServerFields::new(
            "smtp",
            "Outgoing mail (SMTP)",
            "smtp.example.com",
            Security::default_smtp_port,
            window,
            cx,
        );

        let fields = login
            .inputs()
            .into_iter()
            .chain(imap.inputs())
            .chain(smtp.inputs());
        let subscriptions = submission::submit_on_enter(fields, Self::sign_in, window, cx);

        Self {
            client,
            login,
            imap,
            smtp,
            cancellable,
            submit: Submit::new("sign in"),
            _subscriptions: subscriptions,
        }
    }

    fn values(&self, cx: &App) -> AccountValues {
        AccountValues {
            login: self.login.values(cx),
            imap: self.imap.values(cx),
            smtp: self.smtp.values(cx),
        }
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = self.values(cx);
        let client = self.client.clone();
        let request = values.new_account().map(|new| async move {
            client
                .add_account(new, values.login.password, Verify::Login)
                .await
        });
        self.submit.start(
            |this: &mut Self| &mut this.submit,
            request,
            |_, _, cx| cx.emit(AddAccountEvent::Added),
            window,
            cx,
        );
    }
}

impl Render for AddAccount {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = self.submit.actions(
            self.cancellable.then(|| {
                Button::new("cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AddAccountEvent::Cancelled)))
            }),
            Button::new("sign-in")
                .label("Sign in")
                .on_click(cx.listener(|this, _, window, cx| this.sign_in(window, cx))),
        );

        div()
            .id("add-account")
            .size_full()
            .overflow_y_scroll()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(rems(36.))
                    .p_6()
                    .gap_6()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xl().font_semibold().child("Add an account"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("catmail signs in to check these settings before saving them."),
                            ),
                    )
                    .child(self.login.render())
                    .child(self.imap.render(|this: &mut Self| &mut this.imap, cx))
                    .child(self.smtp.render(|this: &mut Self| &mut this.smtp, cx))
                    .children(self.submit.error("sign-in-error", cx))
                    .child(actions),
            )
    }
}
