//! Settings › Accounts: each account, the addresses it sends as and which is the default,
//! adding and removing them. Their wording is `catmail_frontend::accounts`'.

use std::rc::Rc;

use catmail_core::{AccountId, IdentityId};
use catmail_frontend::accounts;
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button};
use crate::engine::client;
use crate::form::{NameRequest, ask_name, confirmed};
use crate::icons::{Icon, icon};
use crate::login;
use crate::mailbox::sidebar::{Accounts, with_account};
use crate::tasks::in_view;
use crate::toast::report;

use crate::buttons::labelled_button;

use super::{close, help, section_title, toggle};
use crate::theme::SelectableText as _;

pub fn accounts_section() -> impl IntoElement {
    let accounts = move || global::<Accounts>().with(|a| a.0.clone().unwrap_or_default());
    view()
        .gap(px(16.0))
        .child(section_title("Accounts"))
        .child(view().child(each(
            accounts,
            |entry| entry.account.id,
            |entry, _| account_rows(entry.get_untracked()),
        )))
        .child(view().flex_row().child(labelled_button(
            Icon::Plus,
            "Add account",
            true,
            || true,
            || {
                close();
                login::add_account(true);
            },
        )))
}

fn account_rows(entry: catmail_frontend::mailbox::AccountEntry) -> AnyElement {
    let account = &entry.account;
    let account_id = account.id;
    let muted = move || theme().muted_foreground;
    let identities = entry
        .identities
        .iter()
        .map(|identity| {
            let id = identity.id;
            let default = identity.is_default;
            let saves_sent = identity.server_saves_sent(account);
            view()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .py(px(6.0))
                // Under the account's email, past its icon.
                .pl(px(32.0))
                .child(
                    icon(|| Icon::PaperAirplane, 16.0)
                        .mt(px(2.0))
                        .text_color(muted),
                )
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        // As the From line has it: with its name.
                        .child(
                            text(identity.address().to_string())
                                .text_sm()
                                .selectable_text()
                                .text_color(move || theme().foreground),
                        )
                        .child(help(identity.smtp.host.clone()))
                        .child(view().pt(px(4.0)).child(toggle(
                            "Server saves to Sent",
                            move || saves_sent,
                            move |on| set_saves_sent(id, on),
                        ))),
                )
                .child(
                    view()
                        .flex_row()
                        .gap(px(2.0))
                        .child(icon_button(
                            || Icon::PencilSquare,
                            || "Rename".to_owned(),
                            || true,
                            Size::Row,
                            move || rename_identity(account_id, id),
                        ))
                        // Filled for the default, which stays so until another is made it.
                        .child(icon_button(
                            move || if default { Icon::StarSolid } else { Icon::Star },
                            move || if default { "Default" } else { "Make default" }.to_owned(),
                            || true,
                            Size::Row,
                            move || {
                                if !default {
                                    make_default(id);
                                }
                            },
                        ))
                        .children((!default).then(|| {
                            icon_button(
                                || Icon::Trash,
                                || "Remove address".to_owned(),
                                || true,
                                Size::Row,
                                move || remove_identity(account_id, id),
                            )
                            .into_element()
                        })),
                )
                .into_element()
        })
        .collect::<Vec<_>>();
    view()
        .py(px(10.0))
        .border_b_1()
        .border_color(move || theme().border)
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .child(icon(|| Icon::UserCircle, 20.0).text_color(muted))
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        .child(
                            text(account.email.clone())
                                .text_sm()
                                .selectable_text()
                                .text_color(move || theme().foreground),
                        )
                        .child(help(account.imap.host.clone())),
                )
                // The confirmation says what goes, and that the server keeps it all.
                .child(icon_button(
                    || Icon::Trash,
                    || "Remove account".to_owned(),
                    || true,
                    Size::Row,
                    move || remove_account(account_id),
                )),
        )
        .children(identities)
        .child(
            view()
                .flex_row()
                .pl(px(28.0))
                .pt(px(4.0))
                .child(labelled_button(
                    Icon::Plus,
                    "Add sending address",
                    false,
                    || true,
                    move || login::add_identity(account_id),
                )),
        )
        .into_element()
}

fn entry(id: AccountId) -> Option<catmail_frontend::mailbox::AccountEntry> {
    with_account(id, Clone::clone)
}

/// Asks first, then does `go`: what can't be undone.
fn confirm_then(confirm: catmail_frontend::commands::Confirm, go: impl FnOnce() + 'static) {
    let answer = confirmed(confirm);
    in_view(async move {
        if answer.await {
            go();
        }
    });
}

fn remove_account(id: AccountId) {
    let Some(entry) = entry(id) else {
        return;
    };
    confirm_then(accounts::remove_account(&entry), move || {
        in_view(async move {
            if let Err(e) = client().remove_account(id).await {
                report("Couldn't remove the account", e);
            }
        });
    });
}

fn remove_identity(account: AccountId, id: IdentityId) {
    let Some(confirm) = entry(account).and_then(|e| accounts::remove_identity(&e, id)) else {
        return;
    };
    confirm_then(confirm, move || {
        in_view(async move {
            if let Err(e) = client().remove_identity(id).await {
                report("Couldn't remove the address", e);
            }
        });
    });
}

fn make_default(id: IdentityId) {
    in_view(async move {
        if let Err(e) = client().set_default_identity(id).await {
            report("Couldn't make the address the default", e);
        }
    });
}

/// Whether its server saves what it sends to Sent itself, as the user says now: the
/// guess is not made again.
fn set_saves_sent(id: IdentityId, saves: bool) {
    in_view(async move {
        if let Err(e) = client().set_identity_saves_sent(id, Some(saves)).await {
            report("Couldn't change how sent mail is saved", e);
        }
    });
}

fn rename_identity(account: AccountId, id: IdentityId) {
    let Some(entry) = entry(account) else {
        return;
    };
    let Some(identity) = accounts::identity(&entry, id) else {
        return;
    };
    ask_name(NameRequest {
        title: accounts::rename_title(identity),
        // Empty, the address goes alone.
        name: identity.display_name.clone().unwrap_or_default(),
        placeholder: Some("Optional"),
        ok: "Save",
        what: "change the name",
        may_be_empty: true,
        request: Rc::new(move |name| {
            Box::pin(async move { client().set_identity_name(id, Some(&name)).await.map(drop) })
        }),
    });
}
