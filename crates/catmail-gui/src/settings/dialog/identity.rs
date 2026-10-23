//! The form for another address an account sends as, through an SMTP server of its own
//! (a sending service, say). The login is tried before anything is stored, as for an
//! account.

use catmail_core::{Account, MailClient, NewIdentity, Security, Verify};
use gpui_kit::component::button::Button;
use gpui_kit::component::{StyledExt as _, v_flex};
use gpui_kit::{
    App, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, div,
};

use crate::login_form::{self, LoginFields, LoginValues, ServerFields, ServerValues, Submit};

/// The form is done: the identity was added, or adding it was cancelled.
pub struct IdentityFormClosed;

/// What the form holds, as typed.
#[derive(Debug, Clone)]
struct Values {
    login: LoginValues,
    smtp: ServerValues,
}

impl Values {
    fn new_identity(&self) -> Result<NewIdentity, &'static str> {
        let (email, display_name, username) = self.login.who();
        Ok(NewIdentity {
            email,
            display_name,
            username,
            smtp: self.smtp.config(Security::default_smtp_port)?,
        })
    }
}

pub struct AddIdentity {
    client: MailClient,
    account: Account,
    login: LoginFields,
    smtp: ServerFields,
    submit: Submit,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<IdentityFormClosed> for AddIdentity {}

impl AddIdentity {
    pub fn new(
        client: MailClient,
        account: Account,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let login = LoginFields::new("identity-", account.display_name.clone(), window, cx);
        let smtp = ServerFields::new(
            "identity-smtp",
            "Outgoing mail (SMTP)",
            "smtp.example.com",
            Security::default_smtp_port,
            window,
            cx,
        );
        let fields = login.inputs().into_iter().chain(smtp.inputs());
        let subscriptions = login_form::submit_on_enter(fields, Self::add, window, cx);
        Self {
            client,
            account,
            login,
            smtp,
            submit: Submit::default(),
            _subscriptions: subscriptions,
        }
    }

    fn values(&self, cx: &App) -> Values {
        Values {
            login: self.login.values(cx),
            smtp: self.smtp.values(cx),
        }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = self.values(cx);
        let (client, account) = (self.client.clone(), self.account.id);
        let request = values.new_identity().map(|new| async move {
            client
                .add_identity(account, new, values.login.password, Verify::Login)
                .await
        });
        self.submit.start(
            |this: &mut Self| &mut this.submit,
            request,
            |_, cx| cx.emit(IdentityFormClosed),
            window,
            cx,
        );
    }
}

impl Render for AddIdentity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = self.submit.actions(
            Some(
                Button::new("identity-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(IdentityFormClosed))),
            ),
            Button::new("identity-add")
                .label("Add")
                .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
        );
        v_flex()
            .id("add-identity")
            .gap_5()
            .child(
                div()
                    .font_semibold()
                    .child(format!("New address for {}", self.account.email)),
            )
            .child(self.login.render())
            .child(self.smtp.render(|this: &mut Self| &mut this.smtp, cx))
            .children(self.submit.error("identity-error", cx))
            .child(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_fields_take_the_defaults() {
        let values = Values {
            login: LoginValues {
                email: " news@example.org ".into(),
                password: "key".into(),
                name: String::new(),
                username: String::new(),
            },
            smtp: ServerValues {
                host: "smtp.resend.com".into(),
                port: String::new(),
                security: Security::Tls,
            },
        };
        let new = values.new_identity().unwrap();
        assert_eq!(new.email, "news@example.org");
        assert_eq!(new.username, "news@example.org");
        assert_eq!(new.display_name, None);
        assert_eq!((new.smtp.port, new.smtp.security), (465, Security::Tls));
    }
}
