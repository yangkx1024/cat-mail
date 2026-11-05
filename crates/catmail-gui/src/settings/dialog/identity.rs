//! The form for another address an account sends as, through an SMTP server of its own
//! (a sending service, say), in a dialog over Settings. The login is tried before
//! anything is stored, as for an account.

use catmail_core::{AccountId, MailClient, Security, Verify};
use catmail_frontend::login::IdentityValues;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::v_flex;
use gpui_kit::{
    App, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window,
};

use crate::login_form::{LoginFields, ServerFields};
use crate::submission::{self, DialogForm, Submit};

/// Opens the form for another address of `account`, titled `title`, over the Settings
/// dialog. Being a dialog of its own, Enter (its Confirm) adds the address,
/// and Escape or Cancel close only the form.
pub(super) fn open(
    client: MailClient,
    account: AccountId,
    title: String,
    display_name: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| AddIdentity::new(client, account, display_name, window, cx));
    let title = SharedString::from(title);
    let ids = ("identity-cancel", "identity-add");
    submission::open_dialog(form.clone(), title, 32., ids, "Add", window, cx);
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
            submit: Submit::new("sign in"),
        }
    }

    fn values(&self, cx: &App) -> IdentityValues {
        IdentityValues {
            login: self.login.values(cx),
            smtp: self.smtp.values(cx),
        }
    }
}

impl DialogForm for AddIdentity {
    fn submit(&self) -> &Submit {
        &self.submit
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
