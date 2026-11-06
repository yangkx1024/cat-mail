//! Writing a message, each in its own window, as the GUI's (`composer/`): From, To, Cc,
//! Bcc, Subject, the text, the attachments and the bar of commands. When it saves, sends
//! and closes is `catmail_frontend::composer`'s ([`Lifecycle`]); this is the view.
//!
//! A composer's view runs on its window's own nsh thread; the window is the shell's
//! (`shell::opened`), and closing it asks the view, which saves first and then
//! closes it (`shell::composer_closed`). State is nsh globals, the view's own.

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use catmail_core::{
    DraftAttachment, DraftAttachmentId, DraftId, Event, Identity, IdentityId, RecvError,
    format_addresses,
};
use catmail_frontend::composer::{
    AfterAttachment, AfterSave, Closing, ComposeFrom, Field, FieldError, Fields, Finish, Lifecycle,
    Opening, SAVE_DELAY, Sender, Written, discard_confirm, error_text, new_message_account,
    senders, unsendable, window_title,
};
use catmail_frontend::format;
use futures_channel::mpsc;
use futures_util::StreamExt as _;
use nsh_base::{alert_dialog, theme};
use nsh_core::element::desktop::{MenuItem, popup_menu, set_title};
use nsh_core::element::{ElementRef, FileDropEvent, Owner, Spawned, input, pick_paths};
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox};
use crate::chrome;
use crate::commands::with_accounts;
use crate::commands::{Size, icon_button, primary_button};
use crate::icons::{Icon, icon};
use crate::list;
use crate::nsh_platform::Root;
use crate::shell::{self, on_main};
use crate::sidebar::SelectedPlace;
use crate::toast::{self, Level, notify};

/// Opens a composer window for `from`, from the mailbox. The draft is made (or read)
/// first; failures are reported in the mailbox.
pub fn open(from: ComposeFrom) {
    in_mailbox(async move {
        if on_main(move || shell::begin_opening(from)).await != Some(true) {
            return;
        }
        let opened = from.start(&client()).await;
        if let Err(e) = &opened {
            notify(Level::Error, format!("Couldn't start the message: {e}"));
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

/// The keys that start a message, as the GUI binds them.
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
    let confirm = discard_confirm();
    let answer = alert_dialog()
        .title(confirm.title)
        .description(confirm.description)
        .cancel("cancel", "Cancel")
        .destructive("discard", "Discard")
        .show();
    in_window(async move {
        if answer
            .await
            .is_ok_and(|r| r.action.as_deref() == Some("discard"))
        {
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
            Err(e) => notify(Level::Error, format!("Couldn't choose files: {e}")),
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
                    notify(Level::Error, format!("Couldn't attach {name}: {e}"));
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
            Err(e) => notify(Level::Error, format!("Couldn't remove the attachment: {e}")),
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

// --- The window ---------------------------------------------------------------------

#[component]
fn Composer() -> impl IntoElement {
    let start = START
        .with_borrow(Clone::clone)
        .expect("a composer's view is made by `root`");
    let Opening {
        identities,
        draft,
        focus_body,
        ..
    } = start.opening.clone();
    let content = &draft.content;
    crate::app::own_tasks(Owner::current());
    crate::settings::follow();
    crate::settings::bind_keys(false);
    global::<Shared>().set(Shared(Some(Rc::new(RefCell::new(Composing {
        draft: draft.id,
        life: Lifecycle::new(&start.opening),
        debounce: None,
        owner: Owner::current(),
        inputs: [None; 3],
    })))));
    global::<Typed>().set(Typed {
        to: format_addresses(&content.to),
        cc: format_addresses(&content.cc),
        bcc: format_addresses(&content.bcc),
        subject: content.subject.clone(),
        text: content.text.clone(),
    });
    global::<From>().set(From {
        senders: senders(&identities),
        chosen: Some(draft.identity),
    });
    global::<Attachments>().set(Attachments(draft.attachments.clone()));
    global::<Shown>().set(Shown {
        cc_bcc: !content.cc.is_empty() || !content.bcc.is_empty(),
        ..Shown::default()
    });
    let close_requests = start
        .close_requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(mut asks) = close_requests {
        spawn(async move {
            while asks.next().await.is_some() {
                close_requested();
            }
        });
    }
    spawn(follow_identities(draft.id));
    // The window is named after the subject.
    let title = computed(move || window_title(&global::<Typed>().with(|t| t.subject.clone())));
    effect(move || set_title(title.get()));
    // As the GUI binds them; Ctrl+W is the window's, which asks the view to close.
    on_key("mod+enter", |_| send());
    on_key("escape", |_| close_requested());
    on_key("mod+shift+a", |_| attach());

    view()
        .flex_1()
        .bg(move || theme().background)
        .on_file_drop(dropped)
        .child(chrome::title_bar_of(title))
        .child(error())
        .child(fields(focus_body))
        .child(body(focus_body))
        .child(attachments())
        .child(footer())
        .child(toast::toasts())
}

fn error() -> impl IntoElement {
    let error = move || global::<Shown>().with(|s| s.error.clone());
    when(
        move || error().is_some(),
        move || {
            view()
                .mx(px(20.0))
                .mt(px(12.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(move || theme().destructive)
                .child(
                    text(move || error().unwrap_or_default())
                        .text_sm()
                        .selectable(true)
                        .text_color(move || theme().destructive),
                )
        },
    )
}

/// The field's name in a column, then the field.
fn field_row(label: &'static str) -> View {
    view()
        .flex_row()
        .items_center()
        .h(px(40.0))
        .gap(px(12.0))
        .child(
            text(label)
                .w(px(64.0))
                .flex_shrink_0()
                .text_sm()
                .text_color(move || theme().muted_foreground),
        )
}

/// A text field of the header, borderless in its row. Read-only while the window is
/// ending: what it is saving or sending was taken when that began.
fn header_input(value: String, on_change: impl Fn(&mut Typed, String) + 'static) -> Input {
    input()
        .default_value(value)
        .on_change(move |text: &str| {
            global::<Typed>().update(|t| on_change(t, text.to_owned()));
            changed();
        })
        .readonly(ending)
        .flex_1()
        .text_sm()
        .text_color(move || theme().foreground)
}

fn fields(focus_body: bool) -> impl IntoElement {
    let typed = global::<Typed>().with_untracked(Clone::clone);
    let border = move || theme().border;
    let cc_bcc = move || global::<Shown>().with(|s| s.cc_bcc);
    let show_cc_bcc = move || {
        global::<Shown>().update(|s| s.cc_bcc = true);
        focus(Field::Cc);
    };
    view()
        .flex_shrink_0()
        .px(px(20.0))
        .child(
            field_row("From")
                .border_b_1()
                .border_color(border)
                .child(from()),
        )
        .child(address_row(
            "To",
            Field::To,
            typed.to,
            !focus_body,
            when(
                move || !cc_bcc(),
                move || {
                    view()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .on_press(show_cc_bcc)
                        .child(
                            text("Cc, Bcc")
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || theme().foreground),
                        )
                },
            )
            .into_element(),
        ))
        .child(when(cc_bcc, move || {
            let typed = global::<Typed>().with_untracked(Clone::clone);
            view()
                .child(address_row(
                    "Cc",
                    Field::Cc,
                    typed.cc,
                    false,
                    view().into_element(),
                ))
                .child(address_row(
                    "Bcc",
                    Field::Bcc,
                    typed.bcc,
                    false,
                    view().into_element(),
                ))
        }))
        .child(
            field_row("Subject")
                .border_b_1()
                .border_color(border)
                .child(
                    header_input(typed.subject, |t, text| t.subject = text)
                        .placeholder("Subject".to_owned()),
                ),
        )
}

/// From: who the draft is from, or a choice when it can be sent as more than one
/// identity.
fn from() -> impl IntoElement {
    let title = move || {
        global::<From>().with(|f| {
            f.senders
                .iter()
                .find(|s| Some(s.identity) == f.chosen)
                .map(|s| s.title.clone())
                .unwrap_or_default()
        })
    };
    let choice = move || global::<From>().with(|f| f.senders.len() > 1);
    view()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .min_w(px(0.0))
        .opacity(move || if choice() && ending() { 0.4_f32 } else { 1.0 })
        .on_press(move || {
            if untracked(choice) && !untracked(ending) {
                choose_sender();
            }
        })
        .child(
            text(title)
                .number_of_lines(1)
                .flex_shrink(1.0)
                .min_w(px(0.0))
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(when(choice, || {
            icon(|| Icon::ChevronDown, 12.0).text_color(move || theme().muted_foreground)
        }))
}

fn address_row(
    label: &'static str,
    field: Field,
    value: String,
    auto_focus: bool,
    trailing: AnyElement,
) -> impl IntoElement {
    let input_ref = element_ref();
    composing().borrow_mut().inputs[field_index(field)] = Some(input_ref);
    let error = move || {
        global::<Shown>().with(|s| {
            s.field_error
                .as_ref()
                .filter(|(f, _)| *f == field)
                .map(|(_, message)| message.clone())
        })
    };
    let on_change = move |t: &mut Typed, text: String| match field {
        Field::To => t.to = text,
        Field::Cc => t.cc = text,
        Field::Bcc => t.bcc = text,
    };
    view()
        .border_b_1()
        .border_color(move || theme().border)
        .child(
            field_row(label)
                .child(
                    header_input(value, on_change)
                        .auto_focus(auto_focus)
                        .element_ref(&input_ref),
                )
                .child(trailing),
        )
        .child(when(
            move || error().is_some(),
            move || {
                text(move || error().unwrap_or_default())
                    // Under the field, past the label column and the row's gap.
                    .pl(px(76.0))
                    .pb(px(8.0))
                    .text_xs()
                    .text_color(move || theme().destructive)
            },
        ))
}

fn body(focus_body: bool) -> impl IntoElement {
    let text_ = global::<Typed>().with_untracked(|t| t.text.clone());
    let mut body = input()
        .multiline(true)
        .default_value(text_)
        .on_change(|text: &str| {
            global::<Typed>().update(|t| t.text = text.to_owned());
            changed();
        })
        .readonly(ending)
        .auto_focus(focus_body)
        .flex_1()
        .text_sm()
        .text_color(move || theme().foreground);
    if focus_body {
        // Above the quote.
        body = body.selected_range((0, 0));
    }
    view()
        .flex_1()
        .min_h(px(0.0))
        .px(px(20.0))
        .py(px(10.0))
        .child(body)
}

/// Many of them scroll, in no more than a few rows: the bar under them stays in the
/// window, however small.
fn attachments() -> impl IntoElement {
    when(
        move || global::<Attachments>().with(|a| !a.0.is_empty()),
        move || {
            scroll().flex_shrink_0().max_h(px(128.0)).child(
                view()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(8.0))
                    .px(px(20.0))
                    .py(px(10.0))
                    .child(each(
                        move || global::<Attachments>().with(|a| a.0.clone()),
                        |a| a.id,
                        |attachment, _| attachment_chip(attachment),
                    )),
            )
        },
    )
}

fn attachment_chip(attachment: ReadSignal<DraftAttachment>) -> impl IntoElement {
    let name = move || attachment.with(|a| a.filename.clone());
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(30.0))
        .pl(px(10.0))
        .pr(px(2.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(move || theme().border)
        .child(icon(|| Icon::Document, 16.0).text_color(move || theme().muted_foreground))
        .child(
            text(move || attachment.with(|a| format!("{} · {}", a.filename, format::size(a.size))))
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(icon_button(
            || Icon::XMark,
            move || format!("Remove {}", name()),
            move || !ending(),
            Size::Row,
            move || remove_attachment(attachment.get_untracked().id),
        ))
}

fn footer() -> impl IntoElement {
    let sending = move || global::<Shown>().with(|s| s.ending == Some(Finish::Send));
    let status = move || global::<Shown>().with(|s| s.status.clone());
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(56.0))
        .px(px(16.0))
        .gap(px(8.0))
        .border_t_1()
        .border_color(move || theme().border)
        .child(primary_button(
            Icon::PaperAirplane,
            move || if sending() { "Sending" } else { "Send" }.to_owned(),
            || format!("Send ({})", key_label("mod+enter")),
            move || !ending(),
            send,
        ))
        .child(icon_button(
            || Icon::PaperClip,
            || "Attach files".to_owned(),
            move || !ending(),
            Size::Bar,
            attach,
        ))
        .child(view().flex_1())
        .child(
            text(move || status().unwrap_or_default().into_owned())
                .text_xs()
                .text_color(move || theme().muted_foreground),
        )
        .child(icon_button(
            || Icon::Trash,
            || "Discard draft".to_owned(),
            move || !ending(),
            Size::Bar,
            discard,
        ))
}
