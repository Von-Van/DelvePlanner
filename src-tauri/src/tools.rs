//! The read-tool broker: the only way an agent reads planner state. Tools are allowlisted,
//! typed, and read-only; every call is checked against the run's permissions and budgets, timed,
//! and logged as metadata (never the result text). Writes are not tools: they remain proposals
//! that existing Rust code validates and the user approves.
//!
//! `contracts/tools.v1.json` is generated from `registry` and is the published contract.

pub mod error;
pub mod memory;
pub mod read;
pub mod registry;

use crate::calendar::{CalendarService, DayRange};
use crate::db::{parse_export_bundle, PlannerDatabase};
use crate::error::AppResult;
use crate::model::ExternalEvent;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use error::{ToolError, ToolErrorCode};
use registry::ToolName;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

/// Where busy time from read-only calendars comes from. The app uses its calendar service; the
/// tool server and tests use `NoCalendars`.
pub trait BusySource {
    fn busy(&self, range: &DayRange) -> AppResult<(Vec<ExternalEvent>, bool)>;
}

impl BusySource for CalendarService {
    fn busy(&self, range: &DayRange) -> AppResult<(Vec<ExternalEvent>, bool)> {
        CalendarService::busy(self, range)
    }
}

/// No read-only calendars: no external busy time, and nothing missing.
pub struct NoCalendars;

impl BusySource for NoCalendars {
    fn busy(&self, _range: &DayRange) -> AppResult<(Vec<ExternalEvent>, bool)> {
        Ok((Vec::new(), false))
    }
}

/// What the tools read from for one call.
pub struct ToolSources<'a> {
    pub database: &'a PlannerDatabase,
    pub calendars: &'a dyn BusySource,
}

/// Which free text the tools may return. Titles are always included; descriptions and notes
/// only when allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivacyPolicy {
    #[serde(default)]
    pub include_descriptions: bool,
}

impl PrivacyPolicy {
    fn text(self, value: &str) -> Option<String> {
        (self.include_descriptions && !value.is_empty()).then(|| value.to_string())
    }
}

/// Limits for one agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolBudget {
    pub max_tool_calls: u32,
    /// The largest serialized result a call may return.
    pub max_result_bytes: usize,
    /// A call that takes longer has its result discarded with `timeout`.
    pub max_call_ms: u64,
    /// After this many identical failing calls, the same call is refused with
    /// `budget_exhausted` instead of being run again.
    pub max_identical_failures: u32,
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self {
            max_tool_calls: 24,
            max_result_bytes: 64 * 1024,
            max_call_ms: 2_000,
            max_identical_failures: 2,
        }
    }
}

impl ToolBudget {
    /// The most any run can ask for.
    pub const CEILING: ToolBudget = ToolBudget {
        max_tool_calls: 200,
        max_result_bytes: 256 * 1024,
        max_call_ms: 10_000,
        max_identical_failures: 5,
    };

    /// This budget with every limit held to the ceiling and at least 1.
    pub fn clamped(self) -> Self {
        let ceiling = Self::CEILING;
        Self {
            max_tool_calls: self.max_tool_calls.clamp(1, ceiling.max_tool_calls),
            max_result_bytes: self.max_result_bytes.clamp(1, ceiling.max_result_bytes),
            max_call_ms: self.max_call_ms.clamp(1, ceiling.max_call_ms),
            max_identical_failures: self
                .max_identical_failures
                .clamp(1, ceiling.max_identical_failures),
        }
    }
}

/// How one run may use the tools.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The run's frozen "now". Tools that need today (overdue counts) read it in `time_zone`.
    pub now: DateTime<Utc>,
    pub time_zone: Tz,
    /// `None` permits every implemented tool.
    pub permitted_tools: Option<BTreeSet<ToolName>>,
    pub budget: ToolBudget,
    pub privacy: PrivacyPolicy,
}

impl SessionConfig {
    pub fn new(now: DateTime<Utc>, time_zone: Tz) -> Self {
        Self {
            now,
            time_zone,
            permitted_tools: None,
            budget: ToolBudget::default(),
            privacy: PrivacyPolicy::default(),
        }
    }
}

/// The answer to one tool call, as sent back to the agent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutcome {
    pub call_id: String,
    pub tool: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<usize>,
    pub calls_remaining: u32,
}

/// What the broker remembers about a call: metadata only, for traces.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRecord {
    pub call_id: String,
    pub tool: String,
    pub ok: bool,
    pub error_code: Option<ToolErrorCode>,
    pub duration_ms: u64,
    pub result_bytes: Option<usize>,
}

/// One run's use of the tools: permissions, budgets, and the call log.
pub struct ToolSession {
    config: SessionConfig,
    calls_made: u32,
    failures: HashMap<String, (ToolErrorCode, u32)>,
    log: Vec<CallRecord>,
}

impl ToolSession {
    pub fn new(mut config: SessionConfig) -> Self {
        config.budget = config.budget.clamped();
        Self {
            config,
            calls_made: 0,
            failures: HashMap::new(),
            log: Vec::new(),
        }
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn log(&self) -> &[CallRecord] {
        &self.log
    }

    pub fn calls_remaining(&self) -> u32 {
        self.config
            .budget
            .max_tool_calls
            .saturating_sub(self.calls_made)
    }

    /// Runs one tool call. Every call, refused or not, gets a call id and a log entry; refused
    /// calls don't use up the call budget unless they ran.
    pub fn call(&mut self, sources: &ToolSources<'_>, tool: &str, arguments: Value) -> ToolOutcome {
        let call_id = format!("call-{}", self.log.len() + 1);
        let started = Instant::now();
        let fingerprint = format!("{tool}\u{0}{arguments}");
        let result = self
            .admit(tool, &fingerprint)
            .and_then(|name| {
                self.calls_made += 1;
                self.run(sources, name, arguments)
            })
            .and_then(|value| {
                let elapsed = started.elapsed().as_millis() as u64;
                if elapsed > self.config.budget.max_call_ms {
                    return Err(ToolError::new(
                        ToolErrorCode::Timeout,
                        format!(
                            "The call took {elapsed} ms, over its {} ms budget.",
                            self.config.budget.max_call_ms
                        ),
                    ));
                }
                let bytes = serde_json::to_vec(&value)
                    .map_err(|_| {
                        ToolError::new(ToolErrorCode::Internal, "The result could not be encoded.")
                    })?
                    .len();
                if bytes > self.config.budget.max_result_bytes {
                    return Err(ToolError::budget(format!(
                        "The result is {bytes} bytes, over the {} byte limit. Narrow the request.",
                        self.config.budget.max_result_bytes
                    )));
                }
                Ok((value, bytes))
            });
        let duration_ms = started.elapsed().as_millis() as u64;
        let outcome = match result {
            Ok((value, bytes)) => {
                self.failures.remove(&fingerprint);
                ToolOutcome {
                    call_id,
                    tool: tool.to_string(),
                    ok: true,
                    result: Some(value),
                    error: None,
                    duration_ms,
                    result_bytes: Some(bytes),
                    calls_remaining: self.calls_remaining(),
                }
            }
            Err(error) => {
                if error.code != ToolErrorCode::BudgetExhausted
                    && error.code != ToolErrorCode::PermissionDenied
                {
                    let entry = self.failures.entry(fingerprint).or_insert((error.code, 0));
                    *entry = if entry.0 == error.code {
                        (error.code, entry.1 + 1)
                    } else {
                        (error.code, 1)
                    };
                }
                ToolOutcome {
                    call_id,
                    tool: tool.to_string(),
                    ok: false,
                    result: None,
                    error: Some(error),
                    duration_ms,
                    result_bytes: None,
                    calls_remaining: self.calls_remaining(),
                }
            }
        };
        self.log.push(CallRecord {
            call_id: outcome.call_id.clone(),
            tool: outcome.tool.clone(),
            ok: outcome.ok,
            error_code: outcome.error.as_ref().map(|error| error.code),
            duration_ms: outcome.duration_ms,
            result_bytes: outcome.result_bytes,
        });
        outcome
    }

    /// Checks the tool name, permissions, and budgets before anything is read.
    fn admit(&self, tool: &str, fingerprint: &str) -> Result<ToolName, ToolError> {
        let name = ToolName::parse(tool)
            .ok_or_else(|| ToolError::invalid(format!("There is no tool named '{tool}'.")))?;
        let permitted = self
            .config
            .permitted_tools
            .as_ref()
            .is_none_or(|tools| tools.contains(&name));
        if !permitted {
            return Err(ToolError::new(
                ToolErrorCode::PermissionDenied,
                format!("This run may not use {tool}."),
            ));
        }
        if !name.implemented() {
            return Err(ToolError::new(
                ToolErrorCode::Unavailable,
                format!("{tool} is reserved and not available yet."),
            ));
        }
        if self.calls_made >= self.config.budget.max_tool_calls {
            return Err(ToolError::budget(format!(
                "This run has used all {} of its tool calls.",
                self.config.budget.max_tool_calls
            )));
        }
        if let Some((code, count)) = self.failures.get(fingerprint) {
            if *count >= self.config.budget.max_identical_failures {
                return Err(ToolError::budget(format!(
                    "This exact call already failed {count} times with {}. Change the \
                     arguments or stop.",
                    code.as_str()
                )));
            }
        }
        Ok(name)
    }

    fn run(
        &self,
        sources: &ToolSources<'_>,
        name: ToolName,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        let privacy = self.config.privacy;
        match name {
            ToolName::GetDayContext => {
                encode(read::get_day_context(sources, privacy, parse(arguments)?)?)
            }
            ToolName::ListActiveProjects => {
                let today = self
                    .config
                    .now
                    .with_timezone(&self.config.time_zone)
                    .date_naive()
                    .to_string();
                encode(read::list_active_projects(
                    sources,
                    privacy,
                    &today,
                    parse(arguments)?,
                )?)
            }
            ToolName::GetProjectContext => encode(read::get_project_context(
                sources,
                privacy,
                parse(arguments)?,
            )?),
            ToolName::SearchTasks => {
                encode(read::search_tasks(sources, privacy, parse(arguments)?)?)
            }
            ToolName::GetCapacity => encode(read::get_capacity(sources, parse(arguments)?)?),
            ToolName::GetRecentActions => encode(read::get_recent_actions(parse(arguments)?)?),
            ToolName::GetMemory => encode(memory::get_memory(
                sources,
                self.config.now,
                parse(arguments)?,
            )?),
        }
    }
}

/// A small planner fixture (an export bundle) for tests, benchmarks, and the tool server: the week
/// of Monday 2026-11-02 in America/Chicago.
pub const SAMPLE_FIXTURE: &str = include_str!("tools/fixtures/sample.v1.json");

/// Opens a new database at `path` holding exactly the records of an export bundle. Never point
/// this at a live planner database: the import replaces everything in it.
pub fn open_fixture_database(path: &Path, bundle_json: &str) -> AppResult<PlannerDatabase> {
    if path.exists() {
        return Err(crate::error::AppError::Validation(
            "A fixture database must be created at a new path.".into(),
        ));
    }
    let bundle = parse_export_bundle(bundle_json)?;
    let mut database = PlannerDatabase::open(path)?;
    database.import_bundle(&bundle)?;
    Ok(database)
}

/// Reads arguments into a tool's input type. A missing or null argument object is read as `{}`.
fn parse<T: DeserializeOwned>(arguments: Value) -> Result<T, ToolError> {
    let arguments = match arguments {
        Value::Null => Value::Object(Default::default()),
        Value::Object(_) => arguments,
        _ => return Err(ToolError::invalid("Arguments must be a JSON object.")),
    };
    serde_json::from_value(arguments)
        .map_err(|error| ToolError::invalid(format!("Arguments don't match the schema: {error}")))
}

fn encode<T: Serialize>(value: T) -> Result<Value, ToolError> {
    serde_json::to_value(value)
        .map_err(|_| ToolError::new(ToolErrorCode::Internal, "The result could not be encoded."))
}

#[cfg(test)]
mod tests;
