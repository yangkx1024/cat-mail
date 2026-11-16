//! Drafts: written and kept locally, copied to the server's Drafts folder
//! (`draft_sync.rs`), sent through the outbox (`send.rs`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use catmail_proto::mime::{Outgoing, OutgoingAttachment, build_message};
use catmail_proto::smtp;
use catmail_store::{BlobStore, DraftReply, DraftSnapshot, NewDraftAttachment};
use catmail_types::{
    AccountId, Address, Answer, Attachment, BlobHash, Draft, DraftAttachment, DraftAttachmentId,
    DraftBody, DraftChanges, DraftContent, DraftId, Event, FORWARD_SEPARATOR, Forwarding,
    IdentityId, Message, MessageId, NewFile, Quote, QuoteKind,
};
use uuid::Uuid;

use crate::engine::{Editing, Engine, blocking, lock, now};
use crate::filename::safe_filename;
use crate::{Error, Result, body, display};

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
    blobs: &BlobStore,
    file: impl std::io::Read,
    budget: u64,
) -> Result<Option<(BlobHash, u64)>> {
    let blob = blobs.put(file.take(budget + 1))?;
    // What was stored, not what the file said when it was opened.
    let size = std::fs::metadata(blobs.path(&blob))?.len();
    Ok((size <= budget).then_some((blob, size)))
}

/// Copies the file into the blob store, to be attached to a draft whose attachments take
/// `used` bytes of its `limit`. Blocking.
fn copy_file(
    blobs: &BlobStore,
    file: &NewFile,
    used: u64,
    limit: u64,
) -> Result<NewDraftAttachment> {
    match file {
        NewFile::Path(path) => copy_path(blobs, path, used, limit),
        NewFile::Content { name, content } => store_content(blobs, name, content, used, limit),
    }
}

fn copy_path(blobs: &BlobStore, path: &Path, used: u64, limit: u64) -> Result<NewDraftAttachment> {
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
        copy_within(blobs, file, budget)?.ok_or_else(|| attachments_too_large(limit))?;
    Ok(NewDraftAttachment {
        blob,
        mime_type: mime_type_of(&filename),
        filename,
        size,
        content_id: None,
    })
}

/// Stores `content`, to be attached as `name`.
fn store_content(
    blobs: &BlobStore,
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
        blob: blobs.put(content)?,
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
    engine: &Engine,
    account: AccountId,
    from: Option<IdentityId>,
    mut used: u64,
    files: &[NewFile],
) -> Result<Copied> {
    let (identity, limit) = engine
        .store
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
            let attachment =
                copy_file(&engine.blobs, file, used, limit).map_err(|e| Error::Attach {
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
        let engine = engine.clone();
        move || {
            let editing = engine.editing();
            // Before copying the file too, which can be large.
            not_open_elsewhere(&editing, draft, editor)?;
            let (used, limits) = engine
                .store
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
            let mut new = copy_file(&engine.blobs, &file, used, limit)?;
            if inline {
                new.content_id = Some(new_content_id());
            }
            // An attachment refused here, like one whose draft is gone, leaves an unreferenced
            // blob for GC.
            engine.store.write(|db| {
                not_open_elsewhere(&editing, draft, editor)?;
                // Again, for the draft: another file may have been attached meanwhile.
                if db.drafts().attachments_size(draft)? + new.size > limit {
                    return Ok(Err(attachments_too_large(limit)));
                }
                let Some(attachment) = db.drafts().add_attachment(draft, &new, now())? else {
                    return Ok(Ok(None));
                };
                let path = engine.blobs.path(&new.blob);
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
        let (blob, size) = blobs
            .remove(&original.id)
            .ok_or(Error::AttachmentNotFound(original.id))??;
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
        let engine = engine.clone();
        move || copy_files(&engine, account, from, said, &files)
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

/// What a message attached whole is named: its subject, as Thunderbird and Outlook name
/// it, with `.eml`, made safe to save (see [`safe_filename`]).
fn eml_name(subject: &str) -> String {
    // A slash is part of the subject, not a directory.
    let subject = subject.replace(['/', '\\'], "_");
    let subject = subject.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if subject.is_empty() {
        return "message.eml".into();
    }
    safe_filename(Some(&format!("{subject}.eml")), "message.eml")
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
        let engine = engine.clone();
        move || copy_files(&engine, account, from, used, &files)
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
    let (mut images, mut renamed) = (Vec::new(), HashMap::new());
    for (id, part) in wanted {
        let found = blobs
            .remove(&part.id)
            .unwrap_or(Err(Error::AttachmentNotFound(part.id)));
        let (blob, size) = match found {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(attachment = %part.id, "left a quoted image out: {e}");
                continue;
            }
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
        let engine = engine.clone();
        move || {
            let mut from = from;
            if !files.is_empty() {
                let used = attachments.iter().map(|a| a.size).sum();
                let copied = copy_files(&engine, account, from, used, &files)?;
                from = Some(copied.identity);
                attachments.extend(copied.attachments);
            }
            Ok(engine.store.write(|db| {
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
            })?)
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

/// Where a draft built as a message goes.
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
    engine: &Engine,
    from: &Address,
    snapshot: &DraftSnapshot,
    message_id: &str,
    destination: Destination,
) -> Result<Vec<u8>> {
    let data = snapshot
        .blobs
        .iter()
        .map(|blob| engine.blobs.read(blob))
        .collect::<std::io::Result<Vec<_>>>()?;
    let draft = &snapshot.draft;
    let (with_bcc, eight_bit) = match destination {
        Destination::Drafts => (true, true),
        Destination::Smtp => {
            let announced = engine.store.read(|db| {
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

/// Parses what a user types into a To / Cc / Bcc field: addresses separated by commas
/// or semicolons, each either bare or with a display name (`张三 <zhang@example.com>`,
/// `"Doe, Jane" <jane@example.com>`). Every entry must be exactly that: anything else is
/// an error, never a recipient silently left out. Every address must be usable for
/// sending.
pub fn parse_addresses(input: &str) -> Result<Vec<Address>> {
    if input.contains(['\r', '\n']) {
        return Err(Error::InvalidInput("addresses must be on one line".into()));
    }
    split_entries(input)
        .into_iter()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            let address = parse_entry(entry)
                .ok_or_else(|| Error::InvalidInput(format!("`{entry}` is not an email address")))?;
            check_address(&address.email)?;
            Ok(address)
        })
        .collect()
}

/// The addresses as a To / Cc / Bcc field shows them, so that [`parse_addresses`] reads
/// them back: a name with a separator, quote, bracket or backslash in it is quoted
/// (`"Doe, Jane" <jane@example.com>`).
pub fn format_addresses(list: &[Address]) -> String {
    list.iter()
        .map(
            |a| match a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                None => a.email.clone(),
                Some(name) if name.contains([',', ';', '"', '<', '>', '\\']) => {
                    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{escaped}\" <{}>", a.email)
                }
                Some(name) => format!("{name} <{}>", a.email),
            },
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// `input` split at the commas and semicolons that separate addresses, not those inside
/// a quoted name (`"Doe, Jane"`) or angle brackets.
fn split_entries(input: &str) -> Vec<&str> {
    let (mut quoted, mut escaped, mut bracketed, mut start) = (false, false, false, 0);
    let mut entries = Vec::new();
    for (i, c) in input.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' if !bracketed => quoted = !quoted,
            '<' if !quoted => bracketed = true,
            '>' if !quoted => bracketed = false,
            ',' | ';' if !quoted && !bracketed => {
                entries.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    entries.push(&input[start..]);
    entries
}

/// One trimmed entry: `addr`, `<addr>`, `Name <addr>` or `"Name" <addr>`. `None` for
/// anything else, including text after the address and a second address.
fn parse_entry(entry: &str) -> Option<Address> {
    let Some(rest) = entry.strip_suffix('>') else {
        // Bare; `check_address` refuses spaces, brackets and quotes.
        return Some(Address {
            name: None,
            email: entry.to_owned(),
        });
    };
    let (name, email) = rest.rsplit_once('<')?;
    if email.is_empty() {
        return None;
    }
    let name = name.trim();
    let name = match name.strip_prefix('"') {
        Some(quoted) => {
            let mut unquoted = String::new();
            let mut chars = quoted.strip_suffix('"')?.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => unquoted.push(chars.next()?),
                    '"' => return None,
                    c => unquoted.push(c),
                }
            }
            unquoted
        }
        None if name.contains(['"', '<', '>']) => return None,
        None => name.to_owned(),
    };
    Some(Address {
        name: (!name.is_empty()).then_some(name),
        email: email.to_owned(),
    })
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
        strip(&mut quote.intro);
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

/// Addresses must be usable as they are in the SMTP envelope. Checked when saving, so a
/// mistake shows up while the user is still writing.
/// The text of a [`validate`] error, for one returned from a write transaction.
fn invalid_text(e: Error) -> String {
    match e {
        Error::InvalidInput(text) => text,
        other => other.to_string(),
    }
}

pub(crate) fn validate(content: &DraftContent) -> Result<()> {
    for address in content.recipients() {
        check_address(&address.email)?;
    }
    Ok(())
}

pub(crate) fn check_address(email: &str) -> Result<()> {
    if !email.is_ascii() {
        return Err(Error::InvalidInput(format!(
            "`{email}`: addresses with non-ASCII characters are not supported yet"
        )));
    }
    // Beyond the envelope's rules, nothing that would split the address in a header.
    if !smtp::is_envelope_address(email) || email.contains(['(', ')', ',', ';']) {
        return Err(Error::InvalidInput(format!(
            "`{email}` is not an email address"
        )));
    }
    Ok(())
}

/// An address of the account: an identity's, or its own (`None`, written as the default).
type Own = (Option<IdentityId>, String);

/// Recipients, subject and quote of a reply to `message`, written by the account
/// whose addresses are `own` (each with the identity that sends as it), and the identity
/// it is sent as: that of the address the message came from or went to (`None`, the
/// default, for none).
fn reply_content(message: &Message, own: &[Own], all: bool) -> (DraftContent, Option<IdentityId>) {
    let ReplyRecipients { to, cc, written_as } = reply_recipients(message, own, all);
    let content = DraftContent {
        subject: reply_subject(message.summary.subject.as_deref().unwrap_or_default()),
        to,
        cc,
        bcc: Vec::new(),
        body: DraftBody::default(),
        quote: Some(quote(message, QuoteKind::Reply, attribution(message))),
    };
    (content, written_as)
}

struct ReplyRecipients {
    to: Vec<Address>,
    /// Empty unless replying to all.
    cc: Vec<Address>,
    written_as: Option<IdentityId>,
}

fn reply_recipients(message: &Message, own: &[Own], all: bool) -> ReplyRecipients {
    let mine = |a: &Address| {
        own.iter()
            .find(|(_, email)| a.email.eq_ignore_ascii_case(email))
            .map(|(identity, _)| *identity)
    };
    let is_me = |a: &Address| mine(a).is_some();
    let sender = &message.summary.from;
    // Replying to one's own message continues the conversation with its recipients.
    let from_me = sender.iter().any(is_me);
    let written_as = if from_me {
        sender.iter().find_map(mine)
    } else {
        message
            .summary
            .to
            .iter()
            .chain(&message.summary.cc)
            .find_map(mine)
    };
    let to = if from_me {
        message.summary.to.clone()
    } else if !message.reply_to.is_empty() {
        message.reply_to.clone()
    } else {
        sender.clone()
    };
    let mut seen = HashSet::new();
    let mut unique = |a: &Address| seen.insert(a.email.to_ascii_lowercase());
    let to: Vec<Address> = to.into_iter().filter(|a| unique(a)).collect();
    let cc = if all {
        // Those it went to, but for one's own message they are already in `to`.
        let to_others: &[Address] = if from_me { &[] } else { &message.summary.to };
        to_others
            .iter()
            .chain(&message.summary.cc)
            .filter(|a| !is_me(a) && unique(a))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    ReplyRecipients {
        to,
        cc,
        written_as: written_as.flatten(),
    }
}

fn reply_subject(subject: &str) -> String {
    prefixed(
        subject,
        "Re",
        &["re:", "re：", "回复:", "回复：", "答复:", "答复："],
    )
}

fn forward_subject(subject: &str) -> String {
    prefixed(
        subject,
        "Fwd",
        &["fwd:", "fw:", "fwd：", "fw：", "转发:", "转发："],
    )
}

/// `subject` with `prefix: ` in front, unless it starts with one of the `known` ways
/// (lowercase) of writing it already.
fn prefixed(subject: &str, prefix: &str, known: &[&str]) -> String {
    let subject = subject.trim();
    let lower = subject.to_lowercase();
    if known.iter().any(|known| lower.starts_with(known)) {
        subject.to_owned()
    } else {
        format!("{prefix}: {subject}")
    }
}

/// `message` quoted, introduced by `intro`: its text, and its HTML if it has any.
fn quote(message: &Message, kind: QuoteKind, intro: String) -> Quote {
    let body = message.body.as_ref();
    Quote {
        kind,
        intro,
        text: body.and_then(|b| b.text.clone()).unwrap_or_default(),
        html: body.and_then(|b| b.html.clone()),
    }
}

/// Who wrote the message, and when.
fn attribution(message: &Message) -> String {
    let sender = message
        .summary
        .from
        .first()
        .map_or_else(|| "someone".into(), Address::to_string);
    let date = display::local(message.summary.date, "On %Y-%m-%d %H:%M, ");
    format!("{date}{sender} wrote:")
}

/// Subject and quote of a forward of `message`: its headers, then the message as it was.
fn forward_content(message: &Message) -> DraftContent {
    let subject = message.summary.subject.as_deref().unwrap_or_default();
    let date = display::date(message.summary.date);
    let mut intro = String::from(FORWARD_SEPARATOR);
    for (name, value) in [
        ("From", display::addresses(&message.summary.from)),
        ("Date", date),
        ("Subject", subject.trim().to_owned()),
        ("To", display::addresses(&message.summary.to)),
        ("Cc", display::addresses(&message.summary.cc)),
    ] {
        if !value.is_empty() {
            intro.push_str(&format!("\n{name}: {value}"));
        }
    }
    DraftContent {
        subject: forward_subject(subject),
        quote: Some(quote(message, QuoteKind::Forward, intro)),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use catmail_types::{Flags, FolderId, MessageBody, MessageSummary};

    use super::*;

    #[test]
    fn a_copy_past_its_budget_is_refused_not_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(dir.path()).unwrap();
        assert_eq!(copy_within(&blobs, &b"12345"[..], 4).unwrap(), None);
        let (blob, size) = copy_within(&blobs, &b"1234"[..], 4).unwrap().unwrap();
        assert_eq!((blobs.read(&blob).unwrap(), size), (b"1234".to_vec(), 4));
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

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(String::from),
            email: email.into(),
        }
    }

    fn message(from: Address, to: Vec<Address>, cc: Vec<Address>) -> Message {
        Message {
            summary: MessageSummary {
                id: MessageId(1),
                account_id: AccountId(1),
                folder_id: FolderId(1),
                subject: Some("季度报告".into()),
                from: vec![from],
                to,
                cc,
                date: None,
                snippet: None,
                flags: Flags::empty(),
                has_attachments: false,
                labels: Vec::new(),
            },
            reply_to: Vec::new(),
            message_id: Some("m1@example.com".into()),
            references: Vec::new(),
            size: None,
            body: Some(MessageBody {
                text: Some("第一行\n\nsecond\n".into()),
                html: None,
                attachments: Vec::new(),
            }),
        }
    }

    fn me() -> Vec<Own> {
        vec![(None, "me@example.com".into())]
    }

    fn emails(list: &[Address]) -> Vec<&str> {
        list.iter().map(|a| a.email.as_str()).collect()
    }

    #[test]
    fn reply_goes_to_the_sender_and_reply_all_to_everyone_else() {
        let m = message(
            addr(Some("张三"), "zhang@example.com"),
            vec![addr(None, "ME@example.com"), addr(None, "li@example.com")],
            vec![
                addr(None, "wang@example.com"),
                addr(None, "Zhang@example.com"),
            ],
        );
        let (reply, _) = reply_content(&m, &me(), false);
        assert_eq!(emails(&reply.to), ["zhang@example.com"]);
        assert!(reply.cc.is_empty());
        assert_eq!(reply.subject, "Re: 季度报告");
        assert_eq!(reply.body, DraftBody::default(), "the user's to write");
        assert_eq!(
            reply.text_as_sent(),
            "张三 <zhang@example.com> wrote:\n> 第一行\n>\n> second\n"
        );

        let (all, _) = reply_content(&m, &me(), true);
        assert_eq!(emails(&all.to), ["zhang@example.com"]);
        assert_eq!(
            emails(&all.cc),
            ["li@example.com", "wang@example.com"],
            "not me, not twice"
        );
    }

    #[test]
    fn reply_to_and_own_messages() {
        let mut m = message(
            addr(None, "zhang@example.com"),
            vec![addr(None, "list@example.com")],
            vec![],
        );
        m.reply_to = vec![addr(None, "list@example.com")];
        let (reply, _) = reply_content(&m, &me(), true);
        assert_eq!(emails(&reply.to), ["list@example.com"]);
        assert!(reply.cc.is_empty());

        let mine = message(
            addr(None, "me@example.com"),
            vec![addr(None, "zhang@example.com")],
            vec![addr(None, "li@example.com")],
        );
        let (reply, _) = reply_content(&mine, &me(), true);
        assert_eq!(emails(&reply.to), ["zhang@example.com"]);
        assert_eq!(emails(&reply.cc), ["li@example.com"]);
    }

    #[test]
    fn replies_are_written_as_the_address_written_to() {
        // The default, another, and the account's own address no identity sends as.
        let own = [
            (Some(IdentityId(1)), "me@example.com".into()),
            (Some(IdentityId(2)), "news@example.com".into()),
            (None, "account@example.com".into()),
        ];
        let to = |to: &str, cc: &str| {
            message(
                addr(None, "zhang@example.com"),
                vec![addr(None, to)],
                vec![addr(None, cc)],
            )
        };
        let as_ = |m: &Message| reply_content(m, &own, true).1;
        assert_eq!(
            as_(&to("News@example.com", "me@example.com")),
            Some(IdentityId(2))
        );
        assert_eq!(
            as_(&to("li@example.com", "news@example.com")),
            Some(IdentityId(2))
        );
        assert_eq!(
            as_(&to("me@example.com", "li@example.com")),
            Some(IdentityId(1))
        );
        assert_eq!(as_(&to("account@example.com", "li@example.com")), None);
        assert_eq!(as_(&to("li@example.com", "wang@example.com")), None);
        let (all, _) = reply_content(&to("li@example.com", "account@example.com"), &own, true);
        assert_eq!(emails(&all.cc), ["li@example.com"], "the account is me");
        let (all, _) = reply_content(&to("news@example.com", "li@example.com"), &own, true);
        assert_eq!(emails(&all.cc), ["li@example.com"], "an identity is me too");

        let mine = message(
            addr(None, "news@example.com"),
            vec![addr(None, "zhang@example.com")],
            vec![],
        );
        let (reply, from) = reply_content(&mine, &own, true);
        assert_eq!(
            (emails(&reply.to), from),
            (vec!["zhang@example.com"], Some(IdentityId(2)))
        );
    }

    #[test]
    fn reply_subjects() {
        assert_eq!(reply_subject("hello"), "Re: hello");
        assert_eq!(reply_subject("RE: hello"), "RE: hello");
        assert_eq!(reply_subject("回复：你好"), "回复：你好");
        assert_eq!(reply_subject(""), "Re: ");
    }

    #[test]
    fn a_message_attached_whole_is_named_after_its_subject() {
        assert_eq!(eml_name("Report"), "Report.eml");
        assert_eq!(eml_name("Re: a/b\\c?*x*  "), "Re_ a_b_c__x_.eml");
        assert_eq!(eml_name("Sent..."), "Sent.eml");
        assert_eq!(eml_name(" ... "), "message.eml");
        assert_eq!(eml_name("CON"), "_CON.eml");
        let long = eml_name(&"长".repeat(300));
        assert!(long.len() <= 200 && long.ends_with("长.eml"), "{long}");
    }

    #[test]
    fn forward_subjects() {
        assert_eq!(forward_subject("hello"), "Fwd: hello");
        assert_eq!(forward_subject(" FW: hello"), "FW: hello");
        assert_eq!(forward_subject("Fwd：报告"), "Fwd：报告");
        assert_eq!(forward_subject("转发：你好"), "转发：你好");
        assert_eq!(forward_subject("Re: hello"), "Fwd: Re: hello");
    }

    #[test]
    fn forward_shows_the_headers_above_the_text_unquoted() {
        let m = message(
            addr(Some("张三"), "zhang@example.com"),
            vec![addr(None, "me@example.com"), addr(None, "li@example.com")],
            vec![],
        );
        let forward = forward_content(&m);
        assert_eq!(forward.subject, "Fwd: 季度报告");
        assert!(forward.recipients().next().is_none());
        assert_eq!(forward.body, DraftBody::default());
        let quote = forward.quote.as_ref().unwrap();
        assert_eq!(quote.kind, QuoteKind::Forward);
        assert_eq!(
            forward.text_as_sent(),
            "---------- Forwarded message ---------\n\
             From: 张三 <zhang@example.com>\n\
             Subject: 季度报告\n\
             To: me@example.com, li@example.com\n\
             \n\
             第一行\n\nsecond\n"
        );
    }

    #[test]
    fn parses_address_fields() {
        let parsed = parse_addresses(
            r#"张三 <zhang@example.com>, "Doe, Jane" <jane@example.com>,b@example.com"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            [
                addr(Some("张三"), "zhang@example.com"),
                addr(Some("Doe, Jane"), "jane@example.com"),
                addr(None, "b@example.com"),
            ]
        );
        assert_eq!(parse_addresses("  ").unwrap(), []);
        assert_eq!(
            parse_addresses(r#"a@example.com; <b@example.com>, "O\"Brien" <c@example.com>,"#)
                .unwrap(),
            [
                addr(None, "a@example.com"),
                addr(None, "b@example.com"),
                addr(Some("O\"Brien"), "c@example.com"),
            ]
        );
        for bad in [
            "bob@example.com, Carol",
            "bob@example.com; Carol",
            "bob@example.com,,, nobody",
            "Bob <bob@example.com> Carol",
            "Bob <bob@example.com> <carol@example.com>",
            "Team: bob@example.com",
            "bob@example.com (work)",
            "Carol <>",
            "nobody",
            "a@example.com\r\nBcc: x@example.com",
            "Bob <b c@example.com>",
        ] {
            assert!(parse_addresses(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn formatted_addresses_parse_back() {
        let list = [
            addr(Some("Doe, Jane"), "jane@example.com"),
            addr(Some("O\"Brien"), "o@example.com"),
            addr(Some(r"back\slash"), "b@example.com"),
            addr(Some("semi;colon <x>"), "s@example.com"),
            addr(Some("张三"), "zhang@example.com"),
            addr(None, "plain@example.com"),
        ];
        let field = format_addresses(&list);
        assert!(
            field.starts_with(r#""Doe, Jane" <jane@example.com>, "O\"Brien""#),
            "{field}"
        );
        assert_eq!(parse_addresses(&field).unwrap(), list);
    }

    #[test]
    fn addresses_must_fit_the_envelope() {
        assert!(check_address("a@example.com").is_ok());
        for bad in [
            "",
            "a",
            "a@",
            "a b@example.com",
            "<a@example.com>",
            "张三@example.com",
        ] {
            assert!(check_address(bad).is_err(), "{bad:?}");
        }
    }
}
