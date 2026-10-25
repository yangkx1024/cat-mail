//! Terminal output for people: escaping text from strangers, aligned tables, and how
//! each command shows its results (`json.rs` is the same for scripts). How dates,
//! sizes and names read is `catmail_core::display`, shared with the GUI.

use std::borrow::Cow;
use std::collections::HashMap;

use catmail_core::{
    Account, Draft, Flags, Folder, FolderId, FolderKind, Identity, Message, MessageSummary, OpKind,
    OpStatus, OutboxEntry, OutboxStatus, PendingOp, ServerConfig, SyncReport, display,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Shortens to at most `width` terminal columns, marking the cut with `…`.
pub(crate) fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Escapes characters that a terminal would act on rather than show: C0/C1 controls
/// (ESC sequences, line breaks) and bidi overrides that reorder the rest of the line.
/// Subjects, names and server messages come from strangers.
pub(crate) fn printable(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_unsafe) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Left-aligned columns, padded by display width so CJK text lines up. Cells are
/// escaped with [`printable`] before measuring.
pub(crate) fn print_table(header: &[&str], rows: Vec<Vec<String>>) {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| match printable(&cell) {
                    Cow::Owned(escaped) => escaped,
                    Cow::Borrowed(_) => cell,
                })
                .collect()
        })
        .collect();
    let header: Vec<String> = header.iter().map(|h| h.to_string()).collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .chain([&header])
                .map(|r| r[i].width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in [&header].into_iter().chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell}{}", " ".repeat(w - cell.width())))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

/// C0/C1 controls and bidi overrides: what a terminal would act on or reorder rather
/// than show.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Like [`printable`], for multi-line text such as a message body: line breaks and
/// tabs are kept (CRLF becomes LF), everything else unsafe is escaped.
pub(crate) fn printable_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\t' => out.push(c),
            c if is_unsafe(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}

/// Each with its identities: SMTP is its default's.
pub(crate) fn print_accounts(accounts: &[(Account, Vec<Identity>)]) {
    if accounts.is_empty() {
        println!("No accounts. Add one with `catmail account add`.");
        return;
    }
    let rows = accounts
        .iter()
        .map(|(a, identities)| {
            let smtp = identities.iter().find(|i| i.is_default);
            vec![
                a.id.to_string(),
                a.email.clone(),
                server(&a.imap),
                smtp.map(|i| server(&i.smtp)).unwrap_or_default(),
            ]
        })
        .collect();
    print_table(&["ID", "EMAIL", "IMAP", "SMTP"], rows);
}

/// `host:port (security)`.
fn server(s: &ServerConfig) -> String {
    format!("{}:{} ({})", s.host, s.port, s.security)
}

pub(crate) fn print_identities(identities: &[Identity]) {
    if identities.is_empty() {
        println!("No identities. Add one with `catmail identity add`.");
        return;
    }
    let rows = identities
        .iter()
        .map(|i| {
            let default = if i.is_default { "*" } else { "" };
            vec![
                i.id.to_string(),
                i.email.clone(),
                server(&i.smtp),
                default.into(),
            ]
        })
        .collect();
    print_table(&["ID", "EMAIL", "SMTP", "DEFAULT"], rows);
}

pub(crate) fn print_sync_report(report: &SyncReport) {
    let mut quiet = true;
    for folder in &report.folders {
        let mut parts = Vec::new();
        if folder.uidvalidity_changed {
            parts.push("UIDVALIDITY changed, refetched".to_owned());
        }
        for (n, what) in [
            (folder.ops_applied, "local changes made"),
            (
                folder.ops_failed,
                "local changes refused and undone (see `catmail ops`)",
            ),
            (folder.added, "new"),
            (folder.updated, "updated"),
            (folder.removed, "removed"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        if let Some(error) = &folder.error {
            parts.push(format!("failed: {error}"));
        }
        if !parts.is_empty() {
            quiet = false;
            println!(
                "  {}: {}",
                printable(&folder.name),
                printable(&parts.join(", "))
            );
        }
    }
    if quiet {
        println!("  up to date ({} folders)", report.folders.len());
    }
}

/// Why a Gmail account's labels are synced as folders, and how to have labels again.
pub(crate) const ALL_MAIL_HIDDEN: &str = "Gmail's All Mail is hidden from IMAP, so its labels \
    are synced as folders. Turn on \"Show in IMAP\" for All Mail in Gmail's settings.";

/// An account's folders. Gmail's labels hidden from IMAP in its settings are left out,
/// as the GUI leaves them out; a KIND column tells labels from folders, on an account
/// that has them.
pub(crate) fn print_folders(folders: &[Folder]) {
    let (header, rows) = folder_table(folders);
    print_table(&header, rows);
}

fn folder_table(folders: &[Folder]) -> (Vec<&'static str>, Vec<Vec<String>>) {
    let kinds = folders.iter().any(|f| f.kind == FolderKind::Label);
    let rows = folders
        .iter()
        .filter(|f| f.listed)
        .map(|f| {
            let [unread, total] = if f.selectable {
                [f.unread_count.to_string(), f.total_count.to_string()]
            } else {
                ["-".into(), "-".into()]
            };
            let mut row = vec![f.name.clone()];
            if kinds {
                // In words people use; `--json` has the engine's (`location`).
                let kind = match f.kind {
                    FolderKind::Label => "label",
                    FolderKind::Location => "folder",
                };
                row.push(kind.into());
            }
            row.push(f.role.map(|r| r.as_str().to_owned()).unwrap_or_default());
            row.extend([unread, total]);
            row
        })
        .collect();
    let mut header = vec!["FOLDER"];
    if kinds {
        header.push("KIND");
    }
    header.extend(["ROLE", "UNREAD", "TOTAL"]);
    (header, rows)
}

/// The names of the message's user labels ([`Folder::is_user_label`]) but `except`,
/// sorted; `folders` are its account's.
pub(crate) fn user_labels(
    message: &MessageSummary,
    folders: &HashMap<FolderId, Folder>,
    except: Option<FolderId>,
) -> Vec<String> {
    let mut names: Vec<String> = message
        .labels
        .iter()
        .filter(|&&id| Some(id) != except)
        .filter_map(|id| folders.get(id))
        .filter(|f| f.is_user_label())
        .map(|f| f.name.clone())
        .collect();
    names.sort();
    names
}

/// `folders` are the messages' accounts'. The messages of one folder, `listed_in`, or
/// from anywhere (search results), which adds a FOLDER column. A LABELS column, when any
/// has a user label, leaves out `listed_in`: every message has it.
pub(crate) fn print_message_table(
    messages: &[MessageSummary],
    folders: &HashMap<FolderId, Folder>,
    listed_in: Option<FolderId>,
) {
    let (header, rows) = message_table(messages, folders, listed_in);
    print_table(&header, rows);
}

fn message_table(
    messages: &[MessageSummary],
    folders: &HashMap<FolderId, Folder>,
    listed_in: Option<FolderId>,
) -> (Vec<&'static str>, Vec<Vec<String>>) {
    let labels: Vec<Vec<String>> = messages
        .iter()
        .map(|m| user_labels(m, folders, listed_in))
        .collect();
    let with_labels = labels.iter().any(|l| !l.is_empty());
    let rows = messages
        .iter()
        .zip(labels)
        .map(|(m, labels)| {
            let mut marks = String::new();
            marks.push(if m.flags.contains(Flags::SEEN) {
                ' '
            } else {
                '*'
            });
            marks.push(if m.flags.contains(Flags::FLAGGED) {
                '!'
            } else {
                ' '
            });
            marks.push(if m.has_attachments { '@' } else { ' ' });
            let mut row = vec![m.id.to_string(), marks, display::date(m.date)];
            if listed_in.is_none() {
                let name = folders.get(&m.folder_id).map(|f| f.name.as_str());
                row.push(truncate(name.unwrap_or_default(), 16));
            }
            row.push(truncate(display::sender(&m.from), 24));
            if with_labels {
                row.push(truncate(&labels.join(", "), 20));
            }
            row.push(display::subject(m.subject.as_deref()).to_owned());
            row
        })
        .collect();
    let mut header = vec!["ID", "", "DATE"];
    if listed_in.is_none() {
        header.push("FOLDER");
    }
    header.push("FROM");
    if with_labels {
        header.push("LABELS");
    }
    header.push("SUBJECT");
    (header, rows)
}

/// `labels` are the names of its user labels.
pub(crate) fn print_message(message: &Message, labels: &[String], html: bool) {
    let m = &message.summary;
    let header = |name: &str, value: &str| {
        if !value.is_empty() {
            println!("{name:<9}{}", printable(value));
        }
    };
    header("From:", &display::addresses(&m.from));
    header("To:", &display::addresses(&message.to));
    header("Cc:", &display::addresses(&message.cc));
    header("Date:", &display::date(m.date));
    header("Subject:", display::subject(m.subject.as_deref()));
    header("Labels:", &labels.join(", "));
    println!();
    let Some(body) = &message.body else {
        println!("(body not downloaded; run without --offline)");
        return;
    };
    let text = if html {
        body.html.as_deref().or(body.text.as_deref())
    } else {
        body.text.as_deref()
    };
    match text {
        Some(text) => println!("{}", printable_text(text.trim_end())),
        None => println!("(no text)"),
    }
    let attachments: Vec<_> = body.attachments.iter().filter(|a| !a.inline).collect();
    if !attachments.is_empty() {
        println!();
        let rows = attachments
            .iter()
            .map(|a| {
                vec![
                    a.id.to_string(),
                    a.filename.clone().unwrap_or_else(|| "(unnamed)".into()),
                    a.mime_type.clone(),
                    match a.size {
                        Some(size) if a.downloaded => display::size(size.into()),
                        Some(size) => format!("~{}", display::size(size.into())),
                        None => String::new(),
                    },
                ]
            })
            .collect();
        print_table(&["ATTACHMENT", "NAME", "TYPE", "SIZE"], rows);
    }
}

/// `folders` are those the changes are in or move to.
pub(crate) fn print_ops(ops: &[PendingOp], folders: &HashMap<FolderId, Folder>) {
    if ops.is_empty() {
        println!("No changes waiting for the server.");
        return;
    }
    let name = |id: FolderId| {
        folders
            .get(&id)
            .map_or_else(|| id.to_string(), |f| f.name.clone())
    };
    let rows = ops
        .iter()
        .map(|op| {
            vec![
                op.id.to_string(),
                op.status.to_string(),
                change(op.kind, &name),
                name(op.folder_id),
                truncate(op.subject.as_deref().unwrap_or_default(), 30),
                match op.status {
                    OpStatus::Conflict => "folder was rebuilt on the server".into(),
                    _ => op.last_error.clone().unwrap_or_default(),
                },
            ]
        })
        .collect();
    print_table(
        &["ID", "STATUS", "CHANGE", "FOLDER", "SUBJECT", "LAST ERROR"],
        rows,
    );
}

fn change(kind: OpKind, folder: &impl Fn(FolderId) -> String) -> String {
    match kind {
        OpKind::Flags { add, remove } => {
            let names = [
                (Flags::SEEN, "read"),
                (Flags::FLAGGED, "flagged"),
                (Flags::ANSWERED, "answered"),
            ];
            let mut parts = Vec::new();
            for (flag, name) in names {
                if add.contains(flag) {
                    parts.push(format!("+{name}"));
                }
                if remove.contains(flag) {
                    parts.push(format!("-{name}"));
                }
            }
            parts.join(" ")
        }
        OpKind::Move { to } => format!("move to {}", folder(to)),
        OpKind::Delete => "delete".into(),
        OpKind::Label { label, add } => {
            format!("{}label {}", if add { "+" } else { "-" }, folder(label))
        }
    }
}

pub(crate) fn print_drafts(drafts: &[Draft]) {
    if drafts.is_empty() {
        println!("No drafts.");
        return;
    }
    let rows = drafts
        .iter()
        .map(|d| {
            vec![
                d.id.to_string(),
                display::date(Some(d.updated_at)),
                server_status(d).into(),
                truncate(&display::names(d.content.recipients()), 30),
                if d.attachments.is_empty() { "" } else { "@" }.into(),
                display::subject(Some(&d.content.subject)).to_owned(),
            ]
        })
        .collect();
    print_table(&["ID", "CHANGED", "SERVER", "TO", "", "SUBJECT"], rows);
}

/// `from` is the identity it is sent as.
pub(crate) fn print_draft(draft: &Draft, from: Option<&Identity>) {
    let c = &draft.content;
    let header = |name: &str, value: &str| {
        if !value.is_empty() {
            println!("{name:<9}{}", printable(value));
        }
    };
    if let Some(from) = from {
        header("From:", &from.email);
    }
    header("To:", &display::addresses(&c.to));
    header("Cc:", &display::addresses(&c.cc));
    header("Bcc:", &display::addresses(&c.bcc));
    header("Subject:", display::subject(Some(&c.subject)));
    if let Some(outbox) = draft.outbox {
        header("Status:", &format!("being sent (outbox entry {outbox})"));
    }
    match &draft.upload_error {
        Some(error) => header("Server:", &format!("refused to store it: {error}")),
        None if !draft.on_server => header("Server:", "not in the Drafts folder yet"),
        None => {}
    }
    println!();
    println!("{}", printable_text(c.text.trim_end()));
    if !draft.attachments.is_empty() {
        println!();
        let rows = draft
            .attachments
            .iter()
            .map(|a| {
                vec![
                    a.id.to_string(),
                    a.filename.clone(),
                    a.mime_type.clone(),
                    display::size(a.size),
                ]
            })
            .collect();
        print_table(&["ATTACHMENT", "NAME", "TYPE", "SIZE"], rows);
    }
}

/// Whether the server's Drafts folder has the draft as it is.
fn server_status(draft: &Draft) -> &'static str {
    match (draft.on_server, &draft.upload_error) {
        (true, _) => "saved",
        (false, Some(_)) => "refused",
        (false, None) => "",
    }
}

pub(crate) fn print_outbox(entries: &[OutboxEntry]) {
    if entries.is_empty() {
        println!("The outbox is empty.");
        return;
    }
    let rows = entries
        .iter()
        .map(|e| {
            let state = match (e.status, e.next_attempt_at) {
                (OutboxStatus::Queued | OutboxStatus::Accepted, Some(at)) if e.attempts > 0 => {
                    format!("{}, next try {}", e.status, display::date(Some(at)))
                }
                (status, _) => status.to_string(),
            };
            vec![
                e.id.to_string(),
                state,
                truncate(&e.recipients.join(", "), 30),
                truncate(display::subject(Some(&e.subject)), 30),
                e.last_error.clone().unwrap_or_default(),
            ]
        })
        .collect();
    print_table(&["ID", "STATUS", "TO", "SUBJECT", "LAST ERROR"], rows);
}

#[cfg(test)]
mod tests {
    use catmail_core::{AccountId, FolderRole, MessageId};

    use super::*;

    #[test]
    fn printable_escapes_terminal_controls() {
        // =?UTF-8?B?G1sySnNwb29m?= decodes to ESC [2J followed by "spoof".
        assert_eq!(printable("\u{1b}[2Jspoof"), "\\u{1b}[2Jspoof");
        assert_eq!(printable("a\nb\tc\u{9b}"), "a\\u{a}b\\u{9}c\\u{9b}");
        assert_eq!(printable("evil\u{202e}fdp.exe"), "evil\\u{202e}fdp.exe");
        assert!(matches!(
            printable("你好，世界 ok"),
            Cow::Borrowed("你好，世界 ok")
        ));
    }

    fn folder(id: i64, name: &str, role: Option<FolderRole>, kind: FolderKind) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(1),
            name: name.into(),
            delimiter: Some('/'),
            role,
            kind,
            listed: true,
            selectable: true,
            unread_count: 0,
            total_count: 0,
            last_synced_at: None,
        }
    }

    /// INBOX (1) and Important (2) are system labels, Work (3) and Personal (4) the user's,
    /// Hidden (5) hidden from IMAP; All Mail (6) is where messages are.
    fn gmail() -> Vec<Folder> {
        let label = FolderKind::Label;
        let mut hidden = folder(5, "Hidden", None, label);
        hidden.listed = false;
        vec![
            folder(1, "INBOX", Some(FolderRole::Inbox), label),
            folder(2, "[Gmail]/Important", Some(FolderRole::Important), label),
            folder(3, "Work", None, label),
            folder(4, "Personal", None, label),
            hidden,
            folder(
                6,
                "[Gmail]/All Mail",
                Some(FolderRole::All),
                FolderKind::Location,
            ),
        ]
    }

    fn message(id: i64, labels: &[i64]) -> MessageSummary {
        MessageSummary {
            id: MessageId(id),
            account_id: AccountId(1),
            folder_id: FolderId(6),
            subject: Some(format!("m{id}")),
            from: Vec::new(),
            date: None,
            snippet: None,
            flags: Flags::SEEN,
            has_attachments: false,
            labels: labels.iter().map(|&id| FolderId(id)).collect(),
        }
    }

    #[test]
    fn folders_tell_labels_apart_and_leave_hidden_ones_out() {
        let (header, rows) = folder_table(&gmail());
        assert_eq!(header, ["FOLDER", "KIND", "ROLE", "UNREAD", "TOTAL"]);
        let kinds: Vec<_> = rows
            .iter()
            .map(|r| (r[0].as_str(), r[1].as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("INBOX", "label"),
                ("[Gmail]/Important", "label"),
                ("Work", "label"),
                ("Personal", "label"),
                ("[Gmail]/All Mail", "folder"),
            ]
        );
        // Without labels, no column for them.
        let standard = [folder(
            1,
            "INBOX",
            Some(FolderRole::Inbox),
            FolderKind::Location,
        )];
        assert_eq!(
            folder_table(&standard).0,
            ["FOLDER", "ROLE", "UNREAD", "TOTAL"]
        );
    }

    #[test]
    fn messages_show_their_user_labels() {
        let folders: HashMap<_, _> = gmail().into_iter().map(|f| (f.id, f)).collect();
        let messages = [message(1, &[1, 2, 3, 4, 5]), message(2, &[3])];
        // In Work: the other user labels; not INBOX, Important or the hidden one.
        let (header, rows) = message_table(&messages, &folders, Some(FolderId(3)));
        assert_eq!(header, ["ID", "", "DATE", "FROM", "LABELS", "SUBJECT"]);
        assert_eq!(rows[0][4], "Personal");
        assert_eq!(rows[1][4], "");
        // Search results: every user label, and the folder.
        let (header, rows) = message_table(&messages, &folders, None);
        assert_eq!(
            header,
            ["ID", "", "DATE", "FOLDER", "FROM", "LABELS", "SUBJECT"]
        );
        assert_eq!(rows[0][3], "[Gmail]/All Mail");
        assert_eq!(rows[0][5], "Personal, Work");
        // No user labels, no column.
        let (header, _) = message_table(&[message(3, &[1])], &folders, Some(FolderId(1)));
        assert_eq!(header, ["ID", "", "DATE", "FROM", "SUBJECT"]);
    }

    #[test]
    fn truncate_counts_display_width() {
        assert_eq!(truncate("张三丰", 5), "张三…");
        assert_eq!(truncate("abc", 5), "abc");
    }

    #[test]
    fn printable_text_keeps_lines() {
        assert_eq!(
            printable_text("a\r\nb\tc\n\u{1b}[31m"),
            "a\nb\tc\n\\u{1b}[31m"
        );
        assert_eq!(printable_text("x\ry"), "x\\u{d}y");
    }
}
