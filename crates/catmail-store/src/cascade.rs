//! Changes that run on from one repository into others, as methods of [`Db`].
//!
//! A repository changes only its own rows, and looks up only those its rows refer to: a
//! draft's identity and folder, an op's folder, a message's folders and attachments. It
//! never calls a repository whose rows refer to its own. So a change that has to carry on
//! into another repository is made here, each repository's part in turn, in the one
//! transaction the `Db` is bound to: a folder that goes takes the ops and draft copies
//! addressed by its UIDs with it, a draft that was sent goes with its outbox entry.

use catmail_types::{
    Account, AccountId, Flags, FolderId, IdentityId, MessageId, NewAccount, NewIdentity,
    OutboxEntry, OutboxId,
};

use crate::drafts::DraftUpload;
use crate::folders::RemoteFolder;
use crate::touched::Touched;
use crate::{Db, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub changed: bool,
    /// Other folders whose messages changed with them, and the accounts whose queued
    /// changes did: messages moved locally into a deleted folder go back.
    pub touched: Touched,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedIdentity {
    pub account: AccountId,
    /// Its own keyring entry, if it had one.
    pub secret_key: Option<String>,
    /// Drafts written as it, now written as the account's default.
    pub drafts: usize,
}

/// Accounts and identities.
impl Db<'_> {
    /// `secret_key` names the keyring entry that already holds the account's password.
    /// Also makes its first identity, the default: its address through `new.smtp`,
    /// logging in as the account.
    pub fn insert_account(&self, new: &NewAccount, secret_key: &str) -> Result<Account> {
        let account = self.accounts().insert(new, secret_key)?;
        let identity = NewIdentity {
            email: new.email.clone(),
            display_name: new.display_name.clone(),
            username: new.username.clone(),
            smtp: new.smtp.clone(),
        };
        self.identities().insert_default(account.id, &identity)?;
        Ok(account)
    }

    /// Changes the name the identity sends as; `None` for the address alone. Drafts
    /// written as it are uploaded again, for their copies on the server to be from the
    /// new name. Returns its account and how many drafts there were, or `None` if there
    /// is no such identity.
    pub fn set_identity_name(
        &self,
        id: IdentityId,
        name: Option<&str>,
    ) -> Result<Option<(AccountId, usize)>> {
        let Some((account, changed)) = self.identities().set_display_name(id, name)? else {
            return Ok(None);
        };
        let drafts = if changed {
            self.drafts().reupload_identity(id)?
        } else {
            0
        };
        Ok(Some((account, drafts)))
    }

    /// Deletes the identity; drafts written as it go to the account's default. Refused
    /// for the default, and while the outbox has messages to send through it: they would
    /// have no server. `None` if there was no such identity.
    pub fn delete_identity(&self, id: IdentityId, now: i64) -> Result<Option<DeletedIdentity>> {
        let identities = self.identities();
        let Some((account, default)) = identities.successor(id)? else {
            return Ok(None);
        };
        let drafts = self.drafts().reassign_identity(id, default, now)?;
        let secret_key = identities.delete(id)?;
        Ok(Some(DeletedIdentity {
            account,
            secret_key,
            drafts,
        }))
    }
}

/// Folders, and what is addressed by their UIDs: messages, queued ops, draft copies.
impl Db<'_> {
    /// Makes the account's folders match `remote`: inserts new ones, updates changed
    /// attributes, and deletes folders the server no longer lists (with their messages).
    /// A label that messages still carry stays, unlisted: Gmail leaves the labels hidden
    /// in its settings out of LIST, not out of X-GM-LABELS.
    pub fn reconcile_folders(
        &self,
        account: AccountId,
        remote: &[RemoteFolder],
    ) -> Result<Reconciled> {
        let folders = self.folders();
        let listing = folders.store_listing(account, remote)?;
        let mut changed = listing.changed;
        let mut touched = Touched::new();
        for folder in listing.gone {
            touched.merge(self.remove_folder(folder)?);
        }
        for folder in folders.emptied(account)? {
            touched.merge(self.end_generation(folder, None)?);
            changed = true;
        }
        if listing.recount {
            folders.recount(account)?;
        }
        Ok(Reconciled { changed, touched })
    }

    /// Deletes the folder with its messages, as when the server no longer has it. A
    /// label comes off its messages, whose lists show it. Messages moved out of it
    /// locally go with it; those moved into it go back. Returns the other folders whose
    /// messages this changed, and the accounts whose queued changes it did.
    pub fn remove_folder(&self, folder: FolderId) -> Result<Touched> {
        let folders = self.folders();
        // As they are before anything goes.
        let mut touched = folders.lists_of(folder)?;
        touched.merge(self.ops().forget_folder(folder)?);
        self.drafts().end_generation(folder, None)?;
        folders.delete(folder)?;
        Ok(touched)
    }

    /// Starts a new UIDVALIDITY generation (or the first one): every message row with a
    /// UID and the cursor of the old generation are dropped, pending operations against
    /// old UIDs become conflicts, and draft copies addressed by old UIDs are forgotten.
    /// Returns the folders whose messages or queued changes this changed, besides
    /// `folder`'s own messages: local moves out of it are undone.
    pub fn reset_uidvalidity(&self, folder: FolderId, uidvalidity: u32) -> Result<Touched> {
        self.end_generation(folder, Some(uidvalidity))
    }

    /// Ends the UIDVALIDITY generation of every location of the account, as a new
    /// UIDVALIDITY would: its mode changed, so what a message row is changed with it, and
    /// sync fetches them anew. Returns what [`reset_uidvalidity`](Self::reset_uidvalidity)
    /// does.
    pub fn end_generations(&self, account: AccountId) -> Result<Touched> {
        let mut touched = Touched::new();
        for folder in self.folders().sync_states(account)? {
            if folder.uidvalidity.is_some() {
                touched.merge(self.end_generation(folder.id, None)?);
            }
        }
        Ok(touched)
    }

    /// Drops everything tied to the folder's UIDs except what belongs to `next`, the
    /// generation that replaces it (`None`: the folder no longer holds messages at all):
    /// its message rows and cursor, the ops against them, the drafts' copies in it.
    fn end_generation(&self, folder: FolderId, next: Option<u32>) -> Result<Touched> {
        self.folders().end_generation(folder, next)?;
        let touched = self.ops().end_generation(folder, next)?;
        self.drafts().end_generation(folder, next)?;
        Ok(touched)
    }
}

/// Drafts and the outbox.
impl Db<'_> {
    /// Settles an upload: the server has its message at `uids` under `uidvalidity`, which
    /// it just reported (searched by its Message-ID; none if the APPEND didn't go
    /// through). The newest becomes the draft's copy, and the others are queued for
    /// deletion, as is the copy it replaces; see [`Drafts::uploaded`](crate::Drafts).
    /// Returns the folders whose messages or ops changed.
    pub fn settle_draft_upload(
        &self,
        upload: &DraftUpload,
        uidvalidity: u32,
        uids: &[u32],
        now: i64,
    ) -> Result<Touched> {
        let ops = self.ops();
        let mut touched = Touched::new();
        for (folder, uidvalidity, uid) in self.drafts().uploaded(upload, uidvalidity, uids)? {
            touched.merge(ops.delete_on_server(upload.account, folder, uidvalidity, uid, now)?);
        }
        Ok(touched)
    }

    /// The server accepted the message; saving to Sent comes next, at once. A reply
    /// marks the message it answers `\Answered` (locally and through the queue), if that
    /// is still there. Returns what that touched, or `None` if the entry wasn't being sent.
    pub fn mark_outbox_accepted(&self, id: OutboxId, now: i64) -> Result<Option<Touched>> {
        match self.outbox().mark_accepted(id, now)? {
            Some(original) => Ok(Some(self.answered(original, now)?)),
            None => Ok(None),
        }
    }

    /// The user confirmed a message with an unknown outcome arrived: save it to Sent, as
    /// [`mark_outbox_accepted`](Self::mark_outbox_accepted) does.
    pub fn mark_outbox_sent(
        &self,
        id: OutboxId,
        now: i64,
    ) -> Result<Option<(OutboxEntry, Touched)>> {
        let outbox = self.outbox();
        let Some(original) = outbox.mark_sent(id, now)? else {
            return Ok(None);
        };
        let touched = self.answered(original, now)?;
        Ok(outbox.get(id)?.map(|entry| (entry, touched)))
    }

    /// Sent and saved: deletes the entry and the draft it came from (whose copy on the
    /// server goes with the next upload pass). Returns false if the entry is gone
    /// (cancelled meanwhile).
    pub fn finish_outbox(&self, id: OutboxId) -> Result<bool> {
        let Some(draft) = self.outbox().finish(id)? else {
            return Ok(false);
        };
        if let Some(draft) = draft {
            self.drafts().remove(draft)?;
        }
        Ok(true)
    }

    /// Removes the entry. One that never went out hands its draft back for editing;
    /// for an accepted one this gives up saving it to Sent, and its draft is deleted.
    /// A send in progress can't be cancelled (the engine waits for it to end first).
    pub fn cancel_outbox(&self, id: OutboxId) -> Result<Option<OutboxEntry>> {
        let Some((entry, draft)) = self.outbox().cancel(id)? else {
            return Ok(None);
        };
        if let Some(draft) = draft {
            self.drafts().remove(draft)?;
        }
        Ok(Some(entry))
    }

    /// [`cancel_outbox`](Self::cancel_outbox), if no attempt can be working on the entry:
    /// it is queued or failed. An attempt only ever starts on a queued entry, and
    /// atomically ([`Outbox::begin_send`](crate::Outbox::begin_send)), so this needs no
    /// send permit. `None` if the entry is in another state, or gone.
    pub fn withdraw_outbox(&self, id: OutboxId) -> Result<Option<OutboxEntry>> {
        if !self.outbox().withdrawable(id)? {
            return Ok(None);
        }
        self.cancel_outbox(id)
    }

    /// Marks the message a reply answers `\Answered`, locally and through the queue.
    fn answered(&self, original: Option<MessageId>, now: i64) -> Result<Touched> {
        match original {
            Some(message) => self
                .ops()
                .set_flags(&[message], Flags::ANSWERED, Flags::empty(), now),
            None => Ok(Touched::new()),
        }
    }
}
