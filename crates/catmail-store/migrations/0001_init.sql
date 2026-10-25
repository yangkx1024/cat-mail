-- Initial schema.
--
-- Conventions:
--   * Timestamps are unix seconds (INTEGER).
--   * Enum-like columns are TEXT without CHECK constraints so new variants don't need a
--     table rebuild; the Rust side validates them when reading.
--   * Address lists are JSON arrays of {"name": ..., "email": ...}.
--   * Blob hashes are lowercase hex SHA-256 pointing into the on-disk BlobStore. There is
--     no blob table; see BlobStore for the GC contract.
--   * Secrets (passwords) never live here; they go to the OS keyring.
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
    -- Keyring entry holding the password. Random and globally unique; written before
    -- the row exists, so anyone who sees the row also sees the secret.
    secret_key    TEXT    NOT NULL UNIQUE,
    -- The largest message the IMAP server said it takes in APPEND (RFC 7889) when last
    -- connected to, which limits a draft's attachments (the copies in Drafts and Sent).
    -- NULL: not connected to yet, or no limit announced.
    imap_append_limit INTEGER,
    -- standard | gmail: which mailboxes are locations and which labels (see
    -- docs/architecture/labels.md), as found when last connected to.
    mode          TEXT    NOT NULL DEFAULT 'standard',
    created_at    INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

-- The addresses an account sends as, each through an SMTP server and login of its own.
-- One is made with the account (its address and SMTP server); others are added, e.g. a
-- sending service. Exactly one per account is the default, which new drafts are from:
-- it can be changed, not deleted.
CREATE TABLE identities (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email         TEXT    NOT NULL COLLATE NOCASE,
    display_name  TEXT,
    username      TEXT    NOT NULL,
    smtp_host     TEXT    NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_security TEXT    NOT NULL,
    -- Keyring entry holding the SMTP password, as for accounts.secret_key: written before
    -- the row, deleted after it. NULL: the account's (the identity made with it).
    secret_key    TEXT    UNIQUE,
    is_default    INTEGER NOT NULL DEFAULT 0,
    -- The largest message the SMTP server said it accepts (SIZE, RFC 1870) when last
    -- connected to, which limits a draft's attachments. NULL as for imap_append_limit.
    smtp_max_size INTEGER,
    created_at    INTEGER NOT NULL DEFAULT (unixepoch()),
    -- The same address through the same server and login twice is one.
    UNIQUE (account_id, email, smtp_host, smtp_port, username),
    UNIQUE (id, account_id)
) STRICT;

CREATE UNIQUE INDEX identities_default ON identities (account_id) WHERE is_default;

-- One row per remote IMAP mailbox, plus the sync cursor for that mailbox.
--
-- kind (see docs/architecture/labels.md):
--   location  holds messages, each under its own UID: every mailbox of a standard
--             account; All Mail, Trash and Spam on Gmail.
--   label     a Gmail label: its messages are those with a message_labels row for it
--             (Starred: those with \Flagged), and it has no UIDVALIDITY or cursor of
--             its own. One the server doesn't list (hidden in Gmail's settings) that
--             messages still carry has listed = 0.
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
    kind           TEXT    NOT NULL DEFAULT 'location',
    listed         INTEGER NOT NULL DEFAULT 1,
    selectable     INTEGER NOT NULL DEFAULT 1, -- 0 for \Noselect hierarchy nodes
    uidvalidity    INTEGER,
    uidnext        INTEGER,
    highest_modseq INTEGER,
    unread_count   INTEGER NOT NULL DEFAULT 0, -- kept by the *_count_* triggers
    total_count    INTEGER NOT NULL DEFAULT 0,
    last_synced_at INTEGER,
    UNIQUE (account_id, remote_name),
    UNIQUE (id, account_id)
) STRICT;

-- One row per message on the server: its location (a folder of kind 'location'), its
-- UID there, and on Gmail its labels (message_labels). On a standard account a copy of
-- the same email in another folder is another row with its own flags, as on the server;
-- identical raw content is still stored once, because the BlobStore is
-- content-addressed. On Gmail a row is one X-GM-MSGID, whichever of its locations it is
-- in.
--
-- A message moved here locally has no UID until the server makes the move: uid is NULL,
-- and the pending move (pending_ops.message_id = id) says where the server has it
-- meanwhile. The UID comes from COPYUID once moved; without one the row is dropped and
-- the next sync fetches the message. Sync only ever compares the rows with a UID.
CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL,
    folder_id       INTEGER NOT NULL,
    uid             INTEGER,
    -- Mutable per-mailbox state.
    flags           INTEGER NOT NULL DEFAULT 0, -- system flags bitset, see catmail_types::Flags
    keywords_json   TEXT    NOT NULL DEFAULT '[]', -- non-system flags, e.g. ["$Junk"]
    modseq          INTEGER,
    -- Gmail's X-GM-MSGID and X-GM-THRID (unsigned 64-bit, stored as their bits); NULL
    -- on a standard account. The message id stays the same in every location.
    gm_msgid        INTEGER,
    gm_thrid        INTEGER,
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
    reply_to_json   TEXT    NOT NULL DEFAULT '[]',
    date            INTEGER, -- the Date header, else INTERNALDATE; the list sort key
    snippet         TEXT,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    body_state      TEXT    NOT NULL DEFAULT 'headers', -- headers | full (see message_bodies)
    -- The list sort key: `date`, with a missing one sorted last. Lists are ordered by
    -- (list_date, id) and paged by a cursor on it (see catmail_store::messages); never
    -- NULL, so the cursor compares as a row value and seeks in messages_folder_date.
    list_date       INTEGER NOT NULL
                    GENERATED ALWAYS AS (ifnull(date, -9223372036854775808)) VIRTUAL,
    UNIQUE (folder_id, uid),
    UNIQUE (id, account_id),
    FOREIGN KEY (folder_id, account_id) REFERENCES folders(id, account_id) ON DELETE CASCADE
) STRICT;

-- Read backwards for (list_date DESC, id DESC): every index ends with the rowid (`id`),
-- so listing it again would only store it twice.
CREATE INDEX messages_folder_date ON messages (folder_id, list_date);
CREATE INDEX messages_message_id_hdr ON messages (account_id, message_id_hdr);
CREATE UNIQUE INDEX messages_gm_msgid ON messages (account_id, gm_msgid)
    WHERE gm_msgid IS NOT NULL;
-- Gmail's Starred, which is \Flagged rather than a label row; read like
-- messages_folder_date.
CREATE INDEX messages_flagged ON messages (account_id, list_date) WHERE flags & 2 <> 0;

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

-- A local move. Together with messages_count_flags this stays right even if one UPDATE
-- changed both columns: the old folder loses the old state, the new one gains it, and
-- the flags trigger moves the new folder from the old state to the new.
CREATE TRIGGER messages_count_move AFTER UPDATE OF folder_id ON messages
WHEN old.folder_id <> new.folder_id BEGIN
    UPDATE folders SET total_count = total_count - 1,
                       unread_count = unread_count - (old.flags & 1 = 0)
    WHERE id = old.folder_id;
    UPDATE folders SET total_count = total_count + 1,
                       unread_count = unread_count + (old.flags & 1 = 0)
    WHERE id = new.folder_id;
END;

-- The labels of a message on Gmail, besides its location: one row per label mailbox it
-- is in. Starred is \Flagged, never a row here.
CREATE TABLE message_labels (
    message_id INTEGER NOT NULL,
    folder_id  INTEGER NOT NULL,
    account_id INTEGER NOT NULL,
    -- The message's list_date (which never changes), so that a page of a label is two
    -- seeks in message_labels_list as one of a folder is in messages_folder_date.
    list_date  INTEGER NOT NULL,
    PRIMARY KEY (message_id, folder_id),
    FOREIGN KEY (message_id, account_id) REFERENCES messages(id, account_id) ON DELETE CASCADE,
    FOREIGN KEY (folder_id, account_id) REFERENCES folders(id, account_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX message_labels_list ON message_labels (folder_id, list_date, message_id);

-- The messages a label shows are those in All Mail, as on Gmail: not those in Trash or
-- Spam, which keep their labels to have them back when moved out again. Starred shows
-- the flagged ones. Each view has an index, since the triggers below consult them for
-- every message changed.
CREATE VIEW all_mail AS SELECT id FROM folders WHERE kind = 'location' AND role = 'all';
CREATE INDEX folders_all_mail ON folders (id) WHERE kind = 'location' AND role = 'all';
CREATE VIEW starred AS
    SELECT id, account_id FROM folders WHERE kind = 'label' AND role = 'flagged';
CREATE INDEX folders_starred ON folders (account_id) WHERE kind = 'label' AND role = 'flagged';

-- Label counts, like the folder counts above: a message counts in each of its labels,
-- and in Starred if \Flagged, while it is in All Mail. Each change is O(labels).
CREATE TRIGGER labels_count_insert AFTER INSERT ON message_labels BEGIN
    UPDATE folders SET total_count = total_count + 1,
                       unread_count = unread_count + (m.flags & 1 = 0)
    FROM messages m
    WHERE folders.id = new.folder_id AND m.id = new.message_id AND m.folder_id IN all_mail;
END;

CREATE TRIGGER labels_count_delete AFTER DELETE ON message_labels BEGIN
    UPDATE folders SET total_count = total_count - 1,
                       unread_count = unread_count - (m.flags & 1 = 0)
    FROM messages m
    WHERE folders.id = old.folder_id AND m.id = old.message_id AND m.folder_id IN all_mail;
END;

-- Before the message goes, while the label triggers can still see it.
CREATE TRIGGER labels_count_message_delete BEFORE DELETE ON messages BEGIN
    DELETE FROM message_labels WHERE message_id = old.id;
    UPDATE folders SET total_count = total_count - 1,
                       unread_count = unread_count - (old.flags & 1 = 0)
    WHERE old.flags & 2 AND old.folder_id IN all_mail
      AND id IN (SELECT id FROM starred WHERE account_id = old.account_id);
END;

-- And a folder's messages go before the folder, rather than by cascade: by then the
-- trigger above could no longer tell whether they were in All Mail.
CREATE TRIGGER folders_delete_messages BEFORE DELETE ON folders BEGIN
    DELETE FROM messages WHERE folder_id = old.id;
END;

CREATE TRIGGER labels_count_message_insert AFTER INSERT ON messages
WHEN new.flags & 2 AND new.folder_id IN all_mail BEGIN
    UPDATE folders SET total_count = total_count + 1,
                       unread_count = unread_count + (new.flags & 1 = 0)
    WHERE id IN (SELECT id FROM starred WHERE account_id = new.account_id);
END;

-- \Seen or \Flagged changed, or the message moved into or out of All Mail: it leaves
-- the labels it was counted in as it was, and joins them as it is.
CREATE TRIGGER labels_count_message_update AFTER UPDATE OF flags, folder_id ON messages
WHEN (old.flags & 3) <> (new.flags & 3) OR old.folder_id <> new.folder_id BEGIN
    UPDATE folders SET total_count = total_count - 1,
                       unread_count = unread_count - (old.flags & 1 = 0)
    WHERE old.folder_id IN all_mail
      AND (id IN (SELECT folder_id FROM message_labels WHERE message_id = old.id)
           OR (old.flags & 2
               AND id IN (SELECT id FROM starred WHERE account_id = old.account_id)));
    UPDATE folders SET total_count = total_count + 1,
                       unread_count = unread_count + (new.flags & 1 = 0)
    WHERE new.folder_id IN all_mail
      AND (id IN (SELECT folder_id FROM message_labels WHERE message_id = new.id)
           OR (new.flags & 2
               AND id IN (SELECT id FROM starred WHERE account_id = new.account_id)));
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

-- Messages being written. Each is also kept as a message in the server's Drafts
-- folder (remote_*): a draft whose revision differs from remote_revision is uploaded
-- anew (see draft_uploads), and the copy it replaces is deleted through pending_ops.
CREATE TABLE drafts (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id          INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- Same-account only; checked by the repository (SET NULL rules out a composite FK).
    reply_to_message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    -- Sent as this identity. Removing the identity hands its drafts to the account's
    -- default first (the repository; the default itself can't be removed).
    identity_id         INTEGER NOT NULL,
    -- Threading headers of the reply, copied from the original when the draft was
    -- created, so deleting the original doesn't break the thread. Without brackets;
    -- references_hdr is space-separated.
    in_reply_to         TEXT,
    references_hdr      TEXT,
    subject             TEXT    NOT NULL DEFAULT '',
    to_json             TEXT    NOT NULL DEFAULT '[]',
    cc_json             TEXT    NOT NULL DEFAULT '[]',
    bcc_json            TEXT    NOT NULL DEFAULT '[]',
    body_text           TEXT    NOT NULL DEFAULT '',
    -- Bumped by every change, attachments included. Sending builds the message outside
    -- the transaction that queues it, and queues only if this hasn't moved meanwhile.
    revision            INTEGER NOT NULL DEFAULT 0,
    -- Copy uploaded to the server's Drafts folder: where it is, its Message-ID and the
    -- revision it holds. A UID means nothing without the mailbox and its UIDVALIDITY,
    -- so these are set and cleared together. The repository clears them before
    -- deleting a folder or changing its UIDVALIDITY; for a new generation it turns the
    -- copy into an upload to settle, which finds it again by its Message-ID. The CHECK
    -- makes a bare ON DELETE SET NULL fail instead of leaving a dangling UID behind.
    -- Same-account only (SET NULL rules out a composite FK): it is only ever set from an
    -- upload of the draft, whose folder the draft_uploads FK ties to the draft's account.
    remote_folder_id    INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    remote_uidvalidity  INTEGER,
    remote_uid          INTEGER,
    remote_message_id   TEXT,
    remote_revision     INTEGER,
    -- Why the server refused the last upload; cleared by one that succeeds.
    upload_error        TEXT,
    updated_at          INTEGER NOT NULL DEFAULT (unixepoch()),
    CHECK ((remote_folder_id IS NULL) = (remote_uidvalidity IS NULL)
       AND (remote_folder_id IS NULL) = (remote_uid IS NULL)
       AND (remote_folder_id IS NULL) = (remote_message_id IS NULL)
       AND (remote_folder_id IS NULL) = (remote_revision IS NULL)),
    FOREIGN KEY (identity_id, account_id) REFERENCES identities(id, account_id)
) STRICT;

CREATE INDEX drafts_account ON drafts (account_id, updated_at DESC);
CREATE INDEX drafts_identity ON drafts (identity_id);

-- An APPEND of a draft to the Drafts folder whose outcome isn't recorded yet. Each
-- upload has a Message-ID of its own, and the row is committed before the APPEND: if
-- the answer is lost, searching the folder for that Message-ID tells whether the copy
-- was made, so a retry never leaves two. Settled before anything else is uploaded.
-- A copy whose folder got a new UIDVALIDITY comes back here too, to be found again.
-- An upload older than the draft's copy only deletes what it finds.
--
-- draft_id goes NULL when the draft is deleted or sent meanwhile: a copy the upload
-- made is then deleted. Same-account only; checked by the repository (SET NULL rules
-- out a composite FK).
CREATE TABLE draft_uploads (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL,
    draft_id       INTEGER REFERENCES drafts(id) ON DELETE SET NULL,
    folder_id      INTEGER NOT NULL,
    message_id_hdr TEXT    NOT NULL, -- without brackets
    revision       INTEGER NOT NULL,
    FOREIGN KEY (folder_id, account_id) REFERENCES folders(id, account_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX draft_uploads_account ON draft_uploads (account_id);
CREATE INDEX draft_uploads_draft ON draft_uploads (draft_id);

-- Files attached to a draft, copied into the blob store when added.
CREATE TABLE draft_attachments (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    draft_id  INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
    blob_hash TEXT    NOT NULL,
    filename  TEXT    NOT NULL,
    mime_type TEXT    NOT NULL,
    size      INTEGER NOT NULL
) STRICT;

CREATE INDEX draft_attachments_draft ON draft_attachments (draft_id);

-- Messages handed to "send". Each row is a self-contained snapshot: retries never
-- re-read the draft. The SMTP envelope is stored separately from the MIME because Bcc
-- recipients are not in the headers.
--
-- A draft with an outbox row is locked: it can't be edited, and it is deleted with the
-- row once sent. Cancelling a send that never went out deletes the row only, which
-- hands the draft back for editing.
--
-- status:
--   queued    waiting for an attempt (next_attempt_at)
--   sending   SMTP transaction in progress, the final dot not sent yet. Found at
--             startup, it means the process died before the server could have
--             accepted anything, so it goes back to queued.
--   unknown   the final dot is (about to be) sent and the server's verdict hasn't
--             arrived: written *before* sending the dot, so a crash or a lost
--             connection from then on can't lead to an automatic resend, which might
--             deliver twice. The user decides: retry, mark sent, or cancel.
--   accepted  server accepted the message (250 after DATA); saving to Sent is pending.
--             Only the APPEND is retried from here, never the SMTP send.
--   failed    permanently rejected (e.g. 5xx); last_error says why
--   (done)    sent and, where needed, saved to Sent: the row and its draft are deleted.
--
-- attempts counts tries at the current step and is bumped *before* each try: SMTP
-- sends while queued, APPENDs once accepted (reset on acceptance). An APPEND with
-- attempts > 1 first searches Sent for message_id_hdr, since an earlier one may have
-- succeeded without the answer arriving.
CREATE TABLE outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft_id        INTEGER REFERENCES drafts(id) ON DELETE SET NULL,
    -- Sent through this identity's server. An identity can't be removed while entries
    -- use it.
    identity_id     INTEGER NOT NULL,
    raw_blob_hash   TEXT    NOT NULL,
    message_id_hdr  TEXT    NOT NULL, -- without brackets
    subject         TEXT    NOT NULL,
    envelope_from   TEXT    NOT NULL, -- MAIL FROM
    recipients_json TEXT    NOT NULL, -- every RCPT TO address, Bcc included
    status          TEXT    NOT NULL DEFAULT 'queued',
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    last_error      TEXT,
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (identity_id, account_id) REFERENCES identities(id, account_id)
) STRICT;

CREATE INDEX outbox_status ON outbox (status, next_attempt_at);
CREATE INDEX outbox_identity ON outbox (identity_id);
CREATE UNIQUE INDEX outbox_draft ON outbox (draft_id);

-- Local changes already applied optimistically, waiting to be replayed on the server:
-- one row per message, written in the same transaction as the local change.
--
-- (folder_id, uidvalidity, uid) is the message on the server. UIDs are only meaningful
-- within one UIDVALIDITY generation of one mailbox: an op is replayed only after SELECT
-- reports its `uidvalidity`, and becomes a conflict otherwise, as the folder's ops do
-- when a sync finds a new generation, instead of touching whatever message now has
-- that UID. The op's own generation is what counts: the copy of a draft is in Gmail's
-- Drafts, a label, which keeps no UIDVALIDITY locally.
--
-- kind:
--   flags   set flags_add, clear flags_remove: only bits that changed locally, so a
--           refused op can be undone exactly, and flags other clients change meanwhile
--           are left alone (+FLAGS / -FLAGS, never a replace).
--   label   on Gmail, add (label_add = 1) or take off (0) the label target_folder_id
--           (+X-GM-LABELS / -X-GM-LABELS): only if that changed locally, undone like
--           flags. Always in the message's location, never in a label's mailbox.
--   move    to target_folder_id. message_id is the row now shown there, without a UID
--           (see messages); moving or deleting that row again rewrites this op.
--           A move back to its own folder is resolved locally at replay.
--   delete  \Deleted plus expunge. The row is gone.
--
-- Within a folder, flag and label changes are replayed before moves and deletes: one
-- recorded for a message already moved locally (its row is the move's) then reaches the
-- server before the message leaves the folder. A label change waits for an earlier
-- move of its message instead, and follows it to its new place: out of Trash, Gmail
-- takes labels in All Mail.
--
-- status: pending | failed (refused by the server; the local change was undone where
-- nothing later depends on it) | conflict (the UIDs it names are gone with their
-- generation). Failed and conflict rows stay for the user to see, until dismissed.
CREATE TABLE pending_ops (
    id               INTEGER PRIMARY KEY AUTOINCREMENT, -- also the replay order
    account_id       INTEGER NOT NULL,
    folder_id        INTEGER NOT NULL,
    uidvalidity      INTEGER NOT NULL,
    uid              INTEGER NOT NULL,
    kind             TEXT    NOT NULL,
    flags_add        INTEGER NOT NULL DEFAULT 0,
    flags_remove     INTEGER NOT NULL DEFAULT 0,
    target_folder_id INTEGER,
    label_add        INTEGER,
    -- Same-account only; checked by the repository (SET NULL rules out a composite FK).
    message_id       INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    -- A move on a server without MOVE is COPY, then delete, and a COPY repeated after its
    -- answer was lost makes a second copy. So before each COPY the target, its
    -- UIDVALIDITY and UIDNEXT are committed here: a copy it made can only be at or above
    -- that UIDNEXT, and a retry looks there for one byte-identical to the original. If
    -- that can't be told (the target was rebuilt), the op fails, leaving the original.
    -- Once the COPY is answered with COPYUID, the copy's UID (under copy_uidvalidity) is
    -- committed too, before the original is deleted: if that delete's answer is lost,
    -- the original is gone, and only this says which message is the copy (a Message-ID
    -- can't: another message with it may have arrived meanwhile).
    -- No FK: a target deleted since is just one where the copy can't be looked for.
    copy_folder_id   INTEGER,
    copy_uidvalidity INTEGER,
    copy_uidnext     INTEGER,
    copy_uid         INTEGER,
    status           TEXT    NOT NULL DEFAULT 'pending',
    attempts         INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
    -- Folders and their ops go together. Before a folder is deleted the repository undoes
    -- what its ops did locally (messages moved out of it, moves into it).
    FOREIGN KEY (folder_id, account_id) REFERENCES folders(id, account_id) ON DELETE CASCADE,
    FOREIGN KEY (target_folder_id, account_id) REFERENCES folders(id, account_id)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX pending_ops_queue ON pending_ops (folder_id, status, uid);
CREATE INDEX pending_ops_message ON pending_ops (message_id);
CREATE INDEX pending_ops_target ON pending_ops (target_folder_id);
CREATE INDEX pending_ops_account ON pending_ops (account_id, status);

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
