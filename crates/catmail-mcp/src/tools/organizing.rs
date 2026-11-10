//! The organize tier's tools: marking, moving, archiving, labelling and moving to Trash,
//! here and then on the server.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use catmail_core::{ChangeReport, DeleteMode, Flags, MessageId, MessageSummary};
use futures_util::future::join_all;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Failed, Mail, not_found};
use crate::output::{AccountError, Changed, ErrorOut, PendingOut};

/// Messages one change takes at most.
const MAX_MESSAGES: usize = 100;
/// How long a change waits for the server to take it before answering that it is on
/// its way: the replay waits for a sync under way, which can be a long one.
const REPLAY_WAIT: Duration = Duration::from_secs(20);

#[derive(Deserialize, JsonSchema)]
pub(crate) struct MessagesParam {
    /// Message ids, at most 100.
    pub messages: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct MarkParams {
    /// Message ids, at most 100.
    pub messages: Vec<i64>,
    /// Mark them read, or unread with false; left out, as they are.
    pub read: Option<bool>,
    /// Star (flag) them, or take the star off with false; left out, as they are.
    pub starred: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct MoveParams {
    /// Message ids, at most 100, of one account.
    pub messages: Vec<i64>,
    /// The folder to move them to, of their account (on Gmail: a label, Trash or Spam).
    pub to: i64,
    /// The folder or label they were listed in, if any: on Gmail, moving takes that
    /// label off them.
    pub from: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct LabelParams {
    /// Message ids, at most 100, of one Gmail account.
    pub messages: Vec<i64>,
    /// Labels (folder ids, from `list_folders`) to put on them.
    #[serde(default)]
    pub add: Vec<i64>,
    /// Labels to take off them.
    #[serde(default)]
    pub remove: Vec<i64>,
}

impl Mail {
    /// The messages a change is to be made to: each once, all in the call's accounts.
    async fn messages(&self, ids: &[i64]) -> Result<Vec<MessageSummary>, Failed> {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Err(Failed("invalid: no messages given".into()));
        }
        if ids.len() > MAX_MESSAGES {
            return Err(Failed(format!(
                "invalid: at most {MAX_MESSAGES} messages at a time"
            )));
        }
        let found = self
            .client
            .message_summaries(ids.iter().copied().map(MessageId).collect())
            .await?;
        let found: HashMap<_, _> = found
            .into_iter()
            .filter(|m| self.scope.allows(m.account_id))
            .map(|m| (m.id.0, m))
            .collect();
        if let Some(&missing) = ids.iter().find(|id| !found.contains_key(id)) {
            return Err(not_found("message", missing));
        }
        Ok(found.into_values().collect())
    }

    /// Makes `make`'s change to `messages` (each once, all in the call's accounts),
    /// here and then on their servers, and answers with what of it isn't there.
    async fn change<F>(
        &self,
        messages: &[i64],
        make: impl FnOnce(Vec<MessageId>) -> F,
    ) -> Result<Json<Changed>, Failed>
    where
        F: Future<Output = catmail_core::Result<ChangeReport>>,
    {
        let messages = self.messages(messages).await?;
        let change = make(messages.iter().map(|m| m.id).collect()).await?;
        // The engine replays it on its own too; this is to answer with the outcome.
        let replays = join_all(change.accounts.iter().map(|&account| async move {
            let replay = tokio::time::timeout(REPLAY_WAIT, self.client.replay_pending(account));
            (account, replay.await)
        }))
        .await;
        let mut errors = Vec::new();
        for (account, replay) in replays {
            let error = match replay {
                Ok(Ok(report)) => report.error.as_ref().map(ErrorOut::from),
                Ok(Err(e)) => Some(ErrorOut::from(&e)),
                // Still under way; it shows as pending.
                Err(_) => None,
            };
            errors.extend(error.map(|error| AccountError {
                account: account.0,
                error,
            }));
        }
        let pending = self.client.list_pending(None).await?;
        Ok(Json(Changed {
            messages: messages.len(),
            not_on_server: pending
                .iter()
                // By op: a move turned into a conflict has lost its message's row.
                .filter(|op| change.ops.contains(&op.id))
                .map(PendingOut::from)
                .collect(),
            errors,
        }))
    }
}

#[tool_router(router = organize_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Marks messages read or unread, starred or not, here and then on \
                       the server.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn mark(&self, Parameters(p): Parameters<MarkParams>) -> Result<Json<Changed>, Failed> {
        let (mut add, mut remove) = (Flags::empty(), Flags::empty());
        for (wanted, flag) in [(p.read, Flags::SEEN), (p.starred, Flags::FLAGGED)] {
            match wanted {
                Some(true) => add |= flag,
                Some(false) => remove |= flag,
                None => {}
            }
        }
        if add.is_empty() && remove.is_empty() {
            return Err(Failed("invalid: say `read`, `starred` or both".into()));
        }
        self.change(&p.messages, |ids| self.client.set_flags(ids, add, remove))
            .await
    }

    #[tool(
        name = "move",
        description = "Moves messages to another folder of their account. On Gmail, where \
                       folders are labels, it takes off the label of `from` and puts on \
                       `to`'s; moving to Trash or Spam takes the message there.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn move_messages(
        &self,
        Parameters(p): Parameters<MoveParams>,
    ) -> Result<Json<Changed>, Failed> {
        let to = self.folder(p.to).await?;
        let from = match p.from {
            Some(id) => Some(self.folder(id).await?.id),
            None => None,
        };
        self.change(&p.messages, |ids| {
            self.client.move_messages(ids, from, to.id)
        })
        .await
    }

    #[tool(
        description = "Archives messages: to their account's Archive folder, or on Gmail \
                       out of the inbox.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn archive(
        &self,
        Parameters(p): Parameters<MessagesParam>,
    ) -> Result<Json<Changed>, Failed> {
        self.change(&p.messages, |ids| self.client.archive_messages(ids))
            .await
    }

    #[tool(
        description = "Puts Gmail labels on messages and takes others off.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn label(&self, Parameters(p): Parameters<LabelParams>) -> Result<Json<Changed>, Failed> {
        if p.add.is_empty() && p.remove.is_empty() {
            return Err(Failed("invalid: no labels to add or remove".into()));
        }
        let labels = |ids: Vec<i64>| async move {
            let mut folders = Vec::new();
            for id in ids {
                folders.push(self.folder(id).await?.id);
            }
            Ok::<_, Failed>(folders)
        };
        let add = labels(p.add).await?;
        let remove = labels(p.remove).await?;
        self.change(&p.messages, |ids| {
            self.client.label_messages(ids, add, remove)
        })
        .await
    }

    #[tool(
        description = "Moves messages to their account's Trash, where they can be brought \
                       back from. Refused for messages in Trash already: nothing is deleted \
                       for good.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn trash(
        &self,
        Parameters(p): Parameters<MessagesParam>,
    ) -> Result<Json<Changed>, Failed> {
        self.change(&p.messages, |ids| {
            self.client.delete_messages(ids, DeleteMode::OnlyToTrash)
        })
        .await
    }
}
