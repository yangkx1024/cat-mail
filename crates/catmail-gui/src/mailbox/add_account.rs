//! The sign-in form: an account's servers and password. The login is tried against both
//! servers before anything is stored, so a typo leaves no account (or keyring entry)
//! behind, as with `catmail account add`.

use catmail_core::{Account, MailClient, NewAccount, Security};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, rems,
};

use crate::login_form::{self, LoginFields, LoginValues, ServerFields, ServerValues};

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
    signing_in: bool,
    error: Option<SharedString>,
    _sign_in: Task<()>,
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
            signing_in: false,
            error: None,
            _sign_in: Task::ready(()),
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
        if self.signing_in {
            return;
        }
        let values = self.values(cx);
        let new = match values.new_account() {
            Ok(new) => new,
            Err(problem) => {
                self.error = Some(problem.into());
                cx.notify();
                return;
            }
        };
        self.signing_in = true;
        self.error = None;
        cx.notify();
        let client = self.client.clone();
        let password = values.login.password;
        self._sign_in = cx.spawn_in(window, async move |this, cx| {
            let added = client.add_account_checked(new, password).await;
            this.update(cx, |this, cx| {
                this.signing_in = false;
                match added {
                    Ok(account) => cx.emit(AddAccountEvent::Added(account)),
                    Err(e) => this.error = Some(format!("Couldn't sign in: {e}").into()),
                }
                cx.notify();
            })
            .ok();
        });
    }
}

impl Render for AddAccount {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = h_flex()
            .justify_end()
            .gap_2()
            .when(self.cancellable, |this| {
                this.child(
                    // Once the account is being stored it will be, whatever the window does.
                    Button::new("cancel")
                        .label("Cancel")
                        .disabled(self.signing_in)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(AddAccountEvent::Cancelled))),
                )
            })
            .child(
                Button::new("sign-in")
                    .primary()
                    .label("Sign in")
                    .loading(self.signing_in)
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
                    .when_some(self.error.clone(), |this, error| {
                        this.child(login_form::error("sign-in-error", error, cx))
                    })
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
