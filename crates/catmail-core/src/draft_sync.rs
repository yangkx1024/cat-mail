//! Copies of the drafts in the server's Drafts folder.
//!
//! A draft changed since its copy was made, or without one, is uploaded anew: APPEND
//! with `\Draft`, then the copy it replaces is queued for deletion like any message
//! (`pending_ops`), and the replay that follows deletes it under the usual UIDVALIDITY
//! check. Upload first, delete second: the server always has a version of the draft.
//! Deleting or sending a draft queues the deletion of its copy the same way.
//!
//! An APPEND whose answer is lost may still have stored the message, and doing it again
//! would leave two copies. So every upload has a Message-ID of its own and is committed
//! (`draft_uploads`) before the APPEND; one left unsettled is settled before anything
//! else is uploaded, by searching its folder for that Message-ID. The same search finds
//! the copy's UID on servers without UIDPLUS.
//!
//! Runs with the account's sync permit, right before its ops are replayed
//! ([`ops::replay_queued`](crate::ops::replay_queued)).

use std::sync::Arc;

use catmail_proto::imap::{Error as ImapError, Session};
use catmail_store::{DraftUpload, Touched};
use catmail_types::{
    Account, AccountId, DraftId, Event, Flags, FolderId, FolderRole, ReplayReport,
};

use crate::engine::{Engine, blocking, now};
use crate::{Error, Result, connection, drafts, ops};

/// Settles the account's unsettled uploads, then uploads its drafts whose copy is
/// missing or out of date, each once. Fills in `drafts_uploaded` and `drafts_failed`.
/// Without a Drafts folder the drafts stay local.
pub(crate) async fn upload(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
) -> Result<ReplayReport> {
    let mut report = ReplayReport::default();
    if !engine
        .read(move |db| db.drafts().need_upload(account))
        .await?
    {
        return Ok(report);
    }
    let mut touched = Touched::new();
    let result = run(engine, session, account, &mut report, &mut touched).await;
    // Whatever was committed before a failure too.
    if !touched.is_empty() {
        ops::replayed(engine, account, &touched);
    }
    engine.emit(Event::DraftsChanged { account });
    result.map(|()| report)
}

async fn run(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    report: &mut ReplayReport,
    touched: &mut Touched,
) -> Result<()> {
    let (unsettled, target, owner) = engine
        .read(move |db| {
            Ok((
                db.drafts().uploads(account)?,
                db.folders().by_role(account, FolderRole::Drafts)?,
                db.accounts().get(account)?,
            ))
        })
        .await?;
    let owner = owner.ok_or_else(|| Error::AccountNotFound(account.to_string()))?;
    for upload in unsettled {
        touched.extend(
            try_settle(engine, session, &upload, None)
                .await?
                .unwrap_or_default(),
        );
    }
    let Some((folder, remote_name)) = target else {
        return Ok(());
    };
    let stale = engine.read(move |db| db.drafts().stale(account)).await?;
    for draft in stale {
        match upload_one(engine, session, &owner, draft, (folder, &remote_name)).await? {
            Outcome::Uploaded(changed) => {
                report.drafts_uploaded += 1;
                touched.extend(changed);
            }
            Outcome::Failed => report.drafts_failed += 1,
            Outcome::Skipped => {}
        }
    }
    Ok(())
}

/// What became of one draft.
enum Outcome {
    /// On the server; the copy it replaced is queued for deletion (these folders).
    Uploaded(Touched),
    /// Refused, or the message couldn't be built; the draft says why.
    Failed,
    /// Changed or gone meanwhile (the next pass sees it), or stored where isn't known
    /// yet (the next pass settles it).
    Skipped,
}

/// Uploads the draft as it is now to `target`.
async fn upload_one(
    engine: &Arc<Engine>,
    session: &mut Session,
    owner: &Account,
    draft: DraftId,
    (folder, remote_name): (FolderId, &str),
) -> Result<Outcome> {
    let Some(snapshot) = engine.read(move |db| db.drafts().snapshot(draft)).await? else {
        return Ok(Outcome::Skipped);
    };
    let message_id = drafts::new_message_id(&owner.email);
    let (engine2, owner2, message_id2) = (engine.clone(), owner.clone(), message_id.clone());
    let revision = snapshot.revision;
    let built =
        blocking(move || drafts::build(&engine2, &owner2, &snapshot, &message_id2, true)).await;
    let raw = match built {
        Ok(raw) => raw,
        // A local problem (an attachment's blob gone, say): the rest can still go.
        Err(e) => {
            return failed(engine, draft, None, format!("can't build the message: {e}")).await;
        }
    };
    let remote = remote_name.to_owned();
    let Some(upload) = engine
        .write_durable(move |db| {
            db.drafts()
                .begin_upload(draft, revision, (folder, &remote), &message_id)
        })
        .await?
    else {
        return Ok(Outcome::Skipped);
    };
    let appended = match session
        .append(remote_name, Flags::DRAFT | Flags::SEEN, &raw)
        .await
    {
        Ok(appended) => appended,
        // A refused APPEND stores nothing, nor does one that couldn't be sent.
        Err(e @ (ImapError::Command { .. } | ImapError::InvalidInput(_))) => {
            return failed(engine, draft, Some(upload), e.to_string()).await;
        }
        // Refused before the message was sent (Dovecot does that when it can't store
        // anything): nothing stored either, but the connection is stuck in the command.
        // Left as it is, this would fail every sync of the account, since uploads come
        // first.
        Err(e @ ImapError::Aborted { .. }) => {
            let outcome = failed(engine, draft, Some(upload), e.to_string()).await?;
            *session = connection::connect(engine, owner.id).await?;
            return Ok(outcome);
        }
        // Answered OK, but not all of the answer could be read: stored, at a UID to be
        // found by searching.
        Err(e) if !e.is_fatal() => {
            tracing::debug!(%draft, error = %e, "APPEND answer incomplete");
            None
        }
        // Settled by the next pass.
        Err(e) => return Err(e.into()),
    };
    Ok(try_settle(engine, session, &upload, appended)
        .await?
        .map_or(Outcome::Skipped, Outcome::Uploaded))
}

/// Records why the draft couldn't be uploaded, ending `upload` if there is one.
async fn failed(
    engine: &Arc<Engine>,
    draft: DraftId,
    upload: Option<DraftUpload>,
    error: String,
) -> Result<Outcome> {
    tracing::warn!(%draft, %error, "can't upload draft");
    engine
        .write(move |db| match &upload {
            Some(upload) => db.drafts().upload_refused(upload, &error),
            None => db.drafts().upload_failed(draft, &error),
        })
        .await?;
    Ok(Outcome::Failed)
}

/// [`settle`], or `None` if the server refused something on the way (the folder may be
/// gone, which the sync finds out): the next pass tries again.
async fn try_settle(
    engine: &Arc<Engine>,
    session: &mut Session,
    upload: &DraftUpload,
    appended: Option<(u32, u32)>,
) -> Result<Option<Touched>> {
    match settle(engine, session, upload, appended).await {
        Ok(touched) => Ok(Some(touched)),
        Err(Error::Imap(e)) if !e.is_fatal() => {
            tracing::warn!(folder = %upload.remote_name, error = %e,
                "can't tell where an uploaded draft is");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Records where the upload's message is: at `appended` (UIDVALIDITY, UID) if the
/// server said, else wherever its Message-ID is found (nowhere, if the APPEND never
/// arrived). Returns the folders whose messages or ops changed.
async fn settle(
    engine: &Arc<Engine>,
    session: &mut Session,
    upload: &DraftUpload,
    appended: Option<(u32, u32)>,
) -> Result<Touched> {
    let (uidvalidity, uids) = match appended {
        Some((uidvalidity, uid)) => (uidvalidity, vec![uid]),
        None => {
            let status = session.examine(&upload.remote_name).await?;
            let uids = session.uid_search_message_id(&upload.message_id).await?;
            (status.uidvalidity, uids)
        }
    };
    let upload = upload.clone();
    engine
        .write(move |db| db.drafts().uploaded(&upload, uidvalidity, &uids, now()))
        .await
}
