//! Drafts: written and kept locally, copied to the server's Drafts folder
//! (`draft_sync.rs`), sent through the outbox (`send.rs`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use catmail_proto::mime::{Outgoing, OutgoingAttachment, build_message};
use catmail_proto::smtp;
use catmail_store::{DraftReply, DraftSnapshot, NewDraftAttachment};
use catmail_types::{
    Account, AccountId, Address, Draft, DraftAttachment, DraftAttachmentId, DraftContent, DraftId,
    Event, Message, MessageId,
};
use uuid::Uuid;

use crate::engine::{Engine, blocking, now};
use crate::{Error, Result, body, display};

pub(crate) async fn create(
    engine: &Arc<Engine>,
    account: AccountId,
    content: DraftContent,
) -> Result<Draft> {
    validate(&content)?;
    insert(engine, account, content, None, Vec::new()).await
}

pub(crate) async fn update(
    engine: &Arc<Engine>,
    id: DraftId,
    content: DraftContent,
) -> Result<Draft> {
    validate(&content)?;
    let content = without_nul(content);
    let draft = engine
        .write(move |db| {
            if !db.drafts().update(id, &content, now())? {
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
        let blob = engine2.blobs.put(std::fs::File::open(&path)?)?;
        // What was stored, not what the file said when it was opened.
        let size = std::fs::metadata(engine2.blobs.path(&blob))?.len();
        let new = NewDraftAttachment {
            blob,
            mime_type: mime_guess::from_path(&path)
                .first_or_octet_stream()
                .essence_str()
                .to_owned(),
            filename,
            size,
        };
        // An attachment whose draft is gone leaves an unreferenced blob for GC.
        engine2
            .store
            .write(|db| {
                let Some(attachment) = db.drafts().add_attachment(draft, &new, now())? else {
                    return Ok(None);
                };
                Ok(db.drafts().account_of(draft)?.map(|a| (a, attachment)))
            })
            .map_err(Error::from)
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
/// other recipients too, quoting its text. Downloads the body first if needed.
pub(crate) async fn reply(engine: &Arc<Engine>, id: MessageId, all: bool) -> Result<Draft> {
    let message = with_body(engine, id).await?;
    let account_id = message.summary.account_id;
    let account = engine
        .read(move |db| db.accounts().get(account_id))
        .await?
        .ok_or_else(|| Error::AccountNotFound(account_id.to_string()))?;
    let content = reply_content(&message, &account.email, all);
    let mut references = message.references.clone();
    references.extend(message.message_id.clone());
    let reply = DraftReply {
        message: id,
        in_reply_to: message.message_id.clone(),
        references,
    };
    insert(engine, account_id, content, Some(reply), Vec::new()).await
}

/// A new draft forwarding `message` to no one yet, with its attachments. Everything is
/// downloaded first and stored in one transaction, so a failure leaves no draft behind.
pub(crate) async fn forward(engine: &Arc<Engine>, id: MessageId) -> Result<Draft> {
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

async fn insert(
    engine: &Arc<Engine>,
    account: AccountId,
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
            let id = db.drafts().insert(account, &content, reply.as_ref(), now)?;
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

/// The draft as a message from the account, dated now, its attachments read from the
/// blob store. Bcc recipients go into the headers only `with_bcc`, for the draft's copy
/// on the server. Blocking.
pub(crate) fn build(
    engine: &Engine,
    account: &Account,
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
    let from = Address {
        name: account.display_name.clone(),
        email: account.email.clone(),
    };
    build_message(&Outgoing {
        from: &from,
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

/// Recipients, subject and quoted text of a reply to `message`, written by `me`.
fn reply_content(message: &Message, me: &str, all: bool) -> DraftContent {
    let is_me = |a: &Address| a.email.eq_ignore_ascii_case(me);
    let sender = &message.summary.from;
    // Replying to one's own message continues the conversation with its recipients.
    let from_me = sender.iter().any(is_me);
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
    DraftContent {
        subject: reply_subject(message.summary.subject.as_deref().unwrap_or_default()),
        to,
        cc,
        bcc: Vec::new(),
        text: quote(message),
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
        let reply = reply_content(&m, "me@example.com", false);
        assert_eq!(emails(&reply.to), ["zhang@example.com"]);
        assert!(reply.cc.is_empty());
        assert_eq!(reply.subject, "Re: 季度报告");
        assert_eq!(
            reply.text,
            "\n\n张三 <zhang@example.com> wrote:\n> 第一行\n>\n> second\n"
        );

        let all = reply_content(&m, "me@example.com", true);
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
        let reply = reply_content(&m, "me@example.com", true);
        assert_eq!(emails(&reply.to), ["list@example.com"]);
        assert!(reply.cc.is_empty());

        let mine = message(
            addr(None, "me@example.com"),
            vec![addr(None, "zhang@example.com")],
            vec![addr(None, "li@example.com")],
        );
        let reply = reply_content(&mine, "me@example.com", true);
        assert_eq!(emails(&reply.to), ["zhang@example.com"]);
        assert_eq!(emails(&reply.cc), ["li@example.com"]);
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
