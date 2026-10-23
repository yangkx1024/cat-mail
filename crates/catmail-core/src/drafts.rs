//! Drafts: written and kept locally, copied to the server's Drafts folder
//! (`draft_sync.rs`), sent through the outbox (`send.rs`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use catmail_proto::mime::{Outgoing, OutgoingAttachment, build_message};
use catmail_proto::smtp;
use catmail_store::{BlobStore, DraftReply, DraftSnapshot, NewDraftAttachment};
use catmail_types::{
    AccountId, Address, BlobHash, Draft, DraftAttachment, DraftAttachmentId, DraftContent, DraftId,
    Event, IdentityId, Message, MessageId,
};
use uuid::Uuid;

use crate::engine::{Engine, blocking, now};
use crate::{Error, Result, body, display};

pub(crate) async fn create(
    engine: &Arc<Engine>,
    account: AccountId,
    from: Option<IdentityId>,
    content: DraftContent,
) -> Result<Draft> {
    validate(&content)?;
    insert(engine, account, from, content, None, Vec::new()).await
}

pub(crate) async fn update(
    engine: &Arc<Engine>,
    id: DraftId,
    from: IdentityId,
    content: DraftContent,
) -> Result<Draft> {
    validate(&content)?;
    let content = without_nul(content);
    let draft = engine
        .write(move |db| {
            if !db.drafts().update(id, from, &content, now())? {
                return Ok(None);
            }
            db.drafts().get(id)
        })
        .await?
        .ok_or(Error::DraftNotFound(id))?;
    edited(engine, draft.account_id, id);
    Ok(draft)
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

/// Copies the file into the blob store and attaches it; later changes to the file
/// don't affect the draft.
pub(crate) async fn attach_file(
    engine: &Arc<Engine>,
    draft: DraftId,
    path: PathBuf,
) -> Result<DraftAttachment> {
    let engine2 = engine.clone();
    let attachment = blocking(move || {
        // Before opening: on Windows, opening a directory fails as access denied.
        if !std::fs::metadata(&path)?.is_file() {
            return Err(Error::InvalidInput(format!(
                "{} is not a file",
                path.display()
            )));
        }
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| Error::InvalidInput(format!("{} has no file name", path.display())))?;
        let file = std::fs::File::open(&path)?;
        let (used, limits) = engine2
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
                Ok(Some((used, SizeLimits { smtp, append })))
            })?
            .ok_or(Error::DraftNotFound(draft))?;
        let limit = attachments_limit(limits);
        let budget = limit.saturating_sub(used);
        // Spares copying a file that says it is too large; `copy_within` enforces it.
        if file.metadata()?.len() > budget {
            return Err(attachments_too_large(limit));
        }
        let (blob, size) = copy_within(&engine2.blobs, file, budget)?
            .ok_or_else(|| attachments_too_large(limit))?;
        let new = NewDraftAttachment {
            blob,
            mime_type: mime_guess::from_path(&path)
                .first_or_octet_stream()
                .essence_str()
                .to_owned(),
            filename,
            size,
        };
        // An attachment refused here, like one whose draft is gone, leaves an unreferenced
        // blob for GC.
        engine2.store.write(|db| {
            // Again, for the draft: another file may have been attached meanwhile.
            if db.drafts().attachments_size(draft)? + size > limit {
                return Ok(Err(attachments_too_large(limit)));
            }
            let Some(attachment) = db.drafts().add_attachment(draft, &new, now())? else {
                return Ok(Ok(None));
            };
            Ok(Ok(db.drafts().account_of(draft)?.map(|a| (a, attachment))))
        })?
    })
    .await?;
    let (account, attachment) = attachment.ok_or(Error::DraftNotFound(draft))?;
    edited(engine, account, draft);
    Ok(attachment)
}

/// Returns the draft as it is now.
pub(crate) async fn remove_attachment(
    engine: &Arc<Engine>,
    id: DraftAttachmentId,
) -> Result<Draft> {
    let draft = engine
        .write(move |db| {
            let Some(draft) = db.drafts().remove_attachment(id, now())? else {
                return Ok(None);
            };
            db.drafts().get(draft)
        })
        .await?
        .ok_or(Error::DraftAttachmentNotFound(id))?;
    edited(engine, draft.account_id, draft.id);
    Ok(draft)
}

/// A new draft answering `message`: to its sender (or Reply-To), and with `all` to its
/// other recipients too, quoting its text. Sent as `from`, else as the account's
/// identity the message came from or went to. Downloads the body first if needed.
pub(crate) async fn reply(
    engine: &Arc<Engine>,
    id: MessageId,
    all: bool,
    from: Option<IdentityId>,
) -> Result<Draft> {
    let message = with_body(engine, id).await?;
    let account_id = message.summary.account_id;
    // The addresses `reply_content` tells the account's from others by.
    let addresses: Vec<String> = message
        .summary
        .from
        .iter()
        .chain(&message.to)
        .chain(&message.cc)
        .map(|a| a.email.clone())
        .collect();
    let own = engine
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
        .ok_or_else(|| Error::AccountNotFound(account_id.to_string()))?;
    let (content, written_as) = reply_content(&message, &own, all);
    let mut references = message.references.clone();
    references.extend(message.message_id.clone());
    let reply = DraftReply {
        message: id,
        in_reply_to: message.message_id.clone(),
        references,
    };
    let from = from.or(written_as);
    insert(engine, account_id, from, content, Some(reply), Vec::new()).await
}

/// A new draft forwarding `message` to no one yet, with its attachments. Everything is
/// downloaded first and stored in one transaction, so a failure leaves no draft behind.
/// Sent as `from`, the account's default with `None`.
pub(crate) async fn forward(
    engine: &Arc<Engine>,
    id: MessageId,
    from: Option<IdentityId>,
) -> Result<Draft> {
    let message = with_body(engine, id).await?;
    let mut attachments = Vec::new();
    for original in message.body.iter().flat_map(|b| &b.attachments) {
        if original.inline {
            continue;
        }
        // The blob the message already has, its GC grace period restarted to cover the
        // gap until the transaction below refers to it.
        let (blob, size) = body::attachment_blob(engine.clone(), original.id).await?;
        attachments.push(NewDraftAttachment {
            blob,
            filename: original
                .filename
                .as_deref()
                .map(|name| name.replace('\0', ""))
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "attachment".into()),
            mime_type: original.mime_type.clone(),
            size,
        });
    }
    let content = forward_content(&message);
    insert(
        engine,
        message.summary.account_id,
        from,
        content,
        None,
        attachments,
    )
    .await
}

/// The message with its body, downloading it if needed.
async fn with_body(engine: &Arc<Engine>, id: MessageId) -> Result<Message> {
    match engine.read(move |db| db.messages().get(id)).await? {
        Some(message) if message.body.is_some() => Ok(message),
        Some(_) => body::download_message(engine.clone(), id).await,
        None => Err(Error::MessageNotFound(id)),
    }
}

/// Sent as `from`, the account's default with `None`.
async fn insert(
    engine: &Arc<Engine>,
    account: AccountId,
    from: Option<IdentityId>,
    content: DraftContent,
    reply: Option<DraftReply>,
    attachments: Vec<NewDraftAttachment>,
) -> Result<Draft> {
    let content = without_nul(content);
    let draft = engine
        .write(move |db| {
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
        .await?
        .ok_or_else(|| Error::AccountNotFound(account.to_string()))?;
    edited(engine, account, draft.id);
    Ok(draft)
}

/// A new Message-ID, from the account's domain. Random, so it gives nothing away.
pub(crate) fn new_message_id(email: &str) -> String {
    let domain = email.rsplit_once('@').map_or("localhost", |(_, d)| d);
    format!("{}@{domain}", Uuid::new_v4().simple())
}

/// The draft as a message `from` its sender, dated now, its attachments read from the
/// blob store. Bcc recipients go into the headers only `with_bcc`, for the draft's copy
/// on the server. Blocking.
pub(crate) fn build(
    engine: &Engine,
    from: &Address,
    snapshot: &DraftSnapshot,
    message_id: &str,
    with_bcc: bool,
) -> Result<Vec<u8>> {
    let data = snapshot
        .blobs
        .iter()
        .map(|blob| engine.blobs.read(blob))
        .collect::<std::io::Result<Vec<_>>>()?;
    let draft = &snapshot.draft;
    build_message(&Outgoing {
        from,
        to: &draft.content.to,
        cc: &draft.content.cc,
        bcc: if with_bcc { &draft.content.bcc } else { &[] },
        subject: &draft.content.subject,
        text: &draft.content.text,
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
                data,
            })
            .collect(),
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

/// A NUL means nothing in text, and no message stored on the server can contain one
/// (IMAP literals can't carry it), so drafts never keep one: quoted from a message that
/// has one (`=00`), it would fail every upload of the draft.
fn without_nul(mut content: DraftContent) -> DraftContent {
    let strip = |s: &mut String| s.retain(|c| c != '\0');
    strip(&mut content.subject);
    strip(&mut content.text);
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

/// Recipients, subject and quoted text of a reply to `message`, written by the account
/// whose addresses are `own` (each with the identity that sends as it), and the identity
/// it is sent as: that of the address the message came from or went to (`None`, the
/// default, for none).
fn reply_content(message: &Message, own: &[Own], all: bool) -> (DraftContent, Option<IdentityId>) {
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
        message.to.iter().chain(&message.cc).find_map(mine)
    };
    let to = if from_me {
        message.to.clone()
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
        let to_others: &[Address] = if from_me { &[] } else { &message.to };
        to_others
            .iter()
            .chain(&message.cc)
            .filter(|a| !is_me(a) && unique(a))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let content = DraftContent {
        subject: reply_subject(message.summary.subject.as_deref().unwrap_or_default()),
        to,
        cc,
        bcc: Vec::new(),
        text: quote(message),
    };
    (content, written_as.flatten())
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

/// The text of the message, without trailing whitespace; empty if it has none.
fn text_of(message: &Message) -> &str {
    message
        .body
        .as_ref()
        .and_then(|b| b.text.as_deref())
        .unwrap_or_default()
        .trim_end()
}

/// An empty line to write on, then the attribution and the original text, `> `-quoted.
fn quote(message: &Message) -> String {
    let text = text_of(message);
    let sender = message
        .summary
        .from
        .first()
        .map_or_else(|| "someone".into(), Address::to_string);
    let date = display::local(message.summary.date, "On %Y-%m-%d %H:%M, ");
    let mut out = format!("\n\n{date}{sender} wrote:\n");
    for line in text.lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Subject and text of a forward of `message`: an empty start, then its headers and
/// text as they were, unquoted.
fn forward_content(message: &Message) -> DraftContent {
    let subject = message.summary.subject.as_deref().unwrap_or_default();
    let date = display::date(message.summary.date);
    let mut text = String::from("\n\n---------- Forwarded message ---------\n");
    for (name, value) in [
        ("From", display::addresses(&message.summary.from)),
        ("Date", date),
        ("Subject", subject.trim().to_owned()),
        ("To", display::addresses(&message.to)),
        ("Cc", display::addresses(&message.cc)),
    ] {
        if !value.is_empty() {
            text.push_str(&format!("{name}: {value}\n"));
        }
    }
    text.push('\n');
    let original = text_of(message);
    if !original.is_empty() {
        text.push_str(original);
        text.push('\n');
    }
    DraftContent {
        subject: forward_subject(subject),
        text,
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
                date: None,
                snippet: None,
                flags: Flags::empty(),
                has_attachments: false,
            },
            to,
            cc,
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
        assert_eq!(
            reply.text,
            "\n\n张三 <zhang@example.com> wrote:\n> 第一行\n>\n> second\n"
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
        assert_eq!(
            forward.text,
            "\n\n---------- Forwarded message ---------\n\
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
