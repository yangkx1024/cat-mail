//! Creating, renaming and deleting an account's folders (on Gmail, its labels).
//!
//! Made on the server at once, unlike changes to messages, which are queued: a folder
//! that is only local has no UIDVALIDITY to tie moves into it to, and a name the server
//! refuses (taken, not allowed) would only be found out later. Each runs on a pooled
//! connection holding the account's sync permit, then lists the mailboxes and stores
//! them as a sync does, so what the account has is what the server says. See
//! `docs/architecture/folders.md`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use catmail_proto::imap::{Session, utf7};
use catmail_store::Reconciled;
use catmail_types::{
    AccountId, Folder, FolderId, FolderRole, can_change_folder, can_create_in, special_folder,
};

use crate::draft_sync::Uploads;
use crate::engine::{Engine, lock};
use crate::pool::with_session;
use crate::{Error, Result, ops, sync};

/// Creates a folder named `name` under `parent`, which must allow one
/// ([`can_create_in`]), or at the top of the account's folders. On a server that keeps
/// them all under INBOX, "the top" is under INBOX.
pub(crate) async fn create(
    engine: Arc<Engine>,
    account: AccountId,
    parent: Option<FolderId>,
    name: String,
) -> Result<Folder> {
    // Everything is read holding the permit: a rename may be moving the parent.
    let permit = engine.sync_permit(account).await;
    permit
        .run(async {
            let (folders, place) = engine
                .read(move |db| {
                    let folders = db.folders().list(account)?;
                    let place = match parent.or_else(|| top_level(&folders)) {
                        Some(id) => Some((id, db.folders().remote_name(id)?)),
                        None => None,
                    };
                    Ok((folders, place))
                })
                .await?;
            let (remote_name, name) = match place {
                Some((id, remote_name)) => {
                    let (Some(folder), Some(remote_name)) =
                        (folders.iter().find(|f| f.id == id), remote_name)
                    else {
                        return Err(Error::FolderNotFound(id));
                    };
                    // The INBOX a server keeps every folder in is "the top", not asked for.
                    if parent.is_some() && !can_create_in(&folders, id) {
                        return Err(Error::InvalidInput(format!(
                            "folders can't be made in “{}”",
                            folder.name
                        )));
                    }
                    let d = folder.delimiter.ok_or_else(|| {
                        Error::InvalidInput("this server has no folders inside others".into())
                    })?;
                    // Namespaces may have delimiters of their own: the parent's is the one.
                    let leaf = check_name(&name, Some(d))?;
                    named(Some((&remote_name, &folder.name, d)), leaf)
                }
                None => {
                    let delimiter = special_folder(&folders, FolderRole::Inbox)
                        .and_then(|f| f.delimiter)
                        .or_else(|| folders.iter().find_map(|f| f.delimiter));
                    named(None, check_name(&name, delimiter)?)
                }
            };
            check_free(&folders, &name, None)?;

            with_session(&engine, account, {
                let engine = engine.clone();
                let mut again = false;
                async move |session| {
                    if !listed_again(session, &mut again)
                        .await?
                        .is_some_and(|names| names.contains(&remote_name))
                    {
                        session.create(&remote_name).await?;
                    }
                    sync::store_folders(&engine, session, account, |_| Ok(Reconciled::default()))
                        .await?;
                    Ok(())
                }
            })
            .await?;
            engine
                .read(move |db| db.folders().find_by_name(account, &name))
                .await?
                .ok_or_else(|| Error::Conflict("the server doesn't list the new folder".into()))
        })
        .await
}

/// Renames the folder to `name`, under the same folder as before, and the folders under
/// it with it. They keep their messages and queued changes.
pub(crate) async fn rename(engine: Arc<Engine>, id: FolderId, name: String) -> Result<Folder> {
    let permit = engine.sync_permit(account_of(&engine, id).await?).await;
    permit
        .run(async {
            let (folder, folders, old_remote) = editable(&engine, id).await?;
            let account = folder.account_id;
            let leaf = check_name(&name, folder.delimiter)?;
            let parent = folder.delimiter.and_then(|d| {
                let (remote, _) = old_remote.rsplit_once(d)?;
                let (name, _) = folder.name.rsplit_once(d)?;
                Some((remote, name, d))
            });
            let (remote_name, name) = named(parent, leaf);
            if name == folder.name {
                return Ok(folder);
            }
            check_free(&folders, &name, Some(id))?;

            with_session(&engine, account, {
                let engine = engine.clone();
                let mut again = false;
                async move |session| {
                    if !listed_again(session, &mut again)
                        .await?
                        .is_some_and(|names| {
                            names.contains(&remote_name) && !names.contains(&old_remote)
                        })
                    {
                        session.rename(&old_remote, &remote_name).await?;
                    }
                    let (remote_name, name) = (remote_name.clone(), name.clone());
                    sync::store_folders(&engine, session, account, move |db| {
                        db.folders().rename(id, &remote_name, &name)?;
                        Ok(Reconciled {
                            changed: true,
                            ..Default::default()
                        })
                    })
                    .await?;
                    Ok(())
                }
            })
            .await?;
            engine
                .read(move |db| db.folders().get(id))
                .await?
                .ok_or(Error::FolderNotFound(id))
        })
        .await
}

/// Deletes the folder, the folders under it (as the server lists them now), and the
/// messages in them, on the server too. On Gmail a label comes off its messages, which
/// stay. Queued changes go to the server first, so that messages moved out of them
/// aren't deleted with them; it fails, deleting nothing, if some of those changes to
/// their messages are refused or can't be made. Local changes to their messages are
/// refused while it runs. If the server refuses to delete one of the folders, those
/// deleted before it stay deleted.
pub(crate) async fn delete(engine: Arc<Engine>, id: FolderId) -> Result<()> {
    let permit = engine.sync_permit(account_of(&engine, id).await?).await;
    permit
        .run(async {
            let (folder, folders, remote_name) = editable(&engine, id).await?;
            let account = folder.account_id;
            let mut ids: Vec<FolderId> = folders
                .iter()
                .filter(|f| folder.is_above(f))
                .map(|f| f.id)
                .collect();
            ids.push(id);
            let local: Vec<(FolderId, String)> = engine
                .read({
                    let ids = ids.clone();
                    move |db| {
                        let mut local = Vec::new();
                        for id in ids {
                            local.extend(db.folders().remote_name(id)?.map(|name| (id, name)));
                        }
                        Ok(local)
                    }
                })
                .await?;
            // Before the queue is looked at: a change made after that is refused.
            let _deleting = engine.deleting(ids.clone());
            // Every try adds to what was refused, as replaying does (see `replay_account`).
            let replayed = Arc::new(Mutex::new(ops::Replayed::default()));

            with_session(&engine, account, {
                let (engine, folder_name) = (engine.clone(), folder.name.clone());
                let delimiter = folder.delimiter;
                async move |session| {
                    let mut done = std::mem::take(&mut *lock(&replayed));
                    let result =
                        ops::replay_queued(&engine, session, account, Uploads::All, &mut done)
                            .await;
                    let refused_here = done
                        .folders
                        .iter()
                        .any(|(id, r)| r.failed > 0 && ids.contains(id));
                    *lock(&replayed) = done;
                    result?;
                    if refused_here {
                        return Err(Error::Conflict(format!(
                            "the server refused changes to messages in “{folder_name}”, so \
                             nothing was deleted"
                        )));
                    }
                    // In a write: one that went before has committed, and every one after
                    // sees the folders marked.
                    let waiting = engine
                        .write(move |db| db.ops().pending_folders(account))
                        .await?;
                    if let Some(f) = waiting.iter().find(|f| ids.contains(&f.id)) {
                        return Err(Error::Conflict(format!(
                            "changes to messages in “{}” haven't reached the server yet",
                            f.name
                        )));
                    }

                    // As the server has them now: another client may have made folders in
                    // it, and a try after a lost answer finds some deleted already. The
                    // deepest first, as a folder deleted with others under it is only
                    // emptied; one is always longer than the folder it is in.
                    let listed = listed_names(session).await?;
                    let under = delimiter.map(|d| format!("{remote_name}{d}"));
                    let mut doomed: Vec<&String> = listed
                        .iter()
                        .filter(|name| {
                            **name == remote_name
                                || under.as_ref().is_some_and(|u| name.starts_with(u))
                        })
                        .collect();
                    doomed.sort_by_key(|name| std::cmp::Reverse(name.len()));
                    let mut refused = None;
                    let mut deleted = HashSet::new();
                    for name in doomed {
                        if let Err(e) = session.delete(name).await {
                            let e = Error::from(e);
                            if !e.is_refusal() {
                                return Err(e);
                            }
                            refused = Some(e);
                            break;
                        }
                        deleted.insert(name.clone());
                    }
                    // What the server deleted goes here too, whatever it refused after.
                    // Removed first: a label messages carry would otherwise stay, unlisted.
                    let gone: Vec<FolderId> = local
                        .iter()
                        .filter(|(_, name)| deleted.contains(name) || !listed.contains(name))
                        .map(|(id, _)| *id)
                        .collect();
                    sync::store_folders(&engine, session, account, move |db| {
                        let mut done = Reconciled {
                            changed: !gone.is_empty(),
                            ..Default::default()
                        };
                        for id in gone {
                            done.touched.merge(db.folders().remove(id)?);
                        }
                        Ok(done)
                    })
                    .await?;
                    refused.map_or(Ok(()), Err)
                }
            })
            .await
        })
        .await
}

/// The account the folder is in.
pub(crate) async fn account_of(engine: &Arc<Engine>, id: FolderId) -> Result<AccountId> {
    engine
        .read(move |db| db.folders().get(id))
        .await?
        .map(|f| f.account_id)
        .ok_or(Error::FolderNotFound(id))
}

/// The folder, its account's folders and its name on the server, if the user may change
/// it ([`can_change_folder`]).
async fn editable(engine: &Arc<Engine>, id: FolderId) -> Result<(Folder, Vec<Folder>, String)> {
    let found = engine
        .read(move |db| {
            let Some(folder) = db.folders().get(id)? else {
                return Ok(None);
            };
            let folders = db.folders().list(folder.account_id)?;
            Ok(db
                .folders()
                .remote_name(id)?
                .map(|remote_name| (folder, folders, remote_name)))
        })
        .await?;
    let (folder, folders, remote_name) = found.ok_or(Error::FolderNotFound(id))?;
    if !can_change_folder(&folders, id) {
        return Err(Error::InvalidInput(format!(
            "“{}” is kept as it is: the server or catmail treats it as special",
            folder.name
        )));
    }
    Ok((folder, folders, remote_name))
}

/// Where new folders go when no folder is asked for: under INBOX on a server that keeps
/// every folder there (as Courier and Cyrus do), told by every other folder being under
/// it; at the top otherwise. NAMESPACE (RFC 2342) would say, but few servers need it.
fn top_level(folders: &[Folder]) -> Option<FolderId> {
    let inbox = special_folder(folders, FolderRole::Inbox)?;
    let mut others = folders.iter().filter(|f| f.id != inbox.id).peekable();
    (others.peek().is_some() && others.all(|f| inbox.is_above(f))).then_some(inbox.id)
}

/// The name, trimmed, if a folder can have it: one level of a name, without the
/// delimiter, LIST's wildcards, or control characters.
fn check_name(name: &str, delimiter: Option<char>) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidInput("a folder needs a name".into()));
    }
    if let Some(d) = delimiter.filter(|&d| name.contains(d)) {
        return Err(Error::InvalidInput(format!(
            "a folder's name can't have “{d}” in it: the server separates levels with it"
        )));
    }
    if name.chars().any(|c| c.is_control() || c == '*' || c == '%') {
        return Err(Error::InvalidInput(
            "a folder's name can't have “*”, “%” or control characters in it".into(),
        ));
    }
    Ok(name)
}

/// Fails if another folder than `except` has the full name `name`; INBOX is any case.
fn check_free(folders: &[Folder], name: &str, except: Option<FolderId>) -> Result<()> {
    let taken = folders.iter().any(|f| {
        Some(f.id) != except
            && (f.name == name
                || (f.role == Some(FolderRole::Inbox)
                    && f.name.eq_ignore_ascii_case("INBOX")
                    && name.eq_ignore_ascii_case("INBOX")))
    });
    if taken {
        return Err(Error::AlreadyExists(format!("a folder named “{name}”")));
    }
    Ok(())
}

/// The server's name and the display name of the folder `leaf` in `parent` (its server
/// name, display name and delimiter), or at the top. The parent's server name is kept as
/// it is, not encoded again.
fn named(parent: Option<(&str, &str, char)>, leaf: &str) -> (String, String) {
    match parent {
        Some((remote, name, d)) => (
            format!("{remote}{d}{}", utf7::encode(leaf)),
            format!("{name}{d}{leaf}"),
        ),
        None => (utf7::encode(leaf), leaf.to_owned()),
    }
}

/// On a try after the first (`again`, set by this), the names of the server's mailboxes:
/// a dead connection may have taken the command before failing, and doing what it did
/// once more would be refused.
async fn listed_again(session: &mut Session, again: &mut bool) -> Result<Option<HashSet<String>>> {
    if !std::mem::replace(again, true) {
        return Ok(None);
    }
    listed_names(session).await.map(Some)
}

/// The names of the server's mailboxes, as commands spell them.
async fn listed_names(session: &mut Session) -> Result<HashSet<String>> {
    let listed = session.list().await?;
    Ok(listed.into_iter().map(|m| m.raw_name).collect())
}

#[cfg(test)]
mod tests {
    use catmail_types::FolderKind;

    use super::*;

    fn folder(id: i64, name: &str, role: Option<FolderRole>) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(1),
            name: name.into(),
            delimiter: Some('.'),
            role,
            kind: FolderKind::Location,
            listed: true,
            selectable: true,
            inferiors: true,
            unread_count: 0,
            total_count: 0,
            last_synced_at: None,
        }
    }

    #[test]
    fn new_folders_go_under_inbox_where_all_the_others_are() {
        let inbox = folder(1, "INBOX", Some(FolderRole::Inbox));
        let under = [
            inbox.clone(),
            folder(2, "INBOX.Sent", Some(FolderRole::Sent)),
            folder(3, "INBOX.Work", None),
        ];
        assert_eq!(top_level(&under), Some(FolderId(1)));
        let beside = [inbox.clone(), folder(2, "Sent", Some(FolderRole::Sent))];
        assert_eq!(top_level(&beside), None);
        assert_eq!(top_level(std::slice::from_ref(&inbox)), None);
    }

    #[test]
    fn names_are_one_level_of_plain_text() {
        assert_eq!(check_name("  2026 ", Some('/')).unwrap(), "2026");
        assert_eq!(check_name("a.b", Some('/')).unwrap(), "a.b");
        for bad in ["", "  ", "a/b", "a*", "50%", "a\r\nb"] {
            assert!(
                matches!(check_name(bad, Some('/')), Err(Error::InvalidInput(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_name_is_taken_by_another_folder_or_by_inbox_in_any_case() {
        let folders = [
            folder(1, "INBOX", Some(FolderRole::Inbox)),
            folder(2, "Work", None),
        ];
        assert!(check_free(&folders, "Play", None).is_ok());
        assert!(check_free(&folders, "Work", Some(FolderId(2))).is_ok());
        assert!(matches!(
            check_free(&folders, "Work", None),
            Err(Error::AlreadyExists(_))
        ));
        assert!(check_free(&folders, "inbox", None).is_err());
    }
}
