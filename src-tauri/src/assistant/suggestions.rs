//! "Remember this?" (milestone M2): memories the agent suggests and the person approves.
//!
//! The agent can't write memory. It can add a `remember` draft next to its answer or its planner
//! changes. This module checks each one (a known kind, one short sentence, an honest basis, and a
//! project the run actually saw) and offers it on its own card, apart from planner changes, so a
//! memory is never applied as a side effect of approving a schedule change. Nothing is stored
//! until the person says yes; then it goes through `PlannerDatabase::add_memory` with its source:
//! `stated` becomes something the person said, `inferred` becomes the assistant's guess, which
//! stays unconfirmed and expires unless the person confirms it in Settings.

use super::{poisoned, AgentRunRecord, AssistantRuntime, Observed};
use crate::db::{MemoryKind, MemorySettings, MemorySource, NewMemory, MAX_MEMORY_CHARS};
use crate::error::{AppError, AppResult};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// The draft op the agent uses to suggest a memory.
pub const REMEMBER_OP: &str = "remember";
/// The most memories one run may suggest.
pub const MAX_SUGGESTIONS: usize = 3;

/// Who a suggested memory comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionBasis {
    /// The person said it in the request.
    Stated,
    /// The agent concluded it.
    Inferred,
}

impl SuggestionBasis {
    fn source(self) -> MemorySource {
        match self {
            Self::Stated => MemorySource::UserStated,
            Self::Inferred => MemorySource::AssistantInferred,
        }
    }
}

/// Where a suggestion stands.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SuggestionState {
    Pending,
    /// Being saved right now; a second approval is refused.
    Saving,
    Remembered {
        memory_id: String,
    },
    Dismissed,
}

/// One memory the agent suggested, as the Assistant shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySuggestion {
    pub id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub basis: SuggestionBasis,
    pub plan_id: Option<String>,
    /// The project's title as a tool returned it, for the card.
    pub plan_title: Option<String>,
    pub state: SuggestionState,
}

pub fn is_remember(draft: &Value) -> bool {
    draft.get("op").and_then(Value::as_str) == Some(REMEMBER_OP)
}

/// One `remember` draft turned into a suggestion, or the reason it can't be.
pub fn memory_suggestion(draft: &Value, observed: &Observed) -> Result<MemorySuggestion, String> {
    if draft.get("target").is_some_and(|target| !target.is_null()) {
        return Err("`remember` adds a memory, so it can't have a target".into());
    }
    match draft.get("evidence") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                let id = item.as_str().ok_or("evidence must be tool call ids")?;
                if !observed.calls.contains(id) {
                    return Err(format!(
                        "`remember` cites a tool call that didn't return anything: {id}"
                    ));
                }
            }
        }
        Some(_) => return Err("evidence must be a list of tool call ids".into()),
    }
    let Some(Value::Object(payload)) = draft.get("payload") else {
        return Err("`remember` has no details".into());
    };
    if let Some(unknown) = payload
        .keys()
        .find(|key| !["kind", "content", "basis", "planId"].contains(&key.as_str()))
    {
        return Err(format!("`remember` has an unknown field: {unknown}"));
    }
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let kind = text("kind")
        .and_then(MemoryKind::parse)
        .ok_or("`remember` needs a kind: decision, priority, project_fact, open_problem, preference or note")?;
    let content = text("content").map(str::trim).unwrap_or_default();
    if content.is_empty() || content.chars().count() > MAX_MEMORY_CHARS {
        return Err(format!(
            "`remember` needs one sentence of 1-{MAX_MEMORY_CHARS} characters"
        ));
    }
    let basis = match text("basis") {
        Some("stated") => SuggestionBasis::Stated,
        Some("inferred") => SuggestionBasis::Inferred,
        _ => return Err("`remember` needs a basis: stated or inferred".into()),
    };
    let plan_id = match payload.get("planId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if observed.records.contains_key(id) => Some(id.clone()),
        Some(_) => return Err("`remember` names a project no tool returned".into()),
    };
    Ok(MemorySuggestion {
        id: Uuid::new_v4().to_string(),
        kind,
        content: content.to_string(),
        basis,
        plan_title: plan_id
            .as_deref()
            .and_then(|id| observed.title(id))
            .map(str::to_string),
        plan_id,
        state: SuggestionState::Pending,
    })
}

/// Checks a run's `remember` drafts. Returns the suggestions and the problems found; a bad one
/// is dropped without affecting the others or the run's planner changes.
pub fn check_suggestions(
    drafts: &[Value],
    observed: &Observed,
) -> (Vec<MemorySuggestion>, Vec<String>) {
    let mut suggestions: Vec<MemorySuggestion> = Vec::new();
    let mut problems = Vec::new();
    for draft in drafts {
        match memory_suggestion(draft, observed) {
            // The same words for two projects are two memories, each with its own switch.
            Ok(suggestion)
                if suggestions.iter().any(|s| {
                    s.plan_id == suggestion.plan_id
                        && s.content.eq_ignore_ascii_case(&suggestion.content)
                }) => {}
            Ok(suggestion) if suggestions.len() >= MAX_SUGGESTIONS => {
                problems.push(format!(
                    "more than {MAX_SUGGESTIONS} memories suggested at once; dropped: {}",
                    suggestion.content
                ));
            }
            Ok(suggestion) => suggestions.push(suggestion),
            Err(problem) => problems.push(problem),
        }
    }
    (suggestions, problems)
}

/// Leaves out suggestions the app can't offer: none at all while memory is paused, none for a
/// project whose memory is off, and none whose `planId` isn't a project. A tool result holds
/// tasks, events and memories too, so an id the run saw isn't necessarily a project; `is_plan`
/// asks the planner.
pub fn apply_memory_settings(
    record: &mut AgentRunRecord,
    settings: &MemorySettings,
    is_plan: impl Fn(&str) -> bool,
) {
    let before = record.memory_suggestions.len();
    if !settings.enabled {
        record.memory_suggestions.clear();
    } else {
        record.memory_suggestions.retain(|suggestion| {
            suggestion
                .plan_id
                .as_ref()
                .is_none_or(|plan| !settings.disabled_plan_ids.contains(plan))
        });
    }
    let by_settings = before - record.memory_suggestions.len();
    let mut problems = Vec::new();
    if by_settings > 0 {
        problems.push(format!(
            "{by_settings} memory suggestion(s) left out by the memory settings"
        ));
    }
    record.memory_suggestions.retain(|suggestion| {
        let keep = suggestion.plan_id.as_deref().is_none_or(&is_plan);
        if !keep {
            problems.push(format!(
                "`remember` names a record that isn't a project: {}",
                suggestion.plan_id.as_deref().unwrap_or_default()
            ));
        }
        keep
    });
    if let Some(check) = record.draft_check.as_mut() {
        check.memory_problems.extend(problems);
    }
}

/// What approving a suggestion stores.
pub fn new_memory(suggestion: &MemorySuggestion, run_id: Option<&str>) -> NewMemory {
    NewMemory {
        kind: suggestion.kind,
        content: suggestion.content.clone(),
        source: suggestion.basis.source(),
        source_ref: Some(match run_id {
            Some(run) => format!("assistant run {run}"),
            None => "assistant".into(),
        }),
        plan_id: suggestion.plan_id.clone(),
    }
}

impl AssistantRuntime {
    /// Takes a pending suggestion for saving. Each can be approved once.
    pub fn claim_suggestion(
        &self,
        run_key: &str,
        suggestion_id: &str,
    ) -> AppResult<(MemorySuggestion, Option<String>)> {
        let mut runs = self.runs.lock().map_err(|_| poisoned())?;
        let run = runs
            .iter_mut()
            .find(|run| run.run_key == run_key)
            .ok_or(AppError::NotFound)?;
        let run_id = run.run_id.clone();
        let suggestion = run
            .memory_suggestions
            .iter_mut()
            .find(|s| s.id == suggestion_id)
            .ok_or(AppError::NotFound)?;
        if suggestion.state != SuggestionState::Pending {
            return Err(AppError::Validation(
                "That suggestion was already answered.".into(),
            ));
        }
        suggestion.state = SuggestionState::Saving;
        Ok((suggestion.clone(), run_id))
    }

    /// Records the answer to a suggestion and returns it.
    pub fn settle_suggestion(
        &self,
        run_key: &str,
        suggestion_id: &str,
        state: SuggestionState,
    ) -> AppResult<MemorySuggestion> {
        let mut runs = self.runs.lock().map_err(|_| poisoned())?;
        let suggestion = runs
            .iter_mut()
            .find(|run| run.run_key == run_key)
            .and_then(|run| {
                run.memory_suggestions
                    .iter_mut()
                    .find(|s| s.id == suggestion_id)
            })
            .ok_or(AppError::NotFound)?;
        suggestion.state = state;
        Ok(suggestion.clone())
    }

    /// Declines a pending suggestion. Nothing is stored.
    pub fn dismiss_suggestion(
        &self,
        run_key: &str,
        suggestion_id: &str,
    ) -> AppResult<MemorySuggestion> {
        self.claim_suggestion(run_key, suggestion_id)?;
        self.settle_suggestion(run_key, suggestion_id, SuggestionState::Dismissed)
    }
}
