//! The form for another address an account sends as, through an SMTP server of its own
//! (a sending service, say), in a dialog over Settings. The login is tried before
//! anything is stored, as for an account.

use catmail_core::{AccountId, MailClient, NewIdentity, Security, Verify};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::component::v_flex;
use gpui_kit::{
    App, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, rems,
};

use crate::actions::DIALOG_BUTTON;
use crate::login_form::{LoginFields, LoginValues, ServerFields, ServerValues, Submit};

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

/// Opens the form for another address of `account`, whose own address is `email`, over
/// the Settings dialog. Being a dialog of its own, Enter (its Confirm) adds the address,
/// and Escape or Cancel close only the form.
pub(super) fn open(
    client: MailClient,
    account: AccountId,
    email: &str,
    display_name: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| AddIdentity::new(client, account, display_name, window, cx));
    let title = SharedString::from(format!("New address for {email}"));
    let width = rems(32.).to_pixels(window.rem_size());
    let dialog_form = form.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let form = &dialog_form;
        let submit = &form.read(cx).submit;
        // The buttons act through the dialog's own Cancel and Confirm, as Escape and Enter
        // do. On the buttons themselves, not a wrapper: focused, they take Enter and Space.
        let cancel_button = submit.cancel_button(
            Button::new("identity-cancel")
                .label("Cancel")
                .key_context(DIALOG_BUTTON)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        );
        let add_button = submit.submit_button(
            Button::new("identity-add")
                .label("Add")
                .key_context(DIALOG_BUTTON)
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
                }),
        );
        let (add, cancel) = (form.clone(), form.clone());
        dialog
            .title(title.clone())
            .w(width)
            .centered()
            // The form closes the dialog once the address is added.
            .on_ok(move |_, window, cx| {
                add.update(cx, |form, cx| form.add(window, cx));
                false
            })
            .on_cancel(move |_, _, cx| !cancel.read(cx).submit.busy())
            .child(form.clone())
            .footer(DialogFooter::new().child(cancel_button).child(add_button))
    });
    // Only once the dialog is open: it gives focus back to what had it before (Settings)
    // when it closes.
    form.update(cx, |form, cx| form.login.focus(window, cx));
}

struct AddIdentity {
    client: MailClient,
    account: AccountId,
    login: LoginFields,
    smtp: ServerFields,
    submit: Submit,
}

impl AddIdentity {
    fn new(
        client: MailClient,
        account: AccountId,
        display_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let login = LoginFields::new("identity-", display_name, window, cx);
        let smtp = ServerFields::new(
            "identity-smtp",
            "Outgoing mail (SMTP)",
            "smtp.example.com",
            Security::default_smtp_port,
            window,
            cx,
        );
        Self {
            client,
            account,
            login,
            smtp,
            submit: Submit::default(),
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
        let (client, account) = (self.client.clone(), self.account);
        let request = values.new_identity().map(|new| async move {
            client
                .add_identity(account, new, values.login.password, Verify::Login)
                .await
        });
        // Listed with the account once the mailbox reads it again. Nothing can open over
        // this dialog meanwhile, so it is the one closed.
        self.submit.start(
            |this: &mut Self| &mut this.submit,
            request,
            |_, window, cx| window.close_dialog(cx),
            window,
            cx,
        );
    }
}

impl Render for AddIdentity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("add-identity")
            .gap_5()
            .child(self.login.render())
            .child(self.smtp.render(|this: &mut Self| &mut this.smtp, cx))
            .children(self.submit.error("identity-error", cx))
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
