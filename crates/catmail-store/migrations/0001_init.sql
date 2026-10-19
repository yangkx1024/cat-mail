-- Initial schema.
--
-- Conventions:
--   * Timestamps are unix seconds (INTEGER).
--   * Enum-like columns are TEXT without CHECK constraints so new variants don't need a
--     table rebuild; the Rust side validates them when reading.
--   * Address lists are JSON arrays of {"name": ..., "email": ...}.
--   * Blob hashes are lowercase hex SHA-256 pointing into the on-disk BlobStore. There is
--     no blob table; see BlobStore for the GC contract.
--   * Secrets (passwords, OAuth tokens) never live here; they go to the OS keyring.
--   * Tables whose ids leave the database (frontends, keyring keys) use AUTOINCREMENT so
--     a deleted id is never handed out again.
--   * Rows that belong to an account and point at another account-owned row use a
--     composite foreign key over (id, account_id), so they cannot cross accounts.
--     Where that is impossible (ON DELETE SET NULL would also null account_id), the
--     repository checks it.

CREATE TABLE accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    email         TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT,
    username      TEXT    NOT NULL,
    imap_host     TEXT    NOT NULL,
    imap_port     INTEGER NOT NULL,
    imap_security TEXT    NOT NULL,
    smtp_host     TEXT    NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_security TEXT    NOT NULL,
    auth_kind     TEXT    NOT NULL,
    -- Keyring entry holding the password. Random and globally unique; written before
    -- the row exists, so anyone who sees the row also sees the secret.
    secret_key    TEXT    NOT NULL UNIQUE,
    created_at    INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

-- One row per remote IMAP mailbox, plus the sync cursor for that mailbox.
--
-- Invariant: every row in `messages` for a folder belongs to the folder's current
-- `uidvalidity`. When the server reports a new UIDVALIDITY, the folder's messages are
-- deleted and `uidvalidity` is updated in the same transaction.
--
-- The sync cursor (uidnext, highest_modseq) is only written once the folder is fully
-- in sync with the server as of that cursor. Progress within an interrupted sync is
-- the set of message rows itself: the next sync diffs local UIDs against the server's.
CREATE TABLE folders (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_name    TEXT    NOT NULL, -- exactly as the server sends it (modified UTF-7); used in commands
    name           TEXT    NOT NULL, -- decoded for display
    delimiter      TEXT,
    role           TEXT,
    selectable     INTEGER NOT NULL DEFAULT 1, -- 0 for \Noselect hierarchy nodes
    uidvalidity    INTEGER,
    uidnext        INTEGER,
    highest_modseq INTEGER,
    unread_count   INTEGER NOT NULL DEFAULT 0, -- kept by the messages_count_* triggers
    total_count    INTEGER NOT NULL DEFAULT 0,
    last_synced_at INTEGER,
    UNIQUE (account_id, remote_name),
    UNIQUE (id, account_id)
) STRICT;

-- One row per message *in a mailbox* (an IMAP (mailbox, UID) pair). A copy of the same
-- email in another folder is another row with its own flags, as on the server. Identical
-- raw content is still stored once, because the BlobStore is content-addressed.
-- Cross-folder grouping of copies (e.g. via RFC 8474 EMAILID) can be added later as a
-- nullable column; it must never replace per-row state.
CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL,
    folder_id       INTEGER NOT NULL,
    uid             INTEGER NOT NULL,
    -- Mutable per-mailbox state.
    flags           INTEGER NOT NULL DEFAULT 0, -- system flags bitset, see catmail_types::Flags
    keywords_json   TEXT    NOT NULL DEFAULT '[]', -- non-system flags, e.g. ["$Junk"]
    modseq          INTEGER,
    -- Immutable for the lifetime of the (folder, uid) pair.
    internal_date   INTEGER,
    size            INTEGER,
    message_id_hdr  TEXT,
    in_reply_to     TEXT,
    references_hdr  TEXT,
    subject         TEXT,
    from_json       TEXT    NOT NULL DEFAULT '[]',
    to_json         TEXT    NOT NULL DEFAULT '[]',
    cc_json         TEXT    NOT NULL DEFAULT '[]',
    date            INTEGER, -- the Date header, else INTERNALDATE; the list sort key
    snippet         TEXT,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    body_state      TEXT    NOT NULL DEFAULT 'headers', -- headers | full (see message_bodies)
    thread_id       INTEGER,
    UNIQUE (folder_id, uid),
    UNIQUE (id, account_id),
    FOREIGN KEY (folder_id, account_id) REFERENCES folders(id, account_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX messages_folder_date ON messages (folder_id, date DESC, uid DESC);
CREATE INDEX messages_account_date ON messages (account_id, date DESC);
CREATE INDEX messages_thread ON messages (thread_id);
CREATE INDEX messages_message_id_hdr ON messages (account_id, message_id_hdr);

-- Folder counts follow every change to `messages`, whoever makes it (sync, local flag
-- changes, cascading deletes), so no writer can forget them. Unread = no \Seen (bit 0).
CREATE TRIGGER messages_count_insert AFTER INSERT ON messages BEGIN
    UPDATE folders SET total_count = total_count + 1,
                       unread_count = unread_count + (new.flags & 1 = 0)
    WHERE id = new.folder_id;
END;

CREATE TRIGGER messages_count_delete AFTER DELETE ON messages BEGIN
    UPDATE folders SET total_count = total_count - 1,
                       unread_count = unread_count - (old.flags & 1 = 0)
    WHERE id = old.folder_id;
END;

CREATE TRIGGER messages_count_flags AFTER UPDATE OF flags ON messages
WHEN (old.flags & 1) <> (new.flags & 1) BEGIN
    UPDATE folders SET unread_count = unread_count + (new.flags & 1 = 0) - (old.flags & 1 = 0)
    WHERE id = new.folder_id;
END;

-- The downloaded body of a message; exists iff messages.body_state = 'full'. Written in
-- one transaction with the message's attachments rows, its snippet and its FTS entry.
CREATE TABLE message_bodies (
    message_id    INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    text_plain    TEXT,
    text_html     TEXT, -- already sanitized
    raw_blob_hash TEXT  -- the whole raw message; only kept for messages fetched whole
) STRICT;

-- Attachments and inline parts of a downloaded message. Small messages are fetched whole
-- and their parts stored at once; for large ones only the text is fetched, and a part
-- is fetched by its section (and decoded per transfer_encoding) when first opened.
CREATE TABLE attachments (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id        INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part_section      TEXT    NOT NULL, -- IMAP body section, e.g. '1.2'
    transfer_encoding TEXT,             -- as in BODYSTRUCTURE, e.g. 'base64'
    filename          TEXT,
    mime_type         TEXT    NOT NULL,
    size              INTEGER,          -- decoded; an estimate until downloaded
    content_id        TEXT,
    is_inline         INTEGER NOT NULL DEFAULT 0,
    blob_hash         TEXT,             -- NULL until downloaded
    UNIQUE (message_id, part_section)
) STRICT;

CREATE TABLE drafts (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id          INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- Same-account only; checked by the repository (SET NULL rules out a composite FK).
    reply_to_message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    subject             TEXT,
    to_json             TEXT    NOT NULL DEFAULT '[]',
    cc_json             TEXT    NOT NULL DEFAULT '[]',
    bcc_json            TEXT    NOT NULL DEFAULT '[]',
    body_text           TEXT,
    body_html           TEXT,
    -- Copy uploaded to the server's Drafts folder. A UID means nothing without the
    -- mailbox and its UIDVALIDITY, so the three are set and cleared together.
    -- The repository clears all three before deleting a folder or changing its
    -- UIDVALIDITY; the CHECK makes a bare ON DELETE SET NULL fail instead of leaving a
    -- dangling UID behind.
    remote_folder_id    INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    remote_uidvalidity  INTEGER,
    remote_uid          INTEGER,
    updated_at          INTEGER NOT NULL DEFAULT (unixepoch()),
    CHECK ((remote_folder_id IS NULL) = (remote_uidvalidity IS NULL)
       AND (remote_folder_id IS NULL) = (remote_uid IS NULL))
) STRICT;

CREATE TABLE draft_attachments (
    id        INTEGER PRIMARY KEY,
    draft_id  INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    blob_hash TEXT    NOT NULL,
    filename  TEXT    NOT NULL,
    mime_type TEXT    NOT NULL
) STRICT;

CREATE INDEX draft_attachments_draft ON draft_attachments (draft_id);

-- Messages handed to "send". Each row is a self-contained snapshot: retries never
-- re-read the draft. The SMTP envelope is stored separately from the MIME because Bcc
-- recipients are not in the headers.
--
-- status:
--   queued    waiting for an attempt (next_attempt_at)
--   sending   SMTP transaction in progress
--   accepted  server accepted the message (250 after DATA); saving to Sent is pending.
--             Only the APPEND is retried from here, never the SMTP send.
--   unknown   connection lost after the final DATA dot, outcome unknown. Never retried
--             automatically, since resending may deliver twice; the user decides.
--   done      sent and, where needed, saved to Sent
--   failed    permanently rejected (e.g. 5xx); last_error says why
CREATE TABLE outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft_id        INTEGER REFERENCES drafts(id) ON DELETE SET NULL,
    raw_blob_hash   TEXT    NOT NULL,
    envelope_from   TEXT    NOT NULL, -- MAIL FROM
    recipients_json TEXT    NOT NULL, -- every RCPT TO address, Bcc included
    status          TEXT    NOT NULL DEFAULT 'queued',
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    last_error      TEXT,
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE INDEX outbox_status ON outbox (status, next_attempt_at);

-- Local changes already applied optimistically, waiting to be replayed on the server.
-- Written in the same transaction as the local change they describe.
--
-- UIDs are only meaningful within one UIDVALIDITY generation of one mailbox. An op is
-- replayed only after SELECT reports the same `uidvalidity`; otherwise it goes to
-- reconciliation instead of touching whatever message now has that UID.
CREATE TABLE pending_ops (
    id           INTEGER PRIMARY KEY AUTOINCREMENT, -- also the replay order
    account_id   INTEGER NOT NULL,
    folder_id    INTEGER NOT NULL,
    uidvalidity  INTEGER NOT NULL,
    kind         TEXT    NOT NULL, -- set_flags | move | delete | append
    payload_json TEXT    NOT NULL, -- UIDs and kind-specific arguments
    status       TEXT    NOT NULL DEFAULT 'pending', -- pending | failed | conflict
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    created_at   INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (folder_id, account_id) REFERENCES folders(id, account_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX pending_ops_queue ON pending_ops (account_id, folder_id, status, id);

-- Full-text index keyed by messages.id (rowid). Contentless: the text lives in messages
-- and message_bodies, the index only stores tokens. The Messages repository writes a row
-- when a message is inserted and rewrites it when the body arrives. CJK text is
-- pre-segmented on the Rust side, for indexing and querying alike (see fts.rs), since
-- unicode61 treats a run of CJK characters as a single token.
CREATE VIRTUAL TABLE messages_fts USING fts5 (
    subject,
    from_text,
    to_text,
    body,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;
