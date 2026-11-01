//! The Assistant's link to the Python agent (DelveOps piece I).
//!
//! A run goes: the Assistant screen sends a request, the app starts (or reuses) the agent through
//! the sidecar supervisor, answers the agent's read-tool calls from the tool broker and its model
//! calls through the model broker, streams every trace event to the screen as it arrives, and ends
//! with a run record: what was asked, what happened step by step, and what it cost (model and tool
//! calls, tokens, time, the exact model). Those numbers are what the Assistant shows next to each
//! answer, and what the run history (piece G) will store.
//!
//! The agent never writes. When it drafts changes, this module turns each draft into one of the
//! planner's existing `MutationOperation`s, refuses any draft that doesn't parse, names a record or
//! revision the agent never saw, or cites tool calls that didn't happen, and only then offers the
//! changes for review. Applying them goes through the same repository code, revision checks, and
//! single transaction as the planner's own proposals.

use crate::db::{
    ActionOutcome, AgentStep, DraftDecision, DraftTarget, ExecutionState, NewActionDraft,
    NewAgentRun, PlannerDatabase, RunOutcome as StoredOutcome, RunSource, RunStatus,
    RunUsage as StoredUsage,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    ModelResponse, MutationOperation, PlannerResponse, ProposalReference, ProposedOperation,
    RecordKind, ReviewNote,
};
use crate::runtime::ModelIdentity;
use crate::sidecar::{
    ErrorCode, LinkError, ModelBroker, RunOutcome, Sidecar, SidecarCommand, SidecarError,
    SidecarHost, SidecarLimits,
};
use crate::tools::registry::ToolName;
use crate::tools::{SessionConfig, ToolOutcome, ToolSession};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The runner the sidecar entry point loads: the A1 agent adapted to the link.
pub const RUNNER: &str = "delveops_agent.link_runner:run";
/// The window event that carries each trace event of a run in progress.
pub const RUN_EVENT: &str = "assistant-run-event";
/// The architecture the Assistant runs today.
pub const ARCHITECTURE: &str = "A1";
/// Runs kept in memory for the Assistant's recent-runs list until run storage (piece G) lands.
const MAX_RUNS: usize = 20;
/// How long drafted changes wait for review, as for the planner's own proposals.
const REVIEW_TTL_MINUTES: i64 = 15;
/// The most changes one review holds, and the longest summary it shows (the planner's limits).
const MAX_CHANGES: usize = 12;
const MAX_SUMMARY_CHARS: usize = 280;
/// The longest request the Assistant accepts.
pub const MAX_GOAL_CHARS: usize = 2_000;
/// A sidecar left unused this long is stopped; the next request starts a fresh one.
pub const SIDECAR_IDLE: Duration = Duration::from_secs(10 * 60);

/// Where the agent comes from in this build: a packaged executable, or Python source in
/// development.
pub fn locate_agent(resource_dir: &Path) -> Option<(SidecarCommand, &'static str)> {
    let exe = if cfg!(windows) {
        "delveops-agent.exe"
    } else {
        "delveops-agent"
    };
    if let Some(path) = std::env::var_os("DELVEOPS_AGENT_EXE").map(PathBuf::from) {
        return Some((SidecarCommand::frozen(path, RUNNER), "packaged"));
    }
    let bundled = resource_dir.join("delveops-agent").join(exe);
    if bundled.is_file() {
        return Some((SidecarCommand::frozen(bundled, RUNNER), "packaged"));
    }
    let source = std::env::var_os("DELVEOPS_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            // Development builds run the agent straight from the repository.
            cfg!(debug_assertions)
                .then(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-python"))
        })
        .filter(|dir| dir.join("delveops_agent").is_dir())?;
    let python = std::env::var_os("DELVEOPS_AGENT_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
    let source = std::path::absolute(&source).unwrap_or(source);
    Some((
        SidecarCommand::python_source(python, source, RUNNER),
        "source",
    ))
}

/// Every read tool the broker serves; the Assistant may use all of them.
pub fn permitted_tools() -> Vec<String> {
    ToolName::ALL
        .into_iter()
        .filter(|tool| tool.implemented())
        .map(|tool| tool.as_str().to_string())
        .collect()
}

// ------------------------------------------------------------------------------------------------
// What the agent saw

/// The records and calls of one run, as the tool broker answered them. Drafts are checked
/// against this: a change may only name a record, at a revision, that a tool actually returned.
#[derive(Debug, Default)]
pub struct Observed {
    records: HashMap<String, ObservedRecord>,
    calls: HashSet<String>,
}

#[derive(Debug, Default, Clone)]
struct ObservedRecord {
    revision: Option<i64>,
    title: Option<String>,
}

impl Observed {
    /// Notes one `tool_result` payload. Failed calls count as nothing seen.
    pub fn note_tool_result(&mut self, payload: &Value) {
        if payload.get("ok") != Some(&Value::Bool(true)) {
            return;
        }
        if let Some(call_id) = payload.get("callId").and_then(Value::as_str) {
            self.calls.insert(call_id.to_string());
        }
        if let Some(result) = payload.get("result") {
            self.collect(result);
        }
    }

    fn collect(&mut self, value: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(id) = object.get("id").and_then(Value::as_str) {
                    let entry = self.records.entry(id.to_string()).or_default();
                    // A later read refreshes the revision; a summary without one keeps the old.
                    if let Some(revision) = object.get("revision").and_then(Value::as_i64) {
                        entry.revision = Some(revision);
                    }
                    if let Some(title) = ["title", "name"]
                        .iter()
                        .find_map(|key| object.get(*key).and_then(Value::as_str))
                    {
                        entry.title = Some(title.to_string());
                    }
                }
                for child in object.values() {
                    self.collect(child);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| self.collect(item)),
            _ => {}
        }
    }

    fn title(&self, id: &str) -> Option<&str> {
        self.records.get(id)?.title.as_deref()
    }
}

// ------------------------------------------------------------------------------------------------
// Drafts → reviewable changes

/// The kind of record a targeted operation changes, and the field that names it.
fn target_of(op: &str) -> Option<(RecordKind, &'static str)> {
    Some(match op {
        "update_event" | "delete_event" | "reschedule_event" | "set_event_plan" => {
            (RecordKind::Event, "eventId")
        }
        "update_plan" => (RecordKind::Plan, "planId"),
        "update_milestone" | "delete_milestone" => (RecordKind::Milestone, "milestoneId"),
        "update_task"
        | "schedule_task"
        | "set_task_plan"
        | "delete_task"
        | "set_task_workstream" => (RecordKind::Task, "taskId"),
        _ => return None,
    })
}

const CREATE_OPS: [&str; 5] = [
    "create_event",
    "create_plan",
    "create_milestone",
    "create_task",
    "create_workstream",
];

/// One draft turned into a planner operation, or the reason it can't be.
pub fn draft_operation(draft: &Value, observed: &Observed) -> Result<MutationOperation, String> {
    let op = draft
        .get("op")
        .and_then(Value::as_str)
        .ok_or("a draft has no operation")?;
    let target = target_of(op);
    if target.is_none() && !CREATE_OPS.contains(&op) {
        return Err(format!("`{op}` is not a change the planner supports"));
    }
    let Some(Value::Object(payload)) = draft.get("payload") else {
        return Err(format!("`{op}` has no details"));
    };
    let evidence: Vec<&str> = match draft.get("evidence") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().ok_or("evidence must be tool call ids"))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("evidence must be a list of tool call ids".into()),
    };
    if let Some(unknown) = evidence.iter().find(|id| !observed.calls.contains(**id)) {
        return Err(format!(
            "`{op}` cites a tool call that didn't return anything: {unknown}"
        ));
    }

    let mut operation: Map<String, Value> = payload.clone();
    if operation.contains_key("type") {
        return Err(format!("`{op}` details may not set the operation type"));
    }
    operation.insert("type".into(), Value::String(op.into()));
    if let Some((_, id_field)) = target {
        let id = draft
            .pointer("/target/id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("`{op}` doesn't say which record it changes"))?;
        let revision = draft
            .pointer("/target/revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("`{op}` doesn't give the record's revision"))?;
        if evidence.is_empty() {
            return Err(format!(
                "`{op}` changes a record without citing where it saw it"
            ));
        }
        match observed.records.get(id) {
            None => return Err(format!("`{op}` names a record no tool returned")),
            Some(seen) if seen.revision != Some(revision) => {
                return Err(format!(
                    "`{op}` names a revision of the record that no tool returned"
                ))
            }
            Some(_) => {}
        }
        for (field, value) in [(id_field, json!(id)), ("expectedRevision", json!(revision))] {
            match operation.get(field) {
                Some(given) if *given != value => {
                    return Err(format!("`{op}` names two different records"));
                }
                _ => {
                    operation.insert(field.into(), value);
                }
            }
        }
    } else if draft.get("target").is_some_and(|target| !target.is_null()) {
        return Err(format!(
            "`{op}` creates something new, so it can't have a target"
        ));
    }
    serde_json::from_value(Value::Object(operation))
        .map_err(|error| format!("`{op}` doesn't match the planner's format: {error}"))
}

/// The existing records a set of operations touches, titled from what the tools returned.
fn references(operations: &[MutationOperation], observed: &Observed) -> Vec<ProposalReference> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for operation in operations {
        let Ok(Value::Object(fields)) = serde_json::to_value(operation) else {
            continue;
        };
        let op = fields
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut found: Vec<(RecordKind, String)> = Vec::new();
        if let Some((kind, field)) = target_of(op) {
            if let Some(id) = fields.get(field).and_then(Value::as_str) {
                found.push((kind, id.to_string()));
            }
        }
        for (field, kind) in [
            ("plan", RecordKind::Plan),
            ("milestone", RecordKind::Milestone),
            ("workstream", RecordKind::Workstream),
        ] {
            if let Some(id) = fields
                .get(field)
                .and_then(|value| value.get("id")?.as_str())
            {
                found.push((kind, id.to_string()));
            }
        }
        for (kind, id) in found {
            if let Some(title) = observed.title(&id) {
                if seen.insert(id.clone()) {
                    out.push(ProposalReference {
                        id,
                        kind,
                        title: title.to_string(),
                    });
                }
            }
        }
    }
    out
}

/// What the app's own checks made of the agent's drafts. Every drafted run gets one, so the
/// share of drafts that pass is measurable.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DraftCheck {
    pub drafted: usize,
    pub passed: usize,
    pub problems: Vec<String>,
}

/// Checks every draft. Changes are offered only when all of them pass: a set of changes the
/// agent meant together is never applied in part because one of them was wrong.
pub fn check_drafts(drafts: &[Value], observed: &Observed) -> (DraftCheck, Vec<MutationOperation>) {
    let mut check = DraftCheck {
        drafted: drafts.len(),
        ..DraftCheck::default()
    };
    let mut operations = Vec::new();
    for draft in drafts {
        match draft_operation(draft, observed) {
            Ok(operation) => {
                check.passed += 1;
                operations.push(operation);
            }
            Err(problem) => check.problems.push(problem),
        }
    }
    if drafts.len() > MAX_CHANGES {
        check
            .problems
            .push(format!("more than {MAX_CHANGES} changes at once"));
    }
    if !check.problems.is_empty() {
        operations.clear();
    }
    (check, operations)
}

// ------------------------------------------------------------------------------------------------
// One run

/// What a run cost, counted by the app where it can be (calls) and by the agent where only it
/// knows (tokens).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    pub model_calls: u32,
    pub tool_calls: u32,
    pub tool_failures: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub duration_ms: u64,
}

/// Where the drafted changes of a run stand.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReviewState {
    /// Nothing to review.
    None,
    Pending,
    Applied {
        changes: usize,
    },
    Discarded,
    Expired,
    Failed {
        message: String,
    },
}

/// Everything the Assistant shows about one run, and what run storage will keep.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunRecord {
    /// The screen's own id for the run, known before it starts so live events can be matched.
    pub run_key: String,
    /// The sidecar's id for the run, when it got that far.
    pub run_id: Option<String>,
    pub architecture: String,
    pub goal: String,
    pub started_at: String,
    pub finished_at: String,
    /// `answered`, `drafted`, `needs_clarification`, `cannot_complete`, or `failed` when the run
    /// didn't produce a result (the agent couldn't start, crashed, or was cancelled).
    pub status: String,
    pub stop_reason: Option<String>,
    pub answer: Option<String>,
    pub error: Option<String>,
    pub cancelled: bool,
    pub model: Option<ModelIdentity>,
    pub usage: RunUsage,
    pub draft_check: Option<DraftCheck>,
    pub review: ReviewState,
    /// Validated changes, in the planner's own review shape, while they can still be applied.
    pub proposal: Option<PlannerResponse>,
    pub trace: Vec<Value>,
    /// The run's id in the run history, once saved there.
    pub stored_run_id: Option<String>,
    /// The drafts behind `proposal`, as the agent sent them, for the run history.
    #[serde(skip)]
    pub checked_drafts: Vec<Value>,
    /// The saved drafts behind `proposal`, in the same order as its changes.
    #[serde(skip)]
    pub stored_drafts: Vec<StoredDraft>,
}

/// One draft as the run history saved it: its id and the digest an approval must match.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDraft {
    pub id: String,
    pub digest: String,
}

/// The facts about a run known before it starts.
pub struct RunStart {
    pub run_key: String,
    pub goal: String,
    pub started_at: DateTime<Utc>,
    pub model: Option<ModelIdentity>,
}

/// The app's side of one run: tool calls, model calls, and trace events.
pub struct RunHost<R, E>
where
    R: Fn(&mut ToolSession, &str, Value) -> ToolOutcome + Sync,
    E: Fn(&Value) + Sync,
{
    session: Mutex<ToolSession>,
    read: R,
    broker: Option<ModelBroker>,
    on_event: E,
    observed: Mutex<Observed>,
    trace: Mutex<Vec<Value>>,
}

impl<R, E> RunHost<R, E>
where
    R: Fn(&mut ToolSession, &str, Value) -> ToolOutcome + Sync,
    E: Fn(&Value) + Sync,
{
    pub fn new(config: SessionConfig, read: R, broker: Option<ModelBroker>, on_event: E) -> Self {
        Self {
            session: Mutex::new(ToolSession::new(config)),
            read,
            broker,
            on_event,
            observed: Mutex::new(Observed::default()),
            trace: Mutex::new(Vec::new()),
        }
    }

    pub fn into_parts(self) -> (Observed, Vec<Value>) {
        (
            self.observed.into_inner().unwrap_or_default(),
            self.trace.into_inner().unwrap_or_default(),
        )
    }
}

impl<R, E> SidecarHost for RunHost<R, E>
where
    R: Fn(&mut ToolSession, &str, Value) -> ToolOutcome + Sync,
    E: Fn(&Value) + Sync,
{
    fn call_tool(
        &self,
        _run_id: &str,
        name: &str,
        arguments: &Value,
    ) -> impl Future<Output = Result<Value, LinkError>> + Send {
        let result = (|| {
            let outcome = {
                let mut session = self.session.lock().map_err(|_| broker_down())?;
                (self.read)(&mut session, name, arguments.clone())
            };
            let payload = serde_json::to_value(&outcome).map_err(|_| broker_down())?;
            if let Ok(mut observed) = self.observed.lock() {
                observed.note_tool_result(&payload);
            }
            Ok(payload)
        })();
        std::future::ready(result)
    }

    fn call_model(
        &self,
        _run_id: &str,
        request: &Value,
    ) -> impl Future<Output = Result<Value, LinkError>> + Send {
        let broker = self.broker.clone();
        let request = request.clone();
        async move {
            match broker {
                Some(broker) => broker.call(&request).await,
                None => Err(LinkError::new(
                    ErrorCode::Unavailable,
                    "no local model is chosen",
                    false,
                )),
            }
        }
    }

    fn trace(&self, _run_id: &str, event: &Value) {
        if let Ok(mut trace) = self.trace.lock() {
            trace.push(event.clone());
        }
        (self.on_event)(event);
    }
}

fn broker_down() -> LinkError {
    LinkError::new(ErrorCode::Internal, "the tool broker is unavailable", true)
}

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Builds the run record from how the run ended, and the changes to offer for review, if any.
pub fn finish_run(
    start: RunStart,
    outcome: Result<RunOutcome, SidecarError>,
    observed: &Observed,
    trace: Vec<Value>,
    finished_at: DateTime<Utc>,
) -> (AgentRunRecord, Option<ModelResponse>) {
    let duration_ms =
        u64::try_from((finished_at - start.started_at).num_milliseconds()).unwrap_or(0);
    let tool_failures = trace
        .iter()
        .filter(|event| {
            event.get("kind").and_then(Value::as_str) == Some("tool_result")
                && event.pointer("/data/ok") == Some(&Value::Bool(false))
        })
        .count() as u32;
    let mut record = AgentRunRecord {
        run_key: start.run_key,
        run_id: None,
        architecture: ARCHITECTURE.into(),
        goal: start.goal,
        started_at: iso(start.started_at),
        finished_at: iso(finished_at),
        status: "failed".into(),
        stop_reason: None,
        answer: None,
        error: None,
        cancelled: false,
        model: start.model,
        usage: RunUsage {
            tool_failures,
            duration_ms,
            ..RunUsage::default()
        },
        draft_check: None,
        review: ReviewState::None,
        proposal: None,
        trace,
        stored_run_id: None,
        checked_drafts: Vec::new(),
        stored_drafts: Vec::new(),
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            record.cancelled = matches!(error, SidecarError::Cancelled);
            record.stop_reason = Some(
                if record.cancelled {
                    "cancelled"
                } else {
                    "error"
                }
                .into(),
            );
            record.error = Some(error.to_string());
            return (record, None);
        }
    };
    let result = &outcome.result;
    let text = |key: &str| result.get(key).and_then(Value::as_str).map(str::to_string);
    let usage = |key: &str| {
        result
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_u64)
    };
    record.run_id = Some(outcome.run_id.clone());
    record.status = text("status").unwrap_or_else(|| "cannot_complete".into());
    record.stop_reason = text("stopReason");
    record.answer = text("answer");
    record.error = result
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string);
    record.cancelled = outcome.stopped == Some(crate::sidecar::StopReason::Cancelled);
    record.usage.model_calls = outcome.model_calls;
    record.usage.tool_calls = outcome.tool_calls;
    record.usage.prompt_tokens = usage("promptTokens").unwrap_or(0);
    record.usage.completion_tokens = usage("completionTokens").unwrap_or(0);
    record.usage.duration_ms = u64::try_from(outcome.duration.as_millis()).unwrap_or(duration_ms);

    if record.status != "drafted" {
        return (record, None);
    }
    let drafts = result
        .get("drafts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (check, operations) = check_drafts(&drafts, observed);
    record.draft_check = Some(check);
    if operations.is_empty() {
        return (record, None);
    }
    let summary = record
        .answer
        .clone()
        .map(|summary| {
            summary
                .trim()
                .chars()
                .take(MAX_SUMMARY_CHARS)
                .collect::<String>()
        })
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| "Proposed changes".into());
    let notes = vec![
        ReviewNote {
            reason: None,
            suggested: false,
        };
        operations.len()
    ];
    let expires_at = finished_at + ChronoDuration::minutes(REVIEW_TTL_MINUTES);
    record.proposal = Some(PlannerResponse::Proposal {
        proposal_id: Uuid::new_v4().to_string(),
        summary: summary.clone(),
        operations: ProposedOperation::review(&operations, &notes),
        references: references(&operations, observed),
        expires_at: iso(expires_at),
    });
    let response = ModelResponse::proposal(summary, operations);
    // The same checks the planner's own proposals pass before anyone sees them.
    if let Err(error) = crate::db::validate_model_response(&response) {
        if let Some(check) = record.draft_check.as_mut() {
            check.passed = 0;
            check.problems.push(error.to_string());
        }
        record.proposal = None;
        return (record, None);
    }
    record.review = ReviewState::Pending;
    record.checked_drafts = drafts;
    (record, Some(response))
}

// ------------------------------------------------------------------------------------------------
// The run history

/// The longest text the run history keeps for a summary or a reason.
const MAX_STORED_TEXT: usize = 500;

fn clipped(text: Option<&str>) -> Option<String> {
    text.map(|text| {
        text.trim()
            .chars()
            .take(MAX_STORED_TEXT)
            .collect::<String>()
    })
    .filter(|text| !text.is_empty())
}

fn stored_status(record: &AgentRunRecord) -> RunStatus {
    if record.cancelled {
        return RunStatus::Cancelled;
    }
    match record.status.as_str() {
        "answered" => RunStatus::Answered,
        "drafted" => RunStatus::Drafted,
        "needs_clarification" => RunStatus::NeedsClarification,
        "cannot_complete" => RunStatus::CannotComplete,
        _ => RunStatus::Failed,
    }
}

/// Saves a finished run in the run history: the run, every step, and the drafts it offers for
/// review. Fills in `stored_run_id` and `stored_drafts`. The Evaluation view and the
/// `get_recent_actions` tool read from here.
pub fn store_run(
    database: &mut PlannerDatabase,
    record: &mut AgentRunRecord,
    budget: &Value,
) -> AppResult<()> {
    let model = record.model.as_ref();
    let run = database.start_agent_run(NewAgentRun {
        source: RunSource::Assistant,
        experiment_run_id: None,
        case_id: None,
        case_family: None,
        goal: record.goal.clone(),
        architecture_id: record.architecture.clone(),
        model_name: model.map_or_else(|| "unknown".into(), |model| model.name.clone()),
        model_digest: model.map(|model| model.digest.clone()),
        trigger: "user_message".into(),
        budget: budget.clone(),
        started_at: Some(record.started_at.clone()),
    })?;
    for event in &record.trace {
        // A step the history can't hold (too large, odd shape) is left out, not the whole run.
        if let Ok(step) = serde_json::from_value::<AgentStep>(event.clone()) {
            let _ = database.append_agent_step(&run.id, step);
        }
    }
    let expires_at = match &record.proposal {
        Some(PlannerResponse::Proposal { expires_at, .. }) => Some(expires_at.clone()),
        _ => None,
    };
    let mut stored = Vec::new();
    if let Some(expires_at) = expires_at {
        for draft in &record.checked_drafts {
            let target = draft
                .get("target")
                .filter(|target| !target.is_null())
                .map(|target| serde_json::from_value::<DraftTarget>(target.clone()))
                .transpose()?;
            let saved = database.record_action_draft(
                &run.id,
                NewActionDraft {
                    op: draft
                        .get("op")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    target,
                    payload: draft.get("payload").cloned().unwrap_or(Value::Null),
                    evidence: draft
                        .get("evidence")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    expires_at: expires_at.clone(),
                },
            )?;
            stored.push(StoredDraft {
                id: saved.id,
                digest: saved.payload_digest,
            });
        }
    }
    let usage = &record.usage;
    database.finish_agent_run(
        &run.id,
        StoredOutcome {
            status: stored_status(record),
            outcome_summary: clipped(record.answer.as_deref()),
            failure_reason: clipped(record.error.as_deref().or(record.stop_reason.as_deref())),
            usage: StoredUsage {
                model_calls: i64::from(usage.model_calls),
                tool_calls: i64::from(usage.tool_calls),
                input_tokens: i64::try_from(usage.prompt_tokens).unwrap_or(i64::MAX),
                output_tokens: i64::try_from(usage.completion_tokens).unwrap_or(i64::MAX),
                latency_ms: i64::try_from(usage.duration_ms).ok(),
            },
            passed: None,
            safety_failure: false,
            finished_at: Some(record.finished_at.clone()),
        },
    )?;
    record.stored_run_id = Some(run.id);
    record.stored_drafts = stored;
    Ok(())
}

/// Records the user's decision on each saved draft and, for the ones applied, how it went.
/// `accepted` holds the review handles kept (all when `None`); `outcome` is how applying went.
pub fn store_decisions(
    database: &mut PlannerDatabase,
    drafts: &[StoredDraft],
    accepted: Option<&[String]>,
    outcome: Option<&Result<Value, AppError>>,
) -> AppResult<()> {
    for (index, draft) in drafts.iter().enumerate() {
        let handle = ProposedOperation::handle(index);
        let approved = outcome.is_some() && accepted.is_none_or(|kept| kept.contains(&handle));
        let decision = if approved {
            DraftDecision::Approve
        } else {
            DraftDecision::Reject
        };
        database.decide_action_draft(&draft.id, decision, &draft.digest)?;
        if let (true, Some(outcome)) = (approved, outcome) {
            let result = match outcome {
                Ok(effects) => ActionOutcome {
                    execution_state: ExecutionState::Applied,
                    effects: effects.clone(),
                    error: None,
                },
                Err(error) => ActionOutcome {
                    execution_state: if matches!(error, AppError::Conflict | AppError::NotFound) {
                        ExecutionState::Stale
                    } else {
                        ExecutionState::Failed
                    },
                    effects: Value::Null,
                    error: clipped(Some(&error.to_string())),
                },
            };
            database.record_action_result(&draft.id, result)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// The app's assistant state

/// Whether the Assistant can run the agent in this build.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStatus {
    pub available: bool,
    /// `packaged` or `source`, when available.
    pub source: Option<&'static str>,
    pub architecture: &'static str,
    pub running: bool,
    pub detail: String,
}

struct PendingReview {
    run_key: String,
    proposal_id: String,
    response: ModelResponse,
    expires_at: DateTime<Utc>,
    claimed: bool,
    stored: Vec<StoredDraft>,
}

pub struct AssistantRuntime {
    sidecar: Option<Sidecar>,
    source: Option<&'static str>,
    active: Mutex<Option<CancellationToken>>,
    runs: Mutex<VecDeque<AgentRunRecord>>,
    pending: Mutex<Option<PendingReview>>,
}

impl AssistantRuntime {
    pub fn new(resource_dir: &Path) -> Self {
        let (sidecar, source) = match locate_agent(resource_dir) {
            Some((command, source)) => (
                Some(Sidecar::new(command, SidecarLimits::default())),
                Some(source),
            ),
            None => (None, None),
        };
        Self {
            sidecar,
            source,
            active: Mutex::new(None),
            runs: Mutex::new(VecDeque::new()),
            pending: Mutex::new(None),
        }
    }

    pub fn sidecar(&self) -> AppResult<Sidecar> {
        self.sidecar.clone().ok_or_else(|| {
            AppError::Validation(
                "The new assistant isn't included in this build yet. The planner assistant still works.".into(),
            )
        })
    }

    pub fn status(&self) -> AssistantStatus {
        let running = self
            .active
            .lock()
            .map(|active| active.is_some())
            .unwrap_or(false);
        AssistantStatus {
            available: self.sidecar.is_some(),
            source: self.source,
            architecture: ARCHITECTURE,
            running,
            detail: match self.source {
                Some("packaged") => "Agent A1, packaged with the app".into(),
                Some(_) => "Agent A1, running from source (development)".into(),
                None => "The new assistant isn't included in this build".into(),
            },
        }
    }

    /// Marks a run as started. Only one runs at a time.
    pub fn begin(&self) -> AppResult<CancellationToken> {
        let mut active = self.active.lock().map_err(|_| poisoned())?;
        if active.is_some() {
            return Err(AppError::Validation(
                "The assistant is already working on a request.".into(),
            ));
        }
        let token = CancellationToken::new();
        *active = Some(token.clone());
        Ok(token)
    }

    pub fn end(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
    }

    pub fn cancel(&self) {
        if let Ok(active) = self.active.lock() {
            if let Some(token) = active.as_ref() {
                token.cancel();
            }
        }
    }

    /// Keeps a finished run, and its changes for review. New changes replace any older ones.
    pub fn record(
        &self,
        record: AgentRunRecord,
        response: Option<ModelResponse>,
    ) -> AgentRunRecord {
        let pending = match (&record.proposal, response) {
            (Some(PlannerResponse::Proposal { proposal_id, .. }), Some(response)) => {
                Some(PendingReview {
                    run_key: record.run_key.clone(),
                    proposal_id: proposal_id.clone(),
                    response,
                    expires_at: Utc::now() + ChronoDuration::minutes(REVIEW_TTL_MINUTES),
                    claimed: false,
                    stored: record.stored_drafts.clone(),
                })
            }
            _ => None,
        };
        if let Ok(mut runs) = self.runs.lock() {
            if pending.is_some() {
                // Only the newest changes can be applied; older ones are spent.
                for run in runs.iter_mut() {
                    if run.review == ReviewState::Pending {
                        run.review = ReviewState::Expired;
                        run.proposal = None;
                    }
                }
            }
            runs.push_front(record.clone());
            runs.truncate(MAX_RUNS);
        }
        if let Some(pending) = pending {
            if let Ok(mut slot) = self.pending.lock() {
                *slot = Some(pending);
            }
        }
        record
    }

    /// Recent runs, newest first. Changes that expired since are shown as expired.
    pub fn runs(&self) -> Vec<AgentRunRecord> {
        self.expire();
        self.runs
            .lock()
            .map(|runs| runs.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn expire(&self) {
        let expired = {
            let Ok(mut slot) = self.pending.lock() else {
                return;
            };
            match slot.as_ref() {
                Some(pending) if !pending.claimed && pending.expires_at <= Utc::now() => {
                    slot.take().map(|pending| pending.run_key)
                }
                _ => None,
            }
        };
        if let Some(run_key) = expired {
            self.set_review(&run_key, ReviewState::Expired);
        }
    }

    /// Takes the changes for applying, with their saved drafts. Each set can be applied once.
    pub fn claim(&self, proposal_id: &str) -> AppResult<(ModelResponse, Vec<StoredDraft>)> {
        self.expire();
        let mut slot = self.pending.lock().map_err(|_| poisoned())?;
        let Some(pending) = slot.as_mut() else {
            return Err(AppError::ProposalUnavailable);
        };
        if pending.proposal_id != proposal_id || pending.claimed {
            return Err(AppError::ProposalUnavailable);
        }
        pending.claimed = true;
        Ok((pending.response.clone(), pending.stored.clone()))
    }

    /// Records how applying went: how many changes were applied, or why it failed. The changes
    /// are spent either way.
    pub fn finish_review(&self, proposal_id: &str, result: Result<usize, String>) {
        let run_key = {
            let Ok(mut slot) = self.pending.lock() else {
                return;
            };
            if slot
                .as_ref()
                .is_some_and(|pending| pending.proposal_id == proposal_id)
            {
                slot.take().map(|pending| pending.run_key)
            } else {
                None
            }
        };
        if let Some(run_key) = run_key {
            let state = match result {
                Ok(changes) => ReviewState::Applied { changes },
                Err(message) => ReviewState::Failed { message },
            };
            self.set_review(&run_key, state);
        }
    }

    /// Drops the changes unapplied, and gives back their saved drafts.
    pub fn discard(&self, proposal_id: &str) -> AppResult<Vec<StoredDraft>> {
        let pending = {
            let mut slot = self.pending.lock().map_err(|_| poisoned())?;
            match slot.as_ref() {
                Some(pending) if pending.proposal_id == proposal_id && !pending.claimed => {
                    slot.take()
                }
                _ => return Err(AppError::ProposalUnavailable),
            }
        };
        let Some(pending) = pending else {
            return Ok(Vec::new());
        };
        self.set_review(&pending.run_key, ReviewState::Discarded);
        Ok(pending.stored)
    }

    fn set_review(&self, run_key: &str, state: ReviewState) {
        if let Ok(mut runs) = self.runs.lock() {
            if let Some(run) = runs.iter_mut().find(|run| run.run_key == run_key) {
                run.review = state;
                run.proposal = None;
            }
        }
    }

    /// Stops the agent process if it has been idle; the next request starts it again.
    pub async fn stop_if_idle(&self) {
        if let Some(sidecar) = &self.sidecar {
            sidecar.stop_if_idle(SIDECAR_IDLE).await;
        }
    }

    pub async fn shutdown(&self) {
        if let Some(sidecar) = &self.sidecar {
            sidecar.shutdown().await;
        }
    }
}

fn poisoned() -> AppError {
    AppError::Internal("The assistant's state is unavailable.".into())
}

#[cfg(test)]
mod tests;
