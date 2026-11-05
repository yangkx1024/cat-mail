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

use std::path::PathBuf;

use catmail_core::{DraftAttachment, DraftId, Event, Identity, IdentityId, MailClient, RecvError};
pub use catmail_frontend::composer::{ComposeFrom, new_message_account};
use catmail_frontend::composer::{
    AfterAttachment, Field, FieldError, Fields, Lifecycle, Opening, SAVE_DELAY, Written,
    window_title,
};
use futures_channel::oneshot;
use gpui_kit::component::input::{InputEvent, InputState, Position, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{IndexPath, WindowExt as _};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global,
    PathPromptOptions, SharedString, Subscription, Task, WeakEntity, Window, px, size,
};

/// A composer's window, and the composer in it.
type ComposerWindow = (AnyWindowHandle, WeakEntity<Composer>);

/// The composer windows open, by draft.
#[derive(Default)]
struct OpenComposers {
    open: catmail_frontend::composer::OpenComposers<ComposerWindow>,
    /// Opens on their way, for tests to wait for.
    #[cfg(test)]
    in_flight: usize,
}

impl Global for OpenComposers {}

fn open_composers(cx: &mut App) -> &mut catmail_frontend::composer::OpenComposers<ComposerWindow> {
    &mut cx.default_global::<OpenComposers>().open
}

/// Brings the draft's window forward, if it has one.
fn activate(id: DraftId, cx: &mut App) -> bool {
    let Some((handle, _)) = open_composers(cx).window(id) else {
        return false;
    };
    handle
        .update(cx, |_, window, _| window.activate_window())
        .is_ok()
}

/// Closes every composer window once it has saved what it holds: closing straight away
/// would lose what was typed since the last save. True once the last has closed; false as
/// soon as one can't (an address to correct, a save that failed), which stays open.
pub fn close_all(cx: &mut App) -> oneshot::Receiver<bool> {
    let (windows, answer) = open_composers(cx).close_all();
    for (handle, composer) in windows {
        handle
            .update(cx, |_, window, cx| {
                composer.update(cx, |composer, cx| composer.close(window, cx))
            })
            .ok();
    }
    answer
}

/// The open composers.
#[cfg(test)]
fn composers(cx: &App) -> impl Iterator<Item = &Composer> {
    cx.try_global::<OpenComposers>()
        .into_iter()
        .flat_map(|open| open.open.windows())
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
        && (activate(id, cx) || !open_composers(cx).begin_opening(id))
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
        let opened = from.start(&client).await;
        cx.update(|cx| {
            if let ComposeFrom::Draft(id) = from {
                open_composers(cx).end_opening(id);
            }
            #[cfg(test)]
            {
                cx.default_global::<OpenComposers>().in_flight -= 1;
            }
            let failure = match opened {
                // A window for it opened meanwhile: a reply or forward made it, and it
                // was opened from the drafts before its window came up.
                Ok(opening) if activate(opening.draft.id, cx) => None,
                Ok(opening) => open_window(client, opening, cx)
                    .err()
                    .map(|e| format!("Couldn't open a window: {e:#}")),
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

fn open_window(client: MailClient, opening: Opening, cx: &mut App) -> anyhow::Result<()> {
    let id = opening.draft.id;
    let options = crate::layout::window_options(
        window_title(&opening.draft.content.subject),
        size(px(760.), px(680.)),
        size(px(480.), px(360.)),
        crate::layout::Frame::TitleBar,
        cx,
    );
    let (handle, composer) = crate::layout::open_window(options, cx, |window, cx| {
        cx.new(|cx| Composer::new(client, opening, window, cx))
    })?;
    open_composers(cx).insert(id, (handle, composer.downgrade()));
    Ok(())
}

/// A choice of From: one of the identities the draft can be sent as.
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

fn senders(identities: &[Identity]) -> Vec<Sender> {
    catmail_frontend::composer::senders(identities)
        .into_iter()
        .map(|s| Sender {
            identity: s.identity,
            title: s.title.into(),
        })
        .collect()
}

/// The From line: who the draft is from, or a choice when it can be sent as more than
/// one identity.
enum FromField {
    /// With its identity, kept as it is.
    One(IdentityId, SharedString),
    Choice {
        senders: Entity<SelectState<Vec<Sender>>>,
        _changes: Subscription,
    },
}

pub struct Composer {
    client: MailClient,
    from: FromField,
    draft: DraftId,
    /// When it saves, sends and closes.
    life: Lifecycle,
    to: Entity<InputState>,
    cc: Entity<InputState>,
    bcc: Entity<InputState>,
    subject: Entity<InputState>,
    body: Entity<TextareaState>,
    show_cc_bcc: bool,
    attachments: Vec<DraftAttachment>,
    /// A field that doesn't hold addresses, shown under it once sending (or closing)
    /// needs it to.
    field_error: Option<FieldError>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _debounce: Task<()>,
    _save: Task<()>,
    /// Follows the engine's changes to the identities.
    _identities: Task<()>,
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
        opening: Opening,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let life = Lifecycle::new(&opening);
        let Opening {
            identities,
            draft,
            focus_body,
            ..
        } = opening;
        let content = draft.content;
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
        let identities = Self::follow_identities(&client, draft.id, window, cx);
        // The window is named after the subject (its title bar redraws with the change,
        // `on_input_event`).
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
            life,
            show_cc_bcc: !content.cc.is_empty() || !content.bcc.is_empty(),
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
        if !self.life.changed() {
            return;
        }
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
    fn written(&self, cx: &App) -> (Written, Option<FieldError>) {
        let value = |input: &Entity<InputState>| input.read(cx).value();
        let (to, cc, bcc, subject) = (
            value(&self.to),
            value(&self.cc),
            value(&self.bcc),
            value(&self.subject),
        );
        let text = self.body.read(cx).value();
        let fields = Fields {
            to: &to,
            cc: &cc,
            bcc: &bcc,
            subject: &subject,
            text: &text,
        };
        self.life.written(fields, self.sender_identity(cx))
    }

    /// The From line for `identities`, with `selected` chosen.
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
                    && this.life.saved().from != *identity
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
                .unwrap_or(self.life.saved().from),
        }
    }

    /// Reads the identities the draft can be sent as again whenever the engine says an
    /// account's changed, or came or went (or events were missed), for
    /// [`set_identities`](Self::set_identities).
    fn follow_identities(
        client: &MailClient,
        draft: DraftId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let (client, mut events) = (client.clone(), client.subscribe());
        cx.spawn_in(window, async move |this, cx| {
            loop {
                match events.recv().await {
                    Err(RecvError::Closed) => break,
                    Ok(
                        Event::AccountChanged { .. }
                        | Event::AccountAdded { .. }
                        | Event::AccountRemoved { .. },
                    )
                    | Err(RecvError::Lagged(_)) => {}
                    Ok(_) => continue,
                }
                let (Ok(identities), Ok(Some(stored))) = (
                    client.draft_senders(draft).await,
                    client.get_draft(draft).await,
                ) else {
                    continue;
                };
                let updated = this.update_in(cx, |this, window, cx| {
                    this.set_identities(&identities, stored.identity, window, cx)
                });
                if updated.is_err() {
                    break; // The window is gone.
                }
            }
        })
    }

    /// The identities changed: one was added, made its account's default or removed, or
    /// an account came or went. One removed is replaced by the identity the engine keeps
    /// in the stored draft, `stored` (its account's default, if it was that one); what was
    /// typed stays.
    fn set_identities(
        &mut self,
        identities: &[Identity],
        stored: IdentityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.sender_identity(cx);
        let selected = if identities.iter().any(|i| i.id == current) {
            current
        } else {
            stored
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
        if !self.life.begin_attachment() {
            return;
        }
        let client = self.client.clone();
        let id = self.draft;
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

    /// An attachment change is done; a window waiting to end can go on, or stops if it
    /// failed.
    fn attached(&mut self, failed: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.life.attachment_ended(failed) {
            AfterAttachment::Finish => self.run_finish(window, cx),
            AfterAttachment::Stop(why) => {
                self.stop_closing_all(cx);
                self.error = Some(why.into());
                cx.notify();
            }
            AfterAttachment::Nothing => {}
        }
    }

    fn remove_attachment(
        &mut self,
        attachment: catmail_core::DraftAttachmentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.life.begin_attachment() {
            return;
        }
        let client = self.client.clone();
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
