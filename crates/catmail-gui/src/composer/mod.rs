//! Writing a message, each in its own window: From, To, Cc, Bcc, Subject, the text, the
//! attachments and the bar of commands. When it saves, sends and closes is
//! `catmail_frontend::composer`'s ([`Lifecycle`]); here is what the window does with it,
//! `view.rs` how it looks.
//!
//! A composer's view runs on its window's own nsh thread; the window is the shell's
//! (`shell::opened`), and closing it asks the view, which saves first and then
//! closes it (`shell::composer_closed`). State is nsh globals, the view's own.

mod view;

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use catmail_core::{
    DraftAttachment, DraftAttachmentId, DraftId, Event, Identity, IdentityId, RecvError,
};
use catmail_frontend::composer::{
    AfterAttachment, AfterSave, Closing, ComposeFrom, Field, FieldError, Fields, Finish, Lifecycle,
    Opening, SAVE_DELAY, Sender, Written, discard_confirm, error_text, new_message_account,
    senders, unsendable,
};
use futures_channel::mpsc;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::{ElementRef, FileDropEvent, Owner, Spawned, pick_paths};
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;

use crate::engine::client;
use crate::form::confirmed;
use crate::mailbox::list;
use crate::mailbox::sidebar::{SelectedPlace, with_accounts};
use crate::nsh_platform::Root;
use crate::shell::{self, on_main};
use crate::tasks::in_view;
use crate::toast::report;

use view::Composer;

/// Opens a composer window for `from`, from the mailbox. The draft is made (or read)
/// first; failures are reported in the mailbox.
pub fn open(from: ComposeFrom) {
    in_view(async move {
        if on_main(move || shell::begin_opening(from)).await != Some(true) {
            return;
        }
        let opened = from.start(&client()).await;
        if let Err(e) = &opened {
            report("Couldn't start the message", e);
        }
        let _ = on_main(move || shell::opened(from, opened.ok())).await;
    });
}

/// A new message, from the account of the place shown (or else the first).
pub fn new_message() {
    let place = global::<SelectedPlace>().with_untracked(|p| p.0);
    let account = with_accounts(|accounts| new_message_account(place, accounts));
    if let Some(account) = account {
        open(ComposeFrom::New(account));
    }
}

/// A reply to the message open, to its sender or to everyone it went to.
pub fn reply(all: bool) {
    if let Some(message) = untracked(list::selected_id) {
        open(ComposeFrom::Reply { message, all });
    }
}

pub fn forward() {
    if let Some(message) = untracked(list::selected_id) {
        open(ComposeFrom::Forward(message));
    }
}

/// The keys that start a message.
pub fn bind_keys() {
    on_key("mod+n", |_| new_message());
    on_key("mod+r", |_| reply(false));
    on_key("mod+shift+r", |_| reply(true));
    on_key("mod+shift+f", |_| forward());
}

// --- The view's state ---------------------------------------------------------------

/// What a composer's view is made with ([`root`]).
pub struct Start {
    pub opening: Opening,
    /// The window's asks to close: its close button, Ctrl+W, quitting.
    pub close_requests: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
}

thread_local! {
    /// The start of the view on this thread: nsh's props are values JS has, so it comes
    /// this way. Each view has a thread of its own.
    static START: RefCell<Option<Arc<Start>>> = const { RefCell::new(None) };
}

/// The root of a composer window's view.
pub fn root(start: Arc<Start>) -> Root {
    Root::new::<Composer>(move || {
        START.set(Some(start.clone()));
        Default::default()
    })
}

/// The draft and when it saves. Not reactive.
struct Composing {
    draft: DraftId,
    life: Lifecycle,
    /// The save after typing pauses.
    debounce: Option<Spawned<()>>,
    /// The view's, which the work it starts belongs to.
    owner: Owner,
    /// The address fields, to put the focus in one to fix.
    inputs: [Option<ElementRef>; 3],
}

#[derive(Clone, Default)]
struct Shared(Option<Rc<RefCell<Composing>>>);

fn composing() -> Rc<RefCell<Composing>> {
    global::<Shared>()
        .with_untracked(|s| s.0.clone())
        .expect("the view sets it first")
}

fn with_life<T>(f: impl FnOnce(&mut Lifecycle) -> T) -> T {
    f(&mut composing().borrow_mut().life)
}

fn draft() -> DraftId {
    composing().borrow().draft
}

/// Runs `future` for as long as the window is open.
fn in_window(future: impl Future<Output = ()> + 'static) -> Spawned<()> {
    let owner = composing().borrow().owner;
    owner.run(|| spawn(future))
}

/// The fields as typed: what a save takes. The inputs keep their own text.
#[derive(Clone, Default)]
struct Typed {
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    text: String,
}

/// Who the draft can be from, and who it is.
#[derive(Clone, Default)]
struct From {
    senders: Vec<Sender>,
    chosen: Option<IdentityId>,
}

/// How the window is doing.
#[derive(Clone, Default, PartialEq)]
struct Shown {
    ending: Option<Finish>,
    status: Option<Cow<'static, str>>,
    /// Why the send or close the window was ending with didn't happen.
    error: Option<String>,
    /// A field that doesn't hold addresses, shown under it once sending (or closing)
    /// needs it to.
    field_error: Option<FieldError>,
    cc_bcc: bool,
}

#[derive(Clone, Default)]
struct Attachments(Vec<DraftAttachment>);

fn ending() -> bool {
    global::<Shown>().with(|s| s.ending.is_some())
}

/// Shows what the lifecycle says of the saving and the ending.
fn sync() {
    let (ending, status) = with_life(|l| (l.ending(), l.status()));
    global::<Shown>().update(|s| {
        s.ending = ending;
        s.status = status;
    });
}

// --- Saving, sending, closing -------------------------------------------------------

/// The draft as typed. An address field that doesn't parse keeps its last saved value,
/// and is reported.
fn written() -> (Written, Option<FieldError>) {
    let typed = global::<Typed>().with_untracked(Clone::clone);
    let chosen = global::<From>().with_untracked(|f| f.chosen);
    let composing = composing();
    let composing = composing.borrow();
    let from = chosen.unwrap_or(composing.life.saved().from);
    let fields = Fields {
        to: &typed.to,
        cc: &typed.cc,
        bcc: &typed.bcc,
        subject: &typed.subject,
        text: &typed.text,
    };
    composing.life.written(fields, from)
}

/// A field changed: the draft is saved once typing pauses.
fn changed() {
    if !with_life(Lifecycle::changed) {
        return;
    }
    global::<Shown>().update(|s| s.field_error = None);
    let save_later = in_window(async {
        timer(SAVE_DELAY).await;
        save();
    });
    if let Some(earlier) = composing().borrow_mut().debounce.replace(save_later) {
        earlier.cancel();
    }
    sync();
}

fn save() {
    if !with_life(|l| l.save_due()) {
        return;
    }
    let (written, _) = written();
    let Some(written) = with_life(|l| l.begin_save(written)) else {
        return;
    };
    sync();
    let draft = draft();
    in_window(async move {
        let Written { from, content } = written.clone();
        let result = client().update_draft(draft, from, content).await;
        let result = result.map(drop).map_err(|e| error_text(&e));
        match with_life(|l| l.save_ended(written, result)) {
            AfterSave::Finish => run_finish(),
            AfterSave::SaveLater => changed(),
            AfterSave::Nothing => {}
        }
        sync();
    });
}

fn send() {
    let (written, problem) = written();
    match unsendable(&written, problem) {
        Some(problem) => show_field_error(problem),
        None => finish(Finish::Send),
    }
}

/// The window asks to close: now if everything is saved, after saving what isn't.
fn close_requested() {
    let (written, problem) = written();
    match with_life(|l| l.close(&written, problem.is_some())) {
        Closing::Wait => {}
        Closing::Fix => {
            if let Some(problem) = problem {
                show_field_error(problem);
            }
            stop_closing_all();
        }
        Closing::Now => close_window(),
        Closing::Finish => finish(Finish::Close),
    }
}

fn discard() {
    let answer = confirmed(discard_confirm());
    in_window(async move {
        if answer.await {
            finish(Finish::Discard);
        }
    });
}

fn finish(how: Finish) {
    if with_life(|l| l.ending().is_some()) {
        return;
    }
    global::<Shown>().update(|s| s.error = None);
    if let Some(save_later) = composing().borrow_mut().debounce.take() {
        save_later.cancel();
    }
    if with_life(|l| l.finish(how)) {
        run_finish();
    }
    sync();
}

/// Saves what is left, then sends, deletes or just closes; the window goes when that
/// worked.
fn run_finish() {
    let (written, _) = written();
    let Some(steps) = with_life(|l| l.begin_finish(&written)) else {
        return;
    };
    sync();
    let draft = draft();
    in_window(async move {
        let result = steps.run(&client(), draft, written).await;
        match with_life(|l| l.finish_ended(result.map_err(|e| e.to_string()))) {
            Ok(()) => close_window(),
            Err(e) => {
                stop_closing_all();
                global::<Shown>().update(|s| s.error = Some(e));
            }
        }
        sync();
    });
}

fn close_window() {
    let draft = draft();
    drop(on_main(move || shell::composer_closed(draft)));
}

/// The window stays open to be dealt with: quitting, which waits for every composer to
/// close, doesn't.
fn stop_closing_all() {
    drop(on_main(shell::stop_closing_all));
}

fn show_field_error((field, message): FieldError) {
    global::<Shown>().update(|s| {
        if matches!(field, Field::Cc | Field::Bcc) {
            s.cc_bcc = true;
        }
        s.field_error = Some((field, message));
    });
    focus(field);
}

/// Puts the focus in `field`, once it shows (Cc and Bcc may have just appeared).
fn focus(field: Field) {
    in_window(async move {
        timer(std::time::Duration::ZERO).await;
        let input = composing().borrow().inputs[field_index(field)];
        if let Some(input) = input {
            input.focus();
        }
    });
}

fn field_index(field: Field) -> usize {
    match field {
        Field::To => 0,
        Field::Cc => 1,
        Field::Bcc => 2,
    }
}

// --- Attachments --------------------------------------------------------------------

fn attach() {
    let picked = pick_paths(OpenDialog::files().multiple(true).button_label("Attach"));
    in_window(async move {
        match picked.await {
            Ok(Some(paths)) => attach_paths(paths),
            Ok(None) => {}
            Err(e) => report("Couldn't choose files", e),
        }
    });
}

/// Dropped on the window: its `file:` URLs, as paths.
fn dropped(event: &FileDropEvent) {
    let paths = event
        .uris
        .iter()
        .filter_map(|uri| url::Url::parse(uri).ok()?.to_file_path().ok())
        .collect::<Vec<_>>();
    if !paths.is_empty() {
        attach_paths(paths);
    }
}

fn attach_paths(paths: Vec<PathBuf>) {
    if !with_life(Lifecycle::begin_attachment) {
        return;
    }
    let draft = draft();
    in_window(async move {
        let client = client();
        let mut failed = false;
        for path in paths {
            match client.attach_file(draft, path.clone()).await {
                Ok(attachment) => global::<Attachments>().update(|a| a.0.push(attachment)),
                Err(e) => {
                    failed = true;
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    report(&format!("Couldn't attach {name}"), e);
                }
            }
        }
        attached(failed);
    });
}

fn remove_attachment(attachment: DraftAttachmentId) {
    if !with_life(Lifecycle::begin_attachment) {
        return;
    }
    in_window(async move {
        let result = client().remove_draft_attachment(attachment).await;
        let failed = result.is_err();
        match result {
            Ok(draft) => global::<Attachments>().set(Attachments(draft.attachments)),
            Err(e) => report("Couldn't remove the attachment", e),
        }
        attached(failed);
    });
}

/// An attachment change is done; a window waiting to end goes on, or stays if it failed.
fn attached(failed: bool) {
    match with_life(|l| l.attachment_ended(failed)) {
        AfterAttachment::Finish => run_finish(),
        AfterAttachment::Stop(why) => {
            stop_closing_all();
            global::<Shown>().update(|s| s.error = Some(why));
        }
        AfterAttachment::Nothing => {}
    }
    sync();
}

// --- From ---------------------------------------------------------------------------

/// Reads the identities the draft can be sent as again whenever the engine says an
/// account's changed, or came or went (or events were missed).
async fn follow_identities(draft: DraftId) {
    let client = client();
    let mut events = client.subscribe();
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
        set_identities(&identities, stored.identity);
    }
}

/// The identities changed. One removed is replaced by the identity the engine keeps in
/// the stored draft, `stored`; what was typed stays.
fn set_identities(identities: &[Identity], stored: IdentityId) {
    let saved = with_life(|l| l.saved().from);
    global::<From>().update(|f| {
        let current = f.chosen.unwrap_or(saved);
        f.chosen = Some(if identities.iter().any(|i| i.id == current) {
            current
        } else {
            stored
        });
        f.senders = senders(identities);
    });
}

fn choose_sender() {
    let (senders, chosen) = global::<From>().with_untracked(|f| (f.senders.clone(), f.chosen));
    let items = senders
        .into_iter()
        .map(|sender| {
            let identity = sender.identity;
            MenuItem::new(sender.title)
                .checked(Some(identity) == chosen)
                .on_select(move || {
                    global::<From>().update(|f| f.chosen = Some(identity));
                    if with_life(|l| l.saved().from != identity) {
                        changed();
                    }
                })
        })
        .collect();
    drop(popup_menu(items));
}
