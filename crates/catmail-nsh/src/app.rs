//! The window: folders, the folder's messages, and the message open in a web view.
//!
//! State is nsh globals (one signal per type, per surface). The client's futures are
//! awaited in nsh tasks on the UI thread; the engine does the work on its own runtime.

use std::{collections::HashMap, sync::OnceLock, time::Duration};

use catmail_core::{
    FolderId, FolderRole, MailClient, MessageId, MessageSummary, RecvError,
    html::{DarkPalette, InlineImage, recolor_html},
};
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::page::{self, Colors, FromPage};

static CLIENT: OnceLock<MailClient> = OnceLock::new();

pub fn set_client(client: MailClient) {
    if CLIENT.set(client).is_err() {
        panic!("the client is set once");
    }
}

fn client() -> &'static MailClient {
    CLIENT
        .get()
        .expect("main sets the client before the app starts")
}

/// Messages fetched at a time.
const PAGE: u32 = 200;

#[derive(Clone, PartialEq)]
enum SidebarRow {
    Account(String),
    Folder {
        id: FolderId,
        label: String,
        depth: usize,
        unread: u32,
    },
}

#[derive(Clone, Default)]
struct Sidebar(Vec<SidebarRow>);

#[derive(Clone, Default)]
struct SelectedFolder(Option<FolderId>);

#[derive(Clone, Default)]
struct SelectedMessage(Option<MessageId>);

#[derive(Clone, Default)]
struct Messages {
    list: Vec<MessageSummary>,
    /// The last page was full: there may be more.
    more: bool,
    loading: bool,
}

#[derive(Clone, Default)]
struct Reading {
    subject: String,
    from: String,
    date: String,
    page: String,
}

/// The body's height in points, as the page reports it.
#[derive(Clone, Default)]
struct BodyHeight(f32);

/// The last error, shown at the bottom of the sidebar.
#[derive(Clone, Default)]
struct Status(Option<String>);

fn report(what: &str, error: catmail_core::Error) {
    tracing::warn!(error = format!("{error:#}"), "{what}");
    global::<Status>().set(Status(Some(format!("{what}: {error}"))));
}

#[component]
pub fn App() -> impl IntoElement {
    spawn(load_sidebar());
    spawn(watch_events());
    // The tasks belong to the effects' runs: picking another folder or message cancels
    // what is still loading for the last one.
    effect(move || {
        if let Some(folder) = global::<SelectedFolder>().with(|s| s.0) {
            spawn(load_messages(folder));
        }
    });
    effect(move || {
        // Read here so the page is made again when the appearance changes.
        let dark = dark();
        match global::<SelectedMessage>().with(|s| s.0) {
            Some(id) => {
                spawn(open_message(id, dark));
            }
            None => global::<Reading>().set(Reading::default()),
        }
    });
    view()
        .flex_1()
        .flex_row()
        .bg(move || theme().background)
        .child(sidebar())
        .child(message_list())
        .child(reader())
}

// --- Loading ------------------------------------------------------------------------

async fn load_sidebar() {
    let accounts = match client().list_accounts().await {
        Ok(accounts) => accounts,
        Err(e) => return report("couldn't list accounts", e),
    };
    let mut rows = Vec::new();
    let mut inbox = None;
    for account in accounts {
        rows.push(SidebarRow::Account(account.email.clone()));
        let folders = match client().list_folders(account.id).await {
            Ok(folders) => folders,
            Err(e) => {
                report("couldn't list folders", e);
                continue;
            }
        };
        for folder in folders.into_iter().filter(|f| f.selectable) {
            if inbox.is_none() && folder.role == Some(FolderRole::Inbox) {
                inbox = Some(folder.id);
            }
            let (depth, label) = match folder.delimiter {
                Some(d) => (
                    folder.name.matches(d).count(),
                    folder
                        .name
                        .rsplit(d)
                        .next()
                        .unwrap_or(&folder.name)
                        .to_owned(),
                ),
                None => (0, folder.name.clone()),
            };
            rows.push(SidebarRow::Folder {
                id: folder.id,
                label,
                depth,
                unread: folder.unread_count,
            });
        }
    }
    global::<Sidebar>().set(Sidebar(rows));
    let selected = global::<SelectedFolder>();
    if selected.with_untracked(|s| s.0.is_none()) {
        selected.set(SelectedFolder(inbox));
    }
}

async fn load_messages(folder: FolderId) {
    global::<Messages>().set(Messages {
        loading: true,
        ..Messages::default()
    });
    match client().list_messages(folder, PAGE, None).await {
        Ok(list) => {
            // For looking at the reader without a mouse (screenshots of the spike): the
            // message at that index opens.
            let open = std::env::var("CATMAIL_NSH_OPEN")
                .ok()
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| list.get(i))
                .map(|m| m.id);
            global::<Messages>().set(Messages {
                more: list.len() == PAGE as usize,
                list,
                loading: false,
            });
            if open.is_some() && global::<SelectedMessage>().with_untracked(|s| s.0.is_none()) {
                global::<SelectedMessage>().set(SelectedMessage(open));
            }
        }
        Err(e) => {
            global::<Messages>().update(|m| m.loading = false);
            report("couldn't list messages", e);
        }
    }
}

/// The next page of the open folder.
async fn load_more() {
    let messages = global::<Messages>();
    let Some(folder) = global::<SelectedFolder>().with_untracked(|s| s.0) else {
        return;
    };
    let Some(after) = messages.with_untracked(|m| {
        (m.more && !m.loading)
            .then(|| m.list.last().map(MessageSummary::cursor))
            .flatten()
    }) else {
        return;
    };
    messages.update(|m| m.loading = true);
    match client().list_messages(folder, PAGE, Some(after)).await {
        Ok(page) => messages.update(|m| {
            m.more = page.len() == PAGE as usize;
            m.list.extend(page);
            m.loading = false;
        }),
        Err(e) => {
            messages.update(|m| m.loading = false);
            report("couldn't list messages", e);
        }
    }
}

/// Mail changed: the counts and the open folder's first page again. Events come in
/// bursts during a sync, so a quarter second of them is one refresh.
async fn watch_events() {
    let mut events = client().subscribe();
    loop {
        match events.recv().await {
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return,
        }
        timer(Duration::from_millis(250)).await;
        // Drops what came meanwhile: the refresh covers it.
        events = events.resubscribe();
        load_sidebar().await;
        if let Some(folder) = global::<SelectedFolder>().with_untracked(|s| s.0) {
            refresh_messages(folder).await;
        }
    }
}

/// The open folder's messages again, as many as are shown, keeping the place.
async fn refresh_messages(folder: FolderId) {
    let shown = global::<Messages>().with_untracked(|m| m.list.len()) as u32;
    if let Ok(list) = client().list_messages(folder, shown.max(PAGE), None).await
        && global::<SelectedFolder>().with_untracked(|s| s.0) == Some(folder)
    {
        global::<Messages>().update(|m| {
            m.more = list.len() as u32 == shown.max(PAGE);
            m.list = list;
        });
    }
}

async fn open_message(id: MessageId, dark: bool) {
    global::<BodyHeight>().set(BodyHeight(0.0));
    let message = match client().download_message(id).await {
        Ok(message) => message,
        Err(e) => return report("couldn't open the message", e),
    };
    let summary = &message.summary;
    let body = message.body.as_ref();
    let mut inline = HashMap::new();
    if let Some(body) = body.filter(|b| b.html.is_some()) {
        for part in &body.attachments {
            let Some(content_id) = &part.content_id else {
                continue;
            };
            if !part.mime_type.starts_with("image/") {
                continue;
            }
            match client().attachment_content(part.id).await {
                Ok(bytes) => {
                    inline.insert(
                        content_id.clone(),
                        InlineImage {
                            mime_type: part.mime_type.clone(),
                            bytes,
                        },
                    );
                }
                Err(e) => report("couldn't load an inline image", e),
            }
        }
    }
    // For checking what the page may load: this file's HTML in place of the message's.
    let probe = std::env::var_os("CATMAIL_NSH_HTML").and_then(|f| std::fs::read_to_string(f).ok());
    let html = probe.as_deref().or(body.and_then(|b| b.html.as_deref()));
    // A dark page moves the message's colors toward the theme's, as the GUI does; one
    // whose colors come from CSS variables can't be moved and stays light.
    let recolored = html
        .filter(|_| dark)
        .and_then(|html| recolor_html(html, palette()));
    let (html, dark) = match &recolored {
        Some(html) => (Some(html.as_ref()), true),
        None => (html, false),
    };
    let page = page::body_page(
        html,
        body.and_then(|b| b.text.as_deref()),
        inline,
        &colors(dark),
    );
    global::<Reading>().set(Reading {
        subject: summary.subject.clone().unwrap_or_default(),
        from: summary
            .from
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        date: summary.date.map(format_date).unwrap_or_default(),
        page,
    });
}

fn palette() -> DarkPalette {
    let rgb = |c: AdaptiveColor| {
        let c = c.dark.to_rgb();
        [c.r, c.g, c.b].map(|v| (v * 255.0).round() as u8)
    };
    DarkPalette {
        background: rgb(theme().background),
        foreground: rgb(theme().foreground),
    }
}

fn colors(dark: bool) -> Colors {
    let pick = |c: AdaptiveColor| if dark { c.dark } else { c.light }.to_hex();
    Colors {
        foreground: pick(theme().foreground),
        background: pick(theme().background),
        link: pick(theme().primary),
    }
}

fn format_date(seconds: i64) -> String {
    jiff::Timestamp::from_second(seconds)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

// --- Panes --------------------------------------------------------------------------

fn sidebar() -> impl IntoElement {
    view()
        .w(px(240.0))
        .border_r_1()
        .border_color(move || theme().border)
        .bg(move || theme().sidebar)
        .child(
            virtual_list(move || global::<Sidebar>().with(|s| s.0.len()), sidebar_row)
                .estimated_item_size(px(30.0))
                .flex_1(),
        )
        .child(
            text(move || global::<Status>().with(|s| s.0.clone().unwrap_or_default()))
                .text_xs()
                .px(px(12.0))
                .py(px(6.0))
                .text_color(move || theme().destructive),
        )
}

fn sidebar_row(at: Position) -> impl IntoElement {
    let row = computed(move || global::<Sidebar>().with(|s| s.0.get(at.index()).cloned()));
    let folder = move || match row.get() {
        Some(SidebarRow::Folder { id, .. }) => Some(id),
        _ => None,
    };
    let is_account = move || matches!(row.get(), Some(SidebarRow::Account(_)));
    let selected =
        move || folder().is_some() && folder() == global::<SelectedFolder>().with(|s| s.0);
    view()
        .h(px(30.0))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .pr(px(12.0))
        .pl(move || {
            px(match row.get() {
                Some(SidebarRow::Folder { depth, .. }) => 20.0 + depth as f32 * 14.0,
                _ => 12.0,
            })
        })
        .style_when(selected, |s| s.bg(move || theme().sidebar_accent))
        .on_press(move || {
            if let Some(id) = folder() {
                global::<SelectedMessage>().set(SelectedMessage(None));
                global::<SelectedFolder>().set(SelectedFolder(Some(id)));
            }
        })
        .child(
            text(move || match row.get() {
                Some(SidebarRow::Account(email)) => email,
                Some(SidebarRow::Folder { label, .. }) => label,
                None => String::new(),
            })
            .number_of_lines(1)
            .flex_1()
            .text_sm()
            .style_when(is_account, |s| s.font_weight(FontWeight::SEMIBOLD))
            .text_color(move || {
                if is_account() {
                    theme().muted_foreground
                } else {
                    theme().sidebar_foreground
                }
            }),
        )
        .child(
            text(move || match row.get() {
                Some(SidebarRow::Folder { unread, .. }) if unread > 0 => unread.to_string(),
                _ => String::new(),
            })
            .text_xs()
            .text_color(move || theme().muted_foreground),
        )
}

fn message_list() -> impl IntoElement {
    view()
        .w(px(380.0))
        .border_r_1()
        .border_color(move || theme().border)
        .child(
            virtual_list(
                move || global::<Messages>().with(|m| m.list.len()),
                message_row,
            )
            .keys(move || {
                global::<Messages>()
                    .with(|m| m.list.iter().map(|m| m.id.to_string()).collect::<Vec<_>>())
            })
            .on_load_more(move || {
                spawn(load_more());
            })
            .estimated_item_size(px(76.0))
            .flex_1(),
        )
}

fn message_row(at: Position) -> impl IntoElement {
    let message = computed(move || global::<Messages>().with(|m| m.list.get(at.index()).cloned()));
    let field = move |f: fn(&MessageSummary) -> String| {
        move || message.with(|m| m.as_ref().map(f).unwrap_or_default())
    };
    let id = move || message.with(|m| m.as_ref().map(|m| m.id));
    let unread = move || {
        message.with(|m| {
            m.as_ref()
                .is_some_and(|m| !m.flags.contains(catmail_core::Flags::SEEN))
        })
    };
    let selected = move || id().is_some() && id() == global::<SelectedMessage>().with(|s| s.0);
    view()
        .px(px(14.0))
        .py(px(10.0))
        .gap(px(2.0))
        .border_b_1()
        .border_color(move || theme().border)
        .style_when(selected, |s| s.bg(move || theme().accent))
        .on_press(move || {
            if let Some(id) = id() {
                global::<SelectedMessage>().set(SelectedMessage(Some(id)));
            }
        })
        .child(
            view()
                .flex_row()
                .gap(px(8.0))
                .child(
                    text(field(|m| {
                        m.from
                            .first()
                            .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
                            .unwrap_or_default()
                    }))
                    .number_of_lines(1)
                    .flex_1()
                    .text_sm()
                    .style_when(unread, |s| s.font_weight(FontWeight::SEMIBOLD))
                    .text_color(move || theme().foreground),
                )
                .child(
                    text(field(|m| m.date.map(format_date).unwrap_or_default()))
                        .text_xs()
                        .text_color(move || theme().muted_foreground),
                ),
        )
        .child(
            text(field(|m| m.subject.clone().unwrap_or_default()))
                .number_of_lines(1)
                .text_sm()
                .style_when(unread, |s| s.font_weight(FontWeight::SEMIBOLD))
                .text_color(move || theme().foreground),
        )
        .child(
            text(field(|m| m.snippet.clone().unwrap_or_default()))
                .number_of_lines(1)
                .text_xs()
                .text_color(move || theme().muted_foreground),
        )
}

fn reader() -> impl IntoElement {
    let reading = global::<Reading>();
    scroll().flex_1().child(
        view()
            .child(
                view()
                    .px(px(16.0))
                    .pt(px(16.0))
                    .gap(px(4.0))
                    .child(
                        text(move || reading.with(|r| r.subject.clone()))
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .selectable(true)
                            .text_color(move || theme().foreground),
                    )
                    .child(
                        view()
                            .flex_row()
                            .gap(px(8.0))
                            .child(
                                text(move || reading.with(|r| r.from.clone()))
                                    .flex_1()
                                    .text_sm()
                                    .selectable(true)
                                    .text_color(move || theme().foreground),
                            )
                            .child(
                                text(move || reading.with(|r| r.date.clone()))
                                    .text_xs()
                                    .text_color(move || theme().muted_foreground),
                            ),
                    ),
            )
            .child(
                // One web view for every message: a new page loads into it.
                web_view_html(move || reading.with(|r| r.page.clone()), None)
                    .java_script_enabled(false)
                    .scrollable(false)
                    .injected_java_script(page::BRIDGE.to_owned())
                    .on_message(move |message: &str| match FromPage::parse(message) {
                        Some(FromPage::Height(height)) => {
                            global::<BodyHeight>().set(BodyHeight(height));
                        }
                        Some(FromPage::Open(url)) if is_openable_link(url) => {
                            spawn(open_url(url.to_owned()));
                        }
                        _ => {}
                    })
                    .h(move || px(global::<BodyHeight>().with(|h| h.0).max(40.0))),
            ),
    )
}

/// Web pages and new mail; the sanitizer also lets `cid:` links through, which lead
/// nowhere outside the message.
fn is_openable_link(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
    })
}
