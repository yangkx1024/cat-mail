//! The form for another address an account sends as, through an SMTP server of its own
//! (a sending service, say). The login is tried before anything is stored, as for an
//! account.

use catmail_core::{Account, MailClient, NewIdentity, Security, Verify};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _,
};

use crate::login_form::{self, LoginFields, LoginValues, ServerFields, ServerValues};

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
    adding: bool,
    error: Option<SharedString>,
    _add: Task<()>,
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
            adding: false,
            error: None,
            _add: Task::ready(()),
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
        if self.adding {
            return;
        }
        let values = self.values(cx);
        let new = match values.new_identity() {
            Ok(new) => new,
            Err(problem) => {
                self.error = Some(problem.into());
                cx.notify();
                return;
            }
        };
        self.adding = true;
        self.error = None;
        cx.notify();
        let (client, account) = (self.client.clone(), self.account.id);
        let password = values.login.password;
        self._add = cx.spawn_in(window, async move |this, cx| {
            let added = client
                .add_identity(account, new, password, Verify::Login)
                .await;
            this.update(cx, |this, cx| {
                this.adding = false;
                match added {
                    Ok(_) => cx.emit(IdentityFormClosed),
                    Err(e) => this.error = Some(format!("Couldn't sign in: {e}").into()),
                }
                cx.notify();
            })
            .ok();
        });
    }
}

impl Render for AddIdentity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = h_flex()
            .justify_end()
            .gap_2()
            .child(
                // Once the identity is being stored it will be, whatever the form does.
                Button::new("identity-cancel")
                    .label("Cancel")
                    .disabled(self.adding)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(IdentityFormClosed))),
            )
            .child(
                Button::new("identity-add")
                    .primary()
                    .label("Add")
                    .loading(self.adding)
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
            .when_some(self.error.clone(), |this, error| {
                this.child(login_form::error("identity-error", error, cx))
            })
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
