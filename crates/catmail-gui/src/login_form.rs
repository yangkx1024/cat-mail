//! A login form, shared by the sign-in page (an account) and the form for another sending
//! address (an identity): the fields (who it is, the password, and a server) and the
//! submission.

use std::future::Future;

use catmail_core::{SecretString, Security, ServerConfig};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div, prelude::FluentBuilder as _,
};

/// Security choices, in the order the radio groups show them.
const SECURITY: [(Security, &str); 3] = [
    (Security::Tls, "TLS"),
    (Security::StartTls, "STARTTLS"),
    (Security::Plain, "None"),
];

/// Who signs in, as typed.
#[derive(Debug, Clone)]
pub struct LoginValues {
    pub email: String,
    /// Redacted in `Debug` output.
    pub password: SecretString,
    pub name: String,
    pub username: String,
}

impl LoginValues {
    /// The address, the display name if any, and the username: an empty one is the
    /// address, as in the CLI.
    pub fn who(&self) -> (String, Option<String>, String) {
        let email = self.email.trim();
        let username = match self.username.trim() {
            "" => email,
            username => username,
        };
        let name = self.name.trim();
        (
            email.to_owned(),
            (!name.is_empty()).then(|| name.to_owned()),
            username.to_owned(),
        )
    }
}

/// The fields of who signs in. Their element ids start with a prefix.
pub struct LoginFields {
    email: Entity<InputState>,
    password: Entity<InputState>,
    name: Entity<InputState>,
    username: Entity<InputState>,
    id: &'static str,
}

impl LoginFields {
    /// `name` fills the Name field.
    pub fn new<T>(
        id: &'static str,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Optional")
                .default_value(name.unwrap_or_default())
        });
        let username =
            cx.new(|cx| InputState::new(window, cx).placeholder("Same as the email address"));
        email.update(cx, |email, cx| email.focus(window, cx));
        Self {
            email,
            password,
            name,
            username,
            id,
        }
    }

    pub fn inputs(&self) -> [&Entity<InputState>; 4] {
        [&self.email, &self.password, &self.name, &self.username]
    }

    pub fn values(&self, cx: &App) -> LoginValues {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        LoginValues {
            email: text(&self.email),
            password: text(&self.password).into(),
            name: text(&self.name),
            username: text(&self.username),
        }
    }

    pub fn render(&self) -> Form {
        let id = |field: &str| SharedString::from(format!("{}{field}", self.id));
        Form::new()
            .child(
                Field::new()
                    .label("Email")
                    .child(Input::new(&self.email).id(id("email"))),
            )
            .child(
                Field::new()
                    .label("Password")
                    .child(Input::new(&self.password).id(id("password")).mask_toggle()),
            )
            .child(
                Field::new()
                    .label("Name")
                    .description("Shown to the people you write to.")
                    .child(Input::new(&self.name).id(id("name"))),
            )
            .child(
                Field::new()
                    .label("Username")
                    .child(Input::new(&self.username).id(id("username"))),
            )
    }
}

/// A server's part of the form, as typed.
#[derive(Debug, Clone)]
pub struct ServerValues {
    pub host: String,
    pub port: String,
    pub security: Security,
}

impl ServerValues {
    /// An empty port means the usual one for the security mode, as in the CLI. The rest
    /// (an empty host, say) is the engine's to reject.
    pub fn config(&self, default_port: fn(Security) -> u16) -> Result<ServerConfig, &'static str> {
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

/// The fields of one server, IMAP or SMTP, in the form of a `T`.
pub struct ServerFields {
    /// Prefix of the element ids.
    id: &'static str,
    title: &'static str,
    default_port: fn(Security) -> u16,
    host: Entity<InputState>,
    port: Entity<InputState>,
    security: Security,
}

impl ServerFields {
    pub fn new<T>(
        id: &'static str,
        title: &'static str,
        example_host: &'static str,
        default_port: fn(Security) -> u16,
        window: &mut Window,
        cx: &mut Context<T>,
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

    pub fn inputs(&self) -> [&Entity<InputState>; 2] {
        [&self.host, &self.port]
    }

    pub fn values(&self, cx: &App) -> ServerValues {
        ServerValues {
            host: self.host.read(cx).value().to_string(),
            port: self.port.read(cx).value().to_string(),
            security: self.security,
        }
    }

    /// Also shows the new mode's usual port as the placeholder.
    fn set_security<T: 'static>(
        &mut self,
        security: Security,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        self.security = security;
        let port = (self.default_port)(security).to_string();
        self.port
            .update(cx, |input, cx| input.set_placeholder(port, window, cx));
        cx.notify();
    }

    /// The server's fields; `server` finds them in the form.
    pub fn render<T: 'static>(
        &self,
        server: fn(&mut T) -> &mut ServerFields,
        cx: &mut Context<T>,
    ) -> impl IntoElement + use<T> {
        let id = self.id;
        let selected = SECURITY.iter().position(|(s, _)| *s == self.security);
        v_flex()
            .gap_2()
            .child(div().text_sm().font_semibold().child(self.title))
            .child(
                Form::new()
                    .columns(3)
                    .child(
                        Field::new().label("Server").col_span(2).child(
                            Input::new(&self.host).id(SharedString::from(format!("{id}-host"))),
                        ),
                    )
                    .child(
                        Field::new().label("Port").child(
                            Input::new(&self.port).id(SharedString::from(format!("{id}-port"))),
                        ),
                    )
                    .child(
                        Field::new()
                            .label("Security")
                            .col_span(3)
                            .when(self.security == Security::Plain, |field| {
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

/// Enter in any of the fields submits the form, as in a native login form.
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
    _task: Task<()>,
}

impl Default for Submit {
    fn default() -> Self {
        Self {
            busy: false,
            error: None,
            _task: Task::ready(()),
        }
    }
}

impl Submit {
    /// Runs `request`, unless one is already running, and hands its result to `done`. A
    /// problem with the values (`Err`) or the request's failure shows as the error;
    /// `submit` finds this state in the form.
    pub fn start<T: 'static, R: 'static>(
        &mut self,
        submit: fn(&mut T) -> &mut Submit,
        request: Result<impl Future<Output = catmail_core::Result<R>> + 'static, &'static str>,
        done: impl FnOnce(R, &mut Context<T>) + 'static,
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
            this.update(cx, |this, cx| {
                submit(this).busy = false;
                match result {
                    Ok(value) => done(value, cx),
                    Err(e) => submit(this).error = Some(format!("Couldn't sign in: {e}").into()),
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

    /// The buttons under the form: Cancel, if it has one, and the one that submits it.
    /// Cancel is off while a submission runs: the engine stores the account or identity
    /// whatever the window does (its blocking task isn't cancelled with the form's), so
    /// leaving would only skip what follows.
    pub fn actions(&self, cancel: Option<Button>, submit: Button) -> impl IntoElement + use<> {
        h_flex()
            .justify_end()
            .gap_2()
            .children(cancel.map(|cancel| cancel.disabled(self.busy)))
            .child(submit.primary().loading(self.busy))
    }
}
