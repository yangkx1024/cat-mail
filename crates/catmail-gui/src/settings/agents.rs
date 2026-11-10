//! Settings › Agents: the MCP server AI agents connect to (on this machine's loopback
//! address; from other devices through a tunnel such as `tailscale serve`), the tokens
//! they present, each with what it may do and with which accounts, and the commands that
//! add it to Claude Code and Codex. The server
//! is `shell`'s, what this says `catmail_frontend::mcp`'s, the commands `catmail_mcp`'s
//! (`catmail mcp token add` prints them too).

use std::time::Duration;

use catmail_frontend::format;
use catmail_frontend::login::{Submission, parse_port};
use catmail_frontend::mcp::connect::{self, Example};
use catmail_frontend::mcp::{self, Access, NewToken, Status, Tier, Token, Tokens};
use futures_util::StreamExt as _;
use nsh_base::theme;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, labelled_button};
use crate::clock::clock;
use crate::form::{confirm_then, error, menu_field, submit, text_input};
use crate::icons::{Icon, icon};
use crate::mailbox::sidebar::with_accounts;
use crate::shell;
use crate::tasks::{in_view, off_thread};
use crate::theme::SelectableText as _;
use crate::toast::report;

use super::{help, prefs, section_title, toggle};

/// A token just made, its name and what it is: shown this once.
type Made = Signal<Option<(String, String)>>;

pub fn agents() -> impl IntoElement {
    let made: Made = signal(None);
    view()
        .gap(px(16.0))
        .child(section_title("Agents"))
        .child(server())
        .child(part("Tokens").child(match shell::tokens() {
            Some(tokens) => tokens_list(tokens, made).into_element(),
            None => help(mcp::NO_CONFIG_DIR).into_element(),
        }))
        .child(part("Connect an agent").child(connecting(made)))
}

/// A part of the section, with its heading: set apart by the space above it, more than
/// what is inside a part has between its rows.
fn part(heading: &'static str) -> View {
    view().gap(px(12.0)).pt(px(8.0)).child(
        text(heading)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(move || theme().foreground),
    )
}

// --- The server ---------------------------------------------------------------------

/// The switch, where the server is, and what it listens with.
fn server() -> impl IntoElement {
    let (now, mut changes) = shell::follow_mcp();
    let status = signal(now);
    spawn(async move {
        while let Some(now) = changes.next().await {
            status.set(now);
        }
    });
    let enabled = move || prefs(|s| s.mcp.enabled);
    view()
        .gap(px(16.0))
        .child(
            view()
                .gap(px(6.0))
                .child(toggle("Let AI agents connect over MCP", enabled, |on| {
                    shell::update_settings(|s| s.mcp.enabled = on)
                }))
                .child(when(enabled, move || status_line(status))),
        )
        .child(port_field())
}

/// Under the switch while it is on: where agents connect, or why they can't.
fn status_line(status: Signal<Status>) -> impl IntoElement {
    text(move || status.with(Status::line))
        .text_xs()
        .selectable_text()
        // Lined up with the switch's label.
        .pl(px(46.0))
        .text_color(move || {
            if status.with(|s| matches!(s, Status::Failed(_))) {
                theme().destructive
            } else {
                theme().muted_foreground
            }
        })
}

/// The port, taken once typed (Return, or leaving the field): each keystroke would
/// move the server.
fn port_field() -> impl IntoElement {
    let typed = signal(prefs(|s| s.mcp.port).to_string());
    let error = signal(None::<&'static str>);
    let take = move || match parse_port(&typed.get_untracked()) {
        Ok(port) => {
            error.set(None);
            shell::update_settings(|s| s.mcp.port = port);
        }
        Err(why) => error.set(Some(why)),
    };
    // The label before the field, on one line: a port is short.
    view()
        .gap(px(6.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child(
                    text("Port")
                        .text_sm()
                        .text_color(move || theme().foreground),
                )
                .child(
                    text_input()
                        .w(px(120.0))
                        .label("Port")
                        .default_value(typed.get_untracked())
                        .on_change(move |text: &str| typed.set(text.to_owned()))
                        .on_submit(move |_: &str| take())
                        .on_blur(take),
                ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .text_xs()
                    .text_color(move || theme().destructive)
            },
        ))
}

// --- Tokens -------------------------------------------------------------------------

/// The tokens, one per agent, the one just made, and making another.
fn tokens_list(tokens: Tokens, made: Made) -> impl IntoElement {
    let list = signal(Vec::<Token>::new());
    // Read when the section shows (`catmail mcp token` changes them too), and again
    // each time one is added or removed here. The reading is the section's: it stops
    // when the section goes, with the list it fills.
    let reads = signal(0_u32);
    {
        let tokens = tokens.clone();
        effect(move || {
            reads.get();
            let tokens = tokens.clone();
            spawn(async move {
                match off_thread(move || tokens.list()).await {
                    Some(Ok(read)) => list.set(read),
                    Some(Err(e)) => report("Couldn't read the tokens", e),
                    None => {}
                }
            });
        });
    }
    let name = signal(String::new());
    // What the next token may do: reading only, with every account, unless chosen
    // otherwise (one account at most, here).
    let access = signal(Access::default());
    let submission = signal(Submission::new("add the token"));
    let busy = move || submission.with(Submission::busy);
    let add = {
        let tokens = tokens.clone();
        move || {
            let wanted = name.get_untracked();
            if wanted.trim().is_empty() {
                return;
            }
            let may = access.get_untracked();
            let tokens = tokens.clone();
            let request = async move {
                off_thread(move || tokens.add(&wanted, may).map_err(|e| e.to_string()))
                    .await
                    .unwrap_or_else(|| Err("its thread stopped".to_owned()))
            };
            submit(
                submission,
                Ok(Box::pin(request)),
                move |NewToken { token, secret }| {
                    name.set(String::new());
                    access.set(Access::default());
                    made.set(Some((token.name, secret)));
                    reads.update(|n| *n += 1);
                },
            );
        }
    };
    let add_on_enter = add.clone();
    let rows = each(
        move || list.get(),
        |token| token.name.clone(),
        move |token, _| token_row(token.get_untracked(), tokens.clone(), made, reads),
    );
    view()
        .gap(px(12.0))
        .child(view().child(rows))
        .child(when(move || made.get().is_some(), move || made_token(made)))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    text_input()
                        .flex_1()
                        .placeholder("Name, such as the agent's device")
                        .value(move || name.get())
                        .on_change(move |text: &str| name.set(text.to_owned()))
                        .on_submit(move |_: &str| add_on_enter()),
                )
                .child(labelled_button(
                    Icon::Plus,
                    "Add token",
                    true,
                    move || !busy() && !name.with(|n| n.trim().is_empty()),
                    add,
                )),
        )
        // On a row of their own, halves wide enough for the longest choice on one line.
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    menu_field(
                        "What the agent may do",
                        move || mcp::tier_label(access.with(|a| a.tier)).to_owned(),
                        move || choose_tier(access),
                    )
                    .flex_1(),
                )
                .child(
                    menu_field(
                        "Accounts",
                        move || access.with(|a| mcp::accounts_label(a.accounts.as_deref())),
                        move || choose_account(access),
                    )
                    .flex_1(),
                ),
        )
        .child(error(submission))
}

fn choose_tier(access: Signal<Access>) {
    let current = access.with_untracked(|a| a.tier);
    let items = Tier::ALL
        .into_iter()
        .map(|t| {
            MenuItem::new(mcp::tier_label(t))
                .checked(t == current)
                .on_select(move || access.update(|a| a.tier = t))
        })
        .collect();
    drop(popup_menu(items));
}

/// Every account, or one of them; more than one is `catmail mcp token add`'s.
fn choose_account(access: Signal<Access>) {
    let current = access.with_untracked(|a| a.accounts.clone());
    let emails: Vec<String> =
        with_accounts(|list| list.iter().map(|e| e.account.email.clone()).collect());
    let all = MenuItem::new(mcp::accounts_label(None))
        .checked(current.is_none())
        .on_select(move || access.update(|a| a.accounts = None));
    let items = std::iter::once(all)
        .chain(emails.into_iter().map(|email| {
            let chosen = current.as_deref() == Some(std::slice::from_ref(&email));
            MenuItem::new(email.clone())
                .checked(chosen)
                .on_select(move || access.update(|a| a.accounts = Some(vec![email.clone()])))
        }))
        .collect();
    drop(popup_menu(items));
}

fn token_row(token: Token, tokens: Tokens, made: Made, reads: Signal<u32>) -> AnyElement {
    let name = token.name.clone();
    let remove = move || {
        let (name, tokens) = (name.clone(), tokens.clone());
        confirm_then(mcp::remove_token(&name), move || {
            // Done even if the section goes meanwhile (the file's lock can keep it
            // waiting); only what it shows goes with it.
            in_view(async move {
                let gone = name.clone();
                if let Some(Err(e)) = off_thread(move || tokens.remove(&gone)).await {
                    report("Couldn't remove the token", e);
                    return;
                }
                if reads.try_get().is_none() {
                    return;
                }
                // Shown just now, it can't be used anymore.
                if made.with_untracked(|m| m.as_ref().is_some_and(|(n, _)| *n == name)) {
                    made.set(None);
                }
                reads.update(|n| *n += 1);
            });
        });
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .py(px(6.0))
        .child(icon(|| Icon::Key, 16.0).text_color(move || theme().muted_foreground))
        .child(
            view()
                .flex_1()
                .gap(px(2.0))
                .child(
                    text(token.name.clone())
                        .text_sm()
                        .selectable_text()
                        .text_color(move || theme().foreground),
                )
                .child(help(move || {
                    mcp::token_line(
                        &token.access,
                        &format::date(Some(token.created_at), &clock()),
                    )
                })),
        )
        .child(icon_button(
            || Icon::Trash,
            || "Remove token".to_owned(),
            || true,
            Size::Row,
            remove,
        ))
        .into_element()
}

/// The token just made: shown this once, to copy into the agent's settings.
fn made_token(made: Made) -> impl IntoElement {
    let secret = move || made.with(|m| m.as_ref().map(|(_, s)| s.clone()).unwrap_or_default());
    view()
        .gap(px(6.0))
        .p(px(12.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(move || theme().border)
        .bg(move || theme().muted)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    text(secret)
                        .flex_1()
                        .text_sm()
                        .font_family("monospace")
                        .selectable_text()
                        .text_color(move || theme().foreground),
                )
                .child(copy_button(move || untracked(secret))),
        )
        .child(help(
            "Agents send it as Authorization: Bearer <token>. It isn't shown again.",
        ))
}

/// Copies what `text_` gives, and shows it did: a check, for a moment.
fn copy_button(text_: impl Fn() -> String + 'static) -> impl IntoElement {
    let copied = signal(false);
    // Each press restarts the moment: only the last press's timer ends it.
    let presses = signal(0_u32);
    icon_button(
        move || {
            if copied.get() {
                Icon::Check
            } else {
                Icon::ClipboardDocument
            }
        },
        move || if copied.get() { "Copied" } else { "Copy" }.to_owned(),
        || true,
        Size::Row,
        move || {
            set_clipboard(text_());
            copied.set(true);
            presses.update(|n| *n += 1);
            let this = presses.get_untracked();
            spawn(async move {
                timer(COPIED_SHOWN).await;
                if presses.try_get() == Some(this) {
                    copied.try_set(false);
                }
            });
        },
    )
}

/// How long the copy button shows its check.
const COPIED_SHOWN: Duration = Duration::from_millis(1500);

// --- Connecting ---------------------------------------------------------------------

/// The commands that add the server to Claude Code and Codex: with the token just made,
/// ready to paste, or with a placeholder for one.
fn connecting(made: Made) -> impl IntoElement {
    // Built again only when what they show changes, not with every other setting.
    let port = computed(move || prefs(|s| s.mcp.port));
    let secret = computed(move || made.with(|m| m.as_ref().map(|(_, secret)| secret.clone())));
    dynamic(move || {
        let url = connect::local_url(port.get());
        let secret = secret.get();
        let token = secret.as_deref().unwrap_or(connect::TOKEN_PLACEHOLDER);
        view()
            .gap(px(16.0))
            .children(connect::examples(&url, token).map(|e| example(e).into_element()))
            .children(
                secret
                    .is_none()
                    .then(|| help("Adding a token fills in <token>.").into_element()),
            )
            .child(help(connect::REMOTE_NOTE))
            .into_element()
    })
}

fn example(example: Example) -> impl IntoElement {
    let commands = example.commands.clone();
    view()
        .gap(px(6.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .child(
                    text(example.agent)
                        .flex_1()
                        .text_sm()
                        .text_color(move || theme().foreground),
                )
                .child(copy_button(move || commands.clone())),
        )
        .child(
            view()
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(move || theme().muted)
                .child(
                    text(example.commands)
                        .text_xs()
                        .font_family("monospace")
                        .selectable_text()
                        .text_color(move || theme().foreground),
                ),
        )
        .children(example.note.map(|note| help(note).into_element()))
}
