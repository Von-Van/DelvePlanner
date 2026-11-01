//! What the assistant did, kept on the device so its behaviour can be measured and reviewed.
//!
//! Five record kinds live here:
//!
//! - `experiment_run`: one benchmark or regression run (benchmark version, config hash, model,
//!   runtime, hardware, where the full result files are).
//! - `agent_run`: one attempt at one goal, either a live assistant request or one benchmark case
//!   inside an experiment. Holds the architecture, model + digest, budget, usage, final status and,
//!   for benchmark cases, whether the case passed and whether it broke a safety rule.
//! - `agent_step`: the step-by-step trace of a run, in the shared trace-event shape
//!   (`seq`, `kind`, `at`, `durationMs`, `summary`, `data`).
//! - `agent_action_draft`: a change the agent proposed, as exactly one existing planner operation
//!   with its target revision and a digest of the payload. A draft is never applied from here.
//! - `agent_action_result`: what happened after the user decided on a draft.
//!
//! None of this is planner data: it is never exported with the planner, imports leave it alone,
//! and every record can be deleted. Traces hold metadata and short summaries; step `data` is
//! capped so full prompts and responses don't land here by accident.

use super::{now, utc_string, validate_revision, PlannerDatabase};
use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Live assistant runs kept before the oldest finished ones are forgotten. Benchmark runs belong
/// to an experiment and are kept until that experiment is deleted.
pub const ASSISTANT_RUN_RETENTION: i64 = 500;
pub const MAX_STEPS_PER_RUN: i64 = 500;
pub const MAX_RUN_LIST: u32 = 200;
pub const MAX_RECENT_AGENT_ACTIONS: u32 = 20;
const MAX_GOAL_CHARS: usize = 2_000;
const MAX_SUMMARY_CHARS: usize = 500;
const MAX_SHORT_TEXT_CHARS: usize = 200;
const MAX_STEP_DATA_BYTES: usize = 16 * 1024;
const MAX_JSON_BYTES: usize = 64 * 1024;

/// The operations a draft may carry: the snake_case names of `MutationOperation`'s variants.
pub const DRAFT_OPERATIONS: [&str; 17] = [
    "create_event",
    "update_event",
    "delete_event",
    "reschedule_event",
    "set_event_plan",
    "create_plan",
    "update_plan",
    "create_milestone",
    "update_milestone",
    "delete_milestone",
    "create_task",
    "update_task",
    "schedule_task",
    "set_task_plan",
    "delete_task",
    "create_workstream",
    "set_task_workstream",
];

// ---------------------------------------------------------------------------------------------
// Schema

pub(super) fn ensure_agent_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS experiment_run (
             id TEXT PRIMARY KEY NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('benchmark', 'regression')),
             benchmark_version TEXT NOT NULL,
             config_hash TEXT NOT NULL,
             architecture_id TEXT NOT NULL,
             model_name TEXT NOT NULL,
             model_digest TEXT,
             runtime_version TEXT,
             hardware TEXT,
             split TEXT,
             result_path TEXT,
             notes TEXT,
             status TEXT NOT NULL DEFAULT 'running'
                 CHECK(status IN ('running', 'completed', 'failed', 'cancelled')),
             started_at TEXT NOT NULL,
             finished_at TEXT
         );
         CREATE INDEX IF NOT EXISTS experiment_run_started_idx ON experiment_run(started_at);

         CREATE TABLE IF NOT EXISTS agent_run (
             id TEXT PRIMARY KEY NOT NULL,
             source TEXT NOT NULL CHECK(source IN ('assistant', 'benchmark', 'regression')),
             experiment_run_id TEXT REFERENCES experiment_run(id) ON DELETE CASCADE,
             case_id TEXT,
             case_family TEXT,
             goal TEXT NOT NULL,
             architecture_id TEXT NOT NULL,
             model_name TEXT NOT NULL,
             model_digest TEXT,
             trigger TEXT NOT NULL,
             budget_json TEXT NOT NULL DEFAULT '{}',
             status TEXT NOT NULL DEFAULT 'running'
                 CHECK(status IN ('running', 'answered', 'drafted', 'needs_clarification',
                                  'cannot_complete', 'failed', 'cancelled')),
             outcome_summary TEXT,
             failure_reason TEXT,
             passed INTEGER CHECK(passed IN (0, 1)),
             safety_failure INTEGER NOT NULL DEFAULT 0 CHECK(safety_failure IN (0, 1)),
             model_calls INTEGER NOT NULL DEFAULT 0 CHECK(model_calls >= 0),
             tool_calls INTEGER NOT NULL DEFAULT 0 CHECK(tool_calls >= 0),
             input_tokens INTEGER NOT NULL DEFAULT 0 CHECK(input_tokens >= 0),
             output_tokens INTEGER NOT NULL DEFAULT 0 CHECK(output_tokens >= 0),
             latency_ms INTEGER CHECK(latency_ms >= 0),
             started_at TEXT NOT NULL,
             finished_at TEXT,
             CHECK((source = 'assistant') = (experiment_run_id IS NULL))
         );
         CREATE INDEX IF NOT EXISTS agent_run_started_idx ON agent_run(started_at);
         CREATE INDEX IF NOT EXISTS agent_run_experiment_idx ON agent_run(experiment_run_id);

         CREATE TABLE IF NOT EXISTS agent_step (
             run_id TEXT NOT NULL REFERENCES agent_run(id) ON DELETE CASCADE,
             seq INTEGER NOT NULL CHECK(seq >= 0),
             kind TEXT NOT NULL CHECK(kind IN ('model_call', 'tool_call', 'tool_result', 'plan',
                                               'verify', 'repair', 'draft', 'final', 'error')),
             at TEXT NOT NULL,
             duration_ms INTEGER CHECK(duration_ms >= 0),
             summary TEXT NOT NULL,
             data_json TEXT NOT NULL DEFAULT '{}',
             PRIMARY KEY(run_id, seq)
         );

         CREATE TABLE IF NOT EXISTS agent_action_draft (
             id TEXT PRIMARY KEY NOT NULL,
             run_id TEXT NOT NULL REFERENCES agent_run(id) ON DELETE CASCADE,
             op TEXT NOT NULL,
             target_id TEXT,
             target_revision INTEGER CHECK(target_revision > 0),
             payload_json TEXT NOT NULL,
             payload_digest TEXT NOT NULL,
             evidence_json TEXT NOT NULL DEFAULT '[]',
             state TEXT NOT NULL DEFAULT 'pending'
                 CHECK(state IN ('pending', 'approved', 'rejected', 'expired', 'stale')),
             created_at TEXT NOT NULL,
             expires_at TEXT NOT NULL,
             decided_at TEXT,
             CHECK((target_id IS NULL) = (target_revision IS NULL))
         );
         CREATE INDEX IF NOT EXISTS agent_action_draft_run_idx ON agent_action_draft(run_id);

         CREATE TABLE IF NOT EXISTS agent_action_result (
             id TEXT PRIMARY KEY NOT NULL,
             draft_id TEXT NOT NULL UNIQUE REFERENCES agent_action_draft(id) ON DELETE CASCADE,
             execution_state TEXT NOT NULL CHECK(execution_state IN ('applied', 'failed', 'stale')),
             effects_json TEXT NOT NULL DEFAULT '[]',
             error TEXT,
             recorded_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS agent_action_result_recorded_idx
             ON agent_action_result(recorded_at);",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSource {
    Assistant,
    Benchmark,
    Regression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Answered,
    Drafted,
    NeedsClarification,
    CannotComplete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    ModelCall,
    ToolCall,
    ToolResult,
    Plan,
    Verify,
    Repair,
    Draft,
    Final,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    Pending,
    Approved,
    Rejected,
    Expired,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftDecision {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Applied,
    Failed,
    Stale,
}

macro_rules! text_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            fn parse(value: &str) -> rusqlite::Result<Self> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        format!("unknown {} '{other}'", stringify!($name)).into(),
                    )),
                }
            }
        }
    };
}

text_enum!(RunSource { Assistant => "assistant", Benchmark => "benchmark", Regression => "regression" });
text_enum!(RunStatus {
    Running => "running",
    Answered => "answered",
    Drafted => "drafted",
    NeedsClarification => "needs_clarification",
    CannotComplete => "cannot_complete",
    Failed => "failed",
    Cancelled => "cancelled",
});
text_enum!(StepKind {
    ModelCall => "model_call",
    ToolCall => "tool_call",
    ToolResult => "tool_result",
    Plan => "plan",
    Verify => "verify",
    Repair => "repair",
    Draft => "draft",
    Final => "final",
    Error => "error",
});
text_enum!(ExperimentStatus {
    Running => "running",
    Completed => "completed",
    Failed => "failed",
    Cancelled => "cancelled",
});
text_enum!(DraftState {
    Pending => "pending",
    Approved => "approved",
    Rejected => "rejected",
    Expired => "expired",
    Stale => "stale",
});
text_enum!(ExecutionState { Applied => "applied", Failed => "failed", Stale => "stale" });

/// Starts an experiment: one benchmark or regression run over many cases.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewExperimentRun {
    /// `benchmark` or `regression`.
    pub kind: RunSource,
    pub benchmark_version: String,
    pub config_hash: String,
    pub architecture_id: String,
    pub model_name: String,
    pub model_digest: Option<String>,
    pub runtime_version: Option<String>,
    pub hardware: Option<String>,
    /// `dev`, `heldout`, or another split label.
    pub split: Option<String>,
    /// Where the full result files live, for example `agent_bench/results/…`.
    pub result_path: Option<String>,
    pub notes: Option<String>,
    /// When the experiment ran. Defaults to now; set it when importing a finished result file.
    pub started_at: Option<String>,
}

/// An experiment with its cases rolled up, as the evaluation view lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRun {
    pub id: String,
    pub kind: RunSource,
    pub benchmark_version: String,
    pub config_hash: String,
    pub architecture_id: String,
    pub model_name: String,
    pub model_digest: Option<String>,
    pub runtime_version: Option<String>,
    pub hardware: Option<String>,
    pub split: Option<String>,
    pub result_path: Option<String>,
    pub notes: Option<String>,
    pub status: ExperimentStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub case_count: i64,
    pub passed_count: i64,
    /// Cases scored pass or fail; cases still running or unscored are left out.
    pub scored_count: i64,
    pub safety_failures: i64,
    pub avg_latency_ms: Option<f64>,
}

/// Starts one run: a live assistant request (no experiment) or one benchmark case.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAgentRun {
    pub source: RunSource,
    pub experiment_run_id: Option<String>,
    pub case_id: Option<String>,
    pub case_family: Option<String>,
    pub goal: String,
    pub architecture_id: String,
    pub model_name: String,
    pub model_digest: Option<String>,
    /// What started the run, for example `user_message`, `benchmark`, `suggestion`.
    pub trigger: String,
    /// The resource limits the run was given (`maxModelCalls`, `maxToolCalls`, …).
    #[serde(default)]
    pub budget: Value,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    pub model_calls: i64,
    pub tool_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub latency_ms: Option<i64>,
}

/// How a run ended. `cannot_complete` is a normal ending, not a failure.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutcome {
    pub status: RunStatus,
    pub outcome_summary: Option<String>,
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub usage: RunUsage,
    /// Benchmark score; `None` for live assistant runs.
    pub passed: Option<bool>,
    #[serde(default)]
    pub safety_failure: bool,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub id: String,
    pub source: RunSource,
    pub experiment_run_id: Option<String>,
    pub case_id: Option<String>,
    pub case_family: Option<String>,
    pub goal: String,
    pub architecture_id: String,
    pub model_name: String,
    pub model_digest: Option<String>,
    pub trigger: String,
    pub budget: Value,
    pub status: RunStatus,
    pub outcome_summary: Option<String>,
    pub failure_reason: Option<String>,
    pub passed: Option<bool>,
    pub safety_failure: bool,
    pub usage: RunUsage,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub step_count: i64,
}

/// One trace event, in the shared shape every piece emits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStep {
    pub seq: i64,
    pub kind: StepKind,
    pub at: String,
    pub duration_ms: Option<i64>,
    pub summary: String,
    #[serde(default)]
    pub data: Value,
}

/// The target a draft changes: an existing record at the revision the agent read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftTarget {
    pub id: String,
    pub revision: i64,
}

/// A proposed change in the shared draft shape. Creating operations have no target.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewActionDraft {
    pub op: String,
    pub target: Option<DraftTarget>,
    pub payload: Value,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionDraft {
    pub id: String,
    pub run_id: String,
    pub op: String,
    pub target: Option<DraftTarget>,
    pub payload: Value,
    /// SHA-256 of the operation, target and payload, so an approved draft can be matched to the
    /// exact payload the user saw.
    pub payload_digest: String,
    pub evidence: Vec<String>,
    pub state: DraftState,
    pub created_at: String,
    pub expires_at: String,
    pub decided_at: Option<String>,
    pub result: Option<ActionResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionOutcome {
    pub execution_state: ExecutionState,
    #[serde(default)]
    pub effects: Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    pub id: String,
    pub execution_state: ExecutionState,
    pub effects: Value,
    pub error: Option<String>,
    pub recorded_at: String,
}

/// A run with everything recorded about it, for the trace viewer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunDetail {
    pub run: AgentRun,
    pub steps: Vec<AgentStep>,
    pub drafts: Vec<ActionDraft>,
}

/// Which runs to list, newest first (by `startedAt`, then `id`). To page backwards, pass the
/// last run of the previous page as `before` (its `startedAt`) and `beforeId` (its `id`); runs
/// that started in the same millisecond are then split by id instead of skipped.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunFilter {
    pub source: Option<RunSource>,
    pub experiment_run_id: Option<String>,
    pub before: Option<String>,
    pub before_id: Option<String>,
    pub limit: Option<u32>,
}

/// One decided draft and what came of it, for `get_recent_actions`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentAgentAction {
    pub draft_id: String,
    pub run_id: String,
    pub op: String,
    pub target: Option<DraftTarget>,
    pub state: DraftState,
    pub execution_state: Option<ExecutionState>,
    pub error: Option<String>,
    pub at: String,
}

// ---------------------------------------------------------------------------------------------
// Writes

impl PlannerDatabase {
    pub fn start_experiment_run(&mut self, input: NewExperimentRun) -> AppResult<ExperimentRun> {
        if input.kind == RunSource::Assistant {
            return Err(invalid("An experiment is a benchmark or regression run."));
        }
        let id = Uuid::new_v4().to_string();
        let started_at = input.started_at.unwrap_or_else(now);
        self.connection.execute(
            "INSERT INTO experiment_run (id, kind, benchmark_version, config_hash, architecture_id,
                 model_name, model_digest, runtime_version, hardware, split, result_path, notes,
                 started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                input.kind.as_str(),
                required(&input.benchmark_version, "Benchmark version")?,
                required(&input.config_hash, "Config hash")?,
                required(&input.architecture_id, "Architecture")?,
                required(&input.model_name, "Model")?,
                optional(input.model_digest, "Model digest", MAX_SHORT_TEXT_CHARS)?,
                optional(
                    input.runtime_version,
                    "Runtime version",
                    MAX_SHORT_TEXT_CHARS
                )?,
                optional(input.hardware, "Hardware", MAX_SUMMARY_CHARS)?,
                optional(input.split, "Split", MAX_SHORT_TEXT_CHARS)?,
                optional(input.result_path, "Result path", MAX_SUMMARY_CHARS)?,
                optional(input.notes, "Notes", MAX_GOAL_CHARS)?,
                timestamp(&started_at)?,
            ],
        )?;
        self.experiment_run(&id)
    }

    pub fn finish_experiment_run(
        &mut self,
        id: &str,
        status: ExperimentStatus,
        finished_at: Option<String>,
    ) -> AppResult<ExperimentRun> {
        if status == ExperimentStatus::Running {
            return Err(invalid("A finished experiment needs a final status."));
        }
        let finished_at = finished_at.unwrap_or_else(now);
        let changed = self.connection.execute(
            "UPDATE experiment_run SET status = ?1, finished_at = ?2
             WHERE id = ?3 AND status = 'running'",
            params![status.as_str(), timestamp(&finished_at)?, id],
        )?;
        if changed == 0 {
            self.experiment_run(id)?;
            return Err(AppError::Conflict);
        }
        self.experiment_run(id)
    }

    pub fn start_agent_run(&mut self, input: NewAgentRun) -> AppResult<AgentRun> {
        let id = Uuid::new_v4().to_string();
        let started_at = input.started_at.unwrap_or_else(now);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        match (&input.source, &input.experiment_run_id) {
            (RunSource::Assistant, None) => {}
            (RunSource::Assistant, Some(_)) => {
                return Err(invalid(
                    "A live assistant run doesn't belong to an experiment.",
                ))
            }
            (_, None) => return Err(invalid("A benchmark run belongs to an experiment.")),
            (source, Some(experiment)) => {
                let kind: Option<String> = transaction
                    .query_row(
                        "SELECT kind FROM experiment_run WHERE id = ?1",
                        params![experiment],
                        |row| row.get(0),
                    )
                    .optional()?;
                match kind {
                    None => return Err(AppError::NotFound),
                    Some(kind) if kind != source.as_str() => {
                        return Err(invalid("A run's source must match its experiment."))
                    }
                    Some(_) => {}
                }
            }
        }
        transaction.execute(
            "INSERT INTO agent_run (id, source, experiment_run_id, case_id, case_family, goal,
                 architecture_id, model_name, model_digest, trigger, budget_json, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                input.source.as_str(),
                input.experiment_run_id,
                optional(input.case_id, "Case id", MAX_SHORT_TEXT_CHARS)?,
                optional(input.case_family, "Case family", MAX_SHORT_TEXT_CHARS)?,
                bounded(input.goal.trim(), "A goal", MAX_GOAL_CHARS)?,
                required(&input.architecture_id, "Architecture")?,
                required(&input.model_name, "Model")?,
                optional(input.model_digest, "Model digest", MAX_SHORT_TEXT_CHARS)?,
                required(&input.trigger, "Trigger")?,
                json_text(&object_or_empty(input.budget), MAX_JSON_BYTES, "A budget")?,
                timestamp(&started_at)?,
            ],
        )?;
        if input.source == RunSource::Assistant {
            prune_assistant_runs(&transaction)?;
        }
        transaction.commit()?;
        self.agent_run(&id)
    }

    /// Adds one trace event to a running run. Sequence numbers must increase.
    pub fn append_agent_step(&mut self, run_id: &str, step: AgentStep) -> AppResult<AgentStep> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = run_status(&transaction, run_id)?;
        if status != RunStatus::Running {
            return Err(invalid(
                "Steps can only be added while a run is in progress.",
            ));
        }
        let (count, last): (i64, Option<i64>) = transaction.query_row(
            "SELECT COUNT(*), MAX(seq) FROM agent_step WHERE run_id = ?1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count >= MAX_STEPS_PER_RUN {
            return Err(invalid(
                "This run has recorded as many steps as it can keep.",
            ));
        }
        if step.seq < 0 || last.is_some_and(|last| step.seq <= last) {
            return Err(invalid("Trace steps must arrive in increasing order."));
        }
        if step.duration_ms.is_some_and(|value| value < 0) {
            return Err(invalid("A step can't take negative time."));
        }
        let step = AgentStep {
            summary: bounded(step.summary.trim(), "A step summary", MAX_SUMMARY_CHARS)?.into(),
            at: timestamp(&step.at)?,
            data: object_or_empty(step.data),
            ..step
        };
        transaction.execute(
            "INSERT INTO agent_step (run_id, seq, kind, at, duration_ms, summary, data_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                step.seq,
                step.kind.as_str(),
                step.at,
                step.duration_ms,
                step.summary,
                json_text(&step.data, MAX_STEP_DATA_BYTES, "A step's data")?,
            ],
        )?;
        transaction.commit()?;
        Ok(step)
    }

    pub fn finish_agent_run(&mut self, run_id: &str, outcome: RunOutcome) -> AppResult<AgentRun> {
        if outcome.status == RunStatus::Running {
            return Err(invalid("A finished run needs a final status."));
        }
        let usage = &outcome.usage;
        if [
            usage.model_calls,
            usage.tool_calls,
            usage.input_tokens,
            usage.output_tokens,
            usage.latency_ms.unwrap_or(0),
        ]
        .iter()
        .any(|value| *value < 0)
        {
            return Err(invalid("Run usage can't be negative."));
        }
        let source = self.agent_run(run_id)?.source;
        if source == RunSource::Assistant && outcome.passed.is_some() {
            return Err(invalid("Only benchmark runs are scored."));
        }
        let finished_at = outcome.finished_at.unwrap_or_else(now);
        let changed = self.connection.execute(
            "UPDATE agent_run SET status = ?1, outcome_summary = ?2, failure_reason = ?3,
                 passed = ?4, safety_failure = ?5, model_calls = ?6, tool_calls = ?7,
                 input_tokens = ?8, output_tokens = ?9, latency_ms = ?10, finished_at = ?11
             WHERE id = ?12 AND status = 'running'",
            params![
                outcome.status.as_str(),
                optional(
                    outcome.outcome_summary,
                    "An outcome summary",
                    MAX_SUMMARY_CHARS
                )?,
                optional(
                    outcome.failure_reason,
                    "A failure reason",
                    MAX_SUMMARY_CHARS
                )?,
                outcome.passed,
                outcome.safety_failure,
                usage.model_calls,
                usage.tool_calls,
                usage.input_tokens,
                usage.output_tokens,
                usage.latency_ms,
                timestamp(&finished_at)?,
                run_id,
            ],
        )?;
        if changed == 0 {
            return Err(AppError::Conflict);
        }
        self.agent_run(run_id)
    }

    /// Records a change the agent proposed. It is stored for review only; nothing is applied.
    pub fn record_action_draft(
        &mut self,
        run_id: &str,
        draft: NewActionDraft,
    ) -> AppResult<ActionDraft> {
        let op = draft.op.trim();
        if !DRAFT_OPERATIONS.contains(&op) {
            return Err(invalid(&format!("'{op}' is not a planner operation.")));
        }
        if let Some(target) = &draft.target {
            validate_revision(target.revision)?;
            required(&target.id, "A draft target")?;
        }
        if draft.evidence.len() > 50 {
            return Err(invalid("A draft can cite at most 50 pieces of evidence."));
        }
        if run_status(&self.connection, run_id)? != RunStatus::Running {
            return Err(invalid(
                "Drafts can only be recorded while a run is in progress.",
            ));
        }
        let id = Uuid::new_v4().to_string();
        let digest = payload_digest(op, draft.target.as_ref(), &draft.payload);
        self.connection.execute(
            "INSERT INTO agent_action_draft (id, run_id, op, target_id, target_revision,
                 payload_json, payload_digest, evidence_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                run_id,
                op,
                draft.target.as_ref().map(|target| target.id.as_str()),
                draft.target.as_ref().map(|target| target.revision),
                json_text(&draft.payload, MAX_JSON_BYTES, "A draft payload")?,
                digest,
                serde_json::to_string(&draft.evidence)?,
                now(),
                timestamp(&draft.expires_at)?,
            ],
        )?;
        self.action_draft(&id)
    }

    /// Records the user's decision on a pending draft. Approving checks the draft hasn't expired
    /// and that the caller is approving the exact payload that was shown (`payload_digest`).
    pub fn decide_action_draft(
        &mut self,
        draft_id: &str,
        decision: DraftDecision,
        payload_digest: &str,
    ) -> AppResult<ActionDraft> {
        let draft = self.action_draft(draft_id)?;
        if draft.state != DraftState::Pending {
            return Err(AppError::ProposalUnavailable);
        }
        if draft.payload_digest != payload_digest {
            return Err(AppError::Conflict);
        }
        let decided_at = now();
        let state = if decision == DraftDecision::Approve && draft.expires_at <= decided_at {
            DraftState::Expired
        } else if decision == DraftDecision::Approve {
            DraftState::Approved
        } else {
            DraftState::Rejected
        };
        self.connection.execute(
            "UPDATE agent_action_draft SET state = ?1, decided_at = ?2
             WHERE id = ?3 AND state = 'pending'",
            params![state.as_str(), decided_at, draft_id],
        )?;
        if state == DraftState::Expired {
            return Err(AppError::ProposalExpired);
        }
        self.action_draft(draft_id)
    }

    /// Records what happened when an approved draft was carried out by the planner's own code.
    /// A stale outcome (the target changed since the agent read it) marks the draft stale.
    pub fn record_action_result(
        &mut self,
        draft_id: &str,
        outcome: ActionOutcome,
    ) -> AppResult<ActionDraft> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = transaction
            .query_row(
                "SELECT state FROM agent_action_draft WHERE id = ?1",
                params![draft_id],
                |row| row.get(0),
            )
            .optional()?;
        match state.as_deref() {
            None => return Err(AppError::NotFound),
            Some("approved") => {}
            Some(_) => return Err(invalid("Only an approved draft has a result.")),
        }
        let effects = match outcome.effects {
            Value::Null => Value::Array(Vec::new()),
            other => other,
        };
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO agent_action_result (id, draft_id, execution_state,
                 effects_json, error, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::new_v4().to_string(),
                draft_id,
                outcome.execution_state.as_str(),
                json_text(&effects, MAX_JSON_BYTES, "A draft's effects")?,
                optional(outcome.error, "An error", MAX_SUMMARY_CHARS)?,
                now(),
            ],
        )?;
        if inserted == 0 {
            return Err(AppError::Conflict);
        }
        if outcome.execution_state == ExecutionState::Stale {
            transaction.execute(
                "UPDATE agent_action_draft SET state = 'stale' WHERE id = ?1",
                params![draft_id],
            )?;
        }
        transaction.commit()?;
        self.action_draft(draft_id)
    }

    /// Marks pending drafts whose review window has passed as expired. Returns how many changed.
    pub fn expire_action_drafts(&mut self) -> AppResult<usize> {
        let at = now();
        Ok(self.connection.execute(
            "UPDATE agent_action_draft SET state = 'expired', decided_at = ?1
             WHERE state = 'pending' AND expires_at <= ?1",
            params![at],
        )?)
    }

    /// Deletes one run with its steps, drafts and results.
    pub fn delete_agent_run(&mut self, run_id: &str) -> AppResult<()> {
        let changed = self
            .connection
            .execute("DELETE FROM agent_run WHERE id = ?1", params![run_id])?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// Deletes an experiment and every run recorded under it.
    pub fn delete_experiment_run(&mut self, id: &str) -> AppResult<()> {
        let changed = self
            .connection
            .execute("DELETE FROM experiment_run WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// Forgets all agent history: every run, trace, draft, result and experiment.
    pub fn clear_agent_history(&mut self) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM agent_run", [])?;
        transaction.execute("DELETE FROM experiment_run", [])?;
        transaction.commit()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Reads

const RUN_SELECT: &str = "SELECT r.id, r.source, r.experiment_run_id, r.case_id, r.case_family,
        r.goal, r.architecture_id, r.model_name, r.model_digest, r.trigger, r.budget_json,
        r.status, r.outcome_summary, r.failure_reason, r.passed, r.safety_failure,
        r.model_calls, r.tool_calls, r.input_tokens, r.output_tokens, r.latency_ms,
        r.started_at, r.finished_at,
        (SELECT COUNT(*) FROM agent_step s WHERE s.run_id = r.id)
     FROM agent_run r";

const EXPERIMENT_SELECT: &str = "SELECT e.id, e.kind, e.benchmark_version, e.config_hash,
        e.architecture_id, e.model_name, e.model_digest, e.runtime_version, e.hardware, e.split,
        e.result_path, e.notes, e.status, e.started_at, e.finished_at,
        COUNT(r.id), COALESCE(SUM(r.passed = 1), 0), COUNT(r.passed),
        COALESCE(SUM(r.safety_failure), 0), AVG(r.latency_ms)
     FROM experiment_run e LEFT JOIN agent_run r ON r.experiment_run_id = e.id";

const DRAFT_SELECT: &str = "SELECT d.id, d.run_id, d.op, d.target_id, d.target_revision,
        d.payload_json, d.payload_digest, d.evidence_json, d.state, d.created_at, d.expires_at,
        d.decided_at, x.id, x.execution_state, x.effects_json, x.error, x.recorded_at
     FROM agent_action_draft d LEFT JOIN agent_action_result x ON x.draft_id = d.id";

impl PlannerDatabase {
    pub fn agent_run(&self, id: &str) -> AppResult<AgentRun> {
        self.connection
            .query_row(
                &format!("{RUN_SELECT} WHERE r.id = ?1"),
                params![id],
                run_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }

    pub fn agent_run_detail(&self, id: &str) -> AppResult<AgentRunDetail> {
        let run = self.agent_run(id)?;
        let mut statement = self.connection.prepare(
            "SELECT seq, kind, at, duration_ms, summary, data_json
             FROM agent_step WHERE run_id = ?1 ORDER BY seq",
        )?;
        let steps = statement
            .query_map(params![id], step_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut statement = self.connection.prepare(&format!(
            "{DRAFT_SELECT} WHERE d.run_id = ?1 ORDER BY d.created_at, d.rowid"
        ))?;
        let drafts = statement
            .query_map(params![id], draft_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(AgentRunDetail { run, steps, drafts })
    }

    pub fn list_agent_runs(&self, filter: AgentRunFilter) -> AppResult<Vec<AgentRun>> {
        let limit = list_limit(filter.limit, MAX_RUN_LIST)?;
        let mut statement = self.connection.prepare(&format!(
            "{RUN_SELECT}
             WHERE (?1 IS NULL OR r.source = ?1)
               AND (?2 IS NULL OR r.experiment_run_id = ?2)
               AND (?3 IS NULL OR r.started_at < ?3
                    OR (r.started_at = ?3 AND ?5 IS NOT NULL AND r.id < ?5))
             ORDER BY r.started_at DESC, r.id DESC LIMIT ?4"
        ))?;
        let runs = statement
            .query_map(
                params![
                    filter.source.map(RunSource::as_str),
                    filter.experiment_run_id,
                    filter.before.as_deref().map(timestamp).transpose()?,
                    limit,
                    filter.before_id,
                ],
                run_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }

    pub fn experiment_run(&self, id: &str) -> AppResult<ExperimentRun> {
        self.connection
            .query_row(
                &format!("{EXPERIMENT_SELECT} WHERE e.id = ?1 GROUP BY e.id"),
                params![id],
                experiment_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }

    pub fn list_experiment_runs(&self, limit: Option<u32>) -> AppResult<Vec<ExperimentRun>> {
        let limit = list_limit(limit, MAX_RUN_LIST)?;
        let mut statement = self.connection.prepare(&format!(
            "{EXPERIMENT_SELECT} GROUP BY e.id ORDER BY e.started_at DESC, e.rowid DESC LIMIT ?1"
        ))?;
        let runs = statement
            .query_map(params![limit], experiment_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }

    pub fn action_draft(&self, id: &str) -> AppResult<ActionDraft> {
        self.connection
            .query_row(
                &format!("{DRAFT_SELECT} WHERE d.id = ?1"),
                params![id],
                draft_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }

    /// Recently decided drafts from live assistant runs and what came of them, newest first.
    /// Benchmark runs are left out: they never touch the user's planner.
    pub fn recent_agent_actions(&self, limit: u32) -> AppResult<Vec<RecentAgentAction>> {
        let limit = list_limit(Some(limit), MAX_RECENT_AGENT_ACTIONS)?;
        let mut statement = self.connection.prepare(
            "SELECT d.id, d.run_id, d.op, d.target_id, d.target_revision, d.state,
                    x.execution_state, x.error, COALESCE(x.recorded_at, d.decided_at) AS at
             FROM agent_action_draft d
             JOIN agent_run r ON r.id = d.run_id
             LEFT JOIN agent_action_result x ON x.draft_id = d.id
             WHERE r.source = 'assistant' AND d.decided_at IS NOT NULL
             ORDER BY at DESC, d.rowid DESC LIMIT ?1",
        )?;
        let actions = statement
            .query_map(params![limit], |row| {
                let execution: Option<String> = row.get(6)?;
                Ok(RecentAgentAction {
                    draft_id: row.get(0)?,
                    run_id: row.get(1)?,
                    op: row.get(2)?,
                    target: target_from(row.get(3)?, row.get(4)?),
                    state: DraftState::parse(&row.get::<_, String>(5)?)?,
                    execution_state: execution
                        .as_deref()
                        .map(ExecutionState::parse)
                        .transpose()?,
                    error: row.get(7)?,
                    at: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(actions)
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers

/// Keeps the newest live assistant runs; older finished ones are forgotten. Unfinished runs are
/// never pruned out from under a running agent.
fn prune_assistant_runs(connection: &Connection) -> AppResult<()> {
    connection.execute(
        "DELETE FROM agent_run WHERE id IN (
             SELECT id FROM agent_run
             WHERE source = 'assistant' AND status != 'running'
             ORDER BY started_at DESC, rowid DESC
             LIMIT -1 OFFSET ?1
         )",
        params![ASSISTANT_RUN_RETENTION],
    )?;
    Ok(())
}

fn run_status(connection: &Connection, run_id: &str) -> AppResult<RunStatus> {
    let status: Option<String> = connection
        .query_row(
            "SELECT status FROM agent_run WHERE id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()?;
    let status = status.ok_or(AppError::NotFound)?;
    Ok(RunStatus::parse(&status)?)
}

/// SHA-256 over a canonical form of the operation, its target and its payload (object keys
/// sorted), so the same draft always has the same digest.
pub fn payload_digest(op: &str, target: Option<&DraftTarget>, payload: &Value) -> String {
    let mut canonical = String::new();
    write_canonical(
        &serde_json::json!({ "op": op, "target": target, "payload": payload }),
        &mut canonical,
    );
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn invalid(message: &str) -> AppError {
    AppError::Validation(message.to_string())
}

fn bounded<'a>(value: &'a str, what: &str, max: usize) -> AppResult<&'a str> {
    if value.is_empty() {
        return Err(invalid(&format!("{what} can't be empty.")));
    }
    if value.chars().count() > max {
        return Err(invalid(&format!("{what} is longer than {max} characters.")));
    }
    Ok(value)
}

fn required<'a>(value: &'a str, what: &str) -> AppResult<&'a str> {
    bounded(value.trim(), what, MAX_SHORT_TEXT_CHARS)
}

fn optional(value: Option<String>, what: &str, max: usize) -> AppResult<Option<String>> {
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Ok(Some(bounded(text, what, max)?.to_string())),
    }
}

/// Parses an ISO-8601 time with a zone and stores it in the planner's UTC form, so stored times
/// sort and compare as text.
fn timestamp(value: &str) -> AppResult<String> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .map(|time| utc_string(time.with_timezone(&chrono::Utc)))
        .map_err(|_| invalid(&format!("'{value}' is not an ISO-8601 time with a zone.")))
}

fn object_or_empty(value: Value) -> Value {
    match value {
        Value::Null => Value::Object(Default::default()),
        other => other,
    }
}

fn json_text(value: &Value, max_bytes: usize, what: &str) -> AppResult<String> {
    let text = serde_json::to_string(value)?;
    if text.len() > max_bytes {
        return Err(invalid(&format!(
            "{what} is larger than {} KiB.",
            max_bytes / 1024
        )));
    }
    Ok(text)
}

fn list_limit(limit: Option<u32>, max: u32) -> AppResult<u32> {
    let limit = limit.unwrap_or(50.min(max));
    if !(1..=max).contains(&limit) {
        return Err(invalid(&format!(
            "A list limit must be between 1 and {max}."
        )));
    }
    Ok(limit)
}

fn parse_json(text: String) -> rusqlite::Result<Value> {
    serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
    })
}

fn target_from(id: Option<String>, revision: Option<i64>) -> Option<DraftTarget> {
    Some(DraftTarget {
        id: id?,
        revision: revision?,
    })
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<AgentRun> {
    Ok(AgentRun {
        id: row.get(0)?,
        source: RunSource::parse(&row.get::<_, String>(1)?)?,
        experiment_run_id: row.get(2)?,
        case_id: row.get(3)?,
        case_family: row.get(4)?,
        goal: row.get(5)?,
        architecture_id: row.get(6)?,
        model_name: row.get(7)?,
        model_digest: row.get(8)?,
        trigger: row.get(9)?,
        budget: parse_json(row.get(10)?)?,
        status: RunStatus::parse(&row.get::<_, String>(11)?)?,
        outcome_summary: row.get(12)?,
        failure_reason: row.get(13)?,
        passed: row.get(14)?,
        safety_failure: row.get(15)?,
        usage: RunUsage {
            model_calls: row.get(16)?,
            tool_calls: row.get(17)?,
            input_tokens: row.get(18)?,
            output_tokens: row.get(19)?,
            latency_ms: row.get(20)?,
        },
        started_at: row.get(21)?,
        finished_at: row.get(22)?,
        step_count: row.get(23)?,
    })
}

fn step_from_row(row: &Row<'_>) -> rusqlite::Result<AgentStep> {
    Ok(AgentStep {
        seq: row.get(0)?,
        kind: StepKind::parse(&row.get::<_, String>(1)?)?,
        at: row.get(2)?,
        duration_ms: row.get(3)?,
        summary: row.get(4)?,
        data: parse_json(row.get(5)?)?,
    })
}

fn experiment_from_row(row: &Row<'_>) -> rusqlite::Result<ExperimentRun> {
    Ok(ExperimentRun {
        id: row.get(0)?,
        kind: RunSource::parse(&row.get::<_, String>(1)?)?,
        benchmark_version: row.get(2)?,
        config_hash: row.get(3)?,
        architecture_id: row.get(4)?,
        model_name: row.get(5)?,
        model_digest: row.get(6)?,
        runtime_version: row.get(7)?,
        hardware: row.get(8)?,
        split: row.get(9)?,
        result_path: row.get(10)?,
        notes: row.get(11)?,
        status: ExperimentStatus::parse(&row.get::<_, String>(12)?)?,
        started_at: row.get(13)?,
        finished_at: row.get(14)?,
        case_count: row.get(15)?,
        passed_count: row.get(16)?,
        scored_count: row.get(17)?,
        safety_failures: row.get(18)?,
        avg_latency_ms: row.get(19)?,
    })
}

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<ActionDraft> {
    let evidence: Vec<String> =
        serde_json::from_str(&row.get::<_, String>(7)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, error.into())
        })?;
    let result_id: Option<String> = row.get(12)?;
    let result = match result_id {
        None => None,
        Some(id) => Some(ActionResult {
            id,
            execution_state: ExecutionState::parse(&row.get::<_, String>(13)?)?,
            effects: parse_json(row.get(14)?)?,
            error: row.get(15)?,
            recorded_at: row.get(16)?,
        }),
    };
    Ok(ActionDraft {
        id: row.get(0)?,
        run_id: row.get(1)?,
        op: row.get(2)?,
        target: target_from(row.get(3)?, row.get(4)?),
        payload: parse_json(row.get(5)?)?,
        payload_digest: row.get(6)?,
        evidence,
        state: DraftState::parse(&row.get::<_, String>(8)?)?,
        created_at: row.get(9)?,
        expires_at: row.get(10)?,
        decided_at: row.get(11)?,
        result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::tempdir;

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn assistant_run(database: &mut PlannerDatabase, goal: &str) -> AgentRun {
        database
            .start_agent_run(NewAgentRun {
                source: RunSource::Assistant,
                experiment_run_id: None,
                case_id: None,
                case_family: None,
                goal: goal.into(),
                architecture_id: "A1".into(),
                model_name: "qwen3:8b".into(),
                model_digest: Some("sha256:abc".into()),
                trigger: "user_message".into(),
                budget: json!({ "maxModelCalls": 6, "maxToolCalls": 8 }),
                started_at: None,
            })
            .unwrap()
    }

    fn experiment(database: &mut PlannerDatabase) -> ExperimentRun {
        database
            .start_experiment_run(NewExperimentRun {
                kind: RunSource::Benchmark,
                benchmark_version: "agent_bench 0.1".into(),
                config_hash: "cfg-1".into(),
                architecture_id: "A1".into(),
                model_name: "qwen3:8b".into(),
                model_digest: Some("sha256:abc".into()),
                runtime_version: Some("ollama 0.12".into()),
                hardware: Some("M2, 16 GB".into()),
                split: Some("dev".into()),
                result_path: Some("agent_bench/results/run-1.json".into()),
                notes: None,
                started_at: Some("2026-11-01T09:00:00-07:00".into()),
            })
            .unwrap()
    }

    fn case_run(database: &mut PlannerDatabase, experiment: &str, case: &str) -> AgentRun {
        database
            .start_agent_run(NewAgentRun {
                source: RunSource::Benchmark,
                experiment_run_id: Some(experiment.into()),
                case_id: Some(case.into()),
                case_family: Some("tool_choice".into()),
                goal: format!("Case {case}"),
                architecture_id: "A1".into(),
                model_name: "qwen3:8b".into(),
                model_digest: None,
                trigger: "benchmark".into(),
                budget: Value::Null,
                started_at: None,
            })
            .unwrap()
    }

    fn step(seq: i64, kind: StepKind, summary: &str) -> AgentStep {
        AgentStep {
            seq,
            kind,
            at: "2026-11-01T17:00:00.000Z".into(),
            duration_ms: Some(40),
            summary: summary.into(),
            data: json!({ "tool": "get_day_context" }),
        }
    }

    fn outcome(status: RunStatus, passed: Option<bool>, latency: i64) -> RunOutcome {
        RunOutcome {
            status,
            outcome_summary: Some("Done.".into()),
            failure_reason: None,
            usage: RunUsage {
                model_calls: 2,
                tool_calls: 3,
                input_tokens: 900,
                output_tokens: 120,
                latency_ms: Some(latency),
            },
            passed,
            safety_failure: false,
            finished_at: None,
        }
    }

    fn draft(op: &str, payload: Value) -> NewActionDraft {
        NewActionDraft {
            op: op.into(),
            target: Some(DraftTarget {
                id: "task-1".into(),
                revision: 3,
            }),
            payload,
            evidence: vec!["call-1".into()],
            expires_at: "2999-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn an_assistant_run_records_its_trace_and_outcome() {
        let mut database = database();
        let run = assistant_run(&mut database, "  What's on today?  ");
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.goal, "What's on today?");
        database
            .append_agent_step(&run.id, step(1, StepKind::ToolCall, "Read today"))
            .unwrap();
        database
            .append_agent_step(&run.id, step(2, StepKind::Final, "Answered"))
            .unwrap();
        let finished = database
            .finish_agent_run(&run.id, outcome(RunStatus::Answered, None, 812))
            .unwrap();
        assert_eq!(finished.status, RunStatus::Answered);
        assert_eq!(finished.usage.latency_ms, Some(812));
        assert_eq!(finished.step_count, 2);
        assert!(finished.finished_at.is_some());

        let detail = database.agent_run_detail(&run.id).unwrap();
        assert_eq!(
            detail.steps.iter().map(|step| step.seq).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(detail.steps[0].data, json!({ "tool": "get_day_context" }));
        assert_eq!(detail.run.budget["maxToolCalls"], 8);
        assert_eq!(
            database
                .list_agent_runs(AgentRunFilter::default())
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            database.finish_agent_run(&run.id, outcome(RunStatus::Failed, None, 1)),
            Err(AppError::Conflict)
        ));
    }

    #[test]
    fn steps_must_arrive_in_order_while_the_run_is_open() {
        let mut database = database();
        let run = assistant_run(&mut database, "Plan my week");
        database
            .append_agent_step(&run.id, step(3, StepKind::Plan, "Plan"))
            .unwrap();
        for seq in [3, 2] {
            assert!(matches!(
                database.append_agent_step(&run.id, step(seq, StepKind::Plan, "Again")),
                Err(AppError::Validation(_))
            ));
        }
        let mut big = step(4, StepKind::ModelCall, "Huge");
        big.data = json!({ "prompt": "x".repeat(MAX_STEP_DATA_BYTES) });
        assert!(matches!(
            database.append_agent_step(&run.id, big),
            Err(AppError::Validation(_))
        ));
        assert!(matches!(
            database.append_agent_step("missing", step(1, StepKind::Plan, "Plan")),
            Err(AppError::NotFound)
        ));
        database
            .finish_agent_run(&run.id, outcome(RunStatus::CannotComplete, None, 5))
            .unwrap();
        assert!(matches!(
            database.append_agent_step(&run.id, step(9, StepKind::Final, "Late")),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn an_experiment_rolls_up_its_cases_and_deletes_with_them() {
        let mut database = database();
        let experiment = experiment(&mut database);
        assert_eq!(experiment.started_at, "2026-11-01T16:00:00.000Z");
        assert_eq!(experiment.case_count, 0);
        assert_eq!(experiment.avg_latency_ms, None);

        let pass = case_run(&mut database, &experiment.id, "tc-001");
        database
            .finish_agent_run(&pass.id, outcome(RunStatus::Answered, Some(true), 100))
            .unwrap();
        let fail = case_run(&mut database, &experiment.id, "tc-002");
        let mut unsafe_outcome = outcome(RunStatus::Drafted, Some(false), 300);
        unsafe_outcome.safety_failure = true;
        unsafe_outcome.failure_reason = Some("Drafted a delete it wasn't asked for".into());
        database.finish_agent_run(&fail.id, unsafe_outcome).unwrap();
        case_run(&mut database, &experiment.id, "tc-003");

        let finished = database
            .finish_experiment_run(&experiment.id, ExperimentStatus::Completed, None)
            .unwrap();
        assert_eq!(finished.status, ExperimentStatus::Completed);
        assert_eq!(finished.case_count, 3);
        assert_eq!(finished.passed_count, 1);
        assert_eq!(finished.scored_count, 2);
        assert_eq!(finished.safety_failures, 1);
        assert_eq!(finished.avg_latency_ms, Some(200.0));
        assert_eq!(database.list_experiment_runs(None).unwrap(), [finished]);

        let cases = database
            .list_agent_runs(AgentRunFilter {
                experiment_run_id: Some(experiment.id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(cases.len(), 3);
        database.delete_experiment_run(&experiment.id).unwrap();
        assert!(matches!(
            database.agent_run(&pass.id),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn runs_must_match_their_experiment() {
        let mut database = database();
        let experiment = experiment(&mut database);
        let mut input = NewAgentRun {
            source: RunSource::Assistant,
            experiment_run_id: Some(experiment.id.clone()),
            case_id: None,
            case_family: None,
            goal: "Goal".into(),
            architecture_id: "A1".into(),
            model_name: "qwen3:8b".into(),
            model_digest: None,
            trigger: "user_message".into(),
            budget: Value::Null,
            started_at: None,
        };
        assert!(database.start_agent_run(input.clone()).is_err());
        input.source = RunSource::Regression;
        assert!(database.start_agent_run(input.clone()).is_err());
        input.source = RunSource::Benchmark;
        input.experiment_run_id = None;
        assert!(database.start_agent_run(input.clone()).is_err());
        input.experiment_run_id = Some("missing".into());
        assert!(matches!(
            database.start_agent_run(input),
            Err(AppError::NotFound)
        ));
        let assistant = assistant_run(&mut database, "Goal");
        assert!(database
            .finish_agent_run(&assistant.id, outcome(RunStatus::Answered, Some(true), 1))
            .is_err());
    }

    #[test]
    fn drafts_are_validated_digested_and_decided_once() {
        let mut database = database();
        let run = assistant_run(&mut database, "Move the report task");
        assert!(matches!(
            database.record_action_draft(&run.id, draft("drop_table", json!({}))),
            Err(AppError::Validation(_))
        ));
        let mut bad_revision = draft("update_task", json!({}));
        bad_revision.target.as_mut().unwrap().revision = 0;
        assert!(database.record_action_draft(&run.id, bad_revision).is_err());

        let recorded = database
            .record_action_draft(
                &run.id,
                draft(
                    "update_task",
                    json!({ "status": "done", "title": "Report" }),
                ),
            )
            .unwrap();
        assert_eq!(recorded.state, DraftState::Pending);
        assert_eq!(
            recorded.payload_digest,
            payload_digest(
                "update_task",
                recorded.target.as_ref(),
                &json!({ "title": "Report", "status": "done" })
            )
        );
        assert_ne!(
            recorded.payload_digest,
            payload_digest("update_task", None, &recorded.payload)
        );

        assert!(matches!(
            database.decide_action_draft(&recorded.id, DraftDecision::Approve, "other"),
            Err(AppError::Conflict)
        ));
        assert!(matches!(
            database.record_action_result(
                &recorded.id,
                ActionOutcome {
                    execution_state: ExecutionState::Applied,
                    effects: Value::Null,
                    error: None,
                }
            ),
            Err(AppError::Validation(_))
        ));
        let approved = database
            .decide_action_draft(
                &recorded.id,
                DraftDecision::Approve,
                &recorded.payload_digest,
            )
            .unwrap();
        assert_eq!(approved.state, DraftState::Approved);
        assert!(matches!(
            database.decide_action_draft(
                &recorded.id,
                DraftDecision::Reject,
                &recorded.payload_digest
            ),
            Err(AppError::ProposalUnavailable)
        ));
        let applied = database
            .record_action_result(
                &recorded.id,
                ActionOutcome {
                    execution_state: ExecutionState::Applied,
                    effects: json!([{ "kind": "task", "id": "task-1", "revision": 4 }]),
                    error: None,
                },
            )
            .unwrap();
        assert_eq!(
            applied.result.as_ref().unwrap().execution_state,
            ExecutionState::Applied
        );
        assert!(matches!(
            database.record_action_result(
                &recorded.id,
                ActionOutcome {
                    execution_state: ExecutionState::Failed,
                    effects: Value::Null,
                    error: None,
                }
            ),
            Err(AppError::Conflict)
        ));

        let actions = database.recent_agent_actions(10).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].op, "update_task");
        assert_eq!(actions[0].execution_state, Some(ExecutionState::Applied));
        assert_eq!(
            database.agent_run_detail(&run.id).unwrap().drafts,
            [applied]
        );
    }

    #[test]
    fn stale_and_expired_drafts_are_marked() {
        let mut database = database();
        let run = assistant_run(&mut database, "Reschedule");
        let stale = database
            .record_action_draft(
                &run.id,
                draft("schedule_task", json!({ "day": "2026-11-02" })),
            )
            .unwrap();
        database
            .decide_action_draft(&stale.id, DraftDecision::Approve, &stale.payload_digest)
            .unwrap();
        let stale = database
            .record_action_result(
                &stale.id,
                ActionOutcome {
                    execution_state: ExecutionState::Stale,
                    effects: Value::Null,
                    error: Some("The task changed since it was read.".into()),
                },
            )
            .unwrap();
        assert_eq!(stale.state, DraftState::Stale);
        assert_eq!(stale.result.unwrap().effects, json!([]));

        let mut old = draft("delete_task", json!({}));
        old.expires_at = "2020-01-01T00:00:00Z".into();
        let old = database.record_action_draft(&run.id, old).unwrap();
        assert!(matches!(
            database.decide_action_draft(&old.id, DraftDecision::Approve, &old.payload_digest),
            Err(AppError::ProposalExpired)
        ));
        assert_eq!(
            database.action_draft(&old.id).unwrap().state,
            DraftState::Expired
        );

        let mut pending = draft("delete_task", json!({}));
        pending.expires_at = "2020-01-01T00:00:00Z".into();
        let pending = database.record_action_draft(&run.id, pending).unwrap();
        assert_eq!(database.expire_action_drafts().unwrap(), 1);
        assert_eq!(
            database.action_draft(&pending.id).unwrap().state,
            DraftState::Expired
        );
    }

    #[test]
    fn recent_actions_leave_out_benchmark_runs() {
        let mut database = database();
        let experiment = experiment(&mut database);
        let case = case_run(&mut database, &experiment.id, "tc-001");
        let recorded = database
            .record_action_draft(&case.id, draft("update_task", json!({})))
            .unwrap();
        database
            .decide_action_draft(
                &recorded.id,
                DraftDecision::Reject,
                &recorded.payload_digest,
            )
            .unwrap();
        assert!(database.recent_agent_actions(5).unwrap().is_empty());
        assert!(database.recent_agent_actions(0).is_err());
        assert!(database
            .recent_agent_actions(MAX_RECENT_AGENT_ACTIONS + 1)
            .is_err());
    }

    #[test]
    fn old_assistant_runs_are_pruned_but_running_ones_are_kept() {
        let mut database = database();
        let open = assistant_run(&mut database, "Still going");
        let first = assistant_run(&mut database, "First finished");
        database
            .finish_agent_run(&first.id, outcome(RunStatus::Answered, None, 1))
            .unwrap();
        database
            .connection
            .execute(
                "UPDATE agent_run SET started_at = '2000-01-01T00:00:00.000Z'",
                [],
            )
            .unwrap();
        for index in 0..ASSISTANT_RUN_RETENTION {
            let run = assistant_run(&mut database, &format!("Run {index}"));
            database
                .finish_agent_run(&run.id, outcome(RunStatus::Answered, None, 1))
                .unwrap();
        }
        assistant_run(&mut database, "One more");
        assert!(database.agent_run(&open.id).is_ok());
        assert!(matches!(
            database.agent_run(&first.id),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn listing_pages_backwards_and_clearing_forgets_everything() {
        let mut database = database();
        let experiment = experiment(&mut database);
        case_run(&mut database, &experiment.id, "tc-001");
        let older = assistant_run(&mut database, "Older");
        database
            .connection
            .execute(
                "UPDATE agent_run SET started_at = '2026-10-01T00:00:00.000Z' WHERE id = ?1",
                params![older.id],
            )
            .unwrap();
        assistant_run(&mut database, "Newer");
        let assistants = database
            .list_agent_runs(AgentRunFilter {
                source: Some(RunSource::Assistant),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            assistants
                .iter()
                .map(|run| run.goal.as_str())
                .collect::<Vec<_>>(),
            ["Newer", "Older"]
        );
        let page = database
            .list_agent_runs(AgentRunFilter {
                before: Some("2026-10-15T00:00:00+02:00".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, older.id);

        // Runs that started in the same millisecond are all reachable, page by page.
        database
            .connection
            .execute(
                "UPDATE agent_run SET started_at = '2026-10-20T00:00:00.000Z'",
                [],
            )
            .unwrap();
        let mut seen = Vec::new();
        let mut cursor: Option<(String, String)> = None;
        loop {
            let page = database
                .list_agent_runs(AgentRunFilter {
                    before: cursor.as_ref().map(|(at, _)| at.clone()),
                    before_id: cursor.as_ref().map(|(_, id)| id.clone()),
                    limit: Some(1),
                    ..Default::default()
                })
                .unwrap();
            let Some(last) = page.last() else { break };
            cursor = Some((last.started_at.clone(), last.id.clone()));
            seen.push(last.id.clone());
        }
        assert_eq!(seen.len(), 3);
        assert!(database
            .list_agent_runs(AgentRunFilter {
                limit: Some(MAX_RUN_LIST + 1),
                ..Default::default()
            })
            .is_err());

        database.clear_agent_history().unwrap();
        assert!(database
            .list_agent_runs(AgentRunFilter::default())
            .unwrap()
            .is_empty());
        assert!(database.list_experiment_runs(None).unwrap().is_empty());
    }

    #[test]
    fn schema_eight_gains_empty_agent_history_with_a_backup() {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        drop(PlannerDatabase::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "DROP TABLE agent_action_result; DROP TABLE agent_action_draft;
                 DROP TABLE agent_step; DROP TABLE agent_run; DROP TABLE experiment_run;
                 PRAGMA user_version = 8;",
            )
            .unwrap();
        drop(connection);

        let mut migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), 9);
        assert!(migrated.list_backups().unwrap()[0]
            .name
            .contains("migration"));
        assert!(migrated
            .list_agent_runs(AgentRunFilter::default())
            .unwrap()
            .is_empty());
        assistant_run(&mut migrated, "Works after the upgrade");
    }
}
