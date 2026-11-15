//! The send tier's tool: sending a draft once the user confirms it in the agent's
//! client, asked through MCP elicitation, or at once with a token the user let send
//! without asking.
//!
//! Asking is stateless (MCP's multi round-trip requests): the call answers
//! `input_required` with the question, and the client calls again with the user's answer
//! and the `requestState` it was given. That state is only a handle to what was asked,
//! kept here ([`Asked`]): the client echoes it unchecked, so nothing in it is trusted.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use catmail_core::{DraftId, DraftRevision, SendPreview, display, format_addresses};
use rmcp::RoleServer;
use rmcp::handler::server::tool::{InputResponses, RequestState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResponse, CallToolResult, ElicitRequest, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationSchema, InputRequest, InputRequiredResult,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Failed, Mail, stateless, token_of};
use crate::output::{OutboxOut, cut};
use crate::tokens::Tier;

/// How long the user has to answer: a question older than that is asked again.
const ANSWER_WITHIN: Duration = Duration::from_secs(15 * 60);
/// The key of the question among the call's input requests.
const QUESTION: &str = "send";
/// How much of the draft's text the question shows.
const SHOWN_CHARS: usize = 2_000;

#[derive(Deserialize, JsonSchema)]
pub(crate) struct SendDraftParams {
    /// Draft id.
    pub draft: i64,
}

/// What a question waiting for its answer asked, by the handle given as `requestState`:
/// the revision of the draft shown (`R`).
pub(crate) struct Asked<R = DraftRevision>(Mutex<HashMap<String, Question<R>>>);

impl<R> Default for Asked<R> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

struct Question<R> {
    /// The name of the token whose call asked it.
    token: String,
    draft: DraftId,
    /// The draft as the user was shown it.
    revision: R,
    asked_at: Instant,
}

impl<R: Copy> Asked<R> {
    /// Keeps the question, returning its handle.
    fn ask(&self, token: &str, draft: DraftId, revision: R, now: Instant) -> String {
        let handle = crate::random_handle();
        let mut asked = self.0.lock().expect("asked");
        // Those no one answered.
        asked.retain(|_, q| now.duration_since(q.asked_at) < ANSWER_WITHIN);
        asked.insert(
            handle.clone(),
            Question {
                token: token.to_owned(),
                draft,
                revision,
                asked_at: now,
            },
        );
        handle
    }

    /// The revision of `draft` that `handle` asked `token`'s user about, if it did, not too
    /// long ago. Once only: the handle is gone after.
    fn take(&self, handle: &str, token: &str, draft: DraftId, now: Instant) -> Option<R> {
        let question = self.0.lock().expect("asked").remove(handle)?;
        (question.token == token
            && question.draft == draft
            && now.duration_since(question.asked_at) < ANSWER_WITHIN)
            .then_some(question.revision)
    }
}

/// Whether the call's client can ask its user, statelessly: elicitation in a form, on the
/// protocol version that has multi round-trip requests.
fn can_ask(context: &RequestContext<RoleServer>) -> bool {
    let elicitation = context
        .client_capabilities()
        .and_then(|c| c.elicitation)
        // `{}` is a form, from before there were other modes.
        .is_some_and(|e| e.form.is_some() || e.url.is_none());
    elicitation && stateless(context)
}

/// The question: the draft as it would go out, with the start of its text.
fn question(preview: &SendPreview, token: &str) -> String {
    let draft = &preview.draft;
    let content = &draft.content;
    let mut text = format!(
        "The agent with the token \"{token}\" asks to send this message:\n\nFrom: {}\n",
        preview.from
    );
    for (field, list) in [
        ("To", &content.to),
        ("Cc", &content.cc),
        ("Bcc", &content.bcc),
    ] {
        if !list.is_empty() {
            text.push_str(&format!("{field}: {}\n", format_addresses(list)));
        }
    }
    text.push_str(&format!("Subject: {}\n", content.subject));
    if !draft.attachments.is_empty() {
        let files: Vec<_> = draft
            .attachments
            .iter()
            .map(|a| format!("{} ({})", a.filename, display::size(a.size)))
            .collect();
        text.push_str(&format!("Attachments: {}\n", files.join(", ")));
    }
    let body = content.body.text.trim_end();
    if !body.is_empty() {
        text.push('\n');
        let (shown, cut_off) = cut(body, SHOWN_CHARS);
        text.push_str(&shown);
        if cut_off {
            let more = body.chars().count() - SHOWN_CHARS;
            text.push_str(&format!("\n… ({more} more characters)"));
        }
    }
    text
}

/// The call's answer: the question for the client to put to its user, with the handle
/// to call again with.
fn asking(message: String, handle: String) -> CallToolResponse {
    let request = ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        // Nothing to fill in: accepting is the answer.
        requested_schema: ElicitationSchema::new(BTreeMap::new()),
    });
    let requests = BTreeMap::from([(QUESTION.to_owned(), InputRequest::Elicitation(request))]);
    InputRequiredResult::new(Some(requests), Some(handle)).into()
}

#[tool_router(router = send_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Sends a draft. First the user is asked, in this client, to confirm \
                       it as it would go out (unless they let this token send without \
                       asking); declined, it stays in Drafts. Confirmed, it goes to the \
                       outbox, which sends it in the background: `list_outbox` shows how \
                       it went. A draft changed after it was shown is asked about again.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
        output_schema = rmcp::handler::server::tool::schema_for_output::<OutboxOut>()
    )]
    async fn send_draft(
        &self,
        Parameters(p): Parameters<SendDraftParams>,
        RequestState(state): RequestState,
        InputResponses(answers): InputResponses,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, Failed> {
        // Fails for a draft outside the token's accounts.
        let id = self.draft(p.draft).await?.id;
        let now = Instant::now();
        let shown = state.and_then(|handle| self.asked.take(&handle, &self.caller, id, now));
        let answer = answers
            .as_ref()
            .and_then(|a| a.get(QUESTION))
            .and_then(|a| ElicitResult::deserialize(a).ok())
            .map(|a| a.action);
        if answer
            .as_ref()
            .is_some_and(|a| *a != ElicitationAction::Accept)
        {
            return Err(Failed(format!(
                "declined: the user didn't send draft {}; it stays in Drafts",
                id.0
            )));
        }
        let unasked = token_of(&context).is_some_and(|t| t.access.tier >= Tier::SendUnasked);
        let confirmed = shown.filter(|_| answer.is_some());
        let entry = match confirmed {
            Some(revision) => self.client.send_previewed(id, revision).await?,
            None if unasked => self.client.send_draft(id).await?,
            None if !can_ask(&context) => {
                return Err(Failed(
                    "cannot_ask: this client can't ask the user to confirm sending (MCP \
                     elicitation). The draft stays in Drafts for the user to send, or they \
                     can give this agent a token that sends without asking"
                        .into(),
                ));
            }
            // Not asked yet, or asked too long ago (or by a host since stopped).
            None => {
                let preview = self.client.preview_send(id).await?;
                let handle = self.asked.ask(&self.caller, id, preview.revision, now);
                return Ok(asking(question(&preview, &self.caller), handle));
            }
        };
        tracing::info!(
            token = %self.caller,
            draft = id.0,
            recipients = ?entry.recipients,
            asked = confirmed.is_some(),
            "MCP send queued"
        );
        let out = serde_json::to_value(OutboxOut::from(&entry)).expect("outbox entry serializes");
        Ok(CallToolResult::structured(out).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_answered_once_by_the_token_that_asked_it_in_time() {
        let asked = Asked::<u8>::default();
        let (draft, start) = (DraftId(1), Instant::now());
        let revision = 7;
        let handle = asked.ask("laptop", draft, revision, start);
        assert_eq!(asked.take(&handle, "laptop", draft, start), Some(revision));
        assert_eq!(asked.take(&handle, "laptop", draft, start), None);

        let handle = asked.ask("laptop", draft, revision, start);
        assert_eq!(asked.take(&handle, "phone", draft, start), None);
        let handle = asked.ask("laptop", draft, revision, start);
        assert_eq!(asked.take(&handle, "laptop", DraftId(2), start), None);
        let handle = asked.ask("laptop", draft, revision, start);
        assert_eq!(
            asked.take(&handle, "laptop", draft, start + ANSWER_WITHIN),
            None
        );
        assert_eq!(asked.take("made up", "laptop", draft, start), None);
    }

    #[test]
    fn unanswered_questions_are_forgotten() {
        let asked = Asked::<u8>::default();
        let start = Instant::now();
        asked.ask("laptop", DraftId(1), 1, start);
        asked.ask("laptop", DraftId(2), 1, start + ANSWER_WITHIN);
        assert_eq!(asked.0.lock().unwrap().len(), 1);
    }
}
