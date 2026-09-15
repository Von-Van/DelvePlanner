mod draft;
mod grounding;
mod preflight;
mod prompt;

use crate::error::{AppError, AppResult};
use crate::model::{
    AppliedProposal, ModelResponse, MutationOperation, PlannerCandidates, PlannerResponse,
    RecordRef, MAX_COMMAND_LENGTH,
};
use crate::runtime::{DownloadProgress, RuntimePhase};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use draft::{Draft, DraftOperation, Resolution, Resolver};
use preflight::{choose_scope, preflight};
use prompt::{output_format, request_context, system_prompt, ContextInput, Schema};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";
pub const MODEL_NAME: &str = "qwen3:8b";
const MEMORY_LIMIT: usize = 4;
const PROPOSAL_TTL_MINUTES: i64 = 10;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Prompt, request context, and reply fit comfortably in this window on 16 GB machines.
const CONTEXT_TOKENS: u32 = 8_192;
const MAX_REPLY_TOKENS: u32 = 1_200;
/// Long enough for a cold model load plus a full twelve-operation reply on a laptop.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    pub phase: RuntimePhase,
    pub running: bool,
    pub model_installed: bool,
    pub model_name: String,
    pub model_digest: Option<String>,
    pub ollama_version: Option<String>,
    pub model_license: Option<String>,
    pub detail: String,
    pub download: Option<DownloadProgress>,
    pub storage_bytes: Option<u64>,
}

/// Which system prompt and grammar a request uses. The schedule scope is the original event-only
/// planner; the planning scope adds plans, milestones, and tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Schedule,
    Planning,
}

#[derive(Debug, Clone)]
struct SessionTurn {
    request: String,
    scope: Scope,
    outcome: SessionOutcome,
}

#[derive(Debug, Clone)]
enum SessionOutcome {
    Proposal {
        proposal_id: String,
        summary: String,
        drafts: Vec<DraftOperation>,
        operations: Vec<MutationOperation>,
        applied: bool,
        /// Every record the applied proposal created or changed, so "it" can reach new records.
        applied_ids: Vec<String>,
    },
    Clarification {
        question: String,
    },
}

#[derive(Clone)]
struct PendingProposal {
    id: String,
    response: ModelResponse,
    expires_at: DateTime<Utc>,
    claimed: bool,
}

#[derive(Default)]
struct AgentState {
    session: Vec<SessionTurn>,
    pending: Option<PendingProposal>,
    current_request: u64,
}

pub struct PlannerAgent {
    client: Client,
    base_url: String,
    model_name: String,
    state: Arc<Mutex<AgentState>>,
    active_cancel: Arc<Mutex<Option<(u64, CancellationToken)>>>,
}

/// One request to the planner with the records it may refer to.
pub struct PlannerRequest<'a> {
    pub command: &'a str,
    pub selected_day: &'a str,
    pub time_zone: &'a str,
    /// The plan the user is looking at, which new work belongs to by default.
    pub active_plan_id: Option<&'a str>,
    pub candidates: &'a PlannerCandidates,
}

impl Default for PlannerAgent {
    fn default() -> Self {
        Self::new(OLLAMA_BASE_URL, MODEL_NAME)
    }
}

impl PlannerAgent {
    pub fn new(base_url: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("HTTP client configuration is valid"),
            base_url: base_url.into(),
            model_name: model_name.into(),
            state: Arc::new(Mutex::new(AgentState::default())),
            active_cancel: Arc::new(Mutex::new(None)),
        }
    }

    pub fn clear_context(&self) -> AppResult<()> {
        self.cancel_current();
        let mut state = self.lock_state()?;
        state.session.clear();
        state.pending = None;
        state.current_request = state.current_request.wrapping_add(1);
        Ok(())
    }

    pub fn cancel_current(&self) {
        if let Ok(mut active) = self.active_cancel.lock() {
            if let Some((_, token)) = active.take() {
                token.cancel();
            }
        }
    }

    pub fn memory_len(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.session.len())
            .unwrap_or_default()
    }

    /// Every event, plan, milestone, and task earlier turns of this session referred to, so the
    /// next request can reach them even when they are not otherwise relevant.
    pub fn referenced_ids(&self) -> Vec<String> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        let mut identifiers = Vec::new();
        for turn in &state.session {
            let SessionOutcome::Proposal {
                operations,
                applied_ids,
                ..
            } = &turn.outcome
            else {
                continue;
            };
            for id in operations
                .iter()
                .flat_map(operation_record_ids)
                .chain(applied_ids.iter().cloned())
            {
                if !identifiers.contains(&id) {
                    identifiers.push(id);
                }
            }
        }
        identifiers
    }

    pub async fn status(&self) -> OllamaStatus {
        ollama_status_at(&self.client, &self.base_url, &self.model_name).await
    }

    /// The deterministic question, if any, this request gets before the model is consulted.
    pub fn preflight(&self, command: &str, candidates: &PlannerCandidates) -> Option<String> {
        let session = self
            .state
            .lock()
            .map(|state| state.session.clone())
            .unwrap_or_default();
        let scope = choose_scope(command, candidates, None, &session);
        preflight(
            command,
            candidates,
            !session.is_empty(),
            scope,
            chrono_tz::UTC,
        )
    }

    pub async fn propose(&self, request: PlannerRequest<'_>) -> AppResult<PlannerResponse> {
        let command = request.command.trim();
        if command.chars().count() > MAX_COMMAND_LENGTH {
            return Err(AppError::Validation(
                "Planner commands must be 1,000 characters or fewer.".into(),
            ));
        }
        let today = NaiveDate::parse_from_str(request.selected_day, "%Y-%m-%d")
            .map_err(|_| AppError::Validation("Dates must use YYYY-MM-DD.".into()))?;
        let zone = request
            .time_zone
            .parse::<Tz>()
            .map_err(|_| AppError::Validation("A valid IANA time zone is required.".into()))?;
        let active_plan_id = request
            .active_plan_id
            .filter(|id| request.candidates.plans.iter().any(|plan| plan.id == *id));

        let (request_id, session, pending_proposal_id) = {
            let mut state = self.lock_state()?;
            state.current_request = state.current_request.wrapping_add(1);
            let pending = state.pending.take().filter(|pending| !pending.claimed);
            (
                state.current_request,
                state.session.clone(),
                pending.map(|pending| pending.id),
            )
        };
        self.cancel_current();

        let scope = choose_scope(command, request.candidates, active_plan_id, &session);
        if let Some(question) = preflight(
            command,
            request.candidates,
            !session.is_empty(),
            scope,
            zone,
        ) {
            return self.finish_response(
                request_id,
                command,
                scope,
                Resolution::Clarification(question),
            );
        }

        let status = self.status().await;
        if !status.running || !status.model_installed {
            return Err(AppError::OllamaUnavailable);
        }

        let token = CancellationToken::new();
        self.active_cancel
            .lock()
            .map_err(|_| {
                AppError::Internal("The planner cancellation state is unavailable.".into())
            })?
            .replace((request_id, token.clone()));
        let context = request_context(&ContextInput {
            command,
            today,
            zone,
            scope,
            candidates: request.candidates,
            active_plan_id,
            session: &session,
            pending_proposal_id: pending_proposal_id.as_deref(),
        });
        let body = ChatRequest {
            model: &self.model_name,
            stream: false,
            think: false,
            messages: [
                ChatMessage {
                    role: "system",
                    content: system_prompt(scope),
                },
                ChatMessage {
                    role: "user",
                    content: &context,
                },
            ],
            format: output_format(scope, request.candidates),
            options: ChatOptions {
                temperature: 0.0,
                num_ctx: CONTEXT_TOKENS,
                num_predict: MAX_REPLY_TOKENS,
            },
        };

        let reply = tokio::select! {
            _ = token.cancelled() => return Err(AppError::RequestCancelled),
            response = self.client.post(format!("{}/api/chat", self.base_url)).json(&body).send() => {
                let response = response.map_err(|_| AppError::OllamaUnavailable)?;
                if !response.status().is_success() {
                    return Err(AppError::OllamaUnavailable);
                }
                if response.content_length().is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
                    return Err(AppError::InvalidModelResponse("the model response exceeded the size limit".into()));
                }
                let bytes = response.bytes().await.map_err(|_| AppError::OllamaUnavailable)?;
                if bytes.len() > MAX_RESPONSE_BYTES {
                    return Err(AppError::InvalidModelResponse("the model response exceeded the size limit".into()));
                }
                serde_json::from_slice::<OllamaChatResponse>(&bytes)
                    .map_err(|error| AppError::InvalidModelResponse(error.to_string()))?
            }
        };
        self.clear_active_cancel(request_id);

        if cfg!(debug_assertions) && std::env::var_os("DAYPLAN_DEBUG_PLANNER").is_some() {
            eprintln!("DayPlan planner reply: {}", reply.message.content);
        }
        let draft = parse_reply(reply)?;
        let said = session
            .iter()
            .map(|turn| turn.request.as_str())
            .chain([command])
            .collect::<Vec<_>>()
            .join("\n");
        let pending = session
            .iter()
            .rev()
            .find_map(|turn| match &turn.outcome {
                SessionOutcome::Proposal {
                    proposal_id,
                    drafts,
                    ..
                } if pending_proposal_id.as_deref() == Some(proposal_id.as_str()) => {
                    Some(drafts.as_slice())
                }
                _ => None,
            })
            .unwrap_or_default();
        let session_ids = self.referenced_ids();
        let recent_ids = session
            .iter()
            .rev()
            .find_map(|turn| match &turn.outcome {
                SessionOutcome::Proposal {
                    operations,
                    applied: true,
                    applied_ids,
                    ..
                } => Some(
                    operations
                        .iter()
                        .flat_map(operation_record_ids)
                        .chain(applied_ids.iter().cloned())
                        .collect::<Vec<_>>(),
                ),
                SessionOutcome::Proposal { operations, .. } => Some(
                    operations
                        .iter()
                        .flat_map(operation_record_ids)
                        .collect::<Vec<_>>(),
                ),
                SessionOutcome::Clarification { .. } => None,
            })
            .unwrap_or_default();
        let resolution = Resolver {
            command,
            said: &said,
            time_zone: zone.name(),
            candidates: request.candidates,
            active_plan_id,
            session_ids: &session_ids,
            pending,
            recent_ids: &recent_ids,
            now: Utc::now(),
        }
        .resolve(draft)?;
        self.finish_response(request_id, command, scope, resolution)
    }

    pub fn claim_pending(&self, proposal_id: &str) -> AppResult<ModelResponse> {
        let mut state = self.lock_state()?;
        let Some(pending) = state.pending.as_mut() else {
            return Err(AppError::ProposalUnavailable);
        };
        if pending.id != proposal_id || pending.claimed {
            return Err(AppError::ProposalUnavailable);
        }
        if pending.expires_at <= Utc::now() {
            state.pending = None;
            return Err(AppError::ProposalExpired);
        }
        pending.claimed = true;
        Ok(pending.response.clone())
    }

    /// Releases a claimed proposal. When it applied, the session remembers the records it created
    /// or changed so a follow-up can refer to them.
    pub fn finish_pending(
        &self,
        proposal_id: &str,
        applied: Option<&AppliedProposal>,
    ) -> AppResult<()> {
        let mut state = self.lock_state()?;
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == proposal_id)
        {
            state.pending = None;
        }
        let Some(result) = applied else {
            return Ok(());
        };
        for turn in state.session.iter_mut().rev() {
            if let SessionOutcome::Proposal {
                proposal_id: recorded,
                applied,
                applied_ids,
                ..
            } = &mut turn.outcome
            {
                if recorded == proposal_id {
                    *applied = true;
                    *applied_ids = [
                        &result.event_ids,
                        &result.plan_ids,
                        &result.milestone_ids,
                        &result.task_ids,
                    ]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn discard_pending(&self, proposal_id: &str) -> AppResult<()> {
        let mut state = self.lock_state()?;
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == proposal_id && !pending.claimed)
        {
            state.pending = None;
            Ok(())
        } else {
            Err(AppError::ProposalUnavailable)
        }
    }

    fn finish_response(
        &self,
        request_id: u64,
        command: &str,
        scope: Scope,
        resolution: Resolution,
    ) -> AppResult<PlannerResponse> {
        let mut state = self.lock_state()?;
        if state.current_request != request_id {
            return Err(AppError::RequestCancelled);
        }
        let public = match resolution {
            Resolution::Clarification(question) => {
                state.pending = None;
                state.session.push(SessionTurn {
                    request: command.to_string(),
                    scope,
                    outcome: SessionOutcome::Clarification {
                        question: question.clone(),
                    },
                });
                PlannerResponse::Clarification { question }
            }
            Resolution::Proposal(proposal) => {
                let proposal_id = Uuid::new_v4().to_string();
                let expires_at = Utc::now() + ChronoDuration::minutes(PROPOSAL_TTL_MINUTES);
                state.pending = Some(PendingProposal {
                    id: proposal_id.clone(),
                    response: ModelResponse::proposal(
                        proposal.summary.clone(),
                        proposal.operations.clone(),
                    ),
                    expires_at,
                    claimed: false,
                });
                state.session.push(SessionTurn {
                    request: command.to_string(),
                    scope,
                    outcome: SessionOutcome::Proposal {
                        proposal_id: proposal_id.clone(),
                        summary: proposal.summary.clone(),
                        drafts: proposal.drafts,
                        operations: proposal.operations.clone(),
                        applied: false,
                        applied_ids: Vec::new(),
                    },
                });
                PlannerResponse::Proposal {
                    proposal_id,
                    summary: proposal.summary,
                    operations: proposal.operations,
                    references: proposal.references,
                    expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                }
            }
        };
        trim_memory(&mut state.session);
        Ok(public)
    }

    fn clear_active_cancel(&self, request_id: u64) {
        if let Ok(mut active) = self.active_cancel.lock() {
            if active
                .as_ref()
                .is_some_and(|(active_id, _)| *active_id == request_id)
            {
                active.take();
            }
        }
    }

    fn lock_state(&self) -> AppResult<MutexGuard<'_, AgentState>> {
        self.state
            .lock()
            .map_err(|_| AppError::Internal("The planner session is unavailable.".into()))
    }
}

/// The IDs of existing records an operation targets or links to.
fn operation_record_ids(operation: &MutationOperation) -> Vec<String> {
    let reference = |reference: &Option<RecordRef>| match reference {
        Some(RecordRef::Existing(existing)) => Some(existing.id.clone()),
        _ => None,
    };
    match operation {
        MutationOperation::CreateEvent { plan, .. } => reference(plan).into_iter().collect(),
        MutationOperation::UpdateEvent { event_id, .. }
        | MutationOperation::DeleteEvent { event_id, .. }
        | MutationOperation::RescheduleEvent { event_id, .. } => vec![event_id.clone()],
        MutationOperation::SetEventPlan { event_id, plan, .. } => std::iter::once(event_id.clone())
            .chain(reference(plan))
            .collect(),
        MutationOperation::CreatePlan { .. } => Vec::new(),
        MutationOperation::UpdatePlan { plan_id, .. } => vec![plan_id.clone()],
        MutationOperation::CreateMilestone { plan, .. } => {
            reference(&Some(plan.clone())).into_iter().collect()
        }
        MutationOperation::UpdateMilestone { milestone_id, .. }
        | MutationOperation::DeleteMilestone { milestone_id, .. } => vec![milestone_id.clone()],
        MutationOperation::CreateTask {
            plan, milestone, ..
        } => reference(plan)
            .into_iter()
            .chain(reference(milestone))
            .collect(),
        MutationOperation::UpdateTask { task_id, .. }
        | MutationOperation::ScheduleTask { task_id, .. }
        | MutationOperation::DeleteTask { task_id, .. } => vec![task_id.clone()],
        MutationOperation::SetTaskPlan {
            task_id,
            plan,
            milestone,
            ..
        } => std::iter::once(task_id.clone())
            .chain(reference(plan))
            .chain(reference(milestone))
            .collect(),
    }
}

pub async fn ollama_status(client: &Client) -> OllamaStatus {
    ollama_status_at(client, OLLAMA_BASE_URL, MODEL_NAME).await
}

async fn ollama_status_at(client: &Client, base_url: &str, model_name: &str) -> OllamaStatus {
    let response = match tokio::time::timeout(
        Duration::from_secs(3),
        client.get(format!("{base_url}/api/tags")).send(),
    )
    .await
    {
        Ok(Ok(response)) if response.status().is_success() => response,
        _ => return unavailable_status(model_name),
    };
    let body: OllamaTags = match response.json().await {
        Ok(body) => body,
        Err(_) => {
            return OllamaStatus {
                phase: RuntimePhase::Error,
                running: true,
                model_installed: false,
                model_name: model_name.into(),
                model_digest: None,
                ollama_version: None,
                model_license: None,
                detail: "Ollama replied with an unreadable model list.".into(),
                download: None,
                storage_bytes: None,
            }
        }
    };
    let installed = body.models.into_iter().find(|model| {
        model.name == model_name || model.name.starts_with(&format!("{model_name}-"))
    });
    let version = tokio::time::timeout(
        Duration::from_secs(3),
        client.get(format!("{base_url}/api/version")).send(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .and_then(|response| response.status().is_success().then_some(response));
    let ollama_version = if let Some(response) = version {
        response
            .json::<OllamaVersion>()
            .await
            .ok()
            .map(|body| body.version)
    } else {
        None
    };
    let model_license = if installed.is_some() {
        match tokio::time::timeout(
            Duration::from_secs(3),
            client
                .post(format!("{base_url}/api/show"))
                .json(&json!({ "model": model_name }))
                .send(),
        )
        .await
        {
            Ok(Ok(response)) if response.status().is_success() => response
                .json::<OllamaShow>()
                .await
                .ok()
                .and_then(|body| summarize_license(&body.license)),
            _ => None,
        }
    } else {
        None
    };
    OllamaStatus {
        phase: if installed.is_some() {
            RuntimePhase::ModelReady
        } else {
            RuntimePhase::ReadyWithoutModel
        },
        running: true,
        model_installed: installed.is_some(),
        model_name: model_name.into(),
        model_digest: installed.as_ref().map(|model| model.digest.clone()),
        ollama_version,
        model_license,
        detail: if installed.is_some() {
            "Local model is ready. Nothing is sent to a cloud service.".into()
        } else {
            format!(
                "DayPlan's local runtime is ready. Download {model_name} to enable AI planning."
            )
        },
        download: None,
        storage_bytes: None,
    }
}

fn unavailable_status(model_name: &str) -> OllamaStatus {
    OllamaStatus {
        phase: RuntimePhase::Unavailable,
        running: false,
        model_installed: false,
        model_name: model_name.into(),
        model_digest: None,
        ollama_version: None,
        model_license: None,
        detail: "DayPlan's local AI runtime is not running.".into(),
        download: None,
        storage_bytes: None,
    }
}

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
    #[serde(default)]
    digest: String,
}

#[derive(Deserialize)]
struct OllamaVersion {
    version: String,
}

#[derive(Deserialize)]
struct OllamaShow {
    #[serde(default)]
    license: String,
}

fn summarize_license(license: &str) -> Option<String> {
    license
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(120).collect())
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    stream: bool,
    think: bool,
    messages: [ChatMessage<'a>; 2],
    format: Schema,
    options: ChatOptions,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatOptions {
    temperature: f32,
    num_ctx: u32,
    num_predict: u32,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
}

/// Reads the model's constrained JSON reply. A reply cut off by the token limit is rejected
/// rather than repaired.
fn parse_reply(body: OllamaChatResponse) -> AppResult<Draft> {
    if body.done_reason.as_deref() == Some("length") {
        return Err(AppError::InvalidModelResponse(
            "the model reply was cut off before it finished".into(),
        ));
    }
    serde_json::from_str(body.message.content.trim())
        .map_err(|error| AppError::InvalidModelResponse(error.to_string()))
}

fn trim_memory(session: &mut Vec<SessionTurn>) {
    if session.len() > MEMORY_LIMIT {
        session.drain(0..session.len() - MEMORY_LIMIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ReminderChange, ScheduleEvent};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn event(id: &str, title: &str) -> ScheduleEvent {
        ScheduleEvent {
            id: id.into(),
            title: title.into(),
            notes: String::new(),
            start_at_utc: "2026-08-12T22:00:00.000Z".into(),
            time_zone: "America/New_York".into(),
            duration_minutes: 60,
            reminder_minutes_before: None,
            reminder_status: crate::model::ReminderStatus::None,
            plan_id: None,
            location: String::new(),
            workstream_id: None,
            owner_id: None,
            revision: 1,
            created_at: "2026-08-12T10:00:00.000Z".into(),
            updated_at: "2026-08-12T10:00:00.000Z".into(),
        }
    }

    fn events(events: Vec<ScheduleEvent>) -> PlannerCandidates {
        PlannerCandidates {
            events,
            ..PlannerCandidates::default()
        }
    }

    fn lunch() -> Resolution {
        let operations = vec![MutationOperation::CreateEvent {
            title: "Lunch".into(),
            notes: String::new(),
            start_at_utc: "2026-08-13T16:00:00.000Z".into(),
            time_zone: "America/New_York".into(),
            duration_minutes: 60,
            reminder_minutes_before: None,
            plan: None,
        }];
        Resolution::Proposal(draft::ResolvedProposal {
            summary: "Add lunch".into(),
            drafts: Vec::new(),
            references: Vec::new(),
            operations,
        })
    }

    #[test]
    fn duplicate_title_requires_a_clarification() {
        let agent = PlannerAgent::default();
        let candidates = events(vec![event("a", "Gym"), event("b", "Gym")]);
        assert!(agent.preflight("move gym to 6 pm", &candidates).is_some());
    }

    #[test]
    fn a_move_without_a_date_requires_a_clarification() {
        let agent = PlannerAgent::default();
        let candidates = events(vec![event("a", "Gym")]);
        assert!(agent.preflight("move gym to 6 pm", &candidates).is_some());
    }

    #[test]
    fn task_reminders_are_explicitly_unsupported() {
        let response = PlannerAgent::default().preflight(
            "remind me about my buy milk task tomorrow",
            &PlannerCandidates::default(),
        );
        assert!(response.is_some());
    }

    #[test]
    fn session_memory_is_bounded_to_four_turns() {
        let agent = PlannerAgent::default();
        {
            let mut state = agent.lock_state().unwrap();
            for index in 0..5 {
                state.session.push(SessionTurn {
                    request: format!("command {index}"),
                    scope: Scope::Schedule,
                    outcome: SessionOutcome::Clarification {
                        question: "Which one?".into(),
                    },
                });
                trim_memory(&mut state.session);
            }
        }
        assert_eq!(agent.memory_len(), 4);
    }

    #[test]
    fn reminder_fields_are_strict_and_bounded() {
        let valid = serde_json::from_value::<ModelResponse>(json!({
            "kind": "proposal",
            "summary": "Remind before gym",
            "operations": [{
                "type": "update_event",
                "eventId": "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
                "expectedRevision": 1,
                "title": null,
                "notes": null,
                "durationMinutes": null,
                "reminderChange": { "action": "set", "minutesBefore": 15 }
            }]
        }))
        .unwrap();
        assert!(crate::db::validate_model_response(&valid).is_ok());
        let ModelResponse::Proposal { mut operations, .. } = valid else {
            panic!("expected a proposal");
        };
        if let MutationOperation::UpdateEvent {
            reminder_change, ..
        } = &mut operations[0]
        {
            *reminder_change = ReminderChange::Set {
                minutes_before: 10_081,
            };
        }
        assert!(
            crate::db::validate_model_response(&ModelResponse::proposal("Remind", operations))
                .is_err()
        );
        assert!(serde_json::from_value::<ReminderChange>(
            json!({ "action": "snooze", "minutesBefore": 15 })
        )
        .is_err());
    }

    #[test]
    fn replies_must_be_complete_json_in_the_draft_format() {
        let reply = |content: &str, done_reason: Option<&str>| OllamaChatResponse {
            message: OllamaMessage {
                content: content.into(),
            },
            done_reason: done_reason.map(Into::into),
        };
        assert!(matches!(
            parse_reply(reply(
                r#"{"kind":"clarification","question":"Which one?"}"#,
                Some("stop")
            )),
            Ok(Draft::Clarification { .. })
        ));
        for (content, reason) in [
            ("I moved your gym session!", Some("stop")),
            (
                r#"{"kind":"clarification","question":"Which"#,
                Some("length"),
            ),
            (
                r#"{"kind":"proposal","summary":"Hi","operations":[],"untrusted":true}"#,
                None,
            ),
        ] {
            assert!(matches!(
                parse_reply(reply(content, reason)),
                Err(AppError::InvalidModelResponse(_))
            ));
        }
    }

    #[test]
    fn pending_proposals_are_single_use() {
        let agent = PlannerAgent::default();
        let public = agent
            .finish_response(0, "add lunch tomorrow at noon", Scope::Schedule, lunch())
            .unwrap();
        let PlannerResponse::Proposal { proposal_id, .. } = public else {
            panic!("expected proposal");
        };
        assert!(agent.claim_pending(&proposal_id).is_ok());
        assert!(matches!(
            agent.claim_pending(&proposal_id),
            Err(AppError::ProposalUnavailable)
        ));
    }

    #[test]
    fn session_references_cover_every_record_kind() {
        let agent = PlannerAgent::default();
        {
            let mut state = agent.lock_state().unwrap();
            state.session.push(SessionTurn {
                request: "finish and move".into(),
                scope: Scope::Planning,
                outcome: SessionOutcome::Proposal {
                    proposal_id: "p".into(),
                    summary: "Finish and move".into(),
                    drafts: Vec::new(),
                    operations: vec![
                        MutationOperation::SetTaskPlan {
                            task_id: "task".into(),
                            expected_revision: 1,
                            plan: Some(RecordRef::existing("plan")),
                            milestone: Some(RecordRef::new_title("New milestone")),
                        },
                        MutationOperation::DeleteEvent {
                            event_id: "event".into(),
                            expected_revision: 1,
                        },
                    ],
                    applied: false,
                    applied_ids: Vec::new(),
                },
            });
        }
        agent
            .finish_pending(
                "p",
                Some(&AppliedProposal {
                    task_ids: vec!["created".into()],
                    ..AppliedProposal::default()
                }),
            )
            .unwrap();
        assert_eq!(agent.referenced_ids(), ["task", "plan", "event", "created"]);
    }

    async fn serve_chat(content: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 64 * 1024];
                let mut read = 0;
                loop {
                    let count = stream.read(&mut request[read..]).await.unwrap();
                    read += count;
                    let text = String::from_utf8_lossy(&request[..read]);
                    let Some(header_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if count == 0 || read >= header_end + 4 + length {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request[..read]);
                let body = if request.contains("/api/tags") {
                    r#"{"models":[{"name":"qwen3:8b","digest":"sha256:test"}]}"#.to_string()
                } else if request.contains("/api/version") {
                    r#"{"version":"test"}"#.to_string()
                } else if request.contains("/api/show") {
                    r#"{"license":"Apache-2.0"}"#.to_string()
                } else {
                    assert!(request.contains("\"format\""));
                    json!({ "message": { "content": content }, "done_reason": "stop" }).to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    fn request<'a>(command: &'a str, candidates: &'a PlannerCandidates) -> PlannerRequest<'a> {
        PlannerRequest {
            command,
            selected_day: "2026-08-12",
            time_zone: "America/New_York",
            active_plan_id: None,
            candidates,
        }
    }

    #[tokio::test]
    async fn production_http_path_rejects_a_reply_outside_the_contract() {
        let agent = PlannerAgent::new(
            serve_chat("Sure! Lunch is on your calendar.").await,
            MODEL_NAME,
        );
        let candidates = PlannerCandidates::default();
        let result = agent
            .propose(request("add lunch tomorrow at noon", &candidates))
            .await;
        assert!(matches!(result, Err(AppError::InvalidModelResponse(_))));
    }

    #[tokio::test]
    async fn production_http_path_turns_local_times_into_a_reviewable_proposal() {
        let agent = PlannerAgent::new(
            serve_chat(
                r#"{"kind":"proposal","summary":"Add lunch tomorrow at noon.","operations":[{"type":"create_event","title":"lunch","start":"2026-08-13T12:00"}]}"#,
            )
            .await,
            MODEL_NAME,
        );
        let candidates = PlannerCandidates::default();
        let response = agent
            .propose(request("add lunch tomorrow at noon", &candidates))
            .await
            .unwrap();
        let PlannerResponse::Proposal { operations, .. } = response else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            operations.as_slice(),
            [MutationOperation::CreateEvent { title, start_at_utc, .. }]
                if title == "Lunch" && start_at_utc == "2026-08-13T16:00:00.000Z"
        ));
        assert_eq!(agent.referenced_ids(), Vec::<String>::new());
    }
}
