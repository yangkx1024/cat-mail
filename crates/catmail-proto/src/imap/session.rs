use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use catmail_types::{FlagState, Flags, FolderRole, GmailLabel, Security, ServerConfig};
use imap_proto::rfc4315::UidSetMember;
use imap_proto::{
    AttributeValue, Capability, MailboxDatum, MessageSection, NameAttribute, ResponseCode,
    SectionPath, Status, StatusAttribute,
};
use secrecy::ExposeSecret;

use super::command::{Command, Literals};
use super::connection::{Code, Connection, Data, Response};
use super::{
    Capabilities, ChangedFlags, Error, FetchedMessage, Items, ListedMailbox, MailboxStatus, Result,
    utf7,
};
use crate::Credentials;
use crate::mime::{Headers, MimeNode, SUMMARY_HEADER_FIELDS, parse_headers};
use crate::{TlsConfig, net};

/// An IMAP connection, from greeting to logout. Reading never changes the server:
/// mailboxes are opened with EXAMINE, and bodies fetched with `BODY.PEEK`, which leaves
/// `\Seen` alone. Changes take a mailbox opened with [`select`](Self::select) (flags,
/// moves, deletions), or [`append`](Self::append).
pub struct Session {
    conn: Connection,
    caps: Capabilities,
    /// From a PREAUTH greeting, or after `login`.
    authenticated: bool,
    selected: Option<Opened>,
    /// What IDLE reported about flags in `selected`.
    changed_flags: ChangedFlags,
    opened: Instant,
}

/// The mailbox a session has open.
struct Opened {
    /// As the server spells it.
    name: String,
    /// What opening it found.
    status: MailboxStatus,
    /// Opened with SELECT rather than EXAMINE.
    writable: bool,
}

/// Where a COPY or MOVE put the messages (COPYUID, RFC 4315).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyUid {
    /// Of the target mailbox.
    pub uidvalidity: u32,
    /// (old UID, new UID).
    pub uids: Vec<(u32, u32)>,
}

impl Session {
    /// Connects and secures the connection as `server.security` demands. With STARTTLS,
    /// a server that doesn't offer it is an error; there is no silent downgrade.
    pub async fn connect(
        server: &ServerConfig,
        tls: &TlsConfig,
        timeout: Duration,
    ) -> Result<Self> {
        let opened = Instant::now();
        let (stream, server_name) = net::connect(server, tls, timeout).await?;
        let mut conn = Connection::new(stream, timeout);
        let greeting = conn.greeting().await?;
        let mut caps = match greeting.code {
            Some(Code::Known(ResponseCode::Capabilities(caps))) => capabilities(&caps),
            _ => Capabilities::default(),
        };
        let authenticated = match greeting.status {
            Status::Ok => false,
            // Already authenticated, so STARTTLS is no longer possible.
            Status::PreAuth if server.security == Security::StartTls => {
                return Err(Error::Unsupported("STARTTLS (server sent PREAUTH)"));
            }
            Status::PreAuth => true,
            Status::Bye | Status::No | Status::Bad => {
                return Err(Error::Rejected(greeting.text));
            }
        };

        if server.security == Security::StartTls {
            if caps.is_empty() {
                caps = request_capabilities(&mut conn).await?;
            }
            if !caps.has("STARTTLS") {
                return Err(Error::Unsupported("STARTTLS"));
            }
            conn.execute(Command::new("STARTTLS")).await?;
            let tls_stream = net::start_tls(conn.into_stream(), &server_name, tls, timeout).await?;
            // The TLS session has no greeting.
            conn = Connection::new(Box::new(tls_stream), timeout);
            // Anything learned before TLS may have been tampered with.
            caps = Capabilities::default();
        }
        if caps.is_empty() {
            caps = request_capabilities(&mut conn).await?;
        }
        let mut session = Self {
            conn,
            caps: Capabilities::default(),
            authenticated,
            selected: None,
            changed_flags: ChangedFlags::default(),
            opened,
        };
        session.set_capabilities(caps);
        Ok(session)
    }

    /// When [`connect`](Self::connect) began.
    pub fn opened(&self) -> Instant {
        self.opened
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn set_capabilities(&mut self, caps: Capabilities) {
        self.conn.set_literals(if caps.has("LITERAL+") {
            Literals::NonSynchronizing
        } else if caps.has("LITERAL-") {
            Literals::NonSynchronizingUpTo4k
        } else {
            Literals::Synchronizing
        });
        self.caps = caps;
    }

    /// Authenticates, unless the server already did so with a PREAUTH greeting.
    pub async fn login(&mut self, credentials: &Credentials) -> Result<()> {
        if self.authenticated {
            return Ok(());
        }
        let response = if self.caps.has("AUTH=PLAIN") {
            self.conn
                .authenticate("PLAIN", credentials.sasl_plain())
                .await?
        } else if self.caps.has("LOGINDISABLED") {
            return Err(Error::Unsupported("a password login mechanism"));
        } else {
            let command = Command::new("LOGIN")
                .astring(credentials.username.as_bytes())
                .and_then(|c| c.astring(credentials.password.expose_secret().as_bytes()))
                .map_err(|_| Error::AuthFailed("username or password contains a NUL".into()))?;
            self.conn.execute(command).await.map_err(|e| match e {
                Error::Command { text, .. } => Error::AuthFailed(text),
                other => other,
            })?
        };
        // Capabilities usually change after login; servers often include them in the OK.
        let caps = match capabilities_in(&response) {
            Some(caps) => caps,
            None => request_capabilities(&mut self.conn).await?,
        };
        self.set_capabilities(caps);
        self.authenticated = true;
        Ok(())
    }

    /// Every mailbox. A listing without INBOX is `Incomplete` if the user has one after
    /// all: it was cut short or mangled, and the mailboxes it leaves out must not be
    /// taken for deleted ones.
    pub async fn list(&mut self) -> Result<Vec<ListedMailbox>> {
        let response = self
            .conn
            .execute(Command::new("LIST").raw("\"\" \"*\""))
            .await?;
        let mut mailboxes = Vec::new();
        for data in response.data {
            let Data::MailboxData(MailboxDatum::List(list)) = data else {
                continue;
            };
            // INBOX is case-insensitive; the other names are as the server spells them.
            let raw_name = if list.name.eq_ignore_ascii_case("INBOX") {
                "INBOX".to_owned()
            } else {
                list.name.into_owned()
            };
            let selectable = selectable(&list.name_attributes);
            let special_use = list.name_attributes.iter().find_map(special_use);
            mailboxes.push(ListedMailbox {
                name: utf7::decode(&raw_name).unwrap_or_else(|| raw_name.clone()),
                raw_name,
                delimiter: list.delimiter.and_then(|d| d.chars().next()),
                selectable,
                special_use,
            });
        }
        if !mailboxes.iter().any(|m| m.raw_name == "INBOX") && self.has_inbox().await? {
            return Err(Error::Incomplete("LIST"));
        }
        Ok(mailboxes)
    }

    /// RFC 3501 §6.3.8 lists INBOX unless the user has none, which is when selecting it
    /// fails. Asks the server that way, read-only. Leaves no mailbox selected.
    async fn has_inbox(&mut self) -> Result<bool> {
        self.deselect();
        match self
            .conn
            .execute(Command::new("EXAMINE").raw("INBOX"))
            .await
        {
            Ok(_) => Ok(true),
            Err(Error::Command { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Opens `raw_name` read-only. With CONDSTORE, also enables MODSEQ reporting.
    pub async fn examine(&mut self, raw_name: &str) -> Result<MailboxStatus> {
        self.open(raw_name, false).await
    }

    /// Opens `raw_name` read-write, for [`uid_store`](Self::uid_store),
    /// [`uid_move`](Self::uid_move) and [`uid_delete`](Self::uid_delete). Unlike
    /// EXAMINE this may clear `\Recent`, which nothing here relies on.
    pub async fn select(&mut self, raw_name: &str) -> Result<MailboxStatus> {
        self.open(raw_name, true).await
    }

    async fn open(&mut self, raw_name: &str, writable: bool) -> Result<MailboxStatus> {
        self.deselect();
        let command = if writable { "SELECT" } else { "EXAMINE" };
        let mut body = Command::new(command).astring(raw_name.as_bytes())?;
        if self.condstore() {
            body = body.raw("(CONDSTORE)");
        }
        let response = self.conn.execute(body).await?;

        let (mut uidvalidity, mut uidnext, mut highest_modseq, mut no_modseq) =
            (None, None, None, false);
        for code in &response.codes {
            match code {
                Code::Known(ResponseCode::UidValidity(v)) => uidvalidity = Some(*v),
                Code::Known(ResponseCode::UidNext(n)) => uidnext = Some(*n),
                Code::Known(ResponseCode::HighestModSeq(m)) => highest_modseq = Some(*m),
                Code::Other(name) if name == "NOMODSEQ" => no_modseq = true,
                _ => {}
            }
        }
        // EXISTS decides whether messages were expunged; guessing 0 would empty the
        // folder.
        let exists = response
            .data
            .iter()
            .filter_map(|d| match d {
                Data::MailboxData(MailboxDatum::Exists(n)) => Some(*n),
                _ => None,
            })
            .next_back();
        let missing = |what: &str| Error::Command {
            command,
            text: format!("the server sent no {what} for `{raw_name}`"),
        };
        let status = MailboxStatus {
            uidvalidity: uidvalidity.ok_or_else(|| missing("UIDVALIDITY"))?,
            uidnext,
            exists: exists.ok_or_else(|| missing("EXISTS"))?,
            highest_modseq: highest_modseq.filter(|_| self.condstore() && !no_modseq),
        };
        self.selected = Some(Opened {
            name: raw_name.to_owned(),
            status,
            writable,
        });
        self.conn.mailbox_opened(status.exists);
        Ok(status)
    }

    /// The UIDVALIDITY [`examine`](Self::examine) found when it opened `raw_name`, if
    /// that is the mailbox open now, read-only, and `uid` is below the UIDNEXT it found
    /// (none is without one): a message that arrived since can't be fetched here until
    /// the server reports it, Gmail answering with nothing. It holds for as long as the
    /// mailbox stays open (RFC 3501 §2.3.1.1: a server that rebuilds it ends the
    /// session, as Dovecot does), so reading it needs no EXAMINE again. What else opening
    /// it found may be out of date, so it isn't offered.
    pub fn examined(&self, raw_name: &str, uid: u32) -> Option<u32> {
        self.selected
            .as_ref()
            .filter(|o| !o.writable && o.name == raw_name)
            .filter(|o| o.status.uidnext.is_some_and(|next| uid < next))
            .map(|o| o.status.uidvalidity)
    }

    /// Asks the server for nothing but news of the opened mailbox, which keeps a quiet
    /// connection from being closed, and tells whether it still works.
    pub async fn noop(&mut self) -> Result<()> {
        // Only the tagged answer matters.
        self.conn.execute_lossy(Command::new("NOOP")).await?;
        Ok(())
    }

    /// Whether [`idle`](Self::idle) is available (IDLE, RFC 2177).
    pub fn can_idle(&self) -> bool {
        self.caps.has("IDLE")
    }

    /// Waits for the selected mailbox to change on the server (IDLE; see
    /// [`can_idle`](Self::can_idle)), for `stop` to resolve, or for `max` to pass,
    /// whichever comes first. Returns whether the server reported a change: new or
    /// expunged messages, or changed flags. Changes it already reported in answers to
    /// earlier commands since the mailbox was opened count too (and return at once):
    /// a server says so only once, so they would otherwise go unnoticed until the next
    /// one. Changes this session made count as well.
    pub async fn idle(&mut self, stop: impl Future<Output = ()>, max: Duration) -> Result<bool> {
        if !self.can_idle() {
            return Err(Error::Unsupported("IDLE"));
        }
        self.selected_status()?;
        if self.conn.mailbox_changed() {
            return Ok(true);
        }
        let changed = self.conn.idle(stop, max).await?;
        self.resolve_changed_flags().await?;
        Ok(changed)
    }

    /// The messages whose flags the server reported changed in the selected mailbox in
    /// the last [`idle`](Self::idle) since it was opened. Only IDLE says so: outside it,
    /// a FETCH is usually the answer to our own. Always `Unknown` for a change with
    /// CONDSTORE, where what changed is found by MODSEQ instead.
    pub fn changed_flags(&self) -> &ChangedFlags {
        &self.changed_flags
    }

    /// Sets `changed_flags` to the messages the connection saw flags change on, by UID.
    /// It knows them by sequence number, which the next EXPUNGE may change and the next
    /// command may bring one: but not a FETCH by sequence number (RFC 3501 §7.4.1).
    async fn resolve_changed_flags(&mut self) -> Result<()> {
        self.changed_flags = ChangedFlags::Unknown;
        let seqs = match self.conn.take_changed_flags() {
            Some(seqs) if seqs.is_empty() => {
                self.changed_flags = ChangedFlags::default();
                return Ok(());
            }
            Some(seqs) if !self.modseq_enabled() => seqs,
            _ => return Ok(()),
        };
        let Some(set) = uid_set(&seqs) else {
            return Ok(());
        };
        let fetch = Command::new("FETCH").raw(&set).raw("(UID)");
        match self.conn.execute(fetch).await {
            Ok(response) => {
                let uids = group_fetches(response, |_| true).into_keys().collect();
                self.changed_flags = ChangedFlags::Uids(uids);
            }
            Err(e) if !e.is_fatal() => {
                tracing::debug!(error = %e, "can't tell whose flags changed");
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Sets `add` and clears `remove` on messages of the selected mailbox, leaving their
    /// other flags alone. UIDs the mailbox doesn't have are skipped, as IMAP does.
    pub async fn uid_store(&mut self, uids: &[u32], add: Flags, remove: Flags) -> Result<()> {
        let Some(set) = uid_set(uids) else {
            return Ok(());
        };
        for (kind, flags) in [("+FLAGS.SILENT", add), ("-FLAGS.SILENT", remove)] {
            if flags.is_empty() {
                continue;
            }
            let store = Command::new("UID STORE")
                .raw(&set)
                .raw(kind)
                .raw(&format!("({})", system_flags(flags)));
            self.change(store).await?;
        }
        Ok(())
    }

    /// Whether the server has Gmail's extensions (X-GM-EXT-1): [`Items::Gmail`],
    /// [`uid_store_labels`](Self::uid_store_labels) and
    /// [`uid_search_gm_msgid`](Self::uid_search_gm_msgid).
    pub fn is_gmail(&self) -> bool {
        self.caps.has("X-GM-EXT-1")
    }

    fn require_gmail(&self) -> Result<()> {
        if !self.is_gmail() {
            return Err(Error::Unsupported("X-GM-EXT-1"));
        }
        Ok(())
    }

    /// Adds `add` to and removes `remove` from the Gmail labels of messages of the
    /// selected mailbox, leaving their other labels alone (never a replace: labels hidden
    /// from IMAP stay). Starred is `\Flagged`, set with [`uid_store`](Self::uid_store).
    pub async fn uid_store_labels(
        &mut self,
        uids: &[u32],
        add: &[GmailLabel],
        remove: &[GmailLabel],
    ) -> Result<()> {
        self.require_gmail()?;
        let Some(set) = uid_set(uids) else {
            return Ok(());
        };
        for (kind, labels) in [("+X-GM-LABELS", add), ("-X-GM-LABELS", remove)] {
            if labels.is_empty() {
                continue;
            }
            let store =
                Command::new("UID STORE")
                    .raw(&set)
                    .raw(kind)
                    .list(labels, |command, label| match label {
                        GmailLabel::System(role) => system_label(*role)
                            .map(|name| command.raw(name))
                            .ok_or_else(|| {
                                Error::InvalidInput(format!("no Gmail label for {}", role.as_str()))
                            }),
                        GmailLabel::User { remote_name, .. } => {
                            command.astring(remote_name.as_bytes())
                        }
                    })?;
            self.change(store).await?;
        }
        Ok(())
    }

    /// Whether [`uid_move`](Self::uid_move) is available (MOVE, RFC 6851).
    pub fn can_move(&self) -> bool {
        self.caps.has("MOVE")
    }

    /// UIDVALIDITY and UIDNEXT of `raw_name`, without opening it (STATUS); not for the
    /// selected mailbox.
    pub async fn status(&mut self, raw_name: &str) -> Result<(u32, u32)> {
        let command = Command::new("STATUS")
            .astring(raw_name.as_bytes())?
            .raw("(UIDVALIDITY UIDNEXT)");
        let response = self.conn.execute(command).await?;
        let (mut uidvalidity, mut uidnext) = (None, None);
        for data in &response.data {
            let Data::MailboxData(MailboxDatum::Status { status, .. }) = data else {
                continue;
            };
            for item in status {
                match item {
                    StatusAttribute::UidValidity(v) => uidvalidity = Some(*v),
                    StatusAttribute::UidNext(n) => uidnext = Some(*n),
                    _ => {}
                }
            }
        }
        uidvalidity.zip(uidnext).ok_or_else(|| Error::Command {
            command: "STATUS",
            text: format!("the server sent no UIDVALIDITY or UIDNEXT for `{raw_name}`"),
        })
    }

    /// Moves messages of the selected mailbox to `to` (MOVE, RFC 6851; see
    /// [`can_move`](Self::can_move)). Returns where they went if the server says (UIDPLUS). UIDs the
    /// mailbox doesn't have are skipped, so a move is safe to repeat.
    pub async fn uid_move(&mut self, uids: &[u32], to: &str) -> Result<Option<CopyUid>> {
        if !self.can_move() {
            return Err(Error::Unsupported("MOVE"));
        }
        self.copy_or_move(uids, to, "UID MOVE").await
    }

    /// Copies messages of the selected mailbox to `to`; with [`uid_delete`](Self::uid_delete)
    /// a move for servers without MOVE. Unlike a move, a copy repeated after its answer
    /// was lost makes a second one. Returns where they went if the server says (UIDPLUS).
    pub async fn uid_copy(&mut self, uids: &[u32], to: &str) -> Result<Option<CopyUid>> {
        self.copy_or_move(uids, to, "UID COPY").await
    }

    async fn copy_or_move(
        &mut self,
        uids: &[u32],
        to: &str,
        command: &'static str,
    ) -> Result<Option<CopyUid>> {
        let Some(set) = uid_set(uids) else {
            return Ok(None);
        };
        let body = Command::new(command).raw(&set).astring(to.as_bytes())?;
        Ok(copy_uid(&self.change(body).await?, uids.len()))
    }

    /// Deletes messages of the selected mailbox for good: `\Deleted`, then UID EXPUNGE
    /// (UIDPLUS). A server without UIDPLUS only has the plain EXPUNGE, which also removes
    /// any other message someone marked `\Deleted` in this mailbox; as in every client,
    /// that is what the mark asks for.
    pub async fn uid_delete(&mut self, uids: &[u32]) -> Result<()> {
        let Some(set) = uid_set(uids) else {
            return Ok(());
        };
        self.uid_store(uids, Flags::DELETED, Flags::empty()).await?;
        let expunge = if self.caps.has("UIDPLUS") {
            Command::new("UID EXPUNGE").raw(&set)
        } else {
            Command::new("EXPUNGE")
        };
        self.change(expunge).await?;
        Ok(())
    }

    /// Runs a command that changes the selected mailbox. Its answer only matters for
    /// the status and COPYUID, so an unparsable response (an unsolicited FETCH, say)
    /// doesn't fail it: that would make a change that happened look refused.
    async fn change(&mut self, command: Command) -> Result<Response> {
        if !self.selected.as_ref().is_some_and(|o| o.writable) {
            return Err(Error::Protocol(format!(
                "{} needs a mailbox opened with SELECT",
                command.name
            )));
        }
        self.conn.execute_lossy(command).await
    }

    /// Adds `message` to the mailbox `raw_name`. Returns its UIDVALIDITY and UID if the
    /// server says (UIDPLUS). A server that refuses it before taking the message ends the
    /// command all the same: that is a refusal like any other.
    pub async fn append(
        &mut self,
        raw_name: &str,
        flags: Flags,
        message: &[u8],
    ) -> Result<Option<(u32, u32)>> {
        let mut command = Command::new("APPEND").astring(raw_name.as_bytes())?;
        if !flags.is_empty() {
            command = command.raw(&format!("({})", system_flags(flags)));
        }
        let command = command.literal(message)?;
        let response = self.conn.execute(command).await?;
        Ok(response.codes.iter().find_map(|code| match code {
            Code::Known(ResponseCode::AppendUid(uidvalidity, uids)) => {
                expand(uids).next().map(|uid| (*uidvalidity, uid))
            }
            _ => None,
        }))
    }

    /// UIDs of messages in the selected mailbox with this Message-ID (without angle
    /// brackets), ascending.
    pub async fn uid_search_message_id(&mut self, message_id: &str) -> Result<Vec<u32>> {
        let search = Command::new("UID SEARCH")
            .raw("HEADER Message-ID")
            .string(format!("<{message_id}>").as_bytes())
            .map_err(|_| Error::Protocol(format!("invalid Message-ID `{message_id}`")))?;
        self.uid_search(search).await
    }

    /// UIDs of the message with this X-GM-MSGID in the selected mailbox (at most one),
    /// on Gmail.
    pub async fn uid_search_gm_msgid(&mut self, msgid: u64) -> Result<Vec<u32>> {
        self.require_gmail()?;
        let search = Command::new("UID SEARCH").raw(&format!("X-GM-MSGID {msgid}"));
        self.uid_search(search).await
    }

    /// Every UID in the selected mailbox, ascending.
    pub async fn uid_search_all(&mut self) -> Result<Vec<u32>> {
        self.uid_search(Command::new("UID SEARCH").raw("ALL")).await
    }

    /// UIDs from `first` on, ascending. (`first:*` also matches the highest UID when that
    /// is below `first`; such a UID is not returned.)
    pub async fn uid_search_from(&mut self, first: u32) -> Result<Vec<u32>> {
        if first == 0 {
            return self.uid_search_all().await;
        }
        let search = Command::new("UID SEARCH").raw(&format!("UID {first}:*"));
        let mut uids = self.uid_search(search).await?;
        uids.retain(|&uid| uid >= first);
        Ok(uids)
    }

    /// Those of `uids` the selected mailbox has, ascending.
    pub async fn uid_search_uids(&mut self, uids: &[u32]) -> Result<Vec<u32>> {
        let Some(set) = uid_set(uids) else {
            return Ok(Vec::new());
        };
        self.uid_search(Command::new("UID SEARCH").raw("UID").raw(&set))
            .await
    }

    async fn uid_search(&mut self, search: Command) -> Result<Vec<u32>> {
        if self.selected_exists()? == 0 {
            return Ok(Vec::new());
        }
        let response = self.conn.execute(search).await?;
        let mut uids: Vec<u32> = response
            .data
            .into_iter()
            .filter_map(|d| match d {
                Data::MailboxData(MailboxDatum::Search(uids)) => Some(uids),
                _ => None,
            })
            .flatten()
            .filter(|&uid| uid != 0)
            .collect();
        uids.sort_unstable();
        uids.dedup();
        Ok(uids)
    }

    /// Flags, dates, size, summary headers and attachment presence for `uids`, and what
    /// `items` adds; the caller batches long lists. Messages the server no longer has, or
    /// whose answer couldn't be parsed, are simply absent; responses for UIDs that
    /// weren't asked for are ignored.
    pub async fn fetch_summaries(
        &mut self,
        uids: &[u32],
        items: Items,
    ) -> Result<Vec<FetchedMessage>> {
        let (mut fetched, skipped) = self.fetch_summaries_once(uids, true, items).await?;
        // A BODYSTRUCTURE the parser rejects loses the message's answer, whole or just
        // that item. Without asking for it again, such a message is never stored and the
        // folder never gets a complete sync. Attachment presence is then unknown until the
        // body is downloaded, which doesn't depend on it.
        if skipped > 0 {
            let got: HashSet<u32> = fetched.iter().map(|m| m.state.uid).collect();
            let missing: Vec<u32> = uids.iter().copied().filter(|u| !got.contains(u)).collect();
            fetched.extend(self.fetch_summaries_once(&missing, false, items).await?.0);
        }
        Ok(fetched)
    }

    /// The summaries, and how many responses or BODYSTRUCTUREs couldn't be parsed.
    async fn fetch_summaries_once(
        &mut self,
        uids: &[u32],
        structure: bool,
        items: Items,
    ) -> Result<(Vec<FetchedMessage>, usize)> {
        let Some(set) = uid_set(uids) else {
            return Ok((Vec::new(), 0));
        };
        let mut asked = format!(
            "UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS ({})]",
            SUMMARY_HEADER_FIELDS.join(" ")
        );
        if structure {
            asked.push_str(" BODYSTRUCTURE");
        }
        if self.modseq_enabled() {
            asked.push_str(" MODSEQ");
        }
        if items == Items::Gmail {
            asked.push_str(" X-GM-MSGID X-GM-THRID X-GM-LABELS");
        }
        // Lossy: a message whose answer can't be parsed is just missing, and the caller
        // notices the gap.
        let response = self.conn.execute_lossy(uid_fetch(&set, &asked)).await?;

        let mut skipped = response.skipped;
        let wanted: HashSet<u32> = uids.iter().copied().collect();
        let fetched = merge_fetches(response, |uid| wanted.contains(&uid))
            .into_iter()
            .filter_map(|(uid, m)| {
                // Asked for, so missing only if it couldn't be parsed (imap-proto reads a
                // malformed item as one it doesn't know).
                let has_attachments = match m.has_attachments {
                    Some(has) => has,
                    None if structure => {
                        skipped += 1;
                        return None;
                    }
                    None => false,
                };
                // Without FLAGS, the headers or Gmail's items asked for, the message would
                // be stored wrongly; leave it out so the caller sees the gap.
                // INTERNALDATE and RFC822.SIZE may go without.
                let state = m.state(uid, items)?;
                let (gm_msgid, gm_thrid) = match items {
                    Items::Standard => (None, None),
                    Items::Gmail => (Some(m.gm_msgid?), Some(m.gm_thrid?)),
                };
                Some(FetchedMessage {
                    state,
                    internal_date: m.internal_date,
                    size: m.size,
                    headers: *m.headers?,
                    has_attachments,
                    gm_msgid,
                    gm_thrid,
                })
            })
            .collect();
        Ok((fetched, skipped))
    }

    /// Current flags of messages with UIDs in these ranges, or with CONDSTORE only of
    /// those changed since `changed_since`; with [`Items::Gmail`] their labels too (a
    /// change of labels alone raises the MODSEQ).
    pub async fn fetch_flags(
        &mut self,
        uids: &[RangeInclusive<u32>],
        changed_since: Option<u64>,
        items: Items,
    ) -> Result<Vec<FlagState>> {
        let uids = merge_ranges(uids.iter().cloned());
        let Some(set) = sequence_set(&uids) else {
            return Ok(Vec::new());
        };
        if self.selected_exists()? == 0 {
            return Ok(Vec::new());
        }
        let modseq = self.modseq_enabled();
        let mut asked = String::from("UID FLAGS");
        if modseq {
            asked.push_str(" MODSEQ");
        }
        if items == Items::Gmail {
            asked.push_str(" X-GM-LABELS");
        }
        let mut command = uid_fetch(&set, &asked);
        if let Some(since) = changed_since.filter(|&s| modseq && s > 0) {
            command = command.raw(&format!("(CHANGEDSINCE {since})"));
        }
        // Strict: a lost line would hide a flag change for good once the caller stores
        // the new HIGHESTMODSEQ.
        let response = self.conn.execute(command).await?;

        // Some servers answer a range above the highest UID with that one, as for `n:*`.
        let wanted = |uid| {
            let i = uids.partition_point(|r| *r.end() < uid);
            uids.get(i).is_some_and(|r| r.contains(&uid))
        };
        // Every message answered for has its FLAGS (and labels): missing, they couldn't be
        // parsed, and a lost change would stay lost once the caller stores the new
        // HIGHESTMODSEQ.
        merge_fetches(response, wanted)
            .into_iter()
            .map(|(uid, m)| m.state(uid, items).ok_or(INCOMPLETE_FETCH))
            .collect()
    }

    /// RFC822.SIZE of the messages of the selected mailbox with these UIDs; ones it
    /// doesn't have are absent.
    pub async fn fetch_sizes(&mut self, uids: &[u32]) -> Result<HashMap<u32, u32>> {
        let Some(set) = uid_set(uids) else {
            return Ok(HashMap::new());
        };
        let response = self
            .conn
            .execute(uid_fetch(&set, "UID RFC822.SIZE"))
            .await?;
        let wanted: HashSet<u32> = uids.iter().copied().collect();
        merge_fetches(response, |uid| wanted.contains(&uid))
            .into_iter()
            .map(|(uid, m)| Ok((uid, m.size.ok_or(INCOMPLETE_FETCH)?)))
            .collect()
    }

    /// The whole raw message, or `None` if the selected mailbox has no message with
    /// this UID (any more).
    pub async fn fetch_raw(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
        let items = self.fetch_one(uid, "BODY.PEEK[]").await?;
        let data = requested(items, |item| match item {
            AttributeValue::BodySection {
                section: None,
                data,
                ..
            } => Some(data),
            _ => None,
        })?;
        Ok(data.flatten().map(|d| d.into_owned()))
    }

    /// The message's MIME structure, or `None` if there is no message with this UID.
    pub async fn fetch_structure(&mut self, uid: u32) -> Result<Option<MimeNode>> {
        let items = self.fetch_one(uid, "BODYSTRUCTURE").await?;
        requested(items, |item| match item {
            AttributeValue::BodyStructure(structure) => Some(super::body::mime_node(&structure)),
            _ => None,
        })
    }

    /// Body parts by section (`1.2`), still transfer-encoded, or `None` if there is no
    /// message with this UID. A section the server returns no content for is absent. With
    /// no sections, nothing is sent and nothing is found.
    pub async fn fetch_sections(
        &mut self,
        uid: u32,
        sections: &[String],
    ) -> Result<Option<HashMap<String, Vec<u8>>>> {
        self.fetch_parts(uid, sections, false).await
    }

    /// The MIME headers of body parts by section (`BODY.PEEK[1.2.MIME]`), as the sender
    /// wrote them, or `None` if there is no message with this UID. A section the server
    /// returns nothing for is absent.
    pub async fn fetch_mime_headers(
        &mut self,
        uid: u32,
        sections: &[String],
    ) -> Result<Option<HashMap<String, Vec<u8>>>> {
        self.fetch_parts(uid, sections, true).await
    }

    /// `BODY.PEEK[<section>]` for each of `sections`, or with `mime`
    /// `BODY.PEEK[<section>.MIME]`.
    async fn fetch_parts(
        &mut self,
        uid: u32,
        sections: &[String],
        mime: bool,
    ) -> Result<Option<HashMap<String, Vec<u8>>>> {
        // A FETCH must ask for something.
        if sections.is_empty() {
            return Ok(Some(HashMap::new()));
        }
        let suffix = if mime { ".MIME" } else { "" };
        let kind = mime.then_some(MessageSection::Mime);
        let items = sections
            .iter()
            .map(|section| Ok(format!("BODY.PEEK[{}{suffix}]", part(section)?)))
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let Some(fetched) = self.fetch_one(uid, &items).await? else {
            return Ok(None);
        };
        let mut found = HashMap::new();
        for item in fetched {
            if let AttributeValue::BodySection {
                section: Some(SectionPath::Part(numbers, got)),
                data,
                ..
            } = item
                && got == kind
            {
                let section = numbers
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(".");
                found.insert(section, data);
            }
        }
        // Each one asked for is in the answer, if only as NIL; else it couldn't be parsed.
        if sections.iter().any(|s| !found.contains_key(s)) {
            return Err(INCOMPLETE_FETCH);
        }
        Ok(Some(
            found
                .into_iter()
                .filter_map(|(section, data)| Some((section, data?.into_owned())))
                .collect(),
        ))
    }

    /// Items about one message, merged across FETCH responses; `None` if the server sent
    /// nothing about it. Strict: a body part that couldn't be parsed fails the command.
    async fn fetch_one(
        &mut self,
        uid: u32,
        items: &str,
    ) -> Result<Option<Vec<AttributeValue<'static>>>> {
        if uid == 0 {
            return Err(Error::Protocol("UID 0".into()));
        }
        let fetch = uid_fetch(&uid.to_string(), &format!("UID {items}"));
        let response = self.conn.execute(fetch).await?;
        Ok(group_fetches(response, |u| u == uid).remove(&uid))
    }

    /// Ends the session politely. Errors are not interesting at this point.
    pub async fn logout(mut self) {
        match self.conn.execute(Command::new("LOGOUT")).await {
            // The BYE that LOGOUT asks for.
            Ok(_) | Err(Error::Bye(_)) => {}
            Err(e) => tracing::debug!(error = %e, "LOGOUT failed"),
        }
    }

    /// Forgets the selected mailbox, before opening another: if that fails, the server
    /// has none selected either.
    fn deselect(&mut self) {
        self.selected = None;
        self.changed_flags = ChangedFlags::default();
    }

    fn condstore(&self) -> bool {
        self.caps.has("CONDSTORE") || self.caps.has("QRESYNC")
    }

    fn modseq_enabled(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|o| o.status.highest_modseq.is_some())
    }

    fn selected_status(&self) -> Result<MailboxStatus> {
        self.selected
            .as_ref()
            .map(|o| o.status)
            .ok_or_else(|| Error::Protocol("no mailbox selected".into()))
    }

    /// Messages in the selected mailbox, as of the server's last word: searches skip a
    /// mailbox that is empty.
    fn selected_exists(&self) -> Result<u32> {
        self.selected_status()?;
        Ok(self.conn.exists())
    }
}

/// `\Seen \Flagged`, for a flag list.
fn system_flags(flags: Flags) -> String {
    SYSTEM_FLAGS
        .into_iter()
        .filter(|(bit, _)| flags.contains(*bit))
        .map(|(_, flag)| flag)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The UIDs a UID set from the server names, in its order. A range covers the UIDs
/// between its ends, whichever comes first (imap-proto puts the smaller first).
fn expand(set: &[UidSetMember]) -> impl Iterator<Item = u32> + '_ {
    set.iter().flat_map(|member| match member {
        UidSetMember::Uid(uid) => *uid..=*uid,
        UidSetMember::UidRange(range) => range.clone(),
    })
}

/// The COPYUID in an answer to COPY or MOVE (MOVE sends it untagged). `None` if there is
/// none, its UID sets don't pair up, or they name more than the `requested` UIDs.
fn copy_uid(response: &Response, requested: usize) -> Option<CopyUid> {
    response.codes.iter().find_map(|code| {
        let Code::Known(ResponseCode::CopyUid(uidvalidity, source, destination)) = code else {
            return None;
        };
        // The ends come from the server: count before expanding, or `1:4294967295`
        // asks for gigabytes. The copies can't outnumber the UIDs asked for.
        let count = |set: &[UidSetMember]| {
            set.iter()
                .map(|member| match member {
                    UidSetMember::Uid(_) => 1,
                    UidSetMember::UidRange(r) => u64::from(r.end() - r.start()) + 1,
                })
                .sum::<u64>()
        };
        let n = count(source);
        // RFC 4315: both sets list the UIDs in the same order.
        (n == count(destination) && n <= requested as u64).then(|| CopyUid {
            uidvalidity: *uidvalidity,
            uids: expand(source).zip(expand(destination)).collect(),
        })
    })
}

/// `1.2`, checked to be a part specifier.
fn part(section: &str) -> Result<&str> {
    let valid = section
        .split('.')
        .all(|n| n.parse::<u32>().is_ok_and(|n| n > 0) && n.bytes().all(|b| b.is_ascii_digit()));
    if !valid {
        return Err(Error::Protocol(format!("invalid body section `{section}`")));
    }
    Ok(section)
}

/// A FETCH answered OK without an item it asked for: imap-proto reads a malformed item as
/// one it doesn't know, so this is how an item that couldn't be parsed shows.
const INCOMPLETE_FETCH: Error = Error::Incomplete("UID FETCH");

/// The item `pick` takes from the answer about one message (see [`Session::fetch_one`]):
/// `None` if there was no answer, [`INCOMPLETE_FETCH`] if the item is missing from it.
fn requested<T>(
    items: Option<Vec<AttributeValue<'static>>>,
    pick: impl FnMut(AttributeValue<'static>) -> Option<T>,
) -> Result<Option<T>> {
    match items {
        None => Ok(None),
        Some(items) => items
            .into_iter()
            .find_map(pick)
            .map(Some)
            .ok_or(INCOMPLETE_FETCH),
    }
}

/// `UID FETCH <set> (<items>)`.
fn uid_fetch(set: &str, items: &str) -> Command {
    Command::new("UID FETCH")
        .raw(set)
        .raw(&format!("({items})"))
}

async fn request_capabilities(conn: &mut Connection) -> Result<Capabilities> {
    let response = conn.execute(Command::new("CAPABILITY")).await?;
    capabilities_in(&response).ok_or_else(|| Error::Protocol("no CAPABILITY response".into()))
}

fn capabilities_in(response: &Response) -> Option<Capabilities> {
    let from_data = response.data.iter().find_map(|d| match d {
        Data::Capabilities(caps) => Some(caps),
        _ => None,
    });
    let from_code = || {
        response.codes.iter().find_map(|c| match c {
            Code::Known(ResponseCode::Capabilities(caps)) => Some(caps),
            _ => None,
        })
    };
    from_data.or_else(from_code).map(|caps| capabilities(caps))
}

fn capabilities(caps: &[Capability<'_>]) -> Capabilities {
    caps.iter()
        .map(|c| match c {
            Capability::Imap4rev1 => "IMAP4rev1".to_owned(),
            Capability::Auth(mechanism) => format!("AUTH={mechanism}"),
            Capability::Atom(atom) => atom.to_string(),
        })
        .collect()
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
    /// From BODYSTRUCTURE, if the server sent one.
    has_attachments: Option<bool>,
    gm_msgid: Option<u64>,
    gm_thrid: Option<u64>,
    labels: Option<Vec<GmailLabel>>,
}

impl MergedFetch {
    /// `None` if the server never sent FLAGS for the message, or X-GM-LABELS with
    /// [`Items::Gmail`].
    fn state(&self, uid: u32, items: Items) -> Option<FlagState> {
        let (flags, keywords) = self.flags.clone()?;
        let labels = match items {
            Items::Standard => None,
            Items::Gmail => Some(self.labels.clone()?),
        };
        Some(FlagState {
            uid,
            flags,
            keywords,
            modseq: self.modseq,
            labels,
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
            // Parsed once the structure, whose text charsets they may need, is known.
            let mut headers = None;
            let mut structure = None;
            for item in items {
                match item {
                    AttributeValue::Flags(flags) => m.flags = Some(convert_flags(&flags)),
                    AttributeValue::ModSeq(v) => m.modseq = Some(v),
                    AttributeValue::InternalDate(d) => m.internal_date = internal_date(&d),
                    AttributeValue::Rfc822Size(n) => m.size = Some(n),
                    AttributeValue::BodyStructure(bs) => {
                        structure = Some(super::body::mime_node(&bs));
                    }
                    AttributeValue::BodySection { data, .. } => headers = Some(data),
                    AttributeValue::GmailMsgId(id) => m.gm_msgid = Some(id),
                    AttributeValue::GmailThrId(id) => m.gm_thrid = Some(id),
                    AttributeValue::GmailLabels(labels) => {
                        m.labels = Some(labels.iter().filter_map(|l| gmail_label(l)).collect());
                    }
                    _ => {}
                }
            }
            m.has_attachments = structure.as_ref().map(MimeNode::has_attachments);
            if let Some(data) = headers {
                let raw = data.as_deref().unwrap_or_default();
                // Only a header that isn't UTF-8 needs them.
                let hints = match &structure {
                    Some(structure) if std::str::from_utf8(raw).is_err() => {
                        structure.text_charsets()
                    }
                    _ => Vec::new(),
                };
                m.headers = Some(Box::new(parse_headers(raw, &hints)));
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
) -> BTreeMap<u32, Vec<AttributeValue<'static>>> {
    let mut grouped: BTreeMap<u32, Vec<AttributeValue<'static>>> = BTreeMap::new();
    for data in response.data {
        let Data::Fetch(_, items) = data else {
            continue;
        };
        let Some(uid) = items.iter().find_map(|item| match item {
            AttributeValue::Uid(uid) => Some(*uid),
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

/// False for `\Noselect` / `\NonExistent` hierarchy nodes.
fn selectable(attributes: &[NameAttribute<'_>]) -> bool {
    !attributes.iter().any(|a| match a {
        NameAttribute::NoSelect => true,
        NameAttribute::Extension(other) => other.eq_ignore_ascii_case("\\NonExistent"),
        _ => false,
    })
}

/// The role a SPECIAL-USE attribute (RFC 6154, and `\Important` from RFC 8457) gives a
/// mailbox.
fn special_use(attribute: &NameAttribute<'_>) -> Option<FolderRole> {
    Some(match attribute {
        NameAttribute::All => FolderRole::All,
        NameAttribute::Archive => FolderRole::Archive,
        NameAttribute::Drafts => FolderRole::Drafts,
        NameAttribute::Flagged => FolderRole::Flagged,
        NameAttribute::Junk => FolderRole::Junk,
        NameAttribute::Sent => FolderRole::Sent,
        NameAttribute::Trash => FolderRole::Trash,
        NameAttribute::Extension(other) if other.eq_ignore_ascii_case("\\Important") => {
            FolderRole::Important
        }
        _ => return None,
    })
}

/// Gmail's system labels in X-GM-LABELS, by the role of their mailbox.
const SYSTEM_LABELS: [(FolderRole, &str); 4] = [
    (FolderRole::Inbox, "\\Inbox"),
    (FolderRole::Sent, "\\Sent"),
    (FolderRole::Drafts, "\\Draft"),
    (FolderRole::Important, "\\Important"),
];

fn system_label(role: FolderRole) -> Option<&'static str> {
    SYSTEM_LABELS
        .iter()
        .find(|(r, _)| *r == role)
        .map(|(_, name)| *name)
}

/// A label as X-GM-LABELS names it: a system label starts with a backslash (sent as a
/// quoted string, `"\\Inbox"`), a user label is the mailbox name. `\Starred` is left
/// out, being `\Flagged`, as are system labels without a mailbox role.
fn gmail_label(label: &str) -> Option<GmailLabel> {
    if !label.starts_with('\\') {
        let name = utf7::decode(label).unwrap_or_else(|| label.to_owned());
        return Some(GmailLabel::user(label, name));
    }
    let found = SYSTEM_LABELS
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(label))
        .map(|(role, _)| GmailLabel::System(*role));
    if found.is_none() && !label.eq_ignore_ascii_case("\\Starred") {
        tracing::debug!(label, "unknown Gmail system label");
    }
    found
}

/// The system flags stored as [`Flags`] bits.
const SYSTEM_FLAGS: [(Flags, &str); 5] = [
    (Flags::SEEN, "\\Seen"),
    (Flags::FLAGGED, "\\Flagged"),
    (Flags::ANSWERED, "\\Answered"),
    (Flags::DRAFT, "\\Draft"),
    (Flags::DELETED, "\\Deleted"),
];

fn convert_flags<S: AsRef<str>>(flags: &[S]) -> (Flags, Vec<String>) {
    let mut out = Flags::empty();
    let mut keywords = Vec::new();
    for flag in flags {
        let flag = flag.as_ref();
        // \Recent is per-session and meaningless to store.
        if flag.eq_ignore_ascii_case("\\Recent") {
            continue;
        }
        match SYSTEM_FLAGS
            .iter()
            .find(|(_, f)| f.eq_ignore_ascii_case(flag))
        {
            Some((bit, _)) => out.insert(*bit),
            None => keywords.push(flag.to_owned()),
        }
    }
    keywords.sort();
    keywords.dedup();
    (out, keywords)
}

/// INTERNALDATE (`17-Jul-1996 02:44:25 -0700`) as unix seconds. The day may come
/// space-padded (` 2-Jul`), zero-padded (`02-Jul`) or, as QQ Mail sends it against
/// RFC 3501, bare (`2-Jul`).
fn internal_date(value: &str) -> Option<i64> {
    let time = jiff::fmt::strtime::parse("%d-%b-%Y %H:%M:%S %z", value).ok()?;
    Some(time.to_timestamp().ok()?.as_second())
}

/// Compresses UIDs into ranges: `[1, 2, 3, 7]` → `1:3,7`.
fn uid_set(uids: &[u32]) -> Option<String> {
    sequence_set(&merge_ranges(uids.iter().map(|&uid| uid..=uid)))
}

/// The ranges sorted, overlapping and adjacent ones joined; 0, which no UID or sequence
/// number is, left out.
fn merge_ranges(ranges: impl IntoIterator<Item = RangeInclusive<u32>>) -> Vec<RangeInclusive<u32>> {
    let mut ranges: Vec<_> = ranges
        .into_iter()
        .map(|r| (*r.start()).max(1)..=*r.end())
        .filter(|r| !r.is_empty())
        .collect();
    ranges.sort_unstable_by_key(|r| *r.start());
    let mut merged: Vec<RangeInclusive<u32>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if *r.start() <= last.end().saturating_add(1) => {
                *last = *last.start()..=*last.end().max(r.end());
            }
            _ => merged.push(r),
        }
    }
    merged
}

/// The sequence set of ranges as [`merge_ranges`] leaves them; `None` for none.
fn sequence_set(ranges: &[RangeInclusive<u32>]) -> Option<String> {
    let set: Vec<String> = ranges
        .iter()
        .map(|r| {
            if r.start() == r.end() {
                r.start().to_string()
            } else {
                format!("{}:{}", r.start(), r.end())
            }
        })
        .collect();
    (!set.is_empty()).then(|| set.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_uid_sets() {
        assert_eq!(uid_set(&[]), None);
        assert_eq!(uid_set(&[0]), None);
        assert_eq!(uid_set(&[7, 1, 2, 3, 3]).as_deref(), Some("1:3,7"));
        assert_eq!(uid_set(&[1, 3, 5]).as_deref(), Some("1,3,5"));
    }

    #[test]
    fn merges_ranges() {
        let empty = RangeInclusive::new(6, 5);
        assert_eq!(
            merge_ranges([7..=9, 1..=1, 0..=0, 2..=3, 3..=4, empty, 11..=11]),
            [1..=4, 7..=9, 11..=11]
        );
        assert_eq!(merge_ranges([0..=2, 3..=3]), [RangeInclusive::new(1, 3)]);
    }

    #[test]
    fn reads_internaldate() {
        // 1996-07-17 09:44:25 UTC.
        assert_eq!(
            internal_date("17-Jul-1996 02:44:25 -0700"),
            Some(837_596_665)
        );
        let utc = internal_date("02-Oct-2026 00:13:27 +0000");
        assert_eq!(internal_date(" 2-Oct-2026 08:13:27 +0800"), utc);
        assert_eq!(internal_date("2-Oct-2026 08:13:27 +0800"), utc, "QQ Mail");
        assert_eq!(internal_date("02-oct-2026 08:13:27 +0800"), utc);
        assert_eq!(
            internal_date("29-Feb-2024 00:00:00 +0000"),
            Some(1_709_164_800)
        );
        for bad in [
            "",
            "2-Oct-2026",
            "32-Oct-2026 08:13:27 +0800",
            "30-Feb-2026 08:13:27 +0800",
            "2-Foo-2026 08:13:27 +0800",
            "2-Oct-2026 08:13:27 0800",
            "2-Oct-2026 08:13:27 +08",
            "2-Oct-2026 08:13:27 +0800 x",
        ] {
            assert_eq!(internal_date(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn flags_by_name_whatever_their_case() {
        let (flags, keywords) =
            convert_flags(&["\\SEEN", "\\flagged", "\\Recent", "$Junk", "$Junk"]);
        assert_eq!(flags, Flags::SEEN | Flags::FLAGGED);
        assert_eq!(keywords, ["$Junk"]);
    }

    #[test]
    fn list_attributes() {
        let attributes = |line: &[u8]| {
            let Ok((_, imap_proto::Response::MailboxData(MailboxDatum::List(list)))) =
                imap_proto::Response::parse(line)
            else {
                panic!("not a LIST");
            };
            let roles: Vec<_> = list
                .name_attributes
                .iter()
                .filter_map(special_use)
                .collect();
            (selectable(&list.name_attributes), roles)
        };
        assert_eq!(
            attributes(b"* LIST (\\HasNoChildren \\All) \"/\" a\r\n"),
            (true, vec![FolderRole::All])
        );
        assert_eq!(
            attributes(b"* LIST (\\HasNoChildren \\important) \"/\" i\r\n"),
            (true, vec![FolderRole::Important])
        );
        assert_eq!(
            attributes(b"* LIST (\\Noselect) \"/\" b\r\n"),
            (false, vec![])
        );
        assert_eq!(
            attributes(b"* LIST (\\NonExistent \\Trash) \"/\" c\r\n"),
            (false, vec![FolderRole::Trash])
        );
    }

    #[test]
    fn system_labels_are_those_of_the_shared_roles() {
        let roles = SYSTEM_LABELS.map(|(role, _)| role);
        assert_eq!(roles, GmailLabel::SYSTEM_ROLES);
    }

    #[test]
    fn gmail_labels_by_role_or_name() {
        let labels = |line: &[u8]| {
            let Ok((_, imap_proto::Response::Fetch(_, items))) = imap_proto::Response::parse(line)
            else {
                panic!("not a FETCH");
            };
            items
                .iter()
                .find_map(|i| match i {
                    AttributeValue::GmailLabels(labels) => Some(
                        labels
                            .iter()
                            .filter_map(|l| gmail_label(l))
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .unwrap()
        };
        let user = GmailLabel::user;
        assert_eq!(
            labels(
                b"* 1 FETCH (UID 4 X-GM-LABELS (\"\\\\Inbox\" \"\\\\Important\" \"\\\\Starred\" \
                  \"\\\\Draft\" \"\\\\Muted\" Work &ZeVnLIqe- \"Two words\"))\r\n"
            ),
            [
                GmailLabel::System(FolderRole::Inbox),
                GmailLabel::System(FolderRole::Important),
                GmailLabel::System(FolderRole::Drafts),
                user("Work", "Work"),
                user("&ZeVnLIqe-", "日本語"),
                user("Two words", "Two words"),
            ]
        );
        assert_eq!(labels(b"* 1 FETCH (UID 4 X-GM-LABELS ())\r\n"), []);
    }

    #[test]
    fn body_sections_must_be_part_numbers() {
        assert_eq!(part("1.2").unwrap(), "1.2");
        for bad in ["", "0", "1.", "1.x", "1.HEADER", "+1", "1]"] {
            assert!(part(bad).is_err(), "{bad:?}");
        }
    }
}
