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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use catmail_core::{
    AccountId, Draft, DraftAttachment, DraftContent, DraftId, Event, Forwarding, Identity,
    IdentityId, MailClient, MessageId, RecvError, parse_addresses,
};
use gpui_kit::component::input::{InputEvent, InputState, Position, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{IndexPath, WindowExt as _};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global,
    PathPromptOptions, SharedString, Subscription, Task, WeakEntity, Window, px, size,
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
    /// Drafts being read for a window: opening one again meanwhile does nothing, the
    /// window is on its way.
    opening: HashSet<DraftId>,
    /// Opens on their way, for tests to wait for.
    #[cfg(test)]
    in_flight: usize,
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

/// The open composers.
#[cfg(test)]
fn composers(cx: &App) -> impl Iterator<Item = &Composer> {
    cx.try_global::<OpenComposers>()
        .into_iter()
        .flat_map(|open| open.windows.values())
        .filter_map(|(_, composer)| composer.upgrade())
        .map(|composer| composer.read(cx))
}

/// The text of each open composer.
#[cfg(test)]
pub(crate) fn texts(cx: &App) -> Vec<String> {
    composers(cx)
        .map(|composer| composer.body.read(cx).value().to_string())
        .collect()
}

/// Opens a composer window for `from`. The draft is made (or read) first; failures are
/// reported in `window`, the one asking.
pub fn open(client: MailClient, from: ComposeFrom, window: &mut Window, cx: &mut App) {
    if let ComposeFrom::Draft(id) = from
        && (activate(id, cx) || !cx.default_global::<OpenComposers>().opening.insert(id))
    {
        return;
    }
    #[cfg(test)]
    {
        cx.default_global::<OpenComposers>().in_flight += 1;
    }
    let asking = window.window_handle();
    // On the app, not the asking window: closing that must not leave the draft marked
    // as being opened.
    cx.spawn(async move |cx| {
        let opened = async {
            let draft = match from {
                ComposeFrom::New(account) => {
                    client
                        .create_draft(account, None, DraftContent::default(), Vec::new())
                        .await?
                }
                ComposeFrom::Reply { message, all } => {
                    client
                        .reply(message, all, None, String::new(), Vec::new())
                        .await?
                }
                ComposeFrom::Forward(message) => {
                    client
                        .forward(message, None, Forwarding::default(), Vec::new())
                        .await?
                }
                ComposeFrom::Draft(id) => client
                    .get_draft(id)
                    .await?
                    .ok_or(catmail_core::Error::DraftNotFound(id))?,
            };
            let identities = client.list_identities(draft.account_id).await?;
            Ok::<_, catmail_core::Error>((draft, identities))
        }
        .await;
        cx.update(|cx| {
            if let ComposeFrom::Draft(id) = from {
                cx.default_global::<OpenComposers>().opening.remove(&id);
            }
            #[cfg(test)]
            {
                cx.default_global::<OpenComposers>().in_flight -= 1;
            }
            let failure = match opened {
                // A window for it opened meanwhile: a reply or forward made it, and it
                // was opened from the drafts before its window came up.
                Ok((draft, ..)) if activate(draft.id, cx) => None,
                Ok((draft, identities)) => {
                    let created_here = !matches!(from, ComposeFrom::Draft(_));
                    // Where the writing starts: the text once there is someone to send to.
                    let focus_body = match from {
                        ComposeFrom::Reply { .. } => true,
                        ComposeFrom::Draft(_) => !draft.content.to.is_empty(),
                        _ => false,
                    };
                    let opening = Opening {
                        client,
                        identities,
                        draft,
                        created_here,
                        focus_body,
                    };
                    open_window(opening, cx)
                        .err()
                        .map(|e| format!("Couldn't open a window: {e:#}"))
                }
                Err(e) => Some(format!("Couldn't start the message: {e}")),
            };
            if let Some(failure) = failure {
                asking
                    .update(cx, |_, window, cx| {
                        window.push_notification(Notification::error(failure), cx)
                    })
                    .ok();
            }
        });
    })
    .detach();
}

/// Whether every [`open`] has finished.
#[cfg(test)]
pub(crate) fn opened(cx: &App) -> bool {
    cx.try_global::<OpenComposers>()
        .is_none_or(|open| open.in_flight == 0)
}

/// The error each open composer shows, if any.
#[cfg(test)]
pub(crate) fn errors(cx: &App) -> Vec<Option<String>> {
    composers(cx)
        .map(|composer| composer.error.as_ref().map(ToString::to_string))
        .collect()
}

/// What a composer window is opened with.
struct Opening {
    client: MailClient,
    /// The account's identities, to choose From among.
    identities: Vec<Identity>,
    draft: Draft,
    /// Made for this window, see [`Composer::created_here`].
    created_here: bool,
    /// The text has the focus, not To.
    focus_body: bool,
}

fn open_window(opening: Opening, cx: &mut App) -> anyhow::Result<()> {
    let id = opening.draft.id;
    let options = crate::layout::window_options(
        window_title(&opening.draft.content.subject),
        size(px(760.), px(680.)),
        size(px(480.), px(360.)),
        cx,
    );
    let (handle, composer) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Composer::new(opening, window, cx))
    })?;
    cx.default_global::<OpenComposers>()
        .windows
        .insert(id, (handle, composer.downgrade()));
    Ok(())
}

fn window_title(subject: &str) -> String {
    match subject.trim() {
        "" => "New message".into(),
        subject => subject.into(),
    }
}

/// A choice of From: one of the account's identities.
#[derive(Debug, Clone, PartialEq)]
struct Sender {
    identity: IdentityId,
    title: SharedString,
}

impl SelectItem for Sender {
    type Value = IdentityId;

    fn value(&self) -> &Self::Value {
        &self.identity
    }

    fn title(&self) -> SharedString {
        self.title.clone()
    }
}

/// The account's identities, as the engine lists them (the default first). An address
/// there twice (through two servers) is told apart by its server.
fn senders(identities: &[Identity]) -> Vec<Sender> {
    identities
        .iter()
        .map(|identity| {
            let address = identity.address();
            let twice = identities
                .iter()
                .filter(|other| other.email.eq_ignore_ascii_case(&identity.email))
                .count()
                > 1;
            let title = if twice {
                format!("{address} · {}", identity.smtp.host)
            } else {
                address.to_string()
            };
            Sender {
                identity: identity.id,
                title: title.into(),
            }
        })
        .collect()
}

/// The From line: who the draft is from, or a choice when the account has more than one
/// identity.
enum FromField {
    /// With its identity, kept as it is.
    One(IdentityId, SharedString),
    Choice {
        senders: Entity<SelectState<Vec<Sender>>>,
        _changes: Subscription,
    },
}

/// What a save hands to the engine: who the draft is from, and what it says.
#[derive(Debug, Clone, PartialEq)]
struct Written {
    from: IdentityId,
    content: DraftContent,
}

/// An address field, which an error is shown under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    To,
    Cc,
    Bcc,
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
    from: FromField,
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
    saved: Written,
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
    /// Follows the engine's changes to the account's identities.
    _identities: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Composer {
    fn new(opening: Opening, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Opening {
            client,
            identities,
            draft,
            created_here,
            focus_body,
        } = opening;
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
        let from = Self::from_field(&identities, draft.identity, window, cx);
        let identities = Self::follow_identities(&client, draft.account_id, window, cx);
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

        if focus_body {
            body.update(cx, |body, cx| {
                body.focus(window, cx);
                // Above the quote.
                body.set_cursor_position(Position::new(0, 0), window, cx);
            });
        } else {
            to.update(cx, |to, cx| to.focus(window, cx));
        }

        Self {
            client,
            from,
            draft: draft.id,
            created_here,
            edited: false,
            dirty: false,
            saving: false,
            attaching: 0,
            save_state: SaveState::Unchanged,
            finishing: None,
            show_cc_bcc: !content.cc.is_empty() || !content.bcc.is_empty(),
            saved: Written {
                from: draft.identity,
                content,
            },
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
            _identities: identities,
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
        // The fields are read-only by then (see `header_input`).
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

    /// The draft as typed. An address field that doesn't parse keeps its last saved
    /// value, and is reported.
    fn written(&self, cx: &App) -> (Written, Option<(Field, SharedString)>) {
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
        let saved = &self.saved.content;
        let to = field(Field::To, &self.to, &saved.to);
        let cc = field(Field::Cc, &self.cc, &saved.cc);
        let bcc = field(Field::Bcc, &self.bcc, &saved.bcc);
        let content = DraftContent {
            subject: self.subject.read(cx).value().to_string(),
            to,
            cc,
            bcc,
            text: self.body.read(cx).value().to_string(),
        };
        let from = self.sender_identity(cx);
        (Written { from, content }, problem)
    }

    /// The From line for the account's `identities`, with `selected` chosen.
    fn from_field(
        identities: &[Identity],
        selected: IdentityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FromField {
        let senders = senders(identities);
        if senders.len() <= 1 {
            // Every account has its default.
            let title = senders
                .into_iter()
                .next()
                .map(|s| s.title)
                .unwrap_or_default();
            return FromField::One(selected, title);
        }
        let position = senders
            .iter()
            .position(|s| s.identity == selected)
            .map(IndexPath::new);
        let choice = cx.new(|cx| SelectState::new(senders, position, window, cx));
        let changes = cx.subscribe_in(
            &choice,
            window,
            |this, _, event: &SelectEvent<Vec<Sender>>, window, cx| {
                if let SelectEvent::Confirm(Some(identity)) = event
                    && this.saved.from != *identity
                {
                    this.changed(window, cx);
                }
            },
        );
        FromField::Choice {
            senders: choice,
            _changes: changes,
        }
    }

    /// Nothing chosen keeps the one saved.
    fn sender_identity(&self, cx: &App) -> IdentityId {
        match &self.from {
            FromField::One(identity, _) => *identity,
            FromField::Choice { senders, .. } => senders
                .read(cx)
                .selected_value()
                .copied()
                .unwrap_or(self.saved.from),
        }
    }

    /// Reads the account's identities again whenever the engine says they changed (or
    /// events were missed), for [`set_identities`](Self::set_identities).
    fn follow_identities(
        client: &MailClient,
        account: AccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let (client, mut events) = (client.clone(), client.subscribe());
        cx.spawn_in(window, async move |this, cx| {
            loop {
                match events.recv().await {
                    Err(RecvError::Closed) => break,
                    Ok(Event::AccountChanged { account: changed }) if changed == account => {}
                    Err(RecvError::Lagged(_)) => {}
                    Ok(_) => continue,
                }
                let Ok(identities) = client.list_identities(account).await else {
                    continue;
                };
                let updated = this.update_in(cx, |this, window, cx| {
                    this.set_identities(&identities, window, cx)
                });
                if updated.is_err() {
                    break; // The window is gone.
                }
            }
        })
    }

    /// The account's identities changed: one was added, made the default or removed. One
    /// removed is replaced by the default, as the engine did in the stored draft; what was
    /// typed stays.
    fn set_identities(
        &mut self,
        identities: &[Identity],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.sender_identity(cx);
        let selected = match identities.first() {
            Some(default) if !identities.iter().any(|i| i.id == current) => default.id,
            _ => current,
        };
        self.from = Self::from_field(identities, selected, window, cx);
        cx.notify();
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

    /// Made for this window and never changed: closing it leaves nothing behind.
    fn untouched(&self) -> bool {
        self.created_here && !self.edited
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
