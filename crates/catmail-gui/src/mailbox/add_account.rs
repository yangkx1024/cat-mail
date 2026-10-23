//! The sign-in form: an account's servers and password. The login is tried against both
//! servers before anything is stored, so a typo leaves no account (or keyring entry)
//! behind, as with `catmail account add`.

use catmail_core::{Account, MailClient, NewAccount, Security, Verify};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::{
    App, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, rems,
};

use crate::login_form::{self, LoginFields, LoginValues, ServerFields, ServerValues, Submit};

pub enum AddAccountEvent {
    /// The login worked and the account is stored; it has not been synced yet.
    Added(Account),
    Cancelled,
}

/// What the form holds, as typed.
#[derive(Debug, Clone)]
struct Values {
    login: LoginValues,
    imap: ServerValues,
    smtp: ServerValues,
}

impl Values {
    /// The account to add.
    fn new_account(&self) -> Result<NewAccount, &'static str> {
        let (email, display_name, username) = self.login.who();
        Ok(NewAccount {
            email,
            display_name,
            username,
            imap: self.imap.config(Security::default_imap_port)?,
            smtp: self.smtp.config(Security::default_smtp_port)?,
        })
    }
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
        let subscriptions = login_form::submit_on_enter(fields, Self::sign_in, window, cx);

        Self {
            client,
            login,
            imap,
            smtp,
            cancellable,
            submit: Submit::default(),
            _subscriptions: subscriptions,
        }
    }

    fn values(&self, cx: &App) -> Values {
        Values {
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
            |account, cx| cx.emit(AddAccountEvent::Added(account)),
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
                                    .child("cat-mail signs in to check these settings before saving them."),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, security: Security) -> ServerValues {
        ServerValues {
            host: host.into(),
            port: String::new(),
            security,
        }
    }

    fn login() -> LoginValues {
        LoginValues {
            email: " me@example.com ".into(),
            password: "pw".into(),
            name: String::new(),
            username: String::new(),
        }
    }

    fn filled() -> Values {
        Values {
            login: login(),
            imap: server("imap.example.com", Security::Tls),
            smtp: server("smtp.example.com", Security::StartTls),
        }
    }

    #[test]
    fn debug_output_leaves_out_the_password() {
        let values = Values {
            login: LoginValues {
                password: "hunter2".into(),
                ..login()
            },
            ..filled()
        };
        let shown = format!("{values:?}");
        assert!(shown.contains("me@example.com"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn empty_fields_take_the_cli_defaults() {
        let new = filled().new_account().unwrap();
        assert_eq!(new.email, "me@example.com");
        assert_eq!(new.username, "me@example.com");
        assert_eq!(new.display_name, None);
        assert_eq!((new.imap.port, new.imap.security), (993, Security::Tls));
        assert_eq!(
            (new.smtp.port, new.smtp.security),
            (587, Security::StartTls)
        );
    }

    #[test]
    fn typed_values_win() {
        let new = Values {
            login: LoginValues {
                name: " 张三 ".into(),
                username: "zhang".into(),
                ..login()
            },
            imap: ServerValues {
                port: "10143".into(),
                ..server("127.0.0.1", Security::Plain)
            },
            ..filled()
        }
        .new_account()
        .unwrap();
        assert_eq!(new.display_name.as_deref(), Some("张三"));
        assert_eq!(new.username, "zhang");
        assert_eq!((new.imap.port, new.imap.security), (10143, Security::Plain));
    }

    #[test]
    fn rejects_ports_that_are_not_ports() {
        for port in ["0", "65536", "x"] {
            let values = Values {
                imap: ServerValues {
                    port: port.into(),
                    ..server("imap.example.com", Security::Tls)
                },
                ..filled()
            };
            assert_eq!(
                values.new_account().unwrap_err(),
                "A port is a number from 1 to 65535."
            );
        }
    }
}
