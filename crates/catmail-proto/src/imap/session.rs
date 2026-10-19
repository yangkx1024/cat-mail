use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use catmail_types::{FlagState, Flags, FolderRole, Security, ServerConfig};
use imap_next::imap_types::auth::AuthMechanism;
use imap_next::imap_types::command::{CommandBody, FetchModifier, SelectParameter};
use imap_next::imap_types::core::{AString, Vec1};
use imap_next::imap_types::fetch::{MessageDataItem, MessageDataItemName, Part, Section};
use imap_next::imap_types::flag::{Flag, FlagFetch};
use imap_next::imap_types::mailbox::{ListMailbox, Mailbox};
use imap_next::imap_types::response::{Code, Data, GreetingKind};
use imap_next::imap_types::search::SearchKey;
use imap_next::imap_types::sequence::{Sequence, SequenceSet};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;

use super::connection::{Connection, Response, with_timeout};
use super::{
    Capabilities, Credentials, Error, FetchedMessage, ListedMailbox, MailboxStatus, Result, utf7,
};
use crate::TlsConfig;
use crate::mime::{Headers, MimeNode, SUMMARY_HEADER_FIELDS, parse_headers};

/// An IMAP connection, from greeting to logout. Read-only for now: mailboxes are opened
/// with EXAMINE, and bodies fetched with `BODY.PEEK`, which leaves `\Seen` alone.
pub struct Session {
    conn: Connection,
    caps: Capabilities,
    /// From a PREAUTH greeting, or after `login`.
    authenticated: bool,
    selected: Option<MailboxStatus>,
}

impl Session {
    /// Connects and secures the connection as `server.security` demands. With STARTTLS,
    /// a server that doesn't offer it is an error; there is no silent downgrade.
    pub async fn connect(
        server: &ServerConfig,
        tls: &TlsConfig,
        timeout: Duration,
    ) -> Result<Self> {
        let tcp = with_timeout(
            timeout,
            TcpStream::connect((server.host.as_str(), server.port)),
        )
        .await?;
        tcp.set_nodelay(true)?;
        let server_name = ServerName::try_from(server.host.clone())
            .map_err(|_| Error::InvalidServerName(server.host.clone()))?;

        let stream: Box<dyn super::connection::Stream> = match server.security {
            Security::Tls => Box::new(
                with_timeout(timeout, tls.connector().connect(server_name.clone(), tcp)).await?,
            ),
            Security::StartTls | Security::Plain => Box::new(tcp),
        };
        let mut conn = Connection::new(stream, true, timeout);
        let greeting = conn.greeting().await?;
        let mut caps = match greeting.code {
            Some(Code::Capability(caps)) => capabilities(caps.as_ref()),
            _ => Capabilities::default(),
        };
        let authenticated = match greeting.kind {
            GreetingKind::Ok => false,
            GreetingKind::Bye => return Err(Error::Rejected(greeting.text.to_string())),
            // Already authenticated, so STARTTLS is no longer possible.
            GreetingKind::PreAuth if server.security == Security::StartTls => {
                return Err(Error::Unsupported("STARTTLS (server sent PREAUTH)"));
            }
            GreetingKind::PreAuth => true,
        };

        if server.security == Security::StartTls {
            if caps.is_empty() {
                caps = request_capabilities(&mut conn).await?;
            }
            if !caps.has("STARTTLS") {
                return Err(Error::Unsupported("STARTTLS"));
            }
            conn.execute(CommandBody::StartTLS).await?;
            let tls_stream = with_timeout(
                timeout,
                tls.connector().connect(server_name, conn.into_stream()),
            )
            .await?;
            conn = Connection::new(Box::new(tls_stream), false, timeout);
            // Anything learned before TLS may have been tampered with.
            caps = Capabilities::default();
        }
        if caps.is_empty() {
            caps = request_capabilities(&mut conn).await?;
        }
        Ok(Self {
            conn,
            caps,
            authenticated,
            selected: None,
        })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Authenticates, unless the server already did so with a PREAUTH greeting.
    pub async fn login(&mut self, credentials: &Credentials) -> Result<()> {
        if self.authenticated {
            return Ok(());
        }
        let response = match credentials {
            Credentials::Password { username, password } => {
                if self.caps.has("AUTH=PLAIN") {
                    let mut initial = Vec::with_capacity(username.len() + password.len() + 2);
                    initial.push(0);
                    initial.extend_from_slice(username.as_bytes());
                    initial.push(0);
                    initial.extend_from_slice(password.as_bytes());
                    self.conn
                        .authenticate(AuthMechanism::Plain, initial)
                        .await?
                } else if self.caps.has("LOGINDISABLED") {
                    return Err(Error::Unsupported("a password login mechanism"));
                } else {
                    let body =
                        CommandBody::login(username.clone(), password.clone()).map_err(|_| {
                            Error::AuthFailed(
                                "username or password contains a NUL or line break".into(),
                            )
                        })?;
                    self.conn.execute(body).await.map_err(|e| match e {
                        Error::Command { text, .. } => Error::AuthFailed(text),
                        other => other,
                    })?
                }
            }
        };
        // Capabilities usually change after login; servers often include them in the OK.
        self.caps = match capabilities_in(&response) {
            Some(caps) => caps,
            None => request_capabilities(&mut self.conn).await?,
        };
        self.authenticated = true;
        Ok(())
    }

    pub async fn list(&mut self) -> Result<Vec<ListedMailbox>> {
        let body = CommandBody::list("", ListMailbox::try_from("*").expect("valid wildcard"))
            .expect("valid LIST arguments");
        let response = self.conn.execute(body).await?;
        let mut mailboxes = Vec::new();
        for data in response.data {
            let Data::List {
                items,
                delimiter,
                mailbox,
            } = data
            else {
                continue;
            };
            let raw_name = match mailbox {
                Mailbox::Inbox => "INBOX".to_owned(),
                Mailbox::Other(other) => match std::str::from_utf8(other.inner().as_ref()) {
                    Ok(name) => name.to_owned(),
                    Err(_) => {
                        tracing::warn!("skipping mailbox with a non-UTF-8 name");
                        continue;
                    }
                },
            };
            let attributes: Vec<String> = items
                .iter()
                .map(|attr| attr.to_string().to_ascii_lowercase())
                .collect();
            let selectable = !attributes
                .iter()
                .any(|a| a == "\\noselect" || a == "\\nonexistent");
            mailboxes.push(ListedMailbox {
                name: utf7::decode(&raw_name).unwrap_or_else(|| raw_name.clone()),
                raw_name,
                delimiter: delimiter.map(|d| d.inner()),
                selectable,
                special_use: attributes.iter().find_map(|a| special_use(a)),
            });
        }
        Ok(mailboxes)
    }

    /// Opens `raw_name` read-only. With CONDSTORE, also enables MODSEQ reporting.
    pub async fn examine(&mut self, raw_name: &str) -> Result<MailboxStatus> {
        self.selected = None;
        let mailbox = Mailbox::try_from(raw_name.to_owned())
            .map_err(|_| Error::Protocol(format!("invalid mailbox name `{raw_name}`")))?;
        let parameters = if self.condstore() {
            vec![SelectParameter::CondStore]
        } else {
            Vec::new()
        };
        let response = self
            .conn
            .execute(CommandBody::Examine {
                mailbox,
                parameters,
            })
            .await?;

        let (mut uidvalidity, mut uidnext, mut highest_modseq, mut no_modseq) =
            (None, None, None, false);
        for code in &response.codes {
            match code {
                Code::UidValidity(v) => uidvalidity = Some(v.get()),
                Code::UidNext(n) => uidnext = Some(n.get()),
                Code::HighestModSeq(m) => highest_modseq = Some(m.get()),
                Code::NoModSeq => no_modseq = true,
                _ => {}
            }
        }
        // EXISTS decides whether messages were expunged; guessing 0 would empty the
        // folder.
        let exists = response
            .data
            .iter()
            .filter_map(|d| match d {
                Data::Exists(n) => Some(*n),
                _ => None,
            })
            .next_back();
        let missing = |what: &str| Error::Command {
            command: "EXAMINE",
            text: format!("the server sent no {what} for `{raw_name}`"),
        };
        let status = MailboxStatus {
            uidvalidity: uidvalidity.ok_or_else(|| missing("UIDVALIDITY"))?,
            uidnext,
            exists: exists.ok_or_else(|| missing("EXISTS"))?,
            highest_modseq: highest_modseq.filter(|_| self.condstore() && !no_modseq),
        };
        self.selected = Some(status);
        Ok(status)
    }

    /// Every UID in the selected mailbox, ascending.
    pub async fn uid_search_all(&mut self) -> Result<Vec<u32>> {
        self.uid_search(SearchKey::All).await
    }

    /// UIDs from `first` on, ascending. (`first:*` also matches the highest UID when that
    /// is below `first`; such a UID is not returned.)
    pub async fn uid_search_from(&mut self, first: u32) -> Result<Vec<u32>> {
        let Some(first) = NonZeroU32::new(first) else {
            return self.uid_search_all().await;
        };
        let set = SequenceSet::from(Sequence::from(first..));
        let mut uids = self.uid_search(SearchKey::Uid(set)).await?;
        uids.retain(|&uid| uid >= first.get());
        Ok(uids)
    }

    async fn uid_search(&mut self, key: SearchKey<'static>) -> Result<Vec<u32>> {
        if self.selected_status()?.exists == 0 {
            return Ok(Vec::new());
        }
        let response = self
            .conn
            .execute(CommandBody::search(None, Vec1::from(key), true))
            .await?;
        let mut uids: Vec<u32> = response
            .data
            .into_iter()
            .filter_map(|d| match d {
                Data::Search(uids, ..) => Some(uids),
                _ => None,
            })
            .flatten()
            .map(NonZeroU32::get)
            .collect();
        uids.sort_unstable();
        uids.dedup();
        Ok(uids)
    }

    /// Flags, dates, size, summary headers and attachment presence for `uids`; the
    /// caller batches long lists. Messages the server no longer has, or whose answer
    /// couldn't be parsed, are simply absent; responses for UIDs that weren't asked for
    /// are ignored.
    pub async fn fetch_summaries(&mut self, uids: &[u32]) -> Result<Vec<FetchedMessage>> {
        let (mut fetched, skipped) = self.fetch_summaries_once(uids, true).await?;
        // A FETCH line that can't be parsed is lost whole, typically because of a
        // BODYSTRUCTURE the parser rejects. Without asking for it again, such a message
        // is never stored and the folder never gets a complete sync. Attachment presence
        // is then unknown until the body is downloaded, which doesn't depend on it.
        if skipped > 0 {
            let got: HashSet<u32> = fetched.iter().map(|m| m.state.uid).collect();
            let missing: Vec<u32> = uids.iter().copied().filter(|u| !got.contains(u)).collect();
            fetched.extend(self.fetch_summaries_once(&missing, false).await?.0);
        }
        Ok(fetched)
    }

    /// The summaries, and how many response lines couldn't be parsed.
    async fn fetch_summaries_once(
        &mut self,
        uids: &[u32],
        structure: bool,
    ) -> Result<(Vec<FetchedMessage>, usize)> {
        let Some(set) = uid_set(uids) else {
            return Ok((Vec::new(), 0));
        };
        let mut items = vec![
            MessageDataItemName::Uid,
            MessageDataItemName::Flags,
            MessageDataItemName::InternalDate,
            MessageDataItemName::Rfc822Size,
            body_peek(Some(Section::HeaderFields(
                None,
                Vec1::try_from(
                    SUMMARY_HEADER_FIELDS
                        .iter()
                        .map(|f| AString::try_from(*f).expect("valid header field name"))
                        .collect::<Vec<_>>(),
                )
                .expect("non-empty field list"),
            ))),
        ];
        if structure {
            items.push(MessageDataItemName::BodyStructure);
        }
        if self.modseq_enabled() {
            items.push(MessageDataItemName::ModSeq);
        }
        // Lossy: a message whose answer can't be parsed is just missing, and the caller
        // notices the gap.
        let response = self
            .conn
            .execute_lossy(uid_fetch(set, items, Vec::new()))
            .await?;

        let skipped = response.skipped;
        let wanted: HashSet<u32> = uids.iter().copied().collect();
        let fetched = merge_fetches(response, |uid| wanted.contains(&uid))
            .into_iter()
            .filter_map(|(uid, m)| {
                // Without FLAGS or the headers the message would be stored wrongly;
                // leave it out so the caller sees the gap.
                Some(FetchedMessage {
                    state: m.state(uid)?,
                    internal_date: m.internal_date,
                    size: m.size,
                    headers: *m.headers?,
                    has_attachments: m.has_attachments,
                })
            })
            .collect();
        Ok((fetched, skipped))
    }

    /// Current flags of messages with UIDs up to `last_uid`, or with CONDSTORE only of
    /// those changed since `changed_since`.
    pub async fn fetch_flags(
        &mut self,
        last_uid: u32,
        changed_since: Option<u64>,
    ) -> Result<Vec<FlagState>> {
        let Some(last_uid) = NonZeroU32::new(last_uid) else {
            return Ok(Vec::new());
        };
        if self.selected_status()?.exists == 0 {
            return Ok(Vec::new());
        }
        let mut items = vec![MessageDataItemName::Uid, MessageDataItemName::Flags];
        let mut modifiers = Vec::new();
        if self.modseq_enabled() {
            items.push(MessageDataItemName::ModSeq);
            if let Some(since) = changed_since.and_then(NonZeroU64::new) {
                modifiers.push(FetchModifier::ChangedSince(since));
            }
        }
        let set = SequenceSet::from(Sequence::from(NonZeroU32::MIN..=last_uid));
        // Strict: a lost line would hide a flag change for good once the caller stores
        // the new HIGHESTMODSEQ.
        let response = self.conn.execute(uid_fetch(set, items, modifiers)).await?;

        // `1:n` matches the highest UID even if it's above n; skip that one.
        Ok(merge_fetches(response, |uid| uid <= last_uid.get())
            .into_iter()
            .filter_map(|(uid, m)| m.state(uid))
            .collect())
    }

    /// The whole raw message, or `None` if the selected mailbox has no message with
    /// this UID (any more).
    pub async fn fetch_raw(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
        let Some(items) = self.fetch_one(uid, vec![body_peek(None)]).await? else {
            return Ok(None);
        };
        Ok(items.into_iter().find_map(|item| match item {
            MessageDataItem::BodyExt {
                section: None,
                data,
                ..
            } => data.0.map(|d| d.into_inner().into_owned()),
            _ => None,
        }))
    }

    /// The message's MIME structure, or `None` if there is no message with this UID.
    pub async fn fetch_structure(&mut self, uid: u32) -> Result<Option<MimeNode>> {
        let items = self
            .fetch_one(uid, vec![MessageDataItemName::BodyStructure])
            .await?;
        Ok(items.and_then(|items| {
            items.into_iter().find_map(|item| match item {
                MessageDataItem::BodyStructure(structure) => {
                    Some(super::body::mime_node(&structure))
                }
                _ => None,
            })
        }))
    }

    /// Body parts by section (`1.2`), still transfer-encoded, or `None` if there is no
    /// message with this UID. A section the server returns no content for is absent.
    pub async fn fetch_sections(
        &mut self,
        uid: u32,
        sections: &[String],
    ) -> Result<Option<HashMap<String, Vec<u8>>>> {
        let items = sections
            .iter()
            .map(|section| Ok(body_peek(Some(Section::Part(part(section)?)))))
            .collect::<Result<Vec<_>>>()?;
        let Some(fetched) = self.fetch_one(uid, items).await? else {
            return Ok(None);
        };
        Ok(Some(
            fetched
                .into_iter()
                .filter_map(|item| match item {
                    MessageDataItem::BodyExt {
                        section: Some(Section::Part(Part(numbers))),
                        data,
                        ..
                    } => {
                        let section = numbers
                            .as_ref()
                            .iter()
                            .map(|n| n.to_string())
                            .collect::<Vec<_>>()
                            .join(".");
                        Some((section, data.0?.into_inner().into_owned()))
                    }
                    _ => None,
                })
                .collect(),
        ))
    }

    /// Items about one message, merged across FETCH responses; `None` if the server sent
    /// nothing about it. Strict: a body part that couldn't be parsed fails the command.
    async fn fetch_one(
        &mut self,
        uid: u32,
        mut items: Vec<MessageDataItemName<'static>>,
    ) -> Result<Option<Vec<MessageDataItem<'static>>>> {
        let uid = NonZeroU32::new(uid).ok_or_else(|| Error::Protocol("UID 0".into()))?;
        items.insert(0, MessageDataItemName::Uid);
        let set = SequenceSet::from(Sequence::from(uid));
        let response = self.conn.execute(uid_fetch(set, items, Vec::new())).await?;
        Ok(group_fetches(response, |u| u == uid.get()).remove(&uid.get()))
    }

    /// Ends the session politely. Errors are not interesting at this point.
    pub async fn logout(mut self) {
        if let Err(e) = self.conn.execute(CommandBody::Logout).await {
            tracing::debug!(error = %e, "LOGOUT failed");
        }
    }

    fn condstore(&self) -> bool {
        self.caps.has("CONDSTORE") || self.caps.has("QRESYNC")
    }

    fn modseq_enabled(&self) -> bool {
        self.selected.is_some_and(|s| s.highest_modseq.is_some())
    }

    fn selected_status(&self) -> Result<MailboxStatus> {
        self.selected
            .ok_or_else(|| Error::Protocol("no mailbox selected".into()))
    }
}

fn body_peek(section: Option<Section<'static>>) -> MessageDataItemName<'static> {
    MessageDataItemName::BodyExt {
        section,
        partial: None,
        peek: true,
    }
}

/// `1.2` → the part specifier `[1, 2]`.
fn part(section: &str) -> Result<Part> {
    let numbers = section
        .split('.')
        .map(|n| n.parse::<NonZeroU32>().ok())
        .collect::<Option<Vec<_>>>()
        .and_then(|n| Vec1::try_from(n).ok())
        .ok_or_else(|| Error::Protocol(format!("invalid body section `{section}`")))?;
    Ok(Part(numbers))
}

fn uid_fetch(
    sequence_set: SequenceSet,
    items: Vec<MessageDataItemName<'static>>,
    modifiers: Vec<FetchModifier>,
) -> CommandBody<'static> {
    CommandBody::Fetch {
        sequence_set,
        macro_or_item_names: items.into(),
        uid: true,
        modifiers,
    }
}

async fn request_capabilities(conn: &mut Connection) -> Result<Capabilities> {
    let response = conn.execute(CommandBody::Capability).await?;
    capabilities_in(&response).ok_or_else(|| Error::Protocol("no CAPABILITY response".into()))
}

fn capabilities_in(response: &Response) -> Option<Capabilities> {
    let from_data = response.data.iter().find_map(|d| match d {
        Data::Capability(caps) => Some(caps.as_ref()),
        _ => None,
    });
    let from_code = || {
        response.codes.iter().find_map(|c| match c {
            Code::Capability(caps) => Some(caps.as_ref()),
            _ => None,
        })
    };
    from_data.or_else(from_code).map(capabilities)
}

fn capabilities(caps: &[imap_next::imap_types::response::Capability<'_>]) -> Capabilities {
    caps.iter().map(|c| c.to_string()).collect()
}

/// What the FETCH responses for one UID said, combined. A server may split one
/// message's items across several responses (RFC 9051 §7.5.2), and repeat them; later
/// values win.
#[derive(Default)]
struct MergedFetch {
    flags: Option<(Flags, Vec<String>)>,
    modseq: Option<u64>,
    internal_date: Option<i64>,
    size: Option<u32>,
    /// Boxed: flag-only fetches cover the whole mailbox and never set it.
    headers: Option<Box<Headers>>,
    has_attachments: bool,
}

impl MergedFetch {
    /// `None` if the server never sent FLAGS for the message.
    fn state(&self, uid: u32) -> Option<FlagState> {
        let (flags, keywords) = self.flags.clone()?;
        Some(FlagState {
            uid,
            flags,
            keywords,
            modseq: self.modseq,
        })
    }
}

/// Groups FETCH responses by UID, for the UIDs `wanted` accepts. Responses without a UID
/// are unsolicited updates about some other message and are ignored.
fn merge_fetches(response: Response, wanted: impl Fn(u32) -> bool) -> BTreeMap<u32, MergedFetch> {
    group_fetches(response, wanted)
        .into_iter()
        .map(|(uid, items)| {
            let mut m = MergedFetch::default();
            for item in items {
                match item {
                    MessageDataItem::Flags(flags) => m.flags = Some(convert_flags(&flags)),
                    MessageDataItem::ModSeq(v) => m.modseq = Some(v.get()),
                    MessageDataItem::InternalDate(d) => {
                        m.internal_date = Some(d.as_ref().timestamp())
                    }
                    MessageDataItem::Rfc822Size(n) => m.size = Some(n),
                    MessageDataItem::BodyStructure(bs) => {
                        m.has_attachments = super::body::mime_node(&bs).has_attachments()
                    }
                    MessageDataItem::BodyExt { data, .. } => {
                        m.headers = Some(Box::new(parse_headers(
                            data.0.as_ref().map_or(&[][..], |d| d.as_ref()),
                        )));
                    }
                    _ => {}
                }
            }
            (uid, m)
        })
        .collect()
}

/// FETCH items by UID, for the UIDs `wanted` accepts, in response order. A server may
/// split one message's items across several responses (RFC 9051 §7.5.2) and repeat
/// them; later values come later. Responses without a UID are unsolicited updates about
/// some other message and are ignored.
fn group_fetches(
    response: Response,
    wanted: impl Fn(u32) -> bool,
) -> BTreeMap<u32, Vec<MessageDataItem<'static>>> {
    let mut grouped: BTreeMap<u32, Vec<MessageDataItem<'static>>> = BTreeMap::new();
    for data in response.data {
        let Data::Fetch { items, .. } = data else {
            continue;
        };
        let items = items.into_inner();
        let Some(uid) = items.iter().find_map(|item| match item {
            MessageDataItem::Uid(uid) => Some(uid.get()),
            _ => None,
        }) else {
            continue;
        };
        if wanted(uid) {
            grouped.entry(uid).or_default().extend(items);
        }
    }
    grouped
}

/// A SPECIAL-USE attribute (RFC 6154), lower-cased. The role names match; `\\Inbox` is
/// not one of the attributes.
fn special_use(attribute: &str) -> Option<FolderRole> {
    attribute
        .strip_prefix('\\')?
        .parse()
        .ok()
        .filter(|role| *role != FolderRole::Inbox)
}

fn convert_flags(flags: &[FlagFetch<'_>]) -> (Flags, Vec<String>) {
    let mut out = Flags::empty();
    let mut keywords = Vec::new();
    for flag in flags {
        let FlagFetch::Flag(flag) = flag else {
            continue; // \Recent is per-session and meaningless to store.
        };
        match flag {
            Flag::Seen => out.insert(Flags::SEEN),
            Flag::Flagged => out.insert(Flags::FLAGGED),
            Flag::Answered => out.insert(Flags::ANSWERED),
            Flag::Draft => out.insert(Flags::DRAFT),
            Flag::Deleted => out.insert(Flags::DELETED),
            other => keywords.push(other.to_string()),
        }
    }
    keywords.sort();
    keywords.dedup();
    (out, keywords)
}

/// Compresses UIDs into ranges: `[1, 2, 3, 7]` → `1:3,7`.
fn uid_set(uids: &[u32]) -> Option<SequenceSet> {
    let mut sorted: Vec<NonZeroU32> = uids.iter().filter_map(|&u| NonZeroU32::new(u)).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut sequences = Vec::new();
    let mut iter = sorted.into_iter().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while let Some(&next) = iter.peek() {
            if next.get() != end.get() + 1 {
                break;
            }
            end = next;
            iter.next();
        }
        sequences.push(if start == end {
            Sequence::from(start)
        } else {
            Sequence::from(start..=end)
        });
    }
    SequenceSet::try_from(sequences).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_uid_sets() {
        let ranges = |uids: &[u32]| uid_set(uids).map(|s| s.0.as_ref().len());
        assert_eq!(ranges(&[]), None);
        assert_eq!(ranges(&[0]), None);
        assert_eq!(ranges(&[7, 1, 2, 3, 3]), Some(2));
        assert_eq!(ranges(&[1, 3, 5]), Some(3));
    }
}
