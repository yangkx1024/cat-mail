//! The main window: accounts and folders, the selected folder's messages, and the
//! reading pane. Follows the engine's change events, so a sync (started here or not)
//! shows up as it commits.

use catmail_core::{Account, Event, FolderId, FolderRole, MailClient, RecvError};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::h_flex;
use gpui_kit::component::list::{List, ListEvent, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, rems,
};

use crate::add_account::{AddAccount, AddAccountEvent};
use crate::folders::{FolderNode, folder_tree};
use crate::message_list::{self, MessageList};
use crate::reader::Reader;

struct AccountFolders {
    account: Account,
    folders: Vec<FolderNode>,
}

pub struct Mailbox {
    client: MailClient,
    /// `None` until the first load finishes.
    accounts: Option<Vec<AccountFolders>>,
    selected_folder: Option<FolderId>,
    /// Syncs in flight.
    syncing: usize,
    /// The sign-in form, shown instead of the mailbox while it is open.
    adding_account: Option<(Entity<AddAccount>, Subscription)>,
    messages: Entity<ListState<MessageList>>,
    reader: Entity<Reader>,
    /// Folders are being read; `folders_stale` asks for one more read after that.
    loading_folders: bool,
    folders_stale: bool,
    _load_folders: Task<()>,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Mailbox {
    pub fn new(client: MailClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let messages = cx.new(|cx| ListState::new(MessageList::new(client.clone()), window, cx));
        let reader = cx.new(|_| Reader::new(client.clone()));
        let subscriptions = vec![cx.subscribe_in(
            &messages,
            window,
            |this, list, event: &ListEvent, window, cx| match event {
                ListEvent::Select(ix) | ListEvent::Confirm(ix) => {
                    if let Some(id) = list.read(cx).delegate().message(ix.row).map(|m| m.id) {
                        this.reader
                            .update(cx, |reader, cx| reader.open(id, window, cx));
                    }
                }
                ListEvent::Cancel => this.reader.update(cx, |reader, cx| reader.clear(cx)),
            },
        )];

        let mut events = client.subscribe();
        let events = cx.spawn_in(window, async move |this, cx| {
            loop {
                let event = events.recv().await;
                if event == Err(RecvError::Closed) {
                    break;
                }
                let updated = this.update_in(cx, |this, window, cx| match event {
                    Ok(event) => this.on_engine_event(event, window, cx),
                    // Missed some: everything shown may be stale.
                    Err(_) => this.reload(window, cx),
                });
                if updated.is_err() {
                    break; // The window is gone.
                }
            }
        });

        let mut mailbox = Self {
            client,
            accounts: None,
            selected_folder: None,
            syncing: 0,
            adding_account: None,
            messages,
            reader,
            loading_folders: false,
            folders_stale: false,
            _load_folders: Task::ready(()),
            _events: events,
            _subscriptions: subscriptions,
        };
        mailbox.load_folders(window, cx);
        mailbox
    }

    fn on_engine_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::AccountAdded { .. }
            | Event::AccountRemoved { .. }
            | Event::FoldersChanged { .. } => self.load_folders(window, cx),
            Event::MessagesChanged { folder, .. } => {
                // Unread and total counts may have changed too.
                self.load_folders(window, cx);
                if self.selected_folder == Some(folder) {
                    self.messages
                        .update(cx, |list, cx| message_list::refresh(list, window, cx));
                }
            }
            // No composing, no changing messages, and no account status in the reading
            // window yet (M5).
            Event::DraftsChanged { .. }
            | Event::OutboxChanged { .. }
            | Event::MessageSent { .. }
            | Event::PendingOpsChanged { .. }
            | Event::AccountStatusChanged { .. } => {}
        }
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_folders(window, cx);
        self.messages
            .update(cx, |list, cx| message_list::refresh(list, window, cx));
    }

    /// Reads the accounts and their folders again. During a sync, events arrive faster
    /// than this completes, so requests are coalesced: at most one read runs, and one more
    /// follows if anything changed meanwhile.
    fn load_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_folders {
            self.folders_stale = true;
            return;
        }
        self.loading_folders = true;
        let client = self.client.clone();
        self._load_folders = cx.spawn_in(window, async move |this, cx| {
            let loaded = async {
                let mut loaded = Vec::new();
                for account in client.list_accounts().await? {
                    let folders = folder_tree(&client.list_folders(account.id).await?);
                    loaded.push(AccountFolders { account, folders });
                }
                Ok::<_, catmail_core::Error>(loaded)
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                this.loading_folders = false;
                match loaded {
                    Ok(accounts) => this.set_accounts(accounts, window, cx),
                    Err(e) => window.push_notification(
                        Notification::error(format!("Couldn't load folders: {e}")),
                        cx,
                    ),
                }
                if std::mem::take(&mut this.folders_stale) {
                    this.load_folders(window, cx);
                }
            })
            .ok();
        });
    }

    fn set_accounts(
        &mut self,
        accounts: Vec<AccountFolders>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first_load = self.accounts.is_none();
        let still_there = self.selected_folder.is_some_and(|id| {
            accounts
                .iter()
                .any(|a| first_folder(&a.folders, &|f| f.id == id).is_some())
        });
        self.accounts = Some(accounts);
        if !still_there {
            let inbox = self
                .accounts
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find_map(|a| {
                    first_folder(&a.folders, &|f| f.role == Some(FolderRole::Inbox))
                        .or_else(|| first_folder(&a.folders, &|_| true))
                });
            self.select_folder(inbox, window, cx);
        }
        let no_accounts = self.accounts.as_ref().is_some_and(Vec::is_empty);
        if no_accounts && self.adding_account.is_none() {
            self.add_account(false, window, cx);
        } else if first_load {
            self.messages.update(cx, |list, cx| list.focus(window, cx));
        }
        cx.notify();
    }

    /// Opens the sign-in form; `cancellable` is false when there is no mailbox to go back to.
    fn add_account(&mut self, cancellable: bool, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let form = cx.new(|cx| AddAccount::new(client, cancellable, window, cx));
        let subscription = cx.subscribe_in(&form, window, |this, _, event, window, cx| {
            this.adding_account = None;
            match event {
                // Its folders and messages appear through the engine's events.
                AddAccountEvent::Added(account) => {
                    this.sync_accounts(vec![account.clone()], window, cx);
                    this.messages.update(cx, |list, cx| list.focus(window, cx));
                }
                AddAccountEvent::Cancelled => {}
            }
            cx.notify();
        });
        self.adding_account = Some((form, subscription));
        cx.notify();
    }

    fn select_folder(
        &mut self,
        folder: Option<FolderId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_folder == folder && folder.is_some() {
            return;
        }
        self.selected_folder = folder;
        self.messages.update(cx, |list, cx| {
            message_list::show_folder(list, folder, window, cx)
        });
        self.reader.update(cx, |reader, cx| reader.clear(cx));
        cx.notify();
    }

    /// Syncs every account, unless a sync is already running.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing > 0 {
            return;
        }
        let accounts: Vec<Account> = self
            .accounts
            .iter()
            .flatten()
            .map(|a| a.account.clone())
            .collect();
        self.sync_accounts(accounts, window, cx);
    }

    /// Syncs the accounts in turn. Progress shows as it commits, through the engine's
    /// events; only failures are reported here.
    fn sync_accounts(
        &mut self,
        accounts: Vec<Account>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.syncing += 1;
        cx.notify();
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut problems = Vec::new();
            for account in accounts {
                match client.sync_account(account.id).await {
                    Ok(report) => {
                        let failed = report.folders.iter().filter(|f| f.error.is_some()).count();
                        if failed > 0 {
                            problems.push(Notification::warning(format!(
                                "{}: {failed} folder(s) couldn't be synced",
                                account.email
                            )));
                        }
                    }
                    Err(e) => problems.push(Notification::error(format!(
                        "Couldn't sync {}: {e}",
                        account.email
                    ))),
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.syncing -= 1;
                for problem in problems {
                    window.push_notification(problem, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self.accounts.as_deref().unwrap_or_default();
        let header = h_flex()
            .w_full()
            .justify_between()
            .child(div().font_semibold().child("cat-mail"))
            .child(
                Button::new("sync")
                    .ghost()
                    .small()
                    .icon(Lucide::RefreshCw)
                    .label("Sync")
                    .loading(self.syncing > 0)
                    .disabled(accounts.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.sync(window, cx))),
            );
        let footer = Button::new("add-account")
            .ghost()
            .small()
            .icon(Lucide::Plus)
            .label("Add account…")
            .on_click(cx.listener(|this, _, window, cx| this.add_account(true, window, cx)));
        Sidebar::new("folders")
            .w_full()
            .header(header)
            .footer(footer)
            .children(accounts.iter().map(|a| {
                SidebarGroup::new(a.account.email.clone()).child(
                    SidebarMenu::new()
                        .children(a.folders.iter().map(|node| self.render_folder(node, cx))),
                )
            }))
    }

    fn render_folder(&self, node: &FolderNode, cx: &mut Context<Self>) -> SidebarMenuItem {
        let role = node.folder.as_ref().and_then(|f| f.role);
        let mut item = SidebarMenuItem::new(node.label.clone())
            .icon(Icon::new(folder_icon(role)))
            .default_open(true)
            .children(
                node.children
                    .iter()
                    .map(|child| self.render_folder(child, cx)),
            );
        if let Some(folder) = node.selectable() {
            let id = folder.id;
            let unread = folder.unread_count;
            item =
                item.active(self.selected_folder == Some(id))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_folder(Some(id), window, cx)
                    }))
                    .when(unread > 0, |item| {
                        item.accessibility_label(format!("{}, {unread} unread", node.label))
                            .suffix(move |_, cx| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(unread.to_string())
                            })
                    });
        }
        item
    }
}

fn folder_icon(role: Option<FolderRole>) -> Lucide {
    match role {
        Some(FolderRole::Inbox) => Lucide::Inbox,
        Some(FolderRole::Sent) => Lucide::Send,
        Some(FolderRole::Drafts) => Lucide::FilePen,
        Some(FolderRole::Trash) => Lucide::Trash,
        Some(FolderRole::Junk) => Lucide::ShieldAlert,
        Some(FolderRole::Archive) => Lucide::Archive,
        Some(FolderRole::All) => Lucide::Mails,
        Some(FolderRole::Flagged) => Lucide::Flag,
        None => Lucide::Folder,
    }
}

/// The first selectable folder, depth first, that `matches`.
fn first_folder(
    nodes: &[FolderNode],
    matches: &dyn Fn(&catmail_core::Folder) -> bool,
) -> Option<FolderId> {
    nodes.iter().find_map(|node| {
        node.selectable()
            .filter(|f| matches(f))
            .map(|f| f.id)
            .or_else(|| first_folder(&node.children, matches))
    })
}

impl Render for Mailbox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let content = if let Some((form, _)) = &self.adding_account {
            form.clone().into_any_element()
        } else {
            h_resizable("mailbox")
                .child(
                    resizable_panel()
                        .size(rems(16.).to_pixels(rem))
                        .size_range(rems(12.).to_pixels(rem)..rems(28.).to_pixels(rem))
                        .child(self.render_sidebar(cx)),
                )
                .child(
                    resizable_panel()
                        .size(rems(24.).to_pixels(rem))
                        .size_range(rems(16.).to_pixels(rem)..rems(48.).to_pixels(rem))
                        .child(
                            div()
                                .size_full()
                                .border_r_1()
                                .border_color(cx.theme().border)
                                .child(List::new(&self.messages)),
                        ),
                )
                .child(resizable_panel().child(self.reader.clone()))
                .into_any_element()
        };
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use catmail_core::MessageId;
    use gpui_kit::component::list::ListDelegate as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, App, Bounds, HeadlessAppContext, TestAppContext, Window, WindowBounds,
        WindowOptions, px, size,
    };

    use super::*;
    use crate::test_support::{TestMail, wait_until};

    fn window_options() -> WindowOptions {
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                Default::default(),
                size(px(1280.), px(800.)),
            ))),
            focus: false,
            show: false,
            ..Default::default()
        }
    }

    /// Opens the mailbox in `cx`, which is a `TestAppContext` or a `HeadlessAppContext`.
    fn open_mailbox(cx: &mut App, mail: &TestMail) -> (AnyWindowHandle, Entity<Mailbox>) {
        gpui_kit::init(cx);
        let client = mail.client.clone();
        gpui_kit::open_window(window_options(), cx, |window, cx| {
            cx.new(|cx| Mailbox::new(client, window, cx))
        })
        .unwrap()
    }

    fn open(cx: &mut TestAppContext, mail: &TestMail) -> (AnyWindowHandle, Entity<Mailbox>) {
        cx.executor().allow_parking();
        cx.update(|cx| open_mailbox(cx, mail))
    }

    /// The messages listed, top to bottom.
    fn listed(mailbox: &Entity<Mailbox>, cx: &App) -> Vec<catmail_core::MessageSummary> {
        let list = mailbox.read(cx).messages.read(cx).delegate();
        (0..list.items_count(0, cx))
            .filter_map(|row| list.message(row).cloned())
            .collect()
    }

    fn subjects(mailbox: &Entity<Mailbox>, cx: &App) -> Vec<String> {
        listed(mailbox, cx)
            .into_iter()
            .filter_map(|m| m.subject)
            .collect()
    }

    fn id_of(mailbox: &Entity<Mailbox>, subject: &str, cx: &App) -> MessageId {
        listed(mailbox, cx)
            .into_iter()
            .find(|m| m.subject.as_deref() == Some(subject))
            .unwrap()
            .id
    }

    fn shown_in_reader(mailbox: &Entity<Mailbox>, cx: &App) -> Option<MessageId> {
        mailbox.read(cx).reader.read(cx).open_message_id()
    }

    #[gpui_kit::test]
    fn reads_a_message_through_the_window(cx: &mut TestAppContext) {
        let mail = TestMail::start();
        let (window, mailbox) = open(cx, &mail);

        // The inbox is selected on its own, newest first.
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| subjects(&mailbox, cx).len() == 6)
        });
        cx.read(|cx| {
            assert_eq!(subjects(&mailbox, cx)[0], "十月产品更新：新功能一览");
            assert!(mailbox.read(cx).selected_folder.is_some());
        });

        let report = cx.read(|cx| id_of(&mailbox, "月度报表", cx));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("message", report.0 as u64), cx);
        })
        .unwrap();
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| shown_in_reader(&mailbox, cx) == Some(report))
        });

        // It has both a text and an HTML part: text first, HTML on request.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("open-in-browser").is_some());
            window.click("format-html", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.read(|cx| assert!(mailbox.read(cx).reader.read(cx).prefers_html()));

        // The list keeps keyboard focus: the next message opens with the arrow key.
        let next = cx.read(|cx| {
            let list = mailbox.read(cx).messages.read(cx);
            let row = list.selected_index().unwrap().row;
            list.delegate().message(row + 1).unwrap().id
        });
        cx.update_window(window, |_, window, cx| window.press("down", cx))
            .unwrap();
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| shown_in_reader(&mailbox, cx) == Some(next))
        });
    }

    #[gpui_kit::test]
    fn signs_in_from_the_window(cx: &mut TestAppContext) {
        let mail = TestMail::without_account();
        let (window, mailbox) = open(cx, &mail);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| mailbox.read(cx).adding_account.is_some())
        });

        let port = mail.server().port.to_string();
        let fill = |window: &mut Window, cx: &mut App, id: &'static str, text: &str| {
            window.click(id, cx);
            window.press("secondary-a", cx);
            window.input(text, cx);
        };
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            fill(window, cx, "email", "alice@example.com");
            fill(window, cx, "password", "wrong");
            fill(window, cx, "username", "alice");
            fill(window, cx, "imap-host", "127.0.0.1");
            fill(window, cx, "imap-port", &port);
            window.within("imap-security").click(2usize, cx); // None: the fake server is plain
            fill(window, cx, "smtp-host", "127.0.0.1");
            window.click("sign-in", cx);
        })
        .unwrap();

        // The server refuses: the form says so and nothing is stored.
        let mut error = None;
        wait_until(|| {
            cx.run_until_parked();
            error = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("sign-in-error")?.label().map(str::to_owned)
                })
                .unwrap();
            error.is_some()
        });
        assert!(error.unwrap().contains("check the username and password"));
        assert!(
            pollster::block_on(mail.client.list_accounts())
                .unwrap()
                .is_empty()
        );

        // Enter in a field signs in, too.
        cx.update_window(window, |_, window, cx| {
            fill(window, cx, "password", "pw");
            window.press("enter", cx);
        })
        .unwrap();
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| {
                mailbox.read(cx).adding_account.is_none() && subjects(&mailbox, cx).len() == 6
            })
        });
        let accounts = pollster::block_on(mail.client.list_accounts()).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].imap.security, catmail_core::Security::Plain);
    }

    #[gpui_kit::test]
    fn cancel_is_off_once_signing_in_has_started(cx: &mut TestAppContext) {
        let mail = TestMail::start();
        let (window, mailbox) = open(cx, &mail);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| subjects(&mailbox, cx).len() == 6)
        });

        let port = mail.server().port.to_string();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("add-account", cx);
            for (id, text) in [
                ("email", "alice+work@example.com"),
                ("password", "pw"),
                ("username", "alice"),
                ("imap-host", "127.0.0.1"),
                ("imap-port", port.as_str()),
                ("smtp-host", "127.0.0.1"),
            ] {
                window.click(id, cx);
                window.input(text, cx);
            }
            window.within("imap-security").click(2usize, cx);
            window.click("sign-in", cx);
        })
        .unwrap();
        // GPUI's tasks run only when parked, so signing in is still under way here.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            // The account may be stored by now; leaving would only skip its first sync.
            window.click("cancel", cx);
        })
        .unwrap();
        cx.read(|cx| assert!(mailbox.read(cx).adding_account.is_some()));
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| mailbox.read(cx).adding_account.is_none())
        });
        assert_eq!(
            pollster::block_on(mail.client.list_accounts())
                .unwrap()
                .len(),
            2
        );
    }

    #[gpui_kit::test]
    fn a_message_gone_from_the_server_leaves_the_reader(cx: &mut TestAppContext) {
        let mail = TestMail::start();
        let (window, mailbox) = open(cx, &mail);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| subjects(&mailbox, cx).len() == 6)
        });
        // The second fixture, UID 2.
        let hello = cx.read(|cx| id_of(&mailbox, "你好，世界", cx));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("message", hello.0 as u64), cx);
        })
        .unwrap();
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| shown_in_reader(&mailbox, cx) == Some(hello))
        });

        mail.expunge_and_sync(2);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| subjects(&mailbox, cx).len() == 5)
        });
        cx.read(|cx| assert_eq!(shown_in_reader(&mailbox, cx), None));
    }

    #[gpui_kit::test]
    fn shows_what_a_sync_brings_without_losing_the_selection(cx: &mut TestAppContext) {
        let mail = TestMail::start();
        let (window, mailbox) = open(cx, &mail);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| subjects(&mailbox, cx).len() == 6)
        });
        let hello = cx.read(|cx| id_of(&mailbox, "你好，世界", cx));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("message", hello.0 as u64), cx);
        })
        .unwrap();
        let unread = |cx: &App| {
            let mailbox = mailbox.read(cx);
            let folder = mailbox.selected_folder.unwrap();
            mailbox.accounts.as_deref().unwrap()[0]
                .folders
                .iter()
                .find_map(|n| n.folder.as_ref().filter(|f| f.id == folder))
                .unwrap()
                .unread_count
        };
        let unread_before = cx.read(unread);

        // Synced by the client, not by the window: only events tell it.
        mail.deliver_and_sync("Fresh news");
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| subjects(&mailbox, cx).iter().any(|s| s == "Fresh news"))
        });
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| unread(cx) == unread_before + 1)
        });
        cx.read(|cx| {
            let list = mailbox.read(cx).messages.read(cx);
            let row = list.selected_index().unwrap().row;
            assert_eq!(list.delegate().message(row).unwrap().id, hello);
            assert_eq!(shown_in_reader(&mailbox, cx), Some(hello));
        });
    }

    /// Renders the window offscreen to PNG files, to look at how real HTML lays out:
    ///
    /// ```sh
    /// CATMAIL_SCREENSHOTS=/tmp/shots cargo test -p catmail-gui -- --ignored screenshots
    /// ```
    ///
    /// Needs a GPU adapter, or Mesa's lavapipe.
    #[test]
    #[ignore = "writes images for a person to look at"]
    fn screenshots() {
        let dir = PathBuf::from(
            std::env::var_os("CATMAIL_SCREENSHOTS").expect("set CATMAIL_SCREENSHOTS"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let save = |cx: &mut HeadlessAppContext, window: AnyWindowHandle, name: &str| {
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
            let path = dir.join(format!("{name}.png"));
            cx.capture_screenshot(window).unwrap().save(&path).unwrap();
            println!("{}", path.display());
        };

        let empty = TestMail::without_account();
        let (mut cx, window, mailbox) = headless_window(&empty);
        wait_until(|| {
            cx.run_until_parked();
            cx.update(|cx| mailbox.read(cx).adding_account.is_some())
        });
        save(&mut cx, window, "sign-in");
        drop(cx);

        let mail = TestMail::start();
        let (mut cx, window, mailbox) = headless_window(&mail);
        wait_until(|| {
            cx.run_until_parked();
            cx.update(|cx| subjects(&mailbox, cx).len() == 6)
        });

        let shots = [
            ("text", "你好，世界", false),
            ("html-inline-image", "月度报表", true),
            ("html-newsletter", "十月产品更新：新功能一览", true),
        ];
        for (name, subject, html) in shots {
            let id = cx.update(|cx| id_of(&mailbox, subject, cx));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click(("message", id.0 as u64), cx);
            })
            .unwrap();
            wait_until(|| {
                cx.run_until_parked();
                cx.update(|cx| shown_in_reader(&mailbox, cx) == Some(id))
            });
            if html {
                cx.update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    if !mailbox.read(cx).reader.read(cx).prefers_html() {
                        window.click("format-html", cx);
                    }
                })
                .unwrap();
            }
            // Inline images load after the text.
            for _ in 0..20 {
                cx.run_until_parked();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            save(&mut cx, window, name);
        }
    }

    /// A window rendered offscreen by the GPU, so it can be captured.
    fn headless_window(mail: &TestMail) -> (HeadlessAppContext, AnyWindowHandle, Entity<Mailbox>) {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(crate::assets::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.allow_parking();
        let (window, mailbox) = cx.update(|cx| open_mailbox(cx, mail));
        (cx, window, mailbox)
    }
}
