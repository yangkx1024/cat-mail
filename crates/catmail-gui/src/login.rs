//! The login forms: the sign-in page, which takes the window's place while there is no
//! account (and from Settings › Accounts' "Add account"), and the form for another address
//! an account sends as, in a dialog over Settings. Both try the login before anything is
//! stored; their fields and how they read are `catmail_frontend::login`'s.

use std::rc::Rc;

use catmail_core::{AccountId, Security, Verify};
use catmail_frontend::accounts;
use catmail_frontend::login::{
    AccountValues, IdentityValues, LoginValues, NAME_HELP, PLAIN_WARNING, SECURITY, ServerValues,
    Submission,
};
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::engine::client;
use crate::form::{self, field, modal, text_input};
use crate::mailbox::sidebar::with_account;

/// The fields of who signs in.
#[derive(Clone, Copy)]
struct LoginFields {
    email: Signal<String>,
    password: Signal<String>,
    name: Signal<String>,
    username: Signal<String>,
}

impl LoginFields {
    /// `name` fills the Name field.
    fn new(name: Option<String>) -> Self {
        Self {
            email: signal(String::new()),
            password: signal(String::new()),
            name: signal(name.unwrap_or_default()),
            username: signal(String::new()),
        }
    }

    fn values(self) -> LoginValues {
        LoginValues {
            email: self.email.get_untracked(),
            password: self.password.get_untracked().into(),
            name: self.name.get_untracked(),
            username: self.username.get_untracked(),
        }
    }

    /// The fields; Enter in any of them submits the form.
    fn view(self, submit: Rc<dyn Fn()>) -> View {
        let input = move |value: Signal<String>| {
            let submit = submit.clone();
            text_input()
                .default_value(value.get_untracked())
                .on_change(move |text: &str| value.set(text.to_owned()))
                .on_submit(move |_: &str| submit())
        };
        view()
            .gap(px(14.0))
            .child(field(
                "Email",
                None,
                input(self.email)
                    .placeholder("you@example.com".to_owned())
                    .auto_focus(true),
            ))
            .child(field("Password", None, input(self.password).masked(true)))
            .child(field(
                "Name",
                Some(NAME_HELP),
                input(self.name).placeholder("Optional".to_owned()),
            ))
            .child(field(
                "Username",
                None,
                input(self.username).placeholder("Same as the email address".to_owned()),
            ))
    }
}

/// The fields of one server, IMAP or SMTP.
#[derive(Clone, Copy)]
struct ServerFields {
    title: &'static str,
    example_host: &'static str,
    default_port: fn(Security) -> u16,
    host: Signal<String>,
    port: Signal<String>,
    security: Signal<Security>,
}

impl ServerFields {
    fn new(
        title: &'static str,
        example_host: &'static str,
        default_port: fn(Security) -> u16,
    ) -> Self {
        Self {
            title,
            example_host,
            default_port,
            host: signal(String::new()),
            port: signal(String::new()),
            security: signal(Security::Tls),
        }
    }

    fn values(self) -> ServerValues {
        ServerValues {
            host: self.host.get_untracked(),
            port: self.port.get_untracked(),
            security: self.security.get_untracked(),
        }
    }

    fn view(self, submit: Rc<dyn Fn()>) -> View {
        let input = move |value: Signal<String>| {
            let submit = submit.clone();
            text_input()
                .on_change(move |text: &str| value.set(text.to_owned()))
                .on_submit(move |_: &str| submit())
        };
        let Self {
            default_port,
            security,
            ..
        } = self;
        view()
            .gap(px(10.0))
            .child(
                text(self.title)
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(move || theme().foreground),
            )
            .child(
                view()
                    .flex_row()
                    .gap(px(12.0))
                    .child(
                        field(
                            "Server",
                            None,
                            input(self.host).placeholder(self.example_host.to_owned()),
                        )
                        .flex_grow(2.0),
                    )
                    .child(
                        field(
                            "Port",
                            None,
                            // The new mode's usual port, when it changes.
                            input(self.port)
                                .placeholder(move || default_port(security.get()).to_string()),
                        )
                        .flex_grow(1.0),
                    ),
            )
            .child(
                view()
                    .gap(px(6.0))
                    .child(
                        text("Security")
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(move || theme().foreground),
                    )
                    .child(
                        view()
                            .flex_row()
                            .gap(px(16.0))
                            .children(SECURITY.map(|(mode, label)| radio(security, mode, label))),
                    )
                    .child(when(
                        move || security.get() == Security::Plain,
                        || {
                            text(PLAIN_WARNING)
                                .text_xs()
                                .text_color(move || theme().muted_foreground)
                        },
                    )),
            )
    }
}

/// One of a choice's options: a dot, filled when it is the one chosen, and its label.
fn radio(chosen: Signal<Security>, mode: Security, label: &'static str) -> AnyElement {
    let on = move || chosen.get() == mode;
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .role(AccessibilityRole::Radio)
        .label(label)
        .on_press(move || chosen.set(mode))
        .child(
            view()
                .w(px(16.0))
                .h(px(16.0))
                .rounded(px(8.0))
                .border_1()
                .items_center()
                .justify_center()
                .border_color(move || if on() { theme().primary } else { theme().input })
                .child(when(on, || {
                    view()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded(px(4.0))
                        .bg(move || theme().primary)
                })),
        )
        .child(text(label).text_sm().text_color(move || theme().foreground))
        .into_element()
}

// --- The sign-in page ---------------------------------------------------------------

/// The sign-in page in the window's place: `Some(cancellable)`, false for the first
/// account (there is nothing to go back to).
#[derive(Clone, Copy, Default, PartialEq)]
pub struct SignIn(pub Option<bool>);

pub fn add_account(cancellable: bool) {
    global::<SignIn>().set(SignIn(Some(cancellable)));
}

pub fn sign_in_page(cancellable: bool) -> impl IntoElement {
    let login = LoginFields::new(None);
    let imap = ServerFields::new(
        "Incoming mail (IMAP)",
        "imap.example.com",
        Security::default_imap_port,
    );
    let smtp = ServerFields::new(
        "Outgoing mail (SMTP)",
        "smtp.example.com",
        Security::default_smtp_port,
    );
    let submission = signal(Submission::new("sign in"));
    let busy = move || submission.with(Submission::busy);
    let sign_in: Rc<dyn Fn()> = Rc::new(move || {
        let values = AccountValues {
            login: login.values(),
            imap: imap.values(),
            smtp: smtp.values(),
        };
        let request = values.new_account().map(|new| {
            let password = values.login.password;
            Box::pin(async move { client().add_account(new, password, Verify::Login).await })
                as futures_util::future::LocalBoxFuture<'static, _>
        });
        // Background sync syncs it: its folders and messages appear through the engine's
        // events, and how that goes in its status.
        form::submit(submission, request, |_| {
            global::<SignIn>().set(SignIn(None))
        });
    });
    let cancel: Option<Rc<dyn Fn()>> =
        cancellable.then(|| Rc::new(|| global::<SignIn>().set(SignIn(None))) as Rc<dyn Fn()>);
    let submit = sign_in.clone();
    scroll().flex_1().bg(move || theme().background).child(
        view().items_center().child(
            view()
                .w_full()
                .max_w(px(576.0))
                .p(px(24.0))
                .gap(px(24.0))
                .child(
                    view()
                        .gap(px(4.0))
                        .child(
                            text("Add an account")
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(move || theme().foreground),
                        )
                        .child(
                            text("catmail signs in to check these settings before saving them.")
                                .text_sm()
                                .text_color(move || theme().muted_foreground),
                        ),
                )
                .child(login.view(sign_in.clone()))
                .child(imap.view(sign_in.clone()))
                .child(smtp.view(sign_in.clone()))
                .child(form::error(submission))
                .child(form::actions(cancel, "Sign in", busy, move || submit())),
        ),
    )
}

// --- Another sending address --------------------------------------------------------

/// The form for another address of the account, while it is open.
#[derive(Clone, Copy, Default, PartialEq)]
struct AddIdentity(Option<AccountId>);

pub fn add_identity(account: AccountId) {
    global::<AddIdentity>().set(AddIdentity(Some(account)));
}

fn close_identity_form() {
    global::<AddIdentity>().set(AddIdentity(None));
}

/// The form, over Settings, while it is open. The login is tried before the address is
/// stored, as for an account; once it is, the form closes and the address is listed with
/// the account when the sidebar reads it again.
pub fn identity_dialog() -> impl IntoElement {
    dynamic(move || {
        let Some(account) = global::<AddIdentity>().get().0 else {
            return view().into_element();
        };
        let entry = with_account(account, Clone::clone);
        match entry {
            Some(entry) => identity_form(
                account,
                accounts::new_identity_title(&entry),
                accounts::new_identity_name(&entry),
            )
            .into_element(),
            None => view().into_element(),
        }
    })
}

fn identity_form(account: AccountId, title: String, name: Option<String>) -> impl IntoElement {
    let login = LoginFields::new(name);
    let smtp = ServerFields::new(
        "Outgoing mail (SMTP)",
        "smtp.example.com",
        Security::default_smtp_port,
    );
    let submission = signal(Submission::new("sign in"));
    let busy = move || submission.with(Submission::busy);
    let add: Rc<dyn Fn()> = Rc::new(move || {
        let values = IdentityValues {
            login: login.values(),
            smtp: smtp.values(),
        };
        let request = values.new_identity().map(|new| {
            let password = values.login.password;
            Box::pin(async move {
                client()
                    .add_identity(account, new, password, Verify::Login)
                    .await
            }) as futures_util::future::LocalBoxFuture<'static, _>
        });
        form::submit(submission, request, |_| close_identity_form());
    });
    let cancel = move || {
        if !untracked(busy) {
            close_identity_form();
        }
    };
    let submit = add.clone();
    modal(
        512.0,
        cancel,
        view()
            .gap(px(20.0))
            .child(form::title(title))
            .child(
                scroll().max_h(px(520.0)).child(
                    view()
                        .gap(px(20.0))
                        .child(login.view(add.clone()))
                        .child(smtp.view(add.clone())),
                ),
            )
            .child(form::error(submission))
            .child(form::actions(
                Some(Rc::new(cancel)),
                "Add",
                busy,
                move || submit(),
            )),
    )
}
