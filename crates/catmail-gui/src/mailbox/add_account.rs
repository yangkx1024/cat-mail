//! The sign-in form: an account's servers and password. The login is tried against the
//! IMAP server before anything is stored, so a typo leaves no account (or keyring entry)
//! behind. Like `catmail account add`, password accounts only.

use catmail_core::{
    Account, AuthKind, MailClient, NewAccount, SecretString, Security, ServerConfig,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, rems,
};

pub enum AddAccountEvent {
    /// The login worked and the account is stored; it has not been synced yet.
    Added(Account),
    Cancelled,
}

/// Security choices, in the order the radio groups show them.
const SECURITY: [(Security, &str); 3] = [
    (Security::Tls, "TLS"),
    (Security::StartTls, "STARTTLS"),
    (Security::Plain, "None"),
];

/// A server's part of the form, as typed.
#[derive(Debug, Clone)]
struct ServerValues {
    host: String,
    port: String,
    security: Security,
}

impl ServerValues {
    /// An empty port means the usual one for the security mode, as in the CLI. The rest
    /// (an empty host, say) is the engine's to reject.
    fn config(&self, default_port: fn(Security) -> u16) -> Result<ServerConfig, &'static str> {
        let port = match self.port.trim() {
            "" => default_port(self.security),
            port => port
                .parse::<u16>()
                .ok()
                .filter(|&p| p != 0)
                .ok_or("A port is a number from 1 to 65535.")?,
        };
        Ok(ServerConfig {
            host: self.host.trim().to_owned(),
            port,
            security: self.security,
        })
    }
}

/// What the form holds, as typed.
#[derive(Debug, Clone)]
struct Values {
    email: String,
    /// Redacted in `Debug` output.
    password: SecretString,
    name: String,
    username: String,
    imap: ServerValues,
    smtp: ServerValues,
}

impl Values {
    /// The account to add. An empty username is the email address, as in the CLI.
    fn new_account(&self) -> Result<NewAccount, &'static str> {
        let email = self.email.trim();
        let username = match self.username.trim() {
            "" => email,
            username => username,
        };
        let name = self.name.trim();
        Ok(NewAccount {
            email: email.to_owned(),
            display_name: (!name.is_empty()).then(|| name.to_owned()),
            username: username.to_owned(),
            imap: self.imap.config(Security::default_imap_port)?,
            smtp: self.smtp.config(Security::default_smtp_port)?,
            auth: AuthKind::Password,
        })
    }
}

/// The fields of one server, IMAP or SMTP.
struct ServerFields {
    /// Prefix of the element ids.
    id: &'static str,
    title: &'static str,
    default_port: fn(Security) -> u16,
    host: Entity<InputState>,
    port: Entity<InputState>,
    security: Security,
}

impl ServerFields {
    fn new(
        id: &'static str,
        title: &'static str,
        example_host: &'static str,
        default_port: fn(Security) -> u16,
        window: &mut Window,
        cx: &mut Context<AddAccount>,
    ) -> Self {
        let security = Security::Tls;
        let host = cx.new(|cx| InputState::new(window, cx).placeholder(example_host));
        let port = cx
            .new(|cx| InputState::new(window, cx).placeholder(default_port(security).to_string()));
        Self {
            id,
            title,
            default_port,
            host,
            port,
            security,
        }
    }

    fn values(&self, cx: &App) -> ServerValues {
        ServerValues {
            host: self.host.read(cx).value().to_string(),
            port: self.port.read(cx).value().to_string(),
            security: self.security,
        }
    }

    /// Also shows the new mode's usual port as the placeholder.
    fn set_security(
        &mut self,
        security: Security,
        window: &mut Window,
        cx: &mut Context<AddAccount>,
    ) {
        self.security = security;
        let port = (self.default_port)(security).to_string();
        self.port
            .update(cx, |input, cx| input.set_placeholder(port, window, cx));
        cx.notify();
    }
}

pub struct AddAccount {
    client: MailClient,
    email: Entity<InputState>,
    password: Entity<InputState>,
    name: Entity<InputState>,
    username: Entity<InputState>,
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
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Optional"));
        let username =
            cx.new(|cx| InputState::new(window, cx).placeholder("Same as the email address"));
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

        // Enter in any field signs in, as in a native login form.
        let subscriptions = [
            &email, &password, &name, &username, &imap.host, &imap.port, &smtp.host, &smtp.port,
        ]
        .into_iter()
        .map(|field| {
            cx.subscribe_in(field, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.sign_in(window, cx);
                }
            })
        })
        .collect();
        email.update(cx, |email, cx| email.focus(window, cx));

        Self {
            client,
            email,
            password,
            name,
            username,
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
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        Values {
            email: text(&self.email),
            password: text(&self.password).into(),
            name: text(&self.name),
            username: text(&self.username),
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
        let password = values.password;
        self._sign_in = cx.spawn_in(window, async move |this, cx| {
            // `check_login` also validates the settings, as `add_account` would.
            let added = async {
                client.check_login(&new, &password).await?;
                client.add_account(new, password).await
            }
            .await;
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

    fn render_server(
        &self,
        server: fn(&mut Self) -> &mut ServerFields,
        fields: &ServerFields,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = fields.id;
        let selected = SECURITY.iter().position(|(s, _)| *s == fields.security);
        v_flex()
            .gap_2()
            .child(div().text_sm().font_semibold().child(fields.title))
            .child(
                Form::new()
                    .columns(3)
                    .child(Field::new().label("Server").col_span(2).child(
                        Input::new(&fields.host).id(SharedString::from(format!("{id}-host"))),
                    ))
                    .child(Field::new().label("Port").child(
                        Input::new(&fields.port).id(SharedString::from(format!("{id}-port"))),
                    ))
                    .child(
                        Field::new()
                            .label("Security")
                            .col_span(3)
                            .when(fields.security == Security::Plain, |field| {
                                field.description("Unencrypted. Only for local test servers.")
                            })
                            .child(
                                RadioGroup::horizontal(SharedString::from(format!(
                                    "{id}-security"
                                )))
                                .children(SECURITY.iter().map(|(_, label)| *label))
                                .selected_index(selected)
                                .on_change(cx.listener(
                                    move |this, ix: &usize, window, cx| {
                                        server(this).set_security(SECURITY[*ix].0, window, cx)
                                    },
                                )),
                            ),
                    ),
            )
    }
}

impl Render for AddAccount {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let account = Form::new()
            .child(
                Field::new()
                    .label("Email")
                    .child(Input::new(&self.email).id("email")),
            )
            .child(
                Field::new()
                    .label("Password")
                    .child(Input::new(&self.password).id("password").mask_toggle()),
            )
            .child(
                Field::new()
                    .label("Name")
                    .description("Shown to the people you write to.")
                    .child(Input::new(&self.name).id("name")),
            )
            .child(
                Field::new()
                    .label("Username")
                    .child(Input::new(&self.username).id("username")),
            );

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
                    .child(account)
                    .child(self.render_server(|this| &mut this.imap, &self.imap, cx))
                    .child(self.render_server(|this| &mut this.smtp, &self.smtp, cx))
                    .when_some(self.error.clone(), |this, error| {
                        this.child(
                            div()
                                .id("sign-in-error")
                                .role(Role::Alert)
                                .test_support()
                                .aria_label(error.clone())
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
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

    fn filled() -> Values {
        Values {
            email: " me@example.com ".into(),
            password: "pw".into(),
            name: String::new(),
            username: String::new(),
            imap: server("imap.example.com", Security::Tls),
            smtp: server("smtp.example.com", Security::StartTls),
        }
    }

    #[test]
    fn debug_output_leaves_out_the_password() {
        let values = Values {
            password: "hunter2".into(),
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
            name: " 张三 ".into(),
            username: "zhang".into(),
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
