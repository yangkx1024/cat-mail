//! Drafts: written and kept locally, copied to the server's Drafts folder
//! ([`draft_sync`]), sent through the outbox ([`send`](crate::send)). Their addresses are
//! read and checked in [`addresses`]; what a reply or forward starts with is in
//! [`answer`].

pub(crate) mod addresses;
pub(crate) mod answer;
pub(crate) mod draft_sync;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use catmail_proto::mime::{Outgoing, OutgoingAttachment, build_message};
use catmail_store::{DraftReply, DraftSnapshot, NewDraftAttachment};
use catmail_types::{
    AccountId, Address, Answer, Attachment, BlobHash, Draft, DraftAttachment, DraftAttachmentId,
    DraftBody, DraftChanges, DraftContent, DraftId, Event, Forwarding, IdentityId, Message,
    MessageId, NewFile, Quote,
};
use uuid::Uuid;

use crate::engine::storage::Storage;
use crate::engine::{Editing, Engine};
use crate::task::{blocking, lock, now};
use crate::{Error, Result, body, display};
use addresses::{check_address, validate};
use answer::{Own, eml_name, forward_content, forward_subject, reply_content, reply_recipients};

pub(crate) async fn create(
    engine: &Arc<Engine>,
    account: AccountId,
    from: Option<IdentityId>,
    content: DraftContent,
    files: Vec<NewFile>,
) -> Result<Draft> {
    validate(&content)?;
    insert(engine, account, from, content, None, Vec::new(), files).await
}

/// Marks a draft as open in an editor until dropped, from
/// [`MailClient::edit_draft`](crate::MailClient::edit_draft): meanwhile nothing but the
/// editor changes what it says or its attachments. Whoever owns the editor's window
/// holds it, for as long as the window is there.
#[derive(Debug)]
pub struct EditMark {
    editing: Editing,
    draft: DraftId,
}

impl Drop for EditMark {
    fn drop(&mut self) {
        let mut editing = lock(&self.editing);
        if let Some(count) = editing.get_mut(&self.draft) {
            *count -= 1;
            if *count == 0 {
                editing.remove(&self.draft);
            }
        }
    }
}

/// Marks the draft as open in an editor, and reads it. In a write transaction, so a
/// change from someone else is either made before (and read) or refused after.
pub(crate) async fn open(engine: &Arc<Engine>, id: DraftId) -> Result<(EditMark, Draft)> {
    let editing = engine.editing();
    engine
        .write(move |db| {
            let Some(draft) = db.drafts().get(id)? else {
                return Ok(None);
            };
            // Counted: a window still closing as another opens on the same draft.
            *lock(&editing).entry(id).or_default() += 1;
            Ok(Some((EditMark { editing, draft: id }, draft)))
        })
        .await?
        .ok_or(Error::DraftNotFound(id))
}

/// Refuses a change to `draft`, or sending it, while an editor has it open, unless it is
/// that editor's doing (`editor` is the draft it edits): the user would send what they
/// never saw, or their editor would save over the change. Checked inside the write
/// transaction that makes it.
pub(crate) fn not_open_elsewhere(
    editing: &Editing,
    draft: DraftId,
    editor: Option<DraftId>,
) -> catmail_store::Result<()> {
    if editor != Some(draft) && lock(editing).contains_key(&draft) {
        return Err(catmail_store::Error::Conflict(format!(
            "draft {draft} is open in the app, where the user is writing it; try again once \
             they close it"
        )));
    }
    Ok(())
}

pub(crate) async fn update(
    engine: &Arc<Engine>,
    id: DraftId,
    changes: DraftChanges,
    editor: Option<DraftId>,
) -> Result<Draft> {
    let editing = engine.editing();
    let (draft, was_in) = engine
        .write(move |db| {
            // Made to the draft as it is now, in the transaction that stores it: what
            // changed since the caller read it, in the parts it leaves alone, stays.
            let Some(draft) = db.drafts().get(id)? else {
                return Ok(None);
            };
            not_open_elsewhere(&editing, id, editor)?;
            let mut content = draft.content;
            let from = changes.apply(&mut content).unwrap_or(draft.identity);
            validate(&content).map_err(|e| catmail_store::Error::Invalid(invalid_text(e)))?;
            let content = without_nul(content);
            let Some(was_in) = db.drafts().update(id, from, &content, now())? else {
                return Ok(None);
            };
            // Images an agent's or the CLI's change leaves unshown (a text without
            // formatting has none, a quote taken out takes its own) go at once. Those an
            // editor takes out stay until it is done (`drop_unshown_images`): undo there
            // can bring them back.
            if editor.is_none() {
                db.drafts()
                    .remove_images(id, &shown_images(&content), now())?;
            }
            Ok(db.drafts().get(id)?.map(|draft| (draft, was_in)))
        })
        .await?
        .ok_or(Error::DraftNotFound(id))?;
    if was_in != draft.account_id {
        // Moved: gone from there, its copy deleted by the next upload pass.
        engine.draft_gone(was_in, id);
        changed(engine, was_in);
    }
    edited(engine, draft.account_id, id);
    Ok(draft)
}

/// Removes the images of the draft that its HTML no longer shows, all of them without
/// HTML: an editor that is done with the draft, whose undo can't bring them back.
pub(crate) async fn drop_unshown_images(
    engine: &Arc<Engine>,
    id: DraftId,
    editor: Option<DraftId>,
) -> Result<()> {
    let editing = engine.editing();
    let account = engine
        .write(move |db| {
            let Some(draft) = db.drafts().get(id)? else {
                return Ok(None);
            };
            not_open_elsewhere(&editing, id, editor)?;
            if db.drafts().editable(id)?.is_none() {
                return Ok(None);
            }
            let shown = shown_images(&draft.content);
            let removed = db.drafts().remove_images(id, &shown, now())?;
            Ok(removed.then_some(draft.account_id))
        })
        .await?;
    if let Some(account) = account {
        edited(engine, account, id);
    }
    Ok(())
}

/// The Content-IDs of the images the draft's HTML and its quote's show.
fn shown_images(content: &DraftContent) -> HashSet<String> {
    let quote = content.quote.as_ref().and_then(|q| q.html.as_deref());
    [content.body.html.as_deref(), quote]
        .into_iter()
        .flatten()
        .flat_map(catmail_html::cid_image_ids)
        .collect()
}

/// Deletes the draft; its copy on the server goes with the next upload pass.
pub(crate) async fn delete(engine: &Arc<Engine>, id: DraftId) -> Result<()> {
    let account = engine
        .write(move |db| {
            let Some(account) = db.drafts().account_of(id)? else {
                return Ok(None);
            };
            db.drafts().delete(id)?;
            Ok(Some(account))
        })
        .await?
        .ok_or(Error::DraftNotFound(id))?;
    engine.draft_gone(account, id);
    changed(engine, account);
    Ok(())
}

/// Announces a change to the account's drafts, and wakes its background sync to make it
/// on the server.
pub(crate) fn changed(engine: &Engine, account: AccountId) {
    engine.emit(Event::DraftsChanged { account });
    engine.wake(account);
}

/// [`changed`], for a change to one draft that may be followed by more: background sync
/// holds its upload back for a while (see [`Engine::draft_edited`]). Only the first
/// change wakes it, to learn when the upload is due: it finds out about later ones then,
/// rather than syncing for nothing at every pause in typing.
pub(crate) fn edited(engine: &Engine, account: AccountId, draft: DraftId) {
    engine.emit(Event::DraftsChanged { account });
    if engine.draft_edited(account, draft) {
        engine.wake(account);
    }
}

/// What a draft's attached files may add up to while the sending server hasn't said what
/// it accepts: what Gmail and Outlook allow, and most servers. Encoded, it just fits the
/// message size Gmail announces (`SIZE`).
const DEFAULT_ATTACHMENTS_SIZE: u64 = 25 * 1024 * 1024;

/// The most, whatever the servers accept: the message is built in memory, and copied to
/// the Drafts folder whole after each change.
const MAX_ATTACHMENTS_SIZE: u64 = 50 * 1024 * 1024;

/// What a message takes besides its attachments' content: headers, the text, and each
/// attachment's part headers. Room for a long text; sending still checks the real size.
const MESSAGE_OVERHEAD: u64 = 64 * 1024;

/// The largest message servers said they accept when last connected to; `None` where not
/// connected to yet, or no limit announced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SizeLimits {
    /// SMTP `SIZE` (RFC 1870) of the server that sends the draft.
    pub smtp: Option<u64>,
    /// IMAP `APPENDLIMIT` (RFC 7889) of the account, for the copies in Drafts and Sent.
    pub append: Option<u64>,
}

impl From<(Option<u64>, Option<u64>)> for SizeLimits {
    /// As the store reads them: SMTP, then IMAP.
    fn from((smtp, append): (Option<u64>, Option<u64>)) -> Self {
        Self { smtp, append }
    }
}

/// What the attachments of a new draft of `account`, sent as `from`, may add up to, and
/// the identity that is (the default with `None`).
fn new_attachments_limit(
    db: &catmail_store::Db<'_>,
    account: AccountId,
    from: Option<IdentityId>,
) -> catmail_store::Result<(IdentityId, u64)> {
    let (identity, limits) = db.drafts().new_size_limits(account, from)?;
    Ok((identity, attachments_limit(limits.into())))
}

/// What a draft's attached files may add up to, given what the servers said: the SMTP
/// server that sends it (`SIZE`), and the IMAP server the copies in Drafts and Sent go to
/// (`APPENDLIMIT`). Attachments are base64-encoded, 57 bytes to a 78-character line.
fn attachments_limit(limits: SizeLimits) -> u64 {
    let room = |message: u64| message.saturating_sub(MESSAGE_OVERHEAD) / 78 * 57;
    let smtp = limits.smtp.map_or(DEFAULT_ATTACHMENTS_SIZE, room);
    let append = limits.append.map_or(u64::MAX, room);
    smtp.min(append).min(MAX_ATTACHMENTS_SIZE)
}

fn attachments_too_large(limit: u64) -> Error {
    Error::InvalidInput(format!(
        "a message's attachments can't add up to more than {}",
        display::size(limit)
    ))
}

/// Copies `file` into the blob store; returns the blob and its size. `None` if more than
/// `budget` bytes arrive (reading stops one byte past): refused rather than stored cut
/// short, as a file can grow while it is read.
fn copy_within(
    storage: &Storage,
    file: impl std::io::Read,
    budget: u64,
) -> Result<Option<(BlobHash, u64)>> {
    let blob = storage.put(file.take(budget + 1))?;
    // What was stored, not what the file said when it was opened.
    let size = std::fs::metadata(storage.blob_path(&blob))?.len();
    Ok((size <= budget).then_some((blob, size)))
}

/// Copies the file into the blob store, to be attached to a draft whose attachments take
/// `used` bytes of its `limit`. Blocking.
fn copy_file(
    storage: &Storage,
    file: &NewFile,
    used: u64,
    limit: u64,
) -> Result<NewDraftAttachment> {
    match file {
        NewFile::Path(path) => copy_path(storage, path, used, limit),
        NewFile::Content { name, content } => store_content(storage, name, content, used, limit),
    }
}

fn copy_path(storage: &Storage, path: &Path, used: u64, limit: u64) -> Result<NewDraftAttachment> {
    let budget = limit.saturating_sub(used);
    // Before opening: on Windows, opening a directory fails as access denied.
    if !std::fs::metadata(path)?.is_file() {
        return Err(Error::InvalidInput(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::InvalidInput(format!("{} has no file name", path.display())))?;
    let file = std::fs::File::open(path)?;
    // Spares copying a file that says it is too large; `copy_within` enforces it.
    if file.metadata()?.len() > budget {
        return Err(attachments_too_large(limit));
    }
    let (blob, size) =
        copy_within(storage, file, budget)?.ok_or_else(|| attachments_too_large(limit))?;
    Ok(NewDraftAttachment {
        blob,
        mime_type: mime_type_of(&filename),
        filename,
        size,
        content_id: None,
    })
}

fn store_content(
    storage: &Storage,
    name: &str,
    content: &[u8],
    used: u64,
    limit: u64,
) -> Result<NewDraftAttachment> {
    let filename = file_name(name)?;
    let size = content.len() as u64;
    if size > limit.saturating_sub(used) {
        return Err(attachments_too_large(limit));
    }
    Ok(NewDraftAttachment {
        blob: storage.put(content)?,
        mime_type: mime_type_of(&filename),
        filename,
        size,
        content_id: None,
    })
}

/// `name`, trimmed, if it is an attachment's file name as the recipient would save it
/// ([`safe_filename`](crate::safe_filename) leaves it as it is): no directories, nothing
/// that makes `invoice\u{202e}fdp.exe` look like a PDF. Refused rather than changed: what
/// is attached goes by the name it was given.
fn file_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || crate::safe_filename(Some(name), "") != name {
        return Err(Error::InvalidInput(format!(
            "`{}` can't be an attachment's name: no directories, characters some systems \
             refuse (`<>:\"|?*`, control and direction characters), dots at either end, or \
             more than 200 bytes",
            name.escape_debug()
        )));
    }
    Ok(name.to_owned())
}

/// Whether a file of this name can be an image of a draft's HTML
/// ([`DraftEditor::attach_image`](crate::DraftEditor::attach_image)): its type, guessed
/// from the name, is an image's.
pub fn named_as_image(filename: &str) -> bool {
    mime_type_of(filename).starts_with("image/")
}

/// Guessed from the name's extension, as the recipient's mail client will.
fn mime_type_of(filename: &str) -> String {
    mime_guess::from_path(filename)
        .first_or_octet_stream()
        .essence_str()
        .to_owned()
}

/// Files copied for a new draft, and what they were checked against.
struct Copied {
    /// The identity they were checked for, which the draft must be made as: the default
    /// may change while they are copied.
    identity: IdentityId,
    /// What the draft's attachments may add up to, sent as it.
    limit: u64,
    attachments: Vec<NewDraftAttachment>,
}

/// Copies the files to attach to a new draft of `account`, sent as `from`, into the blob
/// store: within what its attachments may add up to, with `used` taken by those it starts
/// with. Blocking.
fn copy_files(
    storage: &Storage,
    account: AccountId,
    from: Option<IdentityId>,
    mut used: u64,
    files: &[NewFile],
) -> Result<Copied> {
    let (identity, limit) = storage
        .read(|db| {
            if db.accounts().get(account)?.is_none() {
                return Ok(None);
            }
            new_attachments_limit(db, account, from).map(Some)
        })?
        .ok_or_else(|| Error::AccountNotFound(account.to_string()))?;
    // Unlike `attach_file`, nothing to check again when the draft is stored: nothing else
    // can attach to it before then. Files copied before one fails are left for GC.
    let attachments = files
        .iter()
        .map(|file| {
            let attachment = copy_file(storage, file, used, limit).map_err(|e| Error::Attach {
                file: file.describe(),
                error: Box::new(e),
            })?;
            used += attachment.size;
            Ok(attachment)
        })
        .collect::<Result<_>>()?;
    Ok(Copied {
        identity,
        limit,
        attachments,
    })
}

/// Copies the file into the blob store and attaches it; later changes to the file
/// don't affect the draft. An `inline` one is an image of the draft's HTML, which it
/// shows by the attachment's new Content-ID; it must be named as an image. Returns the
/// attachment, and the file its content is in.
pub(crate) async fn attach_file(
    engine: &Arc<Engine>,
    draft: DraftId,
    file: NewFile,
    editor: Option<DraftId>,
    inline: bool,
) -> Result<(DraftAttachment, PathBuf)> {
    if inline && !named_as_image(&file.name()) {
        return Err(Error::InvalidInput(format!(
            "{} is not an image",
            file.describe()
        )));
    }
    let attachment = blocking({
        let (storage, editing) = (engine.storage(), engine.editing());
        move || {
            // Before copying the file too, which can be large.
            not_open_elsewhere(&editing, draft, editor)?;
            let (used, limits) = storage
                .read(|db| {
                    // Before copying the file: a draft being sent can't change.
                    if db.drafts().editable(draft)?.is_none() {
                        return Ok(None);
                    }
                    let Some((smtp, append)) = db.drafts().size_limits(draft)? else {
                        return Ok(None);
                    };
                    let used = db.drafts().attachments_size(draft)?;
                    Ok(Some((used, SizeLimits::from((smtp, append)))))
                })?
                .ok_or(Error::DraftNotFound(draft))?;
            let limit = attachments_limit(limits);
            let mut new = copy_file(&storage, &file, used, limit)?;
            if inline {
                new.content_id = Some(new_content_id());
            }
            // An attachment refused here, like one whose draft is gone, leaves an unreferenced
            // blob for GC.
            storage.write(|db| {
                not_open_elsewhere(&editing, draft, editor)?;
                // Again, for the draft: another file may have been attached meanwhile.
                if db.drafts().attachments_size(draft)? + new.size > limit {
                    return Ok(Err(attachments_too_large(limit)));
                }
                let Some(attachment) = db.drafts().add_attachment(draft, &new, now())? else {
                    return Ok(Ok(None));
                };
                let path = storage.blob_path(&new.blob);
                Ok(Ok(db
                    .drafts()
                    .account_of(draft)?
                    .map(|a| (a, attachment, path))))
            })?
        }
    })
    .await?;
    let (account, attachment, path) = attachment.ok_or(Error::DraftNotFound(draft))?;
    edited(engine, account, draft);
    Ok((attachment, path))
}

/// Returns the draft as it is now.
pub(crate) async fn remove_attachment(
    engine: &Arc<Engine>,
    id: DraftAttachmentId,
    editor: Option<DraftId>,
) -> Result<Draft> {
    let editing = engine.editing();
    let draft = engine
        .write(move |db| {
            let Some(draft) = db.drafts().remove_attachment(id, now())? else {
                return Ok(None);
            };
            // Undone if refused: the error rolls the transaction back.
            not_open_elsewhere(&editing, draft, editor)?;
            db.drafts().get(draft)
        })
        .await?
        .ok_or(Error::DraftAttachmentNotFound(id))?;
    edited(engine, draft.account_id, draft.id);
    Ok(draft)
}

/// A new draft answering `message`: to its sender (or Reply-To), and with `all` to its
/// other recipients too, `text` above its quote. Sent as `from`, else as the account's
/// identity the message came from or went to. Downloads the body first if needed, and
/// the images the quote shows.
pub(crate) async fn reply(
    engine: &Arc<Engine>,
    id: MessageId,
    all: bool,
    from: Option<IdentityId>,
    text: String,
    files: Vec<NewFile>,
) -> Result<Draft> {
    let message = with_body(engine, id).await?;
    let account_id = message.summary.account_id;
    let own = own_addresses(engine, &message).await?;
    let (mut content, written_as) = reply_content(&message, &own, all);
    content.body = DraftBody::plain(text.trim_end());
    let from = from.or(written_as);
    let (from, attachments) = answer_attachments(
        engine,
        &message,
        from,
        Vec::new(),
        files,
        &mut content.quote,
    )
    .await?;
    let mut references = message.references.clone();
    references.extend(message.message_id.clone());
    let reply = DraftReply {
        message: id,
        in_reply_to: message.message_id.clone(),
        references,
    };
    insert(
        engine,
        account_id,
        Some(from),
        content,
        Some(reply),
        attachments,
        Vec::new(),
    )
    .await
}

/// Whether replying to all would write to anyone a reply doesn't: false when the
/// message's only other party is the one a reply goes to. Reads only its headers.
pub(crate) async fn can_reply_all(engine: &Arc<Engine>, id: MessageId) -> Result<bool> {
    let message = engine
        .read(move |db| db.messages().headers(id))
        .await?
        .ok_or(Error::MessageNotFound(id))?;
    let own = own_addresses(engine, &message).await?;
    Ok(!reply_recipients(&message, &own, true).cc.is_empty())
}

/// Those of the addresses `message` came from or went to that are the account's, which
/// a reply tells from the others.
async fn own_addresses(engine: &Arc<Engine>, message: &Message) -> Result<Vec<Own>> {
    let account_id = message.summary.account_id;
    let addresses: Vec<String> = message
        .summary
        .from
        .iter()
        .chain(&message.summary.to)
        .chain(&message.summary.cc)
        .map(|a| a.email.clone())
        .collect();
    engine
        .read(move |db| {
            let Some(account) = db.accounts().get(account_id)? else {
                return Ok(None);
            };
            let mut own = Vec::new();
            for email in addresses {
                // The account's address is its own even if no identity sends as it any
                // more.
                if let Some(identity) = db.identities().by_email(account_id, &email)? {
                    own.push((Some(identity), email));
                } else if email.eq_ignore_ascii_case(&account.email) {
                    own.push((None, email));
                }
            }
            Ok(Some(own))
        })
        .await?
        .ok_or_else(|| Error::AccountNotFound(account_id.to_string()))
}

/// A new draft forwarding `message`, with its attachments and the images its HTML shows,
/// as `forwarding` says. Everything is downloaded first and stored in one transaction,
/// so a failure leaves no draft behind (an image that can't be had is left out). Sent as
/// `from`, the account's default with `None`.
pub(crate) async fn forward(
    engine: &Arc<Engine>,
    id: MessageId,
    from: Option<IdentityId>,
    forwarding: Forwarding,
    files: Vec<NewFile>,
) -> Result<Draft> {
    // Before downloading anything.
    for address in forwarding.recipients() {
        check_address(&address.email)?;
    }
    if forwarding.as_attachment {
        return forward_attached(engine, id, from, forwarding, files).await;
    }
    let message = with_body(engine, id).await?;
    let theirs: Vec<_> = message.body.iter().flat_map(|b| b.files()).collect();
    // The blobs the message already has, their GC grace periods restarted to cover the
    // gap until the transaction below refers to them; those not here yet fetched
    // together.
    let wanted: Vec<_> = theirs.iter().map(|a| (a.id, said_size(a))).collect();
    let mut blobs = body::attachment_blobs(engine, &wanted).await?;
    let mut originals = Vec::new();
    for original in theirs {
        let (blob, size) = blobs.take(original.id)?;
        originals.push(NewDraftAttachment {
            blob,
            filename: attachment_name(original.filename.as_deref(), "attachment"),
            mime_type: original.mime_type.clone(),
            size,
            content_id: None,
        });
    }
    let Forwarding {
        to, cc, bcc, text, ..
    } = forwarding;
    let mut content = DraftContent {
        to,
        cc,
        bcc,
        body: DraftBody::plain(text.trim_end()),
        ..forward_content(&message)
    };
    let (from, attachments) =
        answer_attachments(engine, &message, from, originals, files, &mut content.quote).await?;
    insert(
        engine,
        message.summary.account_id,
        Some(from),
        content,
        None,
        attachments,
        Vec::new(),
    )
    .await
}

/// What making `answer` to the message downloads from the server, in bytes, about (as
/// the server gave the sizes): nothing once all it takes is here. For a caller that keeps
/// to a download budget, as [`reply`] and [`forward`] go about it.
pub(crate) async fn download_size(
    engine: &Arc<Engine>,
    id: MessageId,
    answer: Answer,
) -> Result<u64> {
    let (message, raw) = engine
        .read(move |db| {
            let messages = db.messages();
            let Some(message) = messages.get(id)? else {
                return Ok(None);
            };
            Ok(messages.location(id)?.map(|l| (message, l.raw)))
        })
        .await?
        .ok_or(Error::MessageNotFound(id))?;
    // Fetched whole, or the body as it opens, with the images and attachments after.
    let whole = message.size.map_or(0, u64::from);
    let Some(body) = &message.body else {
        return Ok(whole);
    };
    let missing = |a: &&Attachment| !a.downloaded;
    Ok(match answer {
        Answer::ForwardAttached if raw.is_some() => 0,
        Answer::ForwardAttached => whole,
        // The images it shows, which the quote carries.
        Answer::Reply => body.inline_parts().filter(missing).map(said_size).sum(),
        Answer::Forward => body.attachments.iter().filter(missing).map(said_size).sum(),
    })
}

/// A forward of the message as an attachment ([`Forwarding::as_attachment`]): the raw
/// message, downloaded whole first if needed, is the draft's first attachment, and
/// `forwarding.text` all it says. It counts against what the attachments may add up to,
/// checked by the size the server gave before it is downloaded, so a message that can't
/// be sent isn't fetched for nothing.
async fn forward_attached(
    engine: &Arc<Engine>,
    id: MessageId,
    from: Option<IdentityId>,
    forwarding: Forwarding,
    files: Vec<NewFile>,
) -> Result<Draft> {
    // Its headers are enough: the body isn't quoted.
    let message = engine
        .read(move |db| db.messages().headers(id))
        .await?
        .ok_or(Error::MessageNotFound(id))?;
    let account = message.summary.account_id;
    // The files first, with room for the message as big as the server said, which tells
    // what the attachments may add up to.
    let said = message.size.map_or(0, u64::from);
    let copied = blocking({
        let storage = engine.storage();
        move || copy_files(&storage, account, from, said, &files)
    })
    .await?;
    let room = |size: u64| {
        let files: u64 = copied.attachments.iter().map(|a| a.size).sum();
        if size + files > copied.limit {
            Err(attachments_too_large(copied.limit))
        } else {
            Ok(())
        }
    };
    room(said)?;
    // Its GC grace period restarted, to cover the gap until the draft refers to it.
    let (blob, size) = body::raw_blob(engine.clone(), id).await?;
    // Again, as it is.
    room(size)?;
    let subject = message.summary.subject.as_deref().unwrap_or_default();
    let mut attachments = vec![NewDraftAttachment {
        blob,
        filename: eml_name(subject),
        mime_type: "message/rfc822".into(),
        size,
        content_id: None,
    }];
    attachments.extend(copied.attachments);
    let Forwarding {
        to, cc, bcc, text, ..
    } = forwarding;
    let content = DraftContent {
        subject: forward_subject(subject),
        to,
        cc,
        bcc,
        body: DraftBody::plain(text.trim_end()),
        quote: None,
    };
    insert(
        engine,
        account,
        Some(copied.identity),
        content,
        None,
        attachments,
        Vec::new(),
    )
    .await
}

/// The attachments of a reply or forward of `message`, sent as `from`: those it starts
/// with (a forward's, the message's), the `files` asked for, copied in, then the images
/// the quote shows in what room is left. The files come first: they were asked for, and
/// the images are only taken if they fit. Returns them, and the identity they were
/// checked for, which the draft must be made as.
async fn answer_attachments(
    engine: &Arc<Engine>,
    message: &Message,
    from: Option<IdentityId>,
    mut attachments: Vec<NewDraftAttachment>,
    files: Vec<NewFile>,
    quote: &mut Option<Quote>,
) -> Result<(IdentityId, Vec<NewDraftAttachment>)> {
    let account = message.summary.account_id;
    let used = attachments.iter().map(|a| a.size).sum();
    let copied = blocking({
        let storage = engine.storage();
        move || copy_files(&storage, account, from, used, &files)
    })
    .await?;
    let used = used + copied.attachments.iter().map(|a| a.size).sum::<u64>();
    attachments.extend(quoted_images(engine, message, copied.limit, used, quote).await);
    attachments.extend(copied.attachments);
    Ok((copied.identity, attachments))
}

/// Copies the images the quote's HTML shows into the draft to be, under Content-IDs of
/// their own (unique in the message it goes into, and among the draft's images, which
/// are told apart by them), downloading those not here yet. Within `limit`, what the
/// draft's attachments may add up to, with `used` taken: the quote is the user's to
/// send, not what they attach, so an image that doesn't fit or can't be had is left out
/// (it shows as missing) rather than failing the reply.
async fn quoted_images(
    engine: &Arc<Engine>,
    message: &Message,
    limit: u64,
    mut used: u64,
    quote: &mut Option<Quote>,
) -> Vec<NewDraftAttachment> {
    let Some(html) = quote.as_mut().and_then(|q| q.html.as_mut()) else {
        return Vec::new();
    };
    let shown = catmail_html::cid_image_ids(html);
    let parts: Vec<_> = message
        .body
        .iter()
        .flat_map(|b| b.inline_parts())
        .filter_map(|a| Some((a.content_id.as_ref().filter(|id| shown.contains(*id))?, a)))
        .collect();
    if parts.is_empty() {
        return Vec::new();
    }
    // By the sizes the message said: one that can't fit isn't downloaded for nothing.
    let (mut wanted, mut fetch, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
    let mut said = used;
    for (id, part) in parts {
        let size = said_size(part);
        if said + size > limit || !seen.insert(id) {
            continue;
        }
        said += size;
        wanted.push((id, part));
        fetch.push((part.id, size));
    }
    // The blobs the message already has, their GC grace periods restarted to cover the
    // gap until the draft refers to them; those not here yet fetched together.
    let mut blobs = match body::attachment_blobs(engine, &fetch).await {
        Ok(blobs) => blobs,
        Err(e) => {
            tracing::warn!(message = %message.summary.id, "left the quoted images out: {e}");
            return Vec::new();
        }
    };
    if let Some(e) = &blobs.stopped {
        tracing::warn!(message = %message.summary.id,
            "left out the quoted images not here yet: {e}");
    }
    let (mut images, mut renamed) = (Vec::new(), HashMap::new());
    for (id, part) in wanted {
        let (blob, size) = match blobs.parts.remove(&part.id) {
            Some(Ok(found)) => found,
            Some(Err(e)) => {
                tracing::warn!(attachment = %part.id, "left a quoted image out: {e}");
                continue;
            }
            // Not fetched: see `stopped`.
            None => continue,
        };
        if used + size > limit {
            tracing::info!(attachment = %part.id, "left a quoted image out: too large");
            continue;
        }
        used += size;
        let content_id = new_content_id();
        renamed.insert(id.clone(), content_id.clone());
        images.push(NewDraftAttachment {
            blob,
            filename: attachment_name(part.filename.as_deref(), "image"),
            mime_type: part.mime_type.clone(),
            size,
            content_id: Some(content_id),
        });
    }
    if !renamed.is_empty() {
        *html = catmail_html::rename_cid_images(html, renamed);
    }
    images
}

/// An attachment's size as the message said, until it is downloaded.
fn said_size(attachment: &Attachment) -> u64 {
    attachment.size.map_or(0, u64::from)
}

/// An attachment's file name as copied into a draft: without NULs, `fallback` if it has
/// none.
fn attachment_name(name: Option<&str>, fallback: &str) -> String {
    name.map(|name| name.replace('\0', ""))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| fallback.into())
}

/// The message with its body, downloading it if needed.
async fn with_body(engine: &Arc<Engine>, id: MessageId) -> Result<Message> {
    match engine.read(move |db| db.messages().get(id)).await? {
        Some(message) if message.body.is_some() => Ok(message),
        Some(_) => body::download_message(engine.clone(), id).await,
        None => Err(Error::MessageNotFound(id)),
    }
}

/// Sent as `from`, the account's default with `None`. Its `attachments`, then copies of
/// the `files`, stored with it in one transaction.
async fn insert(
    engine: &Arc<Engine>,
    account: AccountId,
    from: Option<IdentityId>,
    content: DraftContent,
    reply: Option<DraftReply>,
    mut attachments: Vec<NewDraftAttachment>,
    files: Vec<NewFile>,
) -> Result<Draft> {
    let content = without_nul(content);
    let draft = blocking({
        let storage = engine.storage();
        move || {
            let mut from = from;
            if !files.is_empty() {
                let used = attachments.iter().map(|a| a.size).sum();
                let copied = copy_files(&storage, account, from, used, &files)?;
                from = Some(copied.identity);
                attachments.extend(copied.attachments);
            }
            storage.write(|db| {
                if db.accounts().get(account)?.is_none() {
                    return Ok(None);
                }
                let now = now();
                let id = db
                    .drafts()
                    .insert(account, from, &content, reply.as_ref(), now)?;
                for attachment in &attachments {
                    db.drafts().add_attachment(id, attachment, now)?;
                }
                db.drafts().get(id)
            })
        }
    })
    .await?
    .ok_or_else(|| Error::AccountNotFound(account.to_string()))?;
    edited(engine, account, draft.id);
    Ok(draft)
}

/// A Content-ID for an image of a draft's HTML: random, and unique within any message
/// it goes into.
fn new_content_id() -> String {
    format!("{}@catmail", Uuid::new_v4().simple())
}

/// A new Message-ID, from the account's domain. Random, so it gives nothing away.
pub(crate) fn new_message_id(email: &str) -> String {
    let domain = email.rsplit_once('@').map_or("localhost", |(_, d)| d);
    format!("{}@{domain}", Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destination {
    /// Its copy in the Drafts folder: with its Bcc recipients in the headers, and 8-bit
    /// content, which IMAP takes.
    Drafts,
    /// Out through its identity's SMTP server: Bcc only in the envelope, 8-bit content
    /// only if the server announced 8BITMIME when last connected to (taken to, until it
    /// was: nearly every one does).
    Smtp,
}

/// The draft as a message `from` its sender, dated now, its attachments read from the
/// blob store, made for where it goes. Blocking.
pub(crate) fn build(
    storage: &Storage,
    from: &Address,
    snapshot: &DraftSnapshot,
    message_id: &str,
    destination: Destination,
) -> Result<Vec<u8>> {
    let data = snapshot
        .blobs
        .iter()
        .map(|blob| {
            storage
                .blob(blob)?
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound).into())
        })
        .collect::<Result<Vec<_>>>()?;
    let draft = &snapshot.draft;
    let (with_bcc, eight_bit) = match destination {
        Destination::Drafts => (true, true),
        Destination::Smtp => {
            let announced = storage.read(|db| {
                let sending = db.identities().sending(draft.account_id, draft.identity)?;
                Ok(sending.and_then(|s| s.smtp))
            })?;
            (false, announced.is_none_or(|a| a.eight_bit_mime))
        }
    };
    build_message(&Outgoing {
        from,
        to: &draft.content.to,
        cc: &draft.content.cc,
        bcc: if with_bcc { &draft.content.bcc } else { &[] },
        subject: &draft.content.subject,
        text: &draft.content.body.text,
        html: draft.content.body.html.as_deref(),
        quote: draft.content.quote.as_ref(),
        message_id,
        in_reply_to: draft.in_reply_to.as_deref(),
        references: &draft.references,
        date: now(),
        attachments: draft
            .attachments
            .iter()
            .zip(&data)
            .map(|(a, data)| OutgoingAttachment {
                filename: &a.filename,
                mime_type: &a.mime_type,
                content_id: a.content_id.as_deref(),
                data,
            })
            .collect(),
        eight_bit,
    })
    .map_err(|e| Error::InvalidInput(e.to_string()))
}

/// An identity's name as stored: `None` when there is nothing to it, without a NUL (see
/// [`without_nul`]).
pub(crate) fn clean_name(name: Option<&str>) -> Option<String> {
    let name = name?.replace('\0', "");
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// A NUL means nothing in text, and no message stored on the server can contain one
/// (IMAP literals can't carry it), so drafts never keep one: quoted from a message that
/// has one (`=00`), it would fail every upload of the draft.
fn without_nul(mut content: DraftContent) -> DraftContent {
    let strip = |s: &mut String| s.retain(|c| c != '\0');
    strip(&mut content.subject);
    strip(&mut content.body.text);
    if let Some(html) = &mut content.body.html {
        strip(html);
    }
    if let Some(quote) = &mut content.quote {
        quote.head.values_mut().for_each(strip);
        strip(&mut quote.text);
        if let Some(html) = &mut quote.html {
            strip(html);
        }
    }
    for address in content
        .to
        .iter_mut()
        .chain(&mut content.cc)
        .chain(&mut content.bcc)
    {
        if let Some(name) = &mut address.name {
            strip(name);
        }
    }
    content
}

/// The text of a [`validate`] error, for one returned from a write transaction.
fn invalid_text(e: Error) -> String {
    match e {
        Error::InvalidInput(text) => text,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySecrets;
    use crate::client::fixtures::open_client;

    #[tokio::test]
    async fn a_copy_past_its_budget_is_refused_not_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let client = open_client(dir.path(), Arc::new(MemorySecrets::default()));
        let storage = client.engine().unwrap().storage();
        assert_eq!(copy_within(&storage, &b"12345"[..], 4).unwrap(), None);
        let (blob, size) = copy_within(&storage, &b"1234"[..], 4).unwrap().unwrap();
        assert_eq!(
            (storage.blob(&blob).unwrap(), size),
            (Some(b"1234".to_vec()), 4)
        );
    }

    #[test]
    fn attachments_fit_what_the_servers_accept_encoded() {
        let limit = |smtp, append| attachments_limit(SizeLimits { smtp, append });
        let encoded = |size: u64| size.div_ceil(57) * 78 + MESSAGE_OVERHEAD;
        assert_eq!(
            limit(None, None),
            DEFAULT_ATTACHMENTS_SIZE,
            "nothing said yet"
        );
        // Gmail's: a little under the default, and it fits.
        let gmail = 35_882_577;
        assert!(limit(Some(gmail), None) > DEFAULT_ATTACHMENTS_SIZE - 64 * 1024);
        assert!(encoded(limit(Some(gmail), None)) <= gmail);
        // More (Fastmail's 70 MB), up to what is built in memory.
        let fastmail = limit(Some(70_000_000), None);
        assert!(fastmail > DEFAULT_ATTACHMENTS_SIZE && fastmail <= MAX_ATTACHMENTS_SIZE);
        assert_eq!(limit(Some(u64::MAX), None), MAX_ATTACHMENTS_SIZE);
        // Less (Postfix's default), and APPENDLIMIT for the copies.
        assert!(encoded(limit(Some(10_240_000), None)) <= 10_240_000);
        assert_eq!(
            limit(Some(gmail), Some(1_000_000)),
            limit(Some(1_000_000), None)
        );
        assert_eq!(limit(Some(1000), None), 0);
    }
}
