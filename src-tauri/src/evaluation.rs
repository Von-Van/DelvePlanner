//! Read-only access to benchmark and regression results for the Model & Evaluation panel.
//!
//! Three sources feed the panel, and none of them is ever written here:
//!
//! * **Benchmark runs**: folders written by `python -m agent_bench run --out …` (a `summary.json`,
//!   a `results.jsonl` and a `traces/` folder), copied into the app's results folder.
//! * **Regression runs**: JSON reports written by `npm run eval` (the frozen 126-case `eval/` track),
//!   copied into the same folder.
//! * **Recorded runs**: experiment and agent runs the app saved in its own database. The database is
//!   opened with a separate read-only connection, so nothing here can change planner data.
//!
//! Nothing in this module runs a benchmark or calls a model. Safety failures are always reported as
//! their own count and never folded into the success rate.

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::error::CommandError;

/// Folder inside the app's data folder that the panel reads result files from.
pub const RESULTS_FOLDER: &str = "evaluation-results";
/// The planner database file name, kept from the app's former name so existing installs open.
const DATABASE_FILE: &str = "dayplan.sqlite3";
/// A result file larger than this is skipped rather than read into memory.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Trace kinds the benchmark harness records itself. The agent's own copies of these are left out of
/// a merged trace, because the harness's record is what actually happened.
const HARNESS_KINDS: [&str; 4] = ["tool_call", "tool_result", "draft", "final"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSource {
    Benchmark,
    Regression,
    Recorded,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationRun {
    pub id: String,
    pub source: RunSource,
    pub label: String,
    pub architecture: Option<String>,
    pub model_name: Option<String>,
    pub model_digest: Option<String>,
    pub started_at: Option<String>,
    pub hardware: Option<String>,
    /// Distinct cases in the run.
    pub cases: u32,
    /// How many times each case was run.
    pub repeats: u32,
    /// Case runs in total (cases × repeats, when every repeat finished).
    pub attempts: u32,
    pub passed_attempts: u32,
    pub success_rate: Option<f64>,
    /// Case runs that broke a safety rule. Reported on its own, never blended into the success rate.
    pub safety_failures: u32,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedCheck {
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyViolation {
    pub rule: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationCase {
    /// Identifies this case run within its run; pass it back to load the trace.
    pub key: String,
    pub case_id: String,
    pub family: Option<String>,
    pub split: Option<String>,
    pub tags: Vec<String>,
    pub repeat: u32,
    pub status: Option<String>,
    pub success: bool,
    pub safe: bool,
    pub failed_checks: Vec<FailedCheck>,
    pub safety_violations: Vec<SafetyViolation>,
    pub latency_ms: Option<f64>,
    pub has_trace: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationRunDetail {
    pub run: EvaluationRun,
    pub cases: Vec<EvaluationCase>,
    /// Shown above the case list when the source cannot list every case.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationOverview {
    pub results_folder: String,
    pub runs: Vec<EvaluationRun>,
    /// Files that could not be read, so a broken file is reported instead of silently missing.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationTrace {
    pub events: Vec<Value>,
    pub status: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationError {
    #[error("That result could not be found. It may have been moved or deleted.")]
    NotFound,
    #[error("That result file could not be read: {0}")]
    Unreadable(String),
}

impl From<EvaluationError> for CommandError {
    fn from(error: EvaluationError) -> Self {
        CommandError::internal(error.to_string())
    }
}

/// Where the panel's inputs live for a given app data folder.
#[derive(Debug, Clone)]
pub struct Sources {
    pub results: PathBuf,
    pub database: PathBuf,
}

impl Sources {
    pub fn in_app_data(directory: &Path) -> Self {
        Self {
            results: directory.join(RESULTS_FOLDER),
            database: directory.join(DATABASE_FILE),
        }
    }

    fn for_app(app: &AppHandle) -> Result<Self, CommandError> {
        let directory = app
            .path()
            .app_data_dir()
            .map_err(|_| CommandError::internal("The app's data folder could not be found."))?;
        Ok(Self::in_app_data(&directory))
    }
}

// -------------------------------------------------------------------------------------------------
// Commands

#[tauri::command]
pub fn list_evaluation_runs(app: AppHandle) -> Result<EvaluationOverview, CommandError> {
    Ok(overview(&Sources::for_app(&app)?))
}

#[tauri::command]
pub fn get_evaluation_run(
    app: AppHandle,
    run_id: String,
) -> Result<EvaluationRunDetail, CommandError> {
    Ok(run_detail(&Sources::for_app(&app)?, &run_id)?)
}

#[tauri::command]
pub fn get_evaluation_trace(
    app: AppHandle,
    run_id: String,
    case_key: String,
) -> Result<EvaluationTrace, CommandError> {
    Ok(case_trace(&Sources::for_app(&app)?, &run_id, &case_key)?)
}

// -------------------------------------------------------------------------------------------------
// Listing

pub fn overview(sources: &Sources) -> EvaluationOverview {
    let mut runs = Vec::new();
    let mut problems = Vec::new();
    for entry in result_entries(&sources.results) {
        let name = entry.name.clone();
        let loaded = if entry.is_dir {
            if !entry.path.join("summary.json").is_file() {
                continue;
            }
            load_benchmark(&entry.path, &name).map(|(run, _)| run)
        } else if name.ends_with(".json") {
            match load_regression(&entry.path, &name) {
                Ok(Some((run, _))) => Ok(run),
                Ok(None) => continue,
                Err(error) => Err(error),
            }
        } else {
            continue;
        };
        match loaded {
            Ok(run) => runs.push(run),
            Err(error) => problems.push(format!("{name}: {error}")),
        }
    }
    match recorded_runs(&sources.database) {
        Ok(recorded) => runs.extend(recorded),
        Err(error) => problems.push(format!("Saved runs: {error}")),
    }
    // Newest first; runs without a date go last.
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    EvaluationOverview {
        results_folder: sources.results.display().to_string(),
        runs,
        problems,
    }
}

pub fn run_detail(sources: &Sources, run_id: &str) -> Result<EvaluationRunDetail, EvaluationError> {
    let (source, name) = split_id(run_id)?;
    match source {
        RunSource::Benchmark => {
            let path = find_entry(&sources.results, name, true)?;
            let (run, cases) = load_benchmark(&path, name)?;
            Ok(EvaluationRunDetail {
                run,
                cases,
                note: None,
            })
        }
        RunSource::Regression => {
            let path = find_entry(&sources.results, name, false)?;
            let (run, cases) = load_regression(&path, name)?.ok_or(EvaluationError::NotFound)?;
            let failed: HashSet<&str> = cases.iter().map(|c| c.case_id.as_str()).collect();
            let note = Some(format!(
                "Regression reports list only the cases that failed. {} of {} cases passed every repeat.",
                run.cases as usize - failed.len().min(run.cases as usize),
                run.cases
            ));
            Ok(EvaluationRunDetail { run, cases, note })
        }
        RunSource::Recorded => recorded_detail(&sources.database, name),
    }
}

pub fn case_trace(
    sources: &Sources,
    run_id: &str,
    case_key: &str,
) -> Result<EvaluationTrace, EvaluationError> {
    let (source, name) = split_id(run_id)?;
    match source {
        RunSource::Benchmark => {
            let path = find_entry(&sources.results, name, true)?;
            let (_, cases) = load_benchmark(&path, name)?;
            let case = cases
                .iter()
                .find(|c| c.key == case_key)
                .ok_or(EvaluationError::NotFound)?;
            let file = path
                .join("traces")
                .join(format!("{}.r{}.json", case.case_id, case.repeat));
            if !file.is_file() {
                return Ok(EvaluationTrace {
                    events: Vec::new(),
                    status: case.status.clone(),
                });
            }
            let value = read_json(&file)?;
            Ok(EvaluationTrace {
                events: merge_traces(&value),
                status: case.status.clone(),
            })
        }
        // Regression reports keep no traces.
        RunSource::Regression => Ok(EvaluationTrace {
            events: Vec::new(),
            status: None,
        }),
        RunSource::Recorded => recorded_trace(&sources.database, name, case_key),
    }
}

// -------------------------------------------------------------------------------------------------
// Files

struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

fn result_entries(folder: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            // Follow no links: only real files and folders inside the results folder count.
            let kind = entry.file_type().ok()?;
            if kind.is_symlink() {
                return None;
            }
            Some(Entry {
                name,
                path: entry.path(),
                is_dir: kind.is_dir(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Resolves a run name to a path by listing the folder, so a crafted id can never reach outside it.
fn find_entry(folder: &Path, name: &str, is_dir: bool) -> Result<PathBuf, EvaluationError> {
    result_entries(folder)
        .into_iter()
        .find(|entry| entry.name == name && entry.is_dir == is_dir)
        .map(|entry| entry.path)
        .ok_or(EvaluationError::NotFound)
}

fn split_id(run_id: &str) -> Result<(RunSource, &str), EvaluationError> {
    let (prefix, name) = run_id.split_once(':').ok_or(EvaluationError::NotFound)?;
    let source = match prefix {
        "benchmark" => RunSource::Benchmark,
        "regression" => RunSource::Regression,
        "recorded" => RunSource::Recorded,
        _ => return Err(EvaluationError::NotFound),
    };
    if name.is_empty() {
        return Err(EvaluationError::NotFound);
    }
    Ok((source, name))
}

fn read_text(path: &Path) -> Result<String, EvaluationError> {
    let size = fs::metadata(path)
        .map_err(|e| EvaluationError::Unreadable(e.to_string()))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(EvaluationError::Unreadable("the file is too large".into()));
    }
    fs::read_to_string(path).map_err(|e| EvaluationError::Unreadable(e.to_string()))
}

fn read_json(path: &Path) -> Result<Value, EvaluationError> {
    serde_json::from_str(&read_text(path)?).map_err(|e| EvaluationError::Unreadable(e.to_string()))
}

fn str_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn f64_of(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn u32_of(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

/// A hardware description from either a plain string or an object of named parts.
fn hardware_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(parts) => {
            let text = parts
                .iter()
                .filter_map(|(key, value)| match value {
                    Value::String(s) => Some(format!("{key}: {s}")),
                    Value::Number(n) => Some(format!("{key}: {n}")),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(", ");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// `(name, digest)` from a model description that is either a string or an object.
fn model_of(value: Option<&Value>) -> (Option<String>, Option<String>) {
    match value {
        Some(Value::String(name)) => (Some(name.clone()), None),
        Some(object @ Value::Object(_)) => (
            str_of(object, "name")
                .or_else(|| str_of(object, "model"))
                .or_else(|| str_of(object, "modelName")),
            str_of(object, "digest").or_else(|| str_of(object, "modelDigest")),
        ),
        _ => (None, None),
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() as f64) * fraction) as usize;
    Some(sorted[index.min(sorted.len() - 1)])
}

/// An `agent_bench` run folder: run metadata from `summary.json`, one case run per line of
/// `results.jsonl`.
fn load_benchmark(
    folder: &Path,
    name: &str,
) -> Result<(EvaluationRun, Vec<EvaluationCase>), EvaluationError> {
    let summary = read_json(&folder.join("summary.json"))?;
    let meta = summary.get("meta").cloned().unwrap_or(Value::Null);
    let overall = summary
        .get("summary")
        .and_then(|s| s.get("overall"))
        .cloned()
        .unwrap_or(Value::Null);

    let mut cases = Vec::new();
    let results = folder.join("results.jsonl");
    if results.is_file() {
        for (line_number, line) in read_text(&results)?.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row: Value = serde_json::from_str(line).map_err(|e| {
                EvaluationError::Unreadable(format!("results.jsonl line {}: {e}", line_number + 1))
            })?;
            cases.push(benchmark_case(&row, folder));
        }
    }

    let distinct: HashSet<&str> = cases.iter().map(|c| c.case_id.as_str()).collect();
    let passed = cases.iter().filter(|c| c.success).count() as u32;
    let unsafe_runs = cases.iter().filter(|c| !c.safe).count() as u32;
    let mut latencies: Vec<f64> = cases.iter().filter_map(|c| c.latency_ms).collect();
    latencies.sort_by(|a, b| a.total_cmp(b));
    let attempts = if cases.is_empty() {
        u32_of(&overall, "runs")
    } else {
        cases.len() as u32
    };
    let (model_name, model_digest) = model_of(meta.get("model"));
    let architecture = str_of(&meta, "architectureId");
    let run = EvaluationRun {
        id: format!("benchmark:{name}"),
        source: RunSource::Benchmark,
        label: str_of(&meta, "agent")
            .filter(|agent| !agent.is_empty())
            .unwrap_or_else(|| name.to_string()),
        architecture,
        model_name,
        model_digest,
        started_at: str_of(&meta, "startedAt"),
        hardware: hardware_of(meta.get("hardware")).or_else(|| str_of(&meta, "platform")),
        cases: if distinct.is_empty() {
            u32_of(&meta, "cases")
        } else {
            distinct.len() as u32
        },
        repeats: u32_of(&meta, "repeats").max(1),
        attempts,
        passed_attempts: passed,
        success_rate: if cases.is_empty() {
            f64_of(&overall, "successRate")
        } else {
            Some(passed as f64 / cases.len() as f64)
        },
        safety_failures: if cases.is_empty() {
            u32_of(&overall, "unsafeRuns")
        } else {
            unsafe_runs
        },
        latency_p50_ms: percentile(&latencies, 0.5).or_else(|| f64_of(&overall, "wallMsP50")),
        latency_p95_ms: percentile(&latencies, 0.95).or_else(|| f64_of(&overall, "wallMsP95")),
    };
    Ok((run, cases))
}

fn benchmark_case(row: &Value, folder: &Path) -> EvaluationCase {
    let case_id = str_of(row, "caseId").unwrap_or_else(|| "unknown".into());
    let repeat = u32_of(row, "repeat");
    let failed_checks = row
        .get("checks")
        .and_then(Value::as_object)
        .map(|checks| {
            checks
                .iter()
                .filter(|(_, check)| check.get("passed").and_then(Value::as_bool) == Some(false))
                .map(|(name, check)| FailedCheck {
                    name: name.clone(),
                    detail: str_of(check, "detail").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let safety_violations: Vec<SafetyViolation> = row
        .get("safety")
        .and_then(|s| s.get("violations"))
        .and_then(Value::as_array)
        .map(|violations| {
            violations
                .iter()
                .map(|v| SafetyViolation {
                    rule: str_of(v, "rule").unwrap_or_else(|| "safety".into()),
                    detail: str_of(v, "detail").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let safe = row
        .get("safe")
        .and_then(Value::as_bool)
        .unwrap_or(safety_violations.is_empty());
    let tags = row
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let has_trace = folder
        .join("traces")
        .join(format!("{case_id}.r{repeat}.json"))
        .is_file();
    EvaluationCase {
        key: format!("{case_id}#{repeat}"),
        family: str_of(row, "family"),
        split: str_of(row, "split"),
        tags,
        repeat,
        status: str_of(row, "status"),
        success: row.get("success").and_then(Value::as_bool).unwrap_or(false),
        safe,
        failed_checks,
        safety_violations,
        latency_ms: row.get("metrics").and_then(|m| f64_of(m, "wallMs")),
        has_trace,
        case_id,
    }
}

/// A report from `npm run eval`. Returns `None` for JSON files that aren't one.
fn load_regression(
    path: &Path,
    name: &str,
) -> Result<Option<(EvaluationRun, Vec<EvaluationCase>)>, EvaluationError> {
    let report = read_json(path)?;
    let Some(runs) = report.get("runs").and_then(Value::as_array) else {
        return Ok(None);
    };
    if report.get("fixtureCount").is_none() {
        return Ok(None);
    }
    let fixtures = u32_of(&report, "fixtureCount");
    let mut exact = 0u32;
    let mut safety_failures = 0u32;
    let mut cases = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let repeat = run
            .get("run")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(index as u32 + 1);
        exact += u32_of(run, "exact");
        safety_failures += u32_of(run, "safetyTotal").saturating_sub(u32_of(run, "safetyExact"));
        for failure in run
            .get("failures")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(text) = failure.as_str() else {
                continue;
            };
            let (case_id, detail) = text
                .split_once(": ")
                .map(|(id, detail)| (id.trim().to_string(), detail.trim().to_string()))
                .unwrap_or_else(|| ("unknown".into(), text.to_string()));
            cases.push(EvaluationCase {
                key: format!("{case_id}#{repeat}"),
                case_id,
                family: None,
                split: None,
                tags: Vec::new(),
                repeat,
                status: None,
                success: false,
                safe: true,
                failed_checks: vec![FailedCheck {
                    name: "expected result".into(),
                    detail,
                }],
                safety_violations: Vec::new(),
                latency_ms: None,
                has_trace: false,
            });
        }
    }
    let attempts = fixtures * runs.len() as u32;
    let run = EvaluationRun {
        id: format!("regression:{name}"),
        source: RunSource::Regression,
        label: format!("Regression set ({fixtures} requests)"),
        // The regression track exercises the existing one-shot planner, baseline A0.
        architecture: str_of(&report, "architectureId").or_else(|| Some("A0".into())),
        model_name: str_of(&report, "modelName"),
        model_digest: str_of(&report, "modelDigest"),
        started_at: str_of(&report, "generatedAt"),
        hardware: hardware_of(report.get("hardware")),
        cases: fixtures,
        repeats: runs.len() as u32,
        attempts,
        passed_attempts: exact,
        success_rate: (attempts > 0).then(|| exact as f64 / attempts as f64),
        safety_failures,
        latency_p50_ms: f64_of(&report, "latencyP50Ms"),
        latency_p95_ms: f64_of(&report, "latencyP95Ms"),
    };
    Ok(Some((run, cases)))
}

/// One run's trace: the harness's record of what happened, plus the agent's own reasoning steps
/// (model calls, plans, checks) that the harness can't see. Steps are ordered by time and renumbered.
fn merge_traces(value: &Value) -> Vec<Value> {
    let mut events: Vec<Value> = value
        .get("trace")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(agent) = value.get("agentTrace").and_then(Value::as_array) {
        events.extend(
            agent
                .iter()
                .filter(|event| {
                    event
                        .get("kind")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| !HARNESS_KINDS.contains(&kind))
                })
                .cloned(),
        );
    }
    events.sort_by(|a, b| {
        let at = |e: &Value| {
            e.get("at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        at(a).cmp(&at(b))
    });
    for (index, event) in events.iter_mut().enumerate() {
        if let Value::Object(map) = event {
            map.insert("seq".into(), Value::from(index as u64 + 1));
        }
    }
    events
}

// -------------------------------------------------------------------------------------------------
// Runs saved in the app's database (read-only)

fn open_read_only(path: &Path) -> Result<Option<Connection>, EvaluationError> {
    if !path.is_file() {
        return Ok(None);
    }
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map(Some)
    .map_err(|e| EvaluationError::Unreadable(e.to_string()))
}

fn columns(connection: &Connection, table: &str) -> HashSet<String> {
    let Ok(mut statement) = connection.prepare(&format!("PRAGMA table_info({table})")) else {
        return HashSet::new();
    };
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// `column` when the table has it, otherwise `NULL`, so an older or newer table shape still reads.
fn pick(available: &HashSet<String>, candidates: &[&str], alias: &str) -> String {
    candidates
        .iter()
        .find(|c| available.contains(**c))
        .map(|c| format!("{c} AS {alias}"))
        .unwrap_or_else(|| format!("NULL AS {alias}"))
}

fn db_error(error: rusqlite::Error) -> EvaluationError {
    EvaluationError::Unreadable(error.to_string())
}

struct AgentRunRow {
    id: String,
    experiment: Option<String>,
    case_id: Option<String>,
    family: Option<String>,
    tags: Option<String>,
    status: Option<String>,
    success: Option<bool>,
    /// True when the run broke a safety rule.
    safety_failure: Option<bool>,
    latency_ms: Option<f64>,
    architecture: Option<String>,
    model_name: Option<String>,
    model_digest: Option<String>,
    started_at: Option<String>,
    failure: Option<String>,
}

fn agent_runs(
    connection: &Connection,
    experiment: Option<&str>,
) -> Result<Vec<AgentRunRow>, EvaluationError> {
    let available = columns(connection, "agent_run");
    if available.is_empty() {
        return Ok(Vec::new());
    }
    let select = [
        pick(&available, &["id"], "id"),
        pick(
            &available,
            &["experiment_run_id", "experiment_id"],
            "experiment",
        ),
        pick(&available, &["case_id"], "case_id"),
        pick(&available, &["family", "case_family"], "family"),
        pick(&available, &["tags", "tags_json"], "tags"),
        pick(&available, &["status", "final_status"], "status"),
        pick(&available, &["passed", "success"], "success"),
        pick(&available, &["safety_failure"], "safety_failure"),
        pick(&available, &["latency_ms", "duration_ms"], "latency_ms"),
        pick(
            &available,
            &["architecture_id", "architecture"],
            "architecture",
        ),
        pick(&available, &["model_name", "model"], "model_name"),
        pick(&available, &["model_digest"], "model_digest"),
        pick(&available, &["started_at", "created_at"], "started_at"),
        pick(
            &available,
            &["failure_reason", "failure", "error"],
            "failure",
        ),
    ]
    .join(", ");
    let filter = match experiment {
        Some(_) if available.contains("experiment_run_id") => " WHERE experiment_run_id = ?1",
        Some(_) if available.contains("experiment_id") => " WHERE experiment_id = ?1",
        Some(_) => return Ok(Vec::new()),
        None => "",
    };
    let sql = format!("SELECT {select} FROM agent_run{filter} ORDER BY started_at, id");
    let mut statement = connection.prepare(&sql).map_err(db_error)?;
    let map = |row: &rusqlite::Row<'_>| {
        Ok(AgentRunRow {
            id: value_text(row.get_ref(0)?).unwrap_or_default(),
            experiment: value_text(row.get_ref(1)?),
            case_id: value_text(row.get_ref(2)?),
            family: value_text(row.get_ref(3)?),
            tags: value_text(row.get_ref(4)?),
            status: value_text(row.get_ref(5)?),
            success: value_bool(row.get_ref(6)?),
            safety_failure: value_bool(row.get_ref(7)?),
            latency_ms: value_f64(row.get_ref(8)?),
            architecture: value_text(row.get_ref(9)?),
            model_name: value_text(row.get_ref(10)?),
            model_digest: value_text(row.get_ref(11)?),
            started_at: value_text(row.get_ref(12)?),
            failure: value_text(row.get_ref(13)?),
        })
    };
    let rows = match experiment {
        Some(id) => statement.query_map([id], map),
        None => statement.query_map([], map),
    }
    .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn value_text(value: rusqlite::types::ValueRef<'_>) -> Option<String> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Text(text) => Some(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Integer(n) => Some(n.to_string()),
        ValueRef::Real(n) => Some(n.to_string()),
        _ => None,
    }
}

fn value_f64(value: rusqlite::types::ValueRef<'_>) -> Option<f64> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Integer(n) => Some(n as f64),
        ValueRef::Real(n) => Some(n),
        ValueRef::Text(text) => String::from_utf8_lossy(text).parse().ok(),
        _ => None,
    }
}

fn value_bool(value: rusqlite::types::ValueRef<'_>) -> Option<bool> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Integer(n) => Some(n != 0),
        ValueRef::Text(text) => match &*String::from_utf8_lossy(text) {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A saved run counts as a success only when it was scored and passed; unscored runs never do.
fn row_success(row: &AgentRunRow) -> bool {
    row.success == Some(true)
}

fn summarize_rows(id: String, label: String, rows: &[AgentRunRow]) -> EvaluationRun {
    let distinct: HashSet<&str> = rows
        .iter()
        .map(|r| r.case_id.as_deref().unwrap_or(r.id.as_str()))
        .collect();
    let passed = rows.iter().filter(|r| row_success(r)).count() as u32;
    let mut latencies: Vec<f64> = rows.iter().filter_map(|r| r.latency_ms).collect();
    latencies.sort_by(|a, b| a.total_cmp(b));
    let first = rows.first();
    EvaluationRun {
        id,
        source: RunSource::Recorded,
        label,
        architecture: first.and_then(|r| r.architecture.clone()),
        model_name: first.and_then(|r| r.model_name.clone()),
        model_digest: first.and_then(|r| r.model_digest.clone()),
        started_at: first.and_then(|r| r.started_at.clone()),
        hardware: None,
        cases: distinct.len() as u32,
        repeats: if distinct.is_empty() {
            0
        } else {
            (rows.len() / distinct.len()).max(1) as u32
        },
        attempts: rows.len() as u32,
        passed_attempts: passed,
        success_rate: (!rows.is_empty()).then(|| passed as f64 / rows.len() as f64),
        safety_failures: rows
            .iter()
            .filter(|r| r.safety_failure == Some(true))
            .count() as u32,
        latency_p50_ms: percentile(&latencies, 0.5),
        latency_p95_ms: percentile(&latencies, 0.95),
    }
}

/// Fills the run fields an `experiment_run` row records over what its agent runs say.
fn apply_experiment(run: &mut EvaluationRun, experiment: &Map<String, Value>) {
    let text = |key: &str| {
        experiment
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    if let Some(value) = text("architecture") {
        run.architecture = Some(value);
    }
    if let Some(value) = text("model_name") {
        run.model_name = Some(value);
    }
    if let Some(value) = text("model_digest") {
        run.model_digest = Some(value);
    }
    if let Some(value) = text("started_at") {
        run.started_at = Some(value);
    }
    if let Some(value) = text("hardware") {
        run.hardware = serde_json::from_str::<Value>(&value)
            .ok()
            .and_then(|parsed| hardware_of(Some(&parsed)))
            .or(Some(value));
    }
    match (text("kind"), text("version")) {
        (Some(kind), Some(version)) => run.label = format!("Saved {kind} run ({version})"),
        (Some(kind), None) => run.label = format!("Saved {kind} run"),
        _ => {}
    }
}

fn experiments(connection: &Connection) -> Result<Vec<Map<String, Value>>, EvaluationError> {
    let available = columns(connection, "experiment_run");
    if available.is_empty() {
        return Ok(Vec::new());
    }
    let fields: [(&[&str], &str); 8] = [
        (&["id"], "id"),
        (&["kind"], "kind"),
        (&["benchmark_version", "config_hash"], "version"),
        (&["architecture_id", "architecture"], "architecture"),
        (&["model_name", "model"], "model_name"),
        (&["model_digest"], "model_digest"),
        (&["started_at", "created_at"], "started_at"),
        (&["hardware", "hardware_json"], "hardware"),
    ];
    let select = fields
        .iter()
        .map(|(candidates, alias)| pick(&available, candidates, alias))
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection
        .prepare(&format!("SELECT {select} FROM experiment_run"))
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |row| {
            let mut map = Map::new();
            for (index, (_, alias)) in fields.iter().enumerate() {
                if let Some(text) = value_text(row.get_ref(index)?) {
                    map.insert((*alias).to_string(), Value::String(text));
                }
            }
            Ok(map)
        })
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

/// One entry per saved experiment, plus one for assistant runs made outside any experiment.
fn recorded_runs(database: &Path) -> Result<Vec<EvaluationRun>, EvaluationError> {
    let Some(connection) = open_read_only(database)? else {
        return Ok(Vec::new());
    };
    let mut runs = Vec::new();
    for experiment in experiments(&connection)? {
        let Some(id) = experiment.get("id").and_then(Value::as_str) else {
            continue;
        };
        let rows = agent_runs(&connection, Some(id))?;
        let mut run = summarize_rows(format!("recorded:{id}"), format!("Experiment {id}"), &rows);
        apply_experiment(&mut run, &experiment);
        runs.push(run);
    }
    let loose: Vec<AgentRunRow> = agent_runs(&connection, None)?
        .into_iter()
        .filter(|r| r.experiment.is_none())
        .collect();
    if !loose.is_empty() {
        let mut run = summarize_rows(
            "recorded:assistant".into(),
            "Assistant runs in this app".into(),
            &loose,
        );
        run.started_at = loose.iter().filter_map(|r| r.started_at.clone()).max();
        runs.push(run);
    }
    Ok(runs)
}

fn recorded_rows(
    connection: &Connection,
    name: &str,
) -> Result<(EvaluationRun, Vec<AgentRunRow>), EvaluationError> {
    if name == "assistant" {
        let rows: Vec<AgentRunRow> = agent_runs(connection, None)?
            .into_iter()
            .filter(|r| r.experiment.is_none())
            .collect();
        let run = summarize_rows(
            "recorded:assistant".into(),
            "Assistant runs in this app".into(),
            &rows,
        );
        return Ok((run, rows));
    }
    let experiment = experiments(connection)?
        .into_iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(name))
        .ok_or(EvaluationError::NotFound)?;
    let rows = agent_runs(connection, Some(name))?;
    let mut run = summarize_rows(
        format!("recorded:{name}"),
        format!("Experiment {name}"),
        &rows,
    );
    apply_experiment(&mut run, &experiment);
    Ok((run, rows))
}

fn recorded_detail(database: &Path, name: &str) -> Result<EvaluationRunDetail, EvaluationError> {
    let connection = open_read_only(database)?.ok_or(EvaluationError::NotFound)?;
    let (run, rows) = recorded_rows(&connection, name)?;
    let has_traces = !columns(&connection, "agent_step").is_empty();
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let cases = rows
        .iter()
        .map(|row| {
            let case_id = row.case_id.clone().unwrap_or_else(|| row.id.clone());
            let repeat = seen.entry(case_id.clone()).or_insert(0);
            let this_repeat = *repeat;
            *repeat += 1;
            let success = row_success(row);
            EvaluationCase {
                key: row.id.clone(),
                family: row.family.clone(),
                split: None,
                tags: row
                    .tags
                    .as_deref()
                    .and_then(|t| serde_json::from_str::<Vec<String>>(t).ok())
                    .unwrap_or_default(),
                repeat: this_repeat,
                status: row.status.clone(),
                success,
                safe: row.safety_failure != Some(true),
                failed_checks: match (&row.failure, success) {
                    (Some(detail), false) => vec![FailedCheck {
                        name: "result".into(),
                        detail: detail.clone(),
                    }],
                    _ => Vec::new(),
                },
                safety_violations: Vec::new(),
                latency_ms: row.latency_ms,
                has_trace: has_traces,
                case_id,
            }
        })
        .collect();
    Ok(EvaluationRunDetail {
        run,
        cases,
        note: None,
    })
}

fn recorded_trace(
    database: &Path,
    name: &str,
    case_key: &str,
) -> Result<EvaluationTrace, EvaluationError> {
    let connection = open_read_only(database)?.ok_or(EvaluationError::NotFound)?;
    let (_, rows) = recorded_rows(&connection, name)?;
    let row = rows
        .iter()
        .find(|r| r.id == case_key)
        .ok_or(EvaluationError::NotFound)?;
    let available = columns(&connection, "agent_step");
    if available.is_empty() {
        return Ok(EvaluationTrace {
            events: Vec::new(),
            status: row.status.clone(),
        });
    }
    let select = [
        pick(&available, &["run_id", "agent_run_id"], "run_id"),
        pick(&available, &["seq"], "seq"),
        pick(&available, &["kind"], "kind"),
        pick(&available, &["at", "created_at"], "at"),
        pick(&available, &["duration_ms"], "duration_ms"),
        pick(&available, &["summary"], "summary"),
        pick(&available, &["data", "data_json"], "data"),
    ]
    .join(", ");
    let run_column = if available.contains("run_id") {
        "run_id"
    } else if available.contains("agent_run_id") {
        "agent_run_id"
    } else {
        return Ok(EvaluationTrace {
            events: Vec::new(),
            status: row.status.clone(),
        });
    };
    let mut statement = connection
        .prepare(&format!(
            "SELECT {select} FROM agent_step WHERE {run_column} = ?1 ORDER BY seq"
        ))
        .map_err(db_error)?;
    let events = statement
        .query_map([&row.id], |r| {
            let mut event = Map::new();
            event.insert(
                "runId".into(),
                Value::String(value_text(r.get_ref(0)?).unwrap_or_default()),
            );
            event.insert(
                "seq".into(),
                Value::from(value_f64(r.get_ref(1)?).unwrap_or(0.0) as u64),
            );
            event.insert(
                "kind".into(),
                Value::String(value_text(r.get_ref(2)?).unwrap_or_else(|| "error".into())),
            );
            event.insert(
                "at".into(),
                Value::String(value_text(r.get_ref(3)?).unwrap_or_default()),
            );
            if let Some(ms) = value_f64(r.get_ref(4)?) {
                event.insert("durationMs".into(), Value::from(ms));
            }
            event.insert(
                "summary".into(),
                Value::String(value_text(r.get_ref(5)?).unwrap_or_default()),
            );
            let data = value_text(r.get_ref(6)?)
                .and_then(|d| serde_json::from_str::<Value>(&d).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| Value::Object(Map::new()));
            event.insert("data".into(), data);
            Ok(Value::Object(event))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(EvaluationTrace {
        events,
        status: row.status.clone(),
    })
}

#[cfg(test)]
mod tests;
