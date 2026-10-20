//! Writing a message, each in its own window. A new message, a reply or a forward is a
//! draft from the start: the engine creates it, and the window saves every change to it
//! (a moment after typing stops), so closing the window or the app loses nothing. A
//! draft nobody touched is deleted again when its window closes.
//!
//! Engine calls for one draft never overlap: the save in flight finishes before the
//! next starts, and sending or closing waits for it, so an older save can't land after
//! the content that was sent.

mod lifecycle;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use catmail_core::{
    Account, AccountId, Draft, DraftAttachment, DraftContent, DraftId, MailClient, MessageId,
    parse_addresses,
};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState, Position, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, Global,
    PathPromptOptions, SharedString, Subscription, Task, TitlebarOptions, WeakEntity, Window,
    WindowBounds, WindowOptions, px, size,
};

/// How long typing has to pause before the draft is saved.
const SAVE_DELAY: Duration = Duration::from_millis(800);

/// What a composer window starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeFrom {
    New(AccountId),
    Reply { message: MessageId, all: bool },
    Forward(MessageId),
    Draft(DraftId),
}

/// The composer windows open, by draft: opening a draft again brings its window forward
/// instead of a second editor of the same draft.
#[derive(Default)]
struct OpenComposers {
    windows: HashMap<DraftId, (AnyWindowHandle, WeakEntity<Composer>)>,
    /// The app quits once the last of them has closed.
    quitting: bool,
}

impl Global for OpenComposers {}

/// Brings the draft's window forward, if it has one.
fn activate(id: DraftId, cx: &mut App) -> bool {
    let Some((handle, _)) = cx
        .default_global::<OpenComposers>()
        .windows
        .get(&id)
        .cloned()
    else {
        return false;
    };
    handle
        .update(cx, |_, window, _| window.activate_window())
        .is_ok()
}

/// Quits once every composer window has saved what it holds and closed; quitting
/// straight away would lose what was typed since the last save. A window that can't
/// close (an address to correct, a save that failed) stays open, and so does the app.
pub fn quit(cx: &mut App) {
    let open: Vec<_> = cx
        .default_global::<OpenComposers>()
        .windows
        .values()
        .cloned()
        .collect();
    if open.is_empty() {
        cx.quit();
        return;
    }
    cx.default_global::<OpenComposers>().quitting = true;
    for (handle, composer) in open {
        handle
            .update(cx, |_, window, cx| {
                composer.update(cx, |composer, cx| composer.close(window, cx))
            })
            .ok();
    }
}

/// Opens a composer window for `from`. The draft is made (or read) first; failures are
/// reported in `window`, the one asking.
pub fn open(client: MailClient, from: ComposeFrom, window: &mut Window, cx: &mut App) {
    if let ComposeFrom::Draft(id) = from
        && activate(id, cx)
    {
        return;
    }
    window
        .spawn(cx, async move |cx| {
            let opened = async {
                let draft = match from {
                    ComposeFrom::New(account) => {
                        client
                            .create_draft(account, DraftContent::default())
                            .await?
                    }
                    ComposeFrom::Reply { message, all } => client.reply(message, all).await?,
                    ComposeFrom::Forward(message) => client.forward(message).await?,
                    ComposeFrom::Draft(id) => client.get_draft(id).await?.ok_or_else(|| {
                        catmail_core::Error::InvalidInput("the draft is gone".into())
                    })?,
                };
                let account = client.get_account(draft.account_id).await?.ok_or_else(|| {
                    catmail_core::Error::AccountNotFound(draft.account_id.to_string())
                })?;
                Ok::<_, catmail_core::Error>((draft, account))
            }
            .await;
            cx.update(|window, cx| match opened {
                // Opened again while this one was loading.
                Ok((draft, _)) if activate(draft.id, cx) => {}
                Ok((draft, account)) => {
                    let created_here = !matches!(from, ComposeFrom::Draft(_));
                    let focus = match from {
                        ComposeFrom::Reply { .. } => Field::Body,
                        ComposeFrom::Draft(_) if !draft.content.to.is_empty() => Field::Body,
                        _ => Field::To,
                    };
                    if let Err(e) = open_window(client, account, draft, created_here, focus, cx) {
                        window.push_notification(
                            Notification::error(format!("Couldn't open a window: {e:#}")),
                            cx,
                        );
                    }
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't start the message: {e}")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
}

fn open_window(
    client: MailClient,
    account: Account,
    draft: Draft,
    created_here: bool,
    focus: Field,
    cx: &mut App,
) -> anyhow::Result<AnyWindowHandle> {
    let id = draft.id;
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(window_title(&draft.content.subject).into()),
            ..Default::default()
        }),
        // Window geometry is in device-independent pixels, not the rem scale.
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(760.), px(680.)),
            cx,
        ))),
        window_min_size: Some(size(px(480.), px(360.))),
        ..Default::default()
    };
    let (handle, composer) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Composer::new(client, account, draft, created_here, focus, window, cx))
    })?;
    cx.default_global::<OpenComposers>()
        .windows
        .insert(id, (handle, composer.downgrade()));
    Ok(handle)
}

fn window_title(subject: &str) -> String {
    match subject.trim() {
        "" => "New message".into(),
        subject => subject.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    To,
    Cc,
    Bcc,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    Send,
    Close,
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveState {
    Unchanged,
    Saved,
    Failed(SharedString),
}

pub struct Composer {
    client: MailClient,
    account: Account,
    draft: DraftId,
    /// Made for this window (not opened from the drafts): deleted again if nothing was
    /// changed before it closes.
    created_here: bool,
    /// Changed since the window opened.
    edited: bool,
    /// Changed since the last save started.
    dirty: bool,
    /// A save, or the send or close that ends the window, is with the engine.
    saving: bool,
    /// Attachments being added or removed. The window ends after them: sending first
    /// would leave one out, and it couldn't be added to a draft being sent.
    attaching: usize,
    save_state: SaveState,
    /// What the window is ending with; it waits for the save in flight.
    finishing: Option<Finish>,
    /// What was last handed to the engine. An address field that doesn't parse keeps
    /// this value until it does.
    saved: DraftContent,
    to: Entity<InputState>,
    cc: Entity<InputState>,
    bcc: Entity<InputState>,
    subject: Entity<InputState>,
    body: Entity<TextareaState>,
    show_cc_bcc: bool,
    attachments: Vec<DraftAttachment>,
    /// A field that doesn't hold addresses, shown under it once sending (or closing)
    /// needs it to.
    field_error: Option<(Field, SharedString)>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _debounce: Task<()>,
    _save: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Composer {
    fn new(
        client: MailClient,
        account: Account,
        draft: Draft,
        created_here: bool,
        focus: Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = draft.content.clone();
        let address_input = |list: &[catmail_core::Address], window: &mut Window, cx: &mut App| {
            let value = catmail_core::format_addresses(list);
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let to = address_input(&content.to, window, cx);
        let cc = address_input(&content.cc, window, cx);
        let bcc = address_input(&content.bcc, window, cx);
        let subject = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Subject")
                .default_value(content.subject.clone())
        });
        let body = cx.new(|cx| TextareaState::new(window, cx).default_value(content.text.clone()));

        let mut subscriptions: Vec<Subscription> = [&to, &cc, &bcc, &subject]
            .into_iter()
            .map(|input| cx.subscribe_in(input, window, Self::on_input_event))
            .collect();
        subscriptions.push(cx.subscribe_in(&body, window, Self::on_input_event));
        // The window is named after the subject.
        subscriptions.push(
            cx.subscribe_in(&subject, window, |_, subject, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    window.set_window_title(&window_title(&subject.read(cx).value()));
                }
            }),
        );
        // Closing the window by its frame saves too; it waits for the save to land.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.should_close(window, cx))
                .unwrap_or(true)
        });

        match focus {
            Field::Body => {
                body.update(cx, |body, cx| {
                    body.focus(window, cx);
                    // Above the quote.
                    body.set_cursor_position(Position::new(0, 0), window, cx);
                });
            }
            _ => to.update(cx, |to, cx| to.focus(window, cx)),
        }

        Self {
            client,
            account,
            draft: draft.id,
            created_here,
            edited: false,
            dirty: false,
            saving: false,
            attaching: 0,
            save_state: SaveState::Unchanged,
            finishing: None,
            show_cc_bcc: !content.cc.is_empty() || !content.bcc.is_empty(),
            saved: content,
            to,
            cc,
            bcc,
            subject,
            body,
            attachments: draft.attachments,
            field_error: None,
            error: None,
            focus_handle: cx.focus_handle(),
            _debounce: Task::ready(()),
            _save: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    fn on_input_event<E>(
        &mut self,
        _: &Entity<E>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.changed(window, cx);
        }
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing.is_some() {
            return;
        }
        self.edited = true;
        self.dirty = true;
        self.field_error = None;
        // Replacing the task cancels the previous timer.
        self._debounce = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update_in(cx, |this, window, cx| this.save(window, cx))
                .ok();
        });
        cx.notify();
    }

    /// The content as typed. An address field that doesn't parse keeps its last saved
    /// value, and is reported.
    fn content(&self, cx: &App) -> (DraftContent, Option<(Field, SharedString)>) {
        let mut problem = None;
        let mut field =
            |field: Field, input: &Entity<InputState>, saved: &[catmail_core::Address]| {
                match parse_addresses(&input.read(cx).value()) {
                    Ok(list) => list,
                    Err(e) => {
                        problem.get_or_insert((field, error_text(&e)));
                        saved.to_vec()
                    }
                }
            };
        let to = field(Field::To, &self.to, &self.saved.to);
        let cc = field(Field::Cc, &self.cc, &self.saved.cc);
        let bcc = field(Field::Bcc, &self.bcc, &self.saved.bcc);
        let content = DraftContent {
            subject: self.subject.read(cx).value().to_string(),
            to,
            cc,
            bcc,
            text: self.body.read(cx).value().to_string(),
        };
        (content, problem)
    }

    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |this, window, cx| this.attach_paths(paths, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn attach_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing.is_some() {
            return;
        }
        let client = self.client.clone();
        let id = self.draft;
        self.edited = true;
        self.attaching += 1;
        cx.spawn_in(window, async move |this, cx| {
            let mut failed = false;
            for path in paths {
                let attached = client.attach_file(id, path.clone()).await;
                failed |= attached.is_err();
                this.update_in(cx, |this, window, cx| {
                    match attached {
                        Ok(attachment) => this.attachments.push(attachment),
                        Err(e) => window.push_notification(
                            Notification::error(format!(
                                "Couldn't attach {}: {e}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            )),
                            cx,
                        ),
                    }
                    cx.notify();
                })
                .ok();
            }
            this.update_in(cx, |this, window, cx| this.attached(failed, window, cx))
                .ok();
        })
        .detach();
    }

    /// An attachment change is done; a window waiting to end can go on. If it failed, a
    /// send or close waiting for it stops instead: the message isn't what the user
    /// meant to send, and the window stays to show why.
    fn attached(&mut self, failed: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.attaching -= 1;
        let Some(how) = self.finishing else {
            return;
        };
        if failed && how != Finish::Discard {
            self.finishing = None;
            self.stop_quitting(cx);
            self.error = Some(
                match how {
                    Finish::Send => "Not sent: check the attachments first.",
                    _ => "Left open: check the attachments first.",
                }
                .into(),
            );
            cx.notify();
        } else if !self.busy() {
            self.run_finish(window, cx);
        }
    }

    /// Something for this draft is with the engine.
    fn busy(&self) -> bool {
        self.saving || self.attaching > 0
    }

    fn remove_attachment(
        &mut self,
        attachment: catmail_core::DraftAttachmentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.finishing.is_some() {
            return;
        }
        let client = self.client.clone();
        self.edited = true;
        self.attaching += 1;
        cx.spawn_in(window, async move |this, cx| {
            let result = client.remove_draft_attachment(attachment).await;
            this.update_in(cx, |this, window, cx| {
                let failed = result.is_err();
                match result {
                    Ok(draft) => this.attachments = draft.attachments,
                    Err(e) => window.push_notification(
                        Notification::error(format!("Couldn't remove the attachment: {e}")),
                        cx,
                    ),
                }
                this.attached(failed, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// The engine's message, as a sentence for the window.
fn error_text(e: &catmail_core::Error) -> SharedString {
    let text = e.to_string();
    let mut chars = text.chars();
    let mut sentence: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return SharedString::default(),
    };
    if !sentence.ends_with('.') {
        sentence.push('.');
    }
    sentence.into()
}
