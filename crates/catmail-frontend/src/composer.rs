//! The composer's model, without its window: what a window starts from, who the draft can
//! be from, the fields as they are saved, and when a window saves, sends and closes.
//!
//! A new message, a reply or a forward is a draft from the start: the engine creates it,
//! and the window saves every change to it (a moment after typing stops), so closing the
//! window or the app loses nothing. A draft nobody touched is deleted again when its
//! window closes.
//!
//! Engine calls for one draft never overlap ([`Lifecycle`]): the save in flight finishes
//! before the next starts, and sending or closing waits for it, so an older save can't
//! land after the content that was sent.

pub mod rich_text;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use catmail_core::{
    AccountId, Address, Draft, DraftBody, DraftChanges, DraftContent, DraftEditor, DraftId,
    EditMark, Forwarding, Identity, IdentityId, MailClient, MessageId, parse_addresses,
};
use futures_channel::oneshot;

use crate::commands::Confirm;
use crate::format;
use crate::mailbox::{AccountEntry, Place};
use rich_text::Images;

/// How long typing has to pause before the draft is saved.
pub const SAVE_DELAY: Duration = Duration::from_millis(800);

/// What a composer window starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeFrom {
    New(AccountId),
    Reply { message: MessageId, all: bool },
    Forward(MessageId),
    Draft(DraftId),
}

/// What a composer window is opened with.
#[derive(Debug, Clone)]
pub struct Opening {
    /// The identities the draft can be sent as, to choose From among.
    pub identities: Vec<Identity>,
    pub draft: Draft,
    /// What the window changes the draft through. Nothing else may while the
    /// [`EditMark`] that [`ComposeFrom::start`] returns with it is held: the window's
    /// owner keeps it for as long as the window is there.
    pub editor: DraftEditor,
    /// Made for this window, see [`Lifecycle::new`].
    pub created_here: bool,
    /// The text has the focus, not To.
    pub focus_body: bool,
    /// The files of the images the draft's HTML shows.
    pub images: Images,
}

impl ComposeFrom {
    /// Makes the draft (or reads it), opens it for editing, and who it can be sent as.
    pub async fn start(self, client: &MailClient) -> catmail_core::Result<(Opening, EditMark)> {
        let id = match self {
            ComposeFrom::New(account) => {
                client
                    .create_draft(account, None, DraftContent::default(), Vec::new())
                    .await?
                    .id
            }
            ComposeFrom::Reply { message, all } => {
                client
                    .reply(message, all, None, String::new(), Vec::new())
                    .await?
                    .id
            }
            ComposeFrom::Forward(message) => {
                client
                    .forward(message, None, Forwarding::default(), Vec::new())
                    .await?
                    .id
            }
            ComposeFrom::Draft(id) => id,
        };
        // Read again as it is opened: as it is from then on, until the window goes.
        let (editor, mark, draft) = client.edit_draft(id).await?;
        let identities = client.draft_senders(draft.id).await?;
        let mut images = Images::default();
        for (id, path) in editor.images().await? {
            images.insert(id, path);
        }
        // Where the writing starts: the text once there is someone to send to.
        let focus_body = match self {
            ComposeFrom::Reply { .. } => true,
            ComposeFrom::Draft(_) => !draft.content.to.is_empty(),
            _ => false,
        };
        let opening = Opening {
            identities,
            draft,
            editor,
            created_here: !matches!(self, ComposeFrom::Draft(_)),
            focus_body,
            images,
        };
        Ok((opening, mark))
    }
}

/// Who a new message is from: the account of the place shown, or else the first.
pub fn new_message_account(place: Option<Place>, accounts: &[AccountEntry]) -> Option<AccountId> {
    place
        .and_then(|place| place.account_in(accounts))
        .or_else(|| accounts.first())
        .map(|entry| entry.account.id)
}

/// The window's title: the message's subject.
pub fn window_title(subject: &str) -> String {
    match subject.trim() {
        "" => "New message".into(),
        subject => subject.into(),
    }
}

/// A choice of From: one of the identities the draft can be sent as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    pub identity: IdentityId,
    pub title: String,
}

/// The identities, as the engine lists them (by account, each one's default first). An
/// address there twice (through two servers) is told apart by its server.
pub fn senders(identities: &[Identity]) -> Vec<Sender> {
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
                title,
            }
        })
        .collect()
}

/// The engine's message, as a sentence for the window.
pub fn error_text(e: &catmail_core::Error) -> String {
    let text = e.to_string();
    let mut chars = text.chars();
    let mut sentence: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !sentence.ends_with('.') {
        sentence.push('.');
    }
    sentence
}

/// What a save hands to the engine: who the draft is from, and what it says.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub from: IdentityId,
    pub content: DraftContent,
}

impl Written {
    /// What the stored draft is.
    pub fn of(draft: &Draft) -> Self {
        Written {
            from: draft.identity,
            content: draft.content.clone(),
        }
    }
}

/// An address field, which an error is shown under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    To,
    Cc,
    Bcc,
}

/// What a field needs changed, and which.
pub type FieldError = (Field, String);

/// The text of the fields, as typed.
#[derive(Debug, Clone)]
pub struct Fields<'a> {
    pub to: &'a str,
    pub cc: &'a str,
    pub bcc: &'a str,
    pub subject: &'a str,
    pub body: DraftBody,
}

/// What asking before discarding a draft says.
pub fn discard_confirm() -> Confirm {
    Confirm {
        title: "Discard this draft?".into(),
        description: "It is deleted here and from the server's Drafts folder.".into(),
        action: "Discard",
    }
}

/// Deleting a saved draft, from its pane, asked first.
pub fn draft_deletion(subject: Option<&str>) -> Confirm {
    Confirm {
        title: format!("Delete the draft “{}”?", format::subject(subject)),
        description: "Its copy in the server's Drafts folder is deleted too.".into(),
        action: "Delete",
    }
}

/// Why a draft written so can't be sent, if it can't.
pub fn unsendable(written: &Written, problem: Option<FieldError>) -> Option<FieldError> {
    problem.or_else(|| {
        written
            .content
            .recipients()
            .next()
            .is_none()
            .then(|| (Field::To, "Add at least one recipient.".into()))
    })
}

/// How the window ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Send,
    Close,
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveState {
    Unchanged,
    Saved,
    Failed(String),
}

/// What to do once a save is back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterSave {
    /// End the window ([`Lifecycle::begin_finish`]): it was waiting for the save.
    Finish,
    /// It failed, or the fields changed meanwhile: save again after a pause.
    SaveLater,
    Nothing,
}

/// What to do once an attachment is added or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterAttachment {
    /// End the window ([`Lifecycle::begin_finish`]): it was waiting for the attachment.
    Finish,
    /// The window doesn't end after all, for this reason: the message isn't what the
    /// user meant to send.
    Stop(String),
    Nothing,
}

/// The saving, sending and closing of one draft's window. The window tells it what the
/// user does, and runs the engine calls it answers with.
#[derive(Debug, Clone)]
pub struct Lifecycle {
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
}

impl Lifecycle {
    /// For a window opened on `opening`'s draft.
    pub fn new(opening: &Opening) -> Self {
        Lifecycle {
            created_here: opening.created_here,
            edited: false,
            dirty: false,
            saving: false,
            attaching: 0,
            save_state: SaveState::Unchanged,
            finishing: None,
            saved: Written::of(&opening.draft),
        }
    }

    /// What was last handed to the engine.
    pub fn saved(&self) -> &Written {
        &self.saved
    }

    /// How the window is ending, once it is: the fields are read-only then, what it
    /// saves or sends was taken when that began.
    pub fn ending(&self) -> Option<Finish> {
        self.finishing
    }

    /// The draft as typed, from `from`. An address field that doesn't parse keeps its
    /// last saved value, and is reported.
    pub fn written(&self, fields: Fields, from: IdentityId) -> (Written, Option<FieldError>) {
        let mut problem = None;
        let mut field = |field: Field, text: &str, saved: &[Address]| match parse_addresses(text) {
            Ok(list) => list,
            Err(e) => {
                problem.get_or_insert((field, error_text(&e)));
                saved.to_vec()
            }
        };
        let saved = &self.saved.content;
        let to = field(Field::To, fields.to, &saved.to);
        let cc = field(Field::Cc, fields.cc, &saved.cc);
        let bcc = field(Field::Bcc, fields.bcc, &saved.bcc);
        let content = DraftContent {
            subject: fields.subject.to_string(),
            to,
            cc,
            bcc,
            body: fields.body,
        };
        (Written { from, content }, problem)
    }

    /// The user changed a field. False while the window is ending: what it sends was
    /// taken already, and the change is ignored. True: save after [`SAVE_DELAY`] without
    /// changes ([`Self::begin_save`]).
    pub fn changed(&mut self) -> bool {
        if self.finishing.is_some() {
            return false;
        }
        self.edited = true;
        self.dirty = true;
        true
    }

    /// Whether a save may start: not while the last one is still with the engine, if
    /// nothing changed, or once the window is ending and saves what is left itself. The
    /// fields need reading only if so.
    pub fn save_due(&self) -> bool {
        !self.saving && self.dirty && self.finishing.is_none()
    }

    /// Starts saving `written`, the fields now: what to hand to `update_draft`, if
    /// anything ([`Self::save_due`], and they changed since the last save).
    pub fn begin_save(&mut self, written: Written) -> Option<Written> {
        if !self.save_due() {
            return None;
        }
        self.dirty = false;
        if written == self.saved {
            return None;
        }
        self.saving = true;
        Some(written)
    }

    /// The save of `written` is back.
    pub fn save_ended(&mut self, written: Written, result: Result<(), String>) -> AfterSave {
        self.saving = false;
        match result {
            Ok(()) => {
                self.saved = written;
                self.save_state = SaveState::Saved;
            }
            Err(e) => {
                self.dirty = true;
                self.save_state = SaveState::Failed(e);
            }
        }
        if self.finishing.is_some() {
            if self.busy() {
                AfterSave::Nothing
            } else {
                AfterSave::Finish
            }
        } else if self.dirty {
            AfterSave::SaveLater
        } else {
            AfterSave::Nothing
        }
    }

    /// Starts adding or removing attachments; false while the window is ending (the
    /// change isn't made).
    pub fn begin_attachment(&mut self) -> bool {
        if self.finishing.is_some() {
            return false;
        }
        self.edited = true;
        self.attaching += 1;
        true
    }

    /// An attachment change is done; a window waiting to end can go on. If it failed, a
    /// send or close waiting for it stops instead, and the window stays to show why.
    pub fn attachment_ended(&mut self, failed: bool) -> AfterAttachment {
        self.attaching -= 1;
        let Some(how) = self.finishing else {
            return AfterAttachment::Nothing;
        };
        if failed && how != Finish::Discard {
            self.finishing = None;
            AfterAttachment::Stop(
                match how {
                    Finish::Send => "Not sent: check the attachments first.",
                    _ => "Left open: check the attachments first.",
                }
                .into(),
            )
        } else if self.busy() {
            AfterAttachment::Nothing
        } else {
            AfterAttachment::Finish
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

    /// What asking the window to close does (`unparsed`: an address field doesn't parse).
    /// Even with everything saved it ends through [`Self::finish`]: the images taken out
    /// of the text go then ([`Steps::drop_images`]).
    pub fn close(&self, unparsed: bool) -> Closing {
        if self.finishing.is_some() {
            Closing::Wait
        } else if unparsed {
            // A field that doesn't parse was never saved (what is saved always parses
            // back, see `format_addresses`): closing would lose what was typed there.
            Closing::Fix
        } else {
            Closing::Finish
        }
    }

    /// The window ends `how`. True: go on now ([`Self::begin_finish`]); false: it is
    /// ending already, or waits for the save or attachments in flight, and the answer
    /// they get says when to go on.
    pub fn finish(&mut self, how: Finish) -> bool {
        if self.finishing.is_some() {
            return false;
        }
        self.finishing = Some(how);
        !self.busy()
    }

    /// The engine calls that end the window, with the fields `written`; run them in the
    /// order of [`Steps`], then [`Self::finish_ended`].
    pub fn begin_finish(&mut self, written: &Written) -> Option<Steps> {
        let how = self.finishing?;
        self.saving = true;
        Some(steps(how, self.untouched(), *written != self.saved))
    }

    /// The window's end is back: on `Ok` it goes; on an error it stays, showing what this
    /// gives.
    pub fn finish_ended(&mut self, result: Result<(), String>) -> Result<(), String> {
        self.saving = false;
        let Err(e) = result else {
            return Ok(());
        };
        let how = self.finishing.take();
        Err(match how {
            Some(Finish::Send) => format!("Couldn't send: {e}"),
            Some(Finish::Discard) => format!("Couldn't discard the draft: {e}"),
            Some(Finish::Close) | None => format!("Couldn't save the draft: {e}"),
        })
    }

    /// What the window says of the saving, if anything.
    pub fn status(&self) -> Option<Cow<'static, str>> {
        let ending = self.finishing.is_some();
        match (&self.save_state, self.saving && !ending) {
            (_, true) => Some("Saving…".into()),
            (SaveState::Saved, _) if !self.dirty => Some("Draft saved".into()),
            (SaveState::Failed(e), _) => Some(format!("Not saved: {e}").into()),
            _ => None,
        }
    }
}

/// What asking the window to close does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    /// It is ending already, and closes itself when that is done.
    Wait,
    /// It stays, showing the field to fix.
    Fix,
    /// It closes once what is left is saved, or the untouched draft deleted
    /// ([`Lifecycle::finish`] with [`Finish::Close`]).
    Finish,
}

/// The engine calls that end the window, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steps {
    /// `delete_draft`, instead of the others.
    pub delete: bool,
    /// `update_draft` with what the fields hold.
    pub save: bool,
    /// `send_draft`.
    pub send: bool,
    /// `drop_unshown_images`: the window's undo, which could bring them back, goes with
    /// it. Not when sending: what is sent has only the images its HTML shows, and the
    /// draft goes once sent; a send that fails leaves the window, and its undo, open.
    pub drop_images: bool,
}

impl Steps {
    /// Runs the calls on `editor`'s draft, saving `written`.
    pub async fn run(
        self,
        client: &MailClient,
        editor: &DraftEditor,
        written: Written,
    ) -> catmail_core::Result<()> {
        let draft = editor.draft();
        if self.delete {
            return client.delete_draft(draft).await;
        }
        if self.save {
            let changes = DraftChanges::all(written.from, written.content);
            editor.update(changes).await?;
        }
        if self.drop_images
            && let Err(e) = editor.drop_unshown_images().await
        {
            // Left for the next time the window closes; nothing is lost meanwhile.
            tracing::warn!(draft = %draft, "couldn't remove the images taken out of the text: {e}");
        }
        if self.send {
            editor.send().await?;
        }
        Ok(())
    }
}

fn steps(how: Finish, untouched: bool, unsaved: bool) -> Steps {
    // Sending an untouched draft (a reply that needs no words) still sends.
    let delete = how == Finish::Discard || (how == Finish::Close && untouched);
    Steps {
        delete,
        save: !delete && unsaved,
        send: how == Finish::Send,
        drop_images: how == Finish::Close && !delete,
    }
}

/// The composer windows open, by draft: opening a draft again brings its window forward
/// instead of a second editor of the same draft. `W` is what the frontend reaches a
/// window by.
#[derive(Debug)]
pub struct OpenComposers<W> {
    windows: HashMap<DraftId, W>,
    /// Drafts being read for a window: opening one again meanwhile does nothing, the
    /// window is on its way.
    opening: HashSet<DraftId>,
    /// Told whether they all closed, while [`Self::close_all`] waits for them.
    closing_all: Option<oneshot::Sender<bool>>,
}

impl<W> Default for OpenComposers<W> {
    fn default() -> Self {
        OpenComposers {
            windows: HashMap::new(),
            opening: HashSet::new(),
            closing_all: None,
        }
    }
}

impl<W: Clone> OpenComposers<W> {
    /// The draft's window, if it has one.
    pub fn window(&self, draft: DraftId) -> Option<W> {
        self.windows.get(&draft).cloned()
    }

    /// The draft's window starts opening (it is read first); false when it is on its way
    /// already.
    pub fn begin_opening(&mut self, draft: DraftId) -> bool {
        self.opening.insert(draft)
    }

    /// The draft was read (or couldn't be): opening it again opens a window again.
    pub fn end_opening(&mut self, draft: DraftId) {
        self.opening.remove(&draft);
    }

    /// The draft's window opened.
    pub fn insert(&mut self, draft: DraftId, window: W) {
        self.windows.insert(draft, window);
    }

    /// The draft's window closed; the last one tells [`Self::close_all`] they all did.
    pub fn remove(&mut self, draft: DraftId) {
        self.windows.remove(&draft);
        if self.windows.is_empty()
            && let Some(closed) = self.closing_all.take()
        {
            closed.send(true).ok();
        }
    }

    /// A window stays open to be dealt with: closing them all failed.
    pub fn stop_closing_all(&mut self) {
        if let Some(closed) = self.closing_all.take() {
            closed.send(false).ok();
        }
    }

    /// Starts closing every window once it has saved what it holds (closing straight
    /// away would lose what was typed since the last save): the windows to ask to close,
    /// and the answer, true once the last has closed and false as soon as one can't (an
    /// address to correct, a save that failed), which stays open.
    pub fn close_all(&mut self) -> (Vec<W>, oneshot::Receiver<bool>) {
        let (closed, answer) = oneshot::channel();
        if self.windows.is_empty() {
            closed.send(true).ok();
            return (Vec::new(), answer);
        }
        self.closing_all = Some(closed);
        (self.windows.values().cloned().collect(), answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_waits_fixes_or_finishes() {
        let mut life = lifecycle();
        assert_eq!(life.close(true), Closing::Fix);
        // Everything saved, it still ends through the steps: they drop the images.
        assert_eq!(life.close(false), Closing::Finish);
        assert!(life.finish(Finish::Close));
        assert_eq!(life.close(true), Closing::Wait);
    }

    #[test]
    fn the_window_ends_with_these_calls() {
        let steps_are = |delete, save, send, drop_images| Steps {
            delete,
            save,
            send,
            drop_images,
        };
        #[rustfmt::skip]
        let cases = [
            // how, untouched, unsaved
            (Finish::Close, false, false, steps_are(false, false, false, true)),
            (Finish::Close, false, true, steps_are(false, true, false, true)),
            (Finish::Close, true, false, steps_are(true, false, false, false)),
            (Finish::Send, false, true, steps_are(false, true, true, false)),
            // A reply that needs no words still goes.
            (Finish::Send, true, false, steps_are(false, false, true, false)),
            (Finish::Discard, false, true, steps_are(true, false, false, false)),
        ];
        for (how, untouched, unsaved, expected) in cases {
            assert_eq!(
                steps(how, untouched, unsaved),
                expected,
                "{how:?} {untouched} {unsaved}"
            );
        }
    }

    fn lifecycle() -> Lifecycle {
        Lifecycle {
            created_here: true,
            edited: false,
            dirty: false,
            saving: false,
            attaching: 0,
            save_state: SaveState::Unchanged,
            finishing: None,
            saved: Written {
                from: IdentityId(1),
                content: DraftContent::default(),
            },
        }
    }

    fn with_subject(subject: &str) -> Written {
        Written {
            from: IdentityId(1),
            content: DraftContent {
                subject: subject.into(),
                ..DraftContent::default()
            },
        }
    }

    #[test]
    fn a_send_waits_for_the_save_in_flight() {
        let mut life = lifecycle();
        assert!(life.changed());
        let saving = life.begin_save(with_subject("a")).unwrap();
        // Typing goes on, and the user sends before the save is back.
        assert!(life.changed());
        assert!(!life.finish(Finish::Send));
        // The fields are taken: further changes are ignored.
        assert!(!life.changed());
        assert_eq!(life.save_ended(saving, Ok(())), AfterSave::Finish);
        let steps = life.begin_finish(&with_subject("ab")).unwrap();
        assert_eq!(
            steps,
            Steps {
                delete: false,
                save: true,
                send: true,
                drop_images: false,
            }
        );
        assert_eq!(life.finish_ended(Ok(())), Ok(()));
    }

    #[test]
    fn one_save_at_a_time_and_a_failed_one_again() {
        let mut life = lifecycle();
        life.changed();
        let saving = life.begin_save(with_subject("a")).unwrap();
        life.changed();
        assert_eq!(life.begin_save(with_subject("ab")), None);
        assert_eq!(life.status().as_deref(), Some("Saving…"));
        assert_eq!(
            life.save_ended(saving, Err("Offline.".into())),
            AfterSave::SaveLater
        );
        assert_eq!(life.status().as_deref(), Some("Not saved: Offline."));
        let saving = life.begin_save(with_subject("ab")).unwrap();
        assert_eq!(life.save_ended(saving, Ok(())), AfterSave::Nothing);
        assert_eq!(life.status().as_deref(), Some("Draft saved"));
        // What was saved needs no save.
        life.changed();
        assert_eq!(life.begin_save(with_subject("ab")), None);
    }

    #[test]
    fn a_failed_attachment_stops_the_send_waiting_for_it() {
        let mut life = lifecycle();
        assert!(life.begin_attachment());
        assert!(!life.finish(Finish::Send));
        assert_eq!(
            life.attachment_ended(true),
            AfterAttachment::Stop("Not sent: check the attachments first.".into())
        );
        assert_eq!(life.ending(), None);
        // A discard goes on regardless.
        assert!(life.begin_attachment());
        assert!(!life.finish(Finish::Discard));
        assert_eq!(life.attachment_ended(true), AfterAttachment::Finish);
    }

    #[test]
    fn an_untouched_draft_goes_with_its_window() {
        let mut life = lifecycle();
        let fields = life.saved().clone();
        assert_eq!(life.close(false), Closing::Finish);
        assert!(life.finish(Finish::Close));
        let steps = life.begin_finish(&fields).unwrap();
        assert!(steps.delete);
        assert_eq!(
            life.finish_ended(Err("gone".into())),
            Err("Couldn't save the draft: gone".into())
        );
        assert_eq!(life.ending(), None);
    }

    #[test]
    fn an_address_that_doesnt_parse_keeps_the_saved_one() {
        let mut life = lifecycle();
        life.saved.content.to = parse_addresses("a@example.com").unwrap();
        let fields = Fields {
            to: "not an address <",
            cc: "b@example.com",
            bcc: "",
            subject: "Hi",
            body: DraftBody::default(),
        };
        let (written, problem) = life.written(fields, IdentityId(2));
        assert_eq!(written.content.to, life.saved.content.to);
        assert_eq!(written.content.cc[0].email, "b@example.com");
        assert_eq!(written.from, IdentityId(2));
        assert_eq!(problem.map(|(field, _)| field), Some(Field::To));
        assert_eq!(life.close(true), Closing::Fix);
    }

    #[test]
    fn no_recipient_is_not_sendable() {
        let written = with_subject("Hi");
        assert_eq!(
            unsendable(&written, None).map(|(field, _)| field),
            Some(Field::To)
        );
    }

    #[test]
    fn close_all_answers_when_the_last_window_goes_or_one_stays() {
        let mut open = OpenComposers::<u32>::default();
        let (windows, mut answer) = open.close_all();
        assert!(windows.is_empty());
        assert_eq!(answer.try_recv(), Ok(Some(true)));

        open.insert(DraftId(1), 1);
        open.insert(DraftId(2), 2);
        let (windows, mut answer) = open.close_all();
        assert_eq!(windows.len(), 2);
        open.remove(DraftId(1));
        assert_eq!(answer.try_recv(), Ok(None));
        open.remove(DraftId(2));
        assert_eq!(answer.try_recv(), Ok(Some(true)));

        open.insert(DraftId(3), 3);
        let (_, mut answer) = open.close_all();
        open.stop_closing_all();
        assert_eq!(answer.try_recv(), Ok(Some(false)));
    }

    #[test]
    fn a_draft_opens_once_at_a_time() {
        let mut open = OpenComposers::<u32>::default();
        assert!(open.begin_opening(DraftId(1)));
        assert!(!open.begin_opening(DraftId(1)));
        open.end_opening(DraftId(1));
        assert!(open.begin_opening(DraftId(1)));
    }

    #[test]
    fn titles_and_sentences() {
        assert_eq!(window_title("  "), "New message");
        assert_eq!(window_title(" Hi "), "Hi");
        let e = catmail_core::Error::DraftNotFound(DraftId(1));
        let text = error_text(&e);
        assert!(text.ends_with('.') && text.starts_with(char::is_uppercase));
    }
}
