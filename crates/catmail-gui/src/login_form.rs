//! A login form, shared by the sign-in page (an account) and the form for another sending
//! address (an identity): the fields (who it is, the password, and a server).

use catmail_core::Security;
pub use catmail_frontend::login::{LoginValues, ServerValues};
use catmail_frontend::login::{NAME_HELP, PLAIN_WARNING, SECURITY};
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{StyledExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};

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

    /// Puts the cursor in the Email field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.email.update(cx, |email, cx| email.focus(window, cx));
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
                    .description(NAME_HELP)
                    .child(Input::new(&self.name).id(id("name"))),
            )
            .child(
                Field::new()
                    .label("Username")
                    .child(Input::new(&self.username).id(id("username"))),
            )
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
                                field.description(PLAIN_WARNING)
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
