//! The draft tier's tools: writing drafts, replies and forwards, changing them and their
//! attachments. None sends anything.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use catmail_core::{
    AccountId, Address, Draft, DraftAttachmentId, DraftChanges, DraftId, Forwarding, IdentityId,
    NewFile, display, parse_addresses,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::downloads::{body_cost, forward_cost};
use super::{Failed, Mail, not_found};
use crate::output::DraftOut;

/// What the files given to a draft in one call may add up to, decoded. Base64 makes
/// them a third larger in the request (`http::MAX_REQUEST_BYTES`).
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

/// A file to attach, given by its content.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct FileIn {
    /// Its file name, e.g. `report.pdf`, which tells its type.
    pub name: String,
    /// Its content, base64.
    pub base64: String,
}

/// Who a new draft goes to.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct Recipients {
    /// Addresses: `Name <email>` or `email`.
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    /// Sent to, but not shown to the others.
    #[serde(default)]
    pub bcc: Vec<String>,
}

impl Recipients {
    /// To, Cc and Bcc, parsed.
    fn parsed(&self) -> Result<[Vec<Address>; 3], Failed> {
        Ok([
            addresses(&self.to)?,
            addresses(&self.cc)?,
            addresses(&self.bcc)?,
        ])
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct CreateDraftParams {
    /// Account id.
    pub account: i64,
    /// The address to send it as, one of the account's `sends_as` [default: the first].
    pub from: Option<String>,
    #[serde(flatten)]
    pub recipients: Recipients,
    #[serde(default)]
    pub subject: String,
    /// Plain text.
    #[serde(default)]
    pub text: String,
    /// Files to attach, together at most 10 MB.
    #[serde(default)]
    pub attachments: Vec<FileIn>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ReplyParams {
    /// The message to reply to.
    pub message: i64,
    /// Also to everyone else it went to [default: false].
    pub all: Option<bool>,
    /// The address to send it as, one of the account's `sends_as` [default: the one the
    /// message came to].
    pub from: Option<String>,
    /// Plain text, written above the quoted message.
    #[serde(default)]
    pub text: String,
    /// Files to attach, together at most 10 MB.
    #[serde(default)]
    pub attachments: Vec<FileIn>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ForwardParams {
    /// The message to forward.
    pub message: i64,
    /// The address to send it as, one of the account's `sends_as` [default: the first].
    pub from: Option<String>,
    #[serde(flatten)]
    pub recipients: Recipients,
    /// Plain text, written above the forwarded message.
    #[serde(default)]
    pub text: String,
    /// Files to attach besides the message's own, together at most 10 MB.
    #[serde(default)]
    pub attachments: Vec<FileIn>,
}

/// What is given replaces what the draft says; what is left out stays.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct UpdateDraftParams {
    /// Draft id.
    pub draft: i64,
    /// The address to send it as, one of its account's `sends_as`.
    pub from: Option<String>,
    pub subject: Option<String>,
    /// Addresses: `Name <email>` or `email`.
    pub to: Option<Vec<String>>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    /// The whole text, a quote included.
    pub text: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct AttachParams {
    /// Draft id.
    pub draft: i64,
    #[serde(flatten)]
    pub file: FileIn,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RemoveAttachmentParams {
    /// Draft id.
    pub draft: i64,
    /// Attachment id, from the draft's `attachments`.
    pub attachment: i64,
}

impl Mail {
    /// Draft `id`, if it is of one of the call's accounts.
    async fn draft(&self, id: i64) -> Result<Draft, Failed> {
        self.client
            .get_draft(DraftId(id))
            .await?
            .filter(|d| self.scope.allows(d.account_id))
            .ok_or_else(|| not_found("draft", id))
    }

    /// The draft as the agent reads it.
    async fn draft_out(&self, draft: &Draft) -> Result<Json<DraftOut>, Failed> {
        let identities = self.client.list_identities(draft.account_id).await?;
        Ok(Json(DraftOut::new(draft, &identities)))
    }

    /// The account's identity that sends as `from` (an address, or `Name <address>`), if
    /// given.
    async fn identity(
        &self,
        account: AccountId,
        from: Option<&str>,
    ) -> Result<Option<IdentityId>, Failed> {
        let Some(from) = from else {
            return Ok(None);
        };
        let email = match parse_addresses(from).ok().as_deref() {
            Some([address]) => address.email.clone(),
            _ => from.trim().to_owned(),
        };
        let identity = self.client.identity_by_address(account, &email).await?;
        identity.map(Some).ok_or_else(|| {
            Failed(format!(
                "not_found: account {} doesn't send as {email}; `sends_as` of \
                 `list_accounts` says what it does",
                account.0
            ))
        })
    }
}

/// Addresses as written: `Name <email>` or `email`, each entry one or several.
fn addresses(entries: &[String]) -> Result<Vec<Address>, Failed> {
    let mut list = Vec::new();
    for entry in entries {
        list.extend(parse_addresses(entry)?);
    }
    Ok(list)
}

/// The file given, decoded: within [`MAX_FILE_BYTES`].
fn file(file: FileIn) -> Result<NewFile, Failed> {
    let FileIn { name, base64 } = file;
    // Lines of base64 are fine too, at the cost of a copy.
    let decoded = if base64.bytes().any(|b| b.is_ascii_whitespace()) {
        let joined: Vec<u8> = base64
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        BASE64.decode(joined)
    } else {
        BASE64.decode(base64)
    };
    let content =
        decoded.map_err(|_| Failed(format!("invalid: the content of {name} isn't base64")))?;
    if content.len() > MAX_FILE_BYTES {
        return Err(files_too_large());
    }
    Ok(NewFile::Content { name, content })
}

/// The files given, decoded: within [`MAX_FILE_BYTES`] together.
fn files(given: Vec<FileIn>) -> Result<Vec<NewFile>, Failed> {
    let mut total = 0;
    given
        .into_iter()
        .map(|given| {
            let new = file(given)?;
            if let NewFile::Content { content, .. } = &new {
                total += content.len();
            }
            if total > MAX_FILE_BYTES {
                return Err(files_too_large());
            }
            Ok(new)
        })
        .collect()
}

fn files_too_large() -> Failed {
    Failed(format!(
        "invalid: the files given at once can't add up to more than {}",
        display::size(MAX_FILE_BYTES as u64)
    ))
}

#[tool_router(router = draft_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Writes a new draft, kept here and in the account's Drafts folder for \
                       the user to read and send. Nothing is sent.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_draft(
        &self,
        Parameters(p): Parameters<CreateDraftParams>,
    ) -> Result<Json<DraftOut>, Failed> {
        // Fails for an account outside the token's.
        self.narrowed(Some(p.account)).await?;
        let account = AccountId(p.account);
        let [to, cc, bcc] = p.recipients.parsed()?;
        let content = catmail_core::DraftContent {
            subject: p.subject,
            to,
            cc,
            bcc,
            text: p.text,
        };
        let files = files(p.attachments)?;
        let from = self.identity(account, p.from.as_deref()).await?;
        let draft = self
            .client
            .create_draft(account, from, content, files)
            .await?;
        self.draft_out(&draft).await
    }

    #[tool(
        description = "Writes a draft replying to a message: to its sender (or Reply-To), \
                       with `all` to everyone else it went to as well, quoting it and in \
                       its thread. Nothing is sent.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn reply(
        &self,
        Parameters(p): Parameters<ReplyParams>,
    ) -> Result<Json<DraftOut>, Failed> {
        let message = self.full_message(p.message).await?;
        let files = files(p.attachments)?;
        let from = self
            .identity(message.summary.account_id, p.from.as_deref())
            .await?;
        let id = message.summary.id;
        let draft = self
            .downloading(
                body_cost(&message),
                self.client
                    .reply(id, p.all.unwrap_or(false), from, p.text, files),
                self.left_of(id, body_cost),
            )
            .await?;
        self.draft_out(&draft).await
    }

    #[tool(
        description = "Writes a draft forwarding a message, with its attachments, below \
                       `text`. Nothing is sent.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn forward(
        &self,
        Parameters(p): Parameters<ForwardParams>,
    ) -> Result<Json<DraftOut>, Failed> {
        let message = self.full_message(p.message).await?;
        let [to, cc, bcc] = p.recipients.parsed()?;
        let forwarding = Forwarding {
            to,
            cc,
            bcc,
            text: p.text,
        };
        let files = files(p.attachments)?;
        let from = self
            .identity(message.summary.account_id, p.from.as_deref())
            .await?;
        let id = message.summary.id;
        let draft = self
            .downloading(
                forward_cost(&message),
                self.client.forward(id, from, forwarding, files),
                self.left_of(id, forward_cost),
            )
            .await?;
        self.draft_out(&draft).await
    }

    #[tool(
        description = "Changes a draft: what is given replaces what it says, the rest \
                       stays. Fails for one the user has handed to \"send\".",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_draft(
        &self,
        Parameters(p): Parameters<UpdateDraftParams>,
    ) -> Result<Json<DraftOut>, Failed> {
        let draft = self.draft(p.draft).await?;
        let parsed = |given: Option<Vec<String>>| given.as_deref().map(addresses).transpose();
        let changes = DraftChanges {
            from: self.identity(draft.account_id, p.from.as_deref()).await?,
            subject: p.subject,
            to: parsed(p.to)?,
            cc: parsed(p.cc)?,
            bcc: parsed(p.bcc)?,
            text: p.text,
        };
        if changes.is_empty() {
            return Err(Failed("invalid: nothing to change".into()));
        }
        // Made to the draft as it is then: what the user changed meanwhile in the editor,
        // in the parts not given here, stays.
        let draft = self.client.update_draft(draft.id, changes).await?;
        self.draft_out(&draft).await
    }

    #[tool(
        description = "Attaches a file, given by its content (at most 10 MB), to a draft.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn attach(
        &self,
        Parameters(p): Parameters<AttachParams>,
    ) -> Result<Json<DraftOut>, Failed> {
        let draft = self.draft(p.draft).await?;
        self.client.attach_file(draft.id, file(p.file)?).await?;
        let draft = self.draft(p.draft).await?;
        self.draft_out(&draft).await
    }

    #[tool(
        description = "Takes an attachment off a draft.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn remove_attachment(
        &self,
        Parameters(p): Parameters<RemoveAttachmentParams>,
    ) -> Result<Json<DraftOut>, Failed> {
        let draft = self.draft(p.draft).await?;
        if !draft.attachments.iter().any(|a| a.id.0 == p.attachment) {
            return Err(not_found("attachment", p.attachment));
        }
        let draft = self
            .client
            .remove_draft_attachment(DraftAttachmentId(p.attachment))
            .await?;
        self.draft_out(&draft).await
    }
}
