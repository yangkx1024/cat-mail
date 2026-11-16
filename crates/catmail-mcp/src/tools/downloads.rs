//! What reading and drafting download from the mail servers, within the token's daily
//! budget ([`Limits`]).

use std::future::{Future, ready};
use std::time::Instant;

use catmail_core::{Answer, Attachment, Message, MessageId};

use super::{Failed, Mail};
use crate::limits::OverLimit;

impl Mail {
    /// The message with its body, downloading it within the token's budget if needed.
    /// Through the engine even when it is here: opening rebuilds a body an older version
    /// stored.
    pub(super) async fn with_body(&self, message: Message) -> Result<Message, Failed> {
        let cost = body_cost(&message);
        let download = self.client.download_message(message.summary.id);
        // A download that fails has stored nothing.
        self.downloading(cost, download, ready(cost)).await
    }

    /// Runs `work`, which downloads about `bytes` from the mail servers, if they fit in
    /// the token's budget. If it fails, gives back what `left` says is still to
    /// download: what it got before failing stays spent, or a call failing after its
    /// download (a reply with a bad file name) would download for free.
    pub(super) async fn downloading<T>(
        &self,
        bytes: u64,
        work: impl Future<Output = catmail_core::Result<T>>,
        left: impl Future<Output = u64>,
    ) -> Result<T, Failed> {
        if bytes == 0 {
            return Ok(work.await?);
        }
        self.limits
            .download(&self.caller, bytes, Instant::now())
            .map_err(over_limit)?;
        let result = work.await;
        if result.is_err() {
            let left = left.await.min(bytes);
            self.limits.refund(&self.caller, left, Instant::now());
        }
        Ok(result?)
    }

    /// What making `answer` to message `id` downloads, as the engine tells it.
    pub(super) async fn to_download(&self, id: MessageId, answer: Answer) -> Result<u64, Failed> {
        Ok(self.client.answer_download_size(id, answer).await?)
    }

    /// What making `answer` to message `id` still takes to download; all of it if that
    /// can't be told.
    pub(super) async fn left_of(&self, id: MessageId, answer: Answer) -> u64 {
        self.client
            .answer_download_size(id, answer)
            .await
            .unwrap_or(u64::MAX)
    }
}

pub(super) fn over_limit(e: OverLimit) -> Failed {
    Failed(format!("over_limit: {e}"))
}

/// What downloading the message's body takes: nothing once it is here.
pub(super) fn body_cost(message: &Message) -> u64 {
    match message.body {
        Some(_) => 0,
        None => message.size.map_or(0, u64::from),
    }
}

/// What downloading the attachment takes: nothing once it is here.
pub(super) fn attachment_cost(attachment: &Attachment) -> u64 {
    if attachment.downloaded {
        0
    } else {
        attachment.size.map_or(0, u64::from)
    }
}
