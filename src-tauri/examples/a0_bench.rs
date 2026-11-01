//! Runs the existing planner (architecture A0) over `agent_bench` cases, for the A0-vs-A1
//! comparison in `docs/research/experiments/`.
//!
//! A0 has no tools: the app hands it a snapshot of relevant records and it answers with one
//! proposal or one clarifying question. This example gives it exactly that. For each case it seeds
//! a throwaway database from the case's starting planner state, asks the planner the case's goal
//! the way the desktop app does (`propose_schedule_changes` in `lib.rs`), and records the reply
//! with every database id translated back to the fixture's ids so `agent_bench` can score it.
//!
//! It never touches the app's own database, and it only reads the model the app already
//! downloaded (the same arrangement as `eval_agent`).
//!
//! ```sh
//! python -m agent_bench.configs.experiment a0-input --out a0-input.json
//! cargo run --release --manifest-path src-tauri/Cargo.toml --example a0_bench -- \
//!   --input a0-input.json --runs 3 --output a0-output.json
//! ```
//!
//! `--endpoint http://127.0.0.1:PORT` uses an Ollama that is already running instead of starting
//! the bundled runtime.

use chrono::{Days, NaiveDate, SecondsFormat, Utc};
use delve_planner_desktop::agent::{asks_to_choose, PlannerAgent, PlannerRequest, MODEL_NAME};
use delve_planner_desktop::db::{CandidateRequest, PlannerDatabase};
use delve_planner_desktop::model::{
    CreateEventInput, CreateMilestoneInput, CreatePlanInput, CreateTaskBlockInput, CreateTaskInput,
    CreateWorkstreamInput, ExternalEvent, MilestoneStatus, PlanStatus, PlannerResponse,
    PlanningFacts, TaskPriority, TaskStatus, UpdatePlanInput, UpdatePlanningProfileInput,
};
use delve_planner_desktop::runtime::OllamaRuntime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tempfile::tempdir;

/// How many coming days the planner sees spare time for, as in `lib.rs`.
const PLANNING_FACT_DAYS: u32 = 14;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputFile {
    #[serde(default)]
    case_set_hash: Option<String>,
    cases: Vec<BenchCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BenchCase {
    case_id: String,
    goal: String,
    /// The local date of the case's frozen `now`, which the app passes as the selected day.
    day: String,
    time_zone: String,
    state: State,
}

/// The starting planner state, in `agent_bench/fixtures/planner.v1.json`'s shape.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(default)]
    working_hours: Option<WorkingHours>,
    #[serde(default)]
    plans: Vec<Value>,
    #[serde(default)]
    milestones: Vec<Value>,
    #[serde(default)]
    workstreams: Vec<Value>,
    #[serde(default)]
    tasks: Vec<Value>,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    task_blocks: Vec<Value>,
    #[serde(default)]
    external_busy: Vec<Value>,
}

#[derive(Deserialize)]
struct WorkingHours {
    start: String,
    end: String,
    /// ISO weekdays, Monday = 1.
    days: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputFile {
    architecture_id: &'static str,
    generated_at: String,
    model_name: String,
    model_digest: Option<String>,
    ollama_version: Option<String>,
    case_set_hash: Option<String>,
    runs: Vec<CaseRun>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseRun {
    case_id: String,
    repeat: usize,
    latency_ms: u128,
    /// `proposal`, `clarification`, or `error`.
    kind: &'static str,
    summary: Option<String>,
    question: Option<String>,
    /// Each operation is a `MutationOperation` (`{"type": …, …}`) with fixture ids.
    operations: Vec<Value>,
    error: Option<String>,
}

struct Options {
    input: PathBuf,
    output: PathBuf,
    runs: usize,
    endpoint: Option<String>,
    only: Vec<String>,
}

#[tokio::main]
async fn main() {
    let options = options().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2)
    });
    let contents = fs::read_to_string(&options.input).unwrap_or_else(|error| {
        eprintln!("Could not read {}: {error}", options.input.display());
        std::process::exit(2)
    });
    let mut input: InputFile = serde_json::from_str(&contents).unwrap_or_else(|error| {
        eprintln!("Invalid input in {}: {error}", options.input.display());
        std::process::exit(2)
    });
    if !options.only.is_empty() {
        input
            .cases
            .retain(|case| options.only.iter().any(|id| id == &case.case_id));
    }
    if input.cases.is_empty() {
        eprintln!("No cases to run.");
        std::process::exit(2);
    }

    // Kept alive for the whole run: dropping the runtime stops the model server it started.
    let mut _runtime = None;
    let endpoint = match &options.endpoint {
        Some(endpoint) => endpoint.clone(),
        None => {
            point_at_installed_models();
            let runtime =
                OllamaRuntime::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), data_directory())
                    .unwrap_or_else(|error| {
                        eprintln!("Cannot initialize Delve Planner's bundled runtime: {error}");
                        std::process::exit(2)
                    });
            let endpoint = runtime.endpoint().to_string();
            _runtime = Some(runtime);
            endpoint
        }
    };
    let probe = PlannerAgent::new(&endpoint, MODEL_NAME);
    let status = match &_runtime {
        Some(runtime) => runtime.started_status(&probe).await,
        None => probe.status().await,
    };
    if !status.running || !status.model_installed {
        eprintln!("Cannot start: {}", status.detail);
        eprintln!("A0 runs against {MODEL_NAME}; install it in Delve Planner or Ollama first.");
        std::process::exit(2);
    }

    let mut runs = Vec::new();
    for repeat in 0..options.runs {
        for case in &input.cases {
            let run = run_case(case, repeat, &endpoint).await;
            println!(
                "{:13} r{}  {}  ({} ms)",
                run.kind, repeat, run.case_id, run.latency_ms
            );
            runs.push(run);
        }
    }
    let output = OutputFile {
        architecture_id: "A0",
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        model_name: status.model_name,
        model_digest: status.model_digest,
        ollama_version: status.ollama_version,
        case_set_hash: input.case_set_hash,
        runs,
    };
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).expect("create output directory");
    }
    fs::write(
        &options.output,
        serde_json::to_vec_pretty(&output).expect("serialize output"),
    )
    .unwrap_or_else(|error| {
        eprintln!("Could not write {}: {error}", options.output.display());
        std::process::exit(2)
    });
    println!("A0 replies: {}", options.output.display());
}

async fn run_case(case: &BenchCase, repeat: usize, endpoint: &str) -> CaseRun {
    let directory = tempdir().expect("temporary directory");
    let mut database =
        PlannerDatabase::open(&directory.path().join("a0-bench.sqlite3")).expect("database");
    let ids = seed(&mut database, case);
    let external = external_busy(case);
    // A fresh planner per run: no conversation carries over between cases or repeats.
    let agent = PlannerAgent::new(endpoint, MODEL_NAME);
    let started = Instant::now();
    let result = propose(&agent, &database, case, &external).await;
    let latency_ms = started.elapsed().as_millis();
    let mut run = CaseRun {
        case_id: case.case_id.clone(),
        repeat,
        latency_ms,
        kind: "error",
        summary: None,
        question: None,
        operations: Vec::new(),
        error: None,
    };
    match result {
        Ok(PlannerResponse::Proposal {
            summary,
            operations,
            ..
        }) => {
            run.kind = "proposal";
            run.summary = Some(summary);
            run.operations = operations
                .into_iter()
                .map(|operation| {
                    let mut value = serde_json::to_value(operation.change).expect("operation");
                    to_fixture_ids(&mut value, &ids);
                    value
                })
                .collect();
        }
        Ok(PlannerResponse::Clarification { question }) => {
            run.kind = "clarification";
            run.question = Some(question);
        }
        Err(error) => run.error = Some(error.to_string()),
    }
    run
}

async fn propose(
    agent: &PlannerAgent,
    database: &PlannerDatabase,
    case: &BenchCase,
    external: &[ExternalEvent],
) -> Result<PlannerResponse, delve_planner_desktop::error::AppError> {
    let mut candidates = database.planner_candidates(&CandidateRequest {
        command: &case.goal,
        selected_day: &case.day,
        time_zone: &case.time_zone,
        referenced_ids: &[],
        active_plan_id: None,
    })?;
    if asks_to_choose(&case.goal) {
        candidates.planning = Some(planning_facts(database, case, external));
    }
    agent
        .propose(PlannerRequest {
            command: &case.goal,
            selected_day: &case.day,
            time_zone: &case.time_zone,
            active_plan_id: None,
            candidates: &candidates,
            week_start: chrono::Weekday::Mon,
        })
        .await
}

/// Spare time on the coming days, as the app works it out, with the fixture's calendar busy
/// times standing in for connected calendars.
fn planning_facts(
    database: &PlannerDatabase,
    case: &BenchCase,
    external: &[ExternalEvent],
) -> PlanningFacts {
    let first = NaiveDate::parse_from_str(&case.day, "%Y-%m-%d").expect("case day");
    let end = first
        .checked_add_days(Days::new(u64::from(PLANNING_FACT_DAYS)))
        .expect("day in range")
        .to_string();
    let facts = database
        .capacity_facts(&case.day, &end, &case.time_zone, None)
        .expect("capacity facts");
    let profile = database.planning_profile().expect("profile");
    let capacity = delve_planner_desktop::capacity::capacity(
        first,
        PLANNING_FACT_DAYS,
        case.time_zone.parse().expect("case time zone"),
        facts,
        external,
        false,
        &profile,
    );
    PlanningFacts::from_capacity(&capacity)
}

fn external_busy(case: &BenchCase) -> Vec<ExternalEvent> {
    case.state
        .external_busy
        .iter()
        .filter_map(|busy| {
            let start = text(busy, "startAtUtc")?;
            let minutes = busy.get("durationMinutes")?.as_i64()?;
            let start_at = chrono::DateTime::parse_from_rfc3339(&start).ok()?;
            let end_at = start_at + chrono::Duration::minutes(minutes);
            Some(ExternalEvent {
                calendar_id: text(busy, "calendar").unwrap_or_default(),
                key: text(busy, "id").unwrap_or_default(),
                title: "Busy".into(),
                location: String::new(),
                all_day: false,
                start_at_utc: Some(start_at.with_timezone(&Utc).to_rfc3339()),
                end_at_utc: Some(end_at.with_timezone(&Utc).to_rfc3339()),
                start_date: None,
                end_date: None,
                busy: true,
                tentative: false,
                recurring: false,
            })
        })
        .collect()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn parse<T: serde::de::DeserializeOwned + Default>(value: &Value, key: &str) -> T {
    value
        .get(key)
        .cloned()
        .and_then(|raw| serde_json::from_value(raw).ok())
        .unwrap_or_default()
}

fn priority(value: &Value) -> TaskPriority {
    match value.get("priority").and_then(Value::as_str) {
        Some("low") => TaskPriority::Low,
        Some("high") => TaskPriority::High,
        Some("critical") => TaskPriority::Critical,
        // The fixture says "medium" for the app's "normal".
        _ => TaskPriority::Normal,
    }
}

fn minute_of_day(clock: &str) -> Option<i64> {
    let (hours, minutes) = clock.split_once(':')?;
    Some(hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?)
}

/// Creates the case's records and returns database id -> fixture id.
fn seed(database: &mut PlannerDatabase, case: &BenchCase) -> HashMap<String, String> {
    let state = &case.state;
    let mut db_ids: HashMap<String, String> = HashMap::new(); // fixture id -> database id
    let id_of = |value: &Value| text(value, "id").expect("fixture record id");

    if let Some(hours) = &state.working_hours {
        let profile = database.planning_profile().expect("profile");
        database
            .update_planning_profile(UpdatePlanningProfileInput {
                revision: profile.revision,
                preferred_start_minute: minute_of_day(&hours.start),
                preferred_end_minute: minute_of_day(&hours.end),
                max_planned_minutes: None,
                focus_minutes: None,
                break_minutes: None,
                no_work_days: (1..=7).filter(|day| !hours.days.contains(day)).collect(),
                energy: None,
                muted_observations: Vec::new(),
            })
            .expect("fixture working hours");
    }
    for fixture in &state.plans {
        let plan = database
            .create_plan(CreatePlanInput {
                title: text(fixture, "title").expect("plan title"),
                description: text(fixture, "description").unwrap_or_default(),
                status: parse::<PlanStatus>(fixture, "status"),
                start_date: text(fixture, "startDate"),
                target_date: text(fixture, "targetDate"),
                color: None,
                links: Vec::new(),
            })
            .expect("fixture plan");
        if fixture.get("archived").and_then(Value::as_bool) == Some(true) {
            database
                .update_plan(UpdatePlanInput {
                    id: plan.id.clone(),
                    revision: plan.revision,
                    title: plan.title.clone(),
                    description: plan.description.clone(),
                    status: plan.status,
                    start_date: plan.start_date.clone(),
                    target_date: plan.target_date.clone(),
                    color: plan.color,
                    archived: true,
                    links: Vec::new(),
                })
                .expect("archive fixture plan");
        }
        db_ids.insert(id_of(fixture), plan.id);
    }
    let lookup = |db_ids: &HashMap<String, String>, value: &Value, key: &str| {
        text(value, key).and_then(|id| db_ids.get(&id).cloned())
    };
    for fixture in &state.workstreams {
        let workstream = database
            .create_workstream(CreateWorkstreamInput {
                plan_id: lookup(&db_ids, fixture, "planId").expect("workstream plan"),
                name: text(fixture, "name").expect("workstream name"),
                description: String::new(),
            })
            .expect("fixture workstream");
        db_ids.insert(id_of(fixture), workstream.id);
    }
    for fixture in &state.milestones {
        let milestone = database
            .create_milestone(CreateMilestoneInput {
                plan_id: lookup(&db_ids, fixture, "planId").expect("milestone plan"),
                title: text(fixture, "title").expect("milestone title"),
                description: text(fixture, "description").unwrap_or_default(),
                target_date: text(fixture, "targetDate"),
                status: parse::<MilestoneStatus>(fixture, "status"),
                workstream_id: None,
            })
            .expect("fixture milestone");
        db_ids.insert(id_of(fixture), milestone.id);
    }
    for fixture in &state.tasks {
        // A task can only wait on one created before it; the fixture lists them in that order.
        let waiting_on = fixture
            .get("waitingOn")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|id| db_ids.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default();
        let task = database
            .create_task(CreateTaskInput {
                title: text(fixture, "title").expect("task title"),
                description: text(fixture, "description").unwrap_or_default(),
                plan_id: lookup(&db_ids, fixture, "planId"),
                milestone_id: lookup(&db_ids, fixture, "milestoneId"),
                workstream_id: lookup(&db_ids, fixture, "workstreamId"),
                owner_id: None,
                due_date: text(fixture, "dueDate"),
                scheduled_day: text(fixture, "scheduledDay"),
                planned_week: None,
                estimated_minutes: fixture.get("estimatedMinutes").and_then(Value::as_i64),
                status: parse::<TaskStatus>(fixture, "status"),
                priority: priority(fixture),
                checklist: Vec::new(),
                recurrence: None,
                waiting_on,
            })
            .expect("fixture task");
        db_ids.insert(id_of(fixture), task.id);
    }
    for fixture in &state.events {
        let event = database
            .create_event(CreateEventInput {
                title: text(fixture, "title").expect("event title"),
                notes: text(fixture, "notes").unwrap_or_default(),
                start_at_utc: text(fixture, "startAtUtc").expect("event start"),
                time_zone: text(fixture, "timeZone").unwrap_or_else(|| case.time_zone.clone()),
                duration_minutes: fixture
                    .get("durationMinutes")
                    .and_then(Value::as_i64)
                    .expect("event duration"),
                reminder_minutes_before: None,
                plan_id: lookup(&db_ids, fixture, "planId"),
                location: String::new(),
                workstream_id: None,
                owner_id: None,
            })
            .expect("fixture event");
        db_ids.insert(id_of(fixture), event.id);
    }
    for fixture in &state.task_blocks {
        let block = database
            .create_task_block(CreateTaskBlockInput {
                task_id: lookup(&db_ids, fixture, "taskId").expect("task block task"),
                start_at_utc: text(fixture, "startAtUtc").expect("task block start"),
                time_zone: case.time_zone.clone(),
                duration_minutes: fixture
                    .get("durationMinutes")
                    .and_then(Value::as_i64)
                    .expect("task block duration"),
            })
            .expect("fixture task block");
        db_ids.insert(id_of(fixture), block.block.id);
    }
    db_ids
        .into_iter()
        .map(|(fixture, db)| (db, fixture))
        .collect()
}

/// Replaces every database id inside an operation with the fixture id it was seeded from.
fn to_fixture_ids(value: &mut Value, ids: &HashMap<String, String>) {
    match value {
        Value::String(text) => {
            if let Some(fixture) = ids.get(text.as_str()) {
                *text = fixture.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| to_fixture_ids(item, ids)),
        Value::Object(map) => map.values_mut().for_each(|item| to_fixture_ids(item, ids)),
        _ => {}
    }
}

/// Runtime state lives in a temp folder, never the app's own: see `eval_agent`.
fn data_directory() -> PathBuf {
    if let Some(path) = env::var_os("DELVE_PLANNER_EVAL_DATA_DIR") {
        return PathBuf::from(path);
    }
    env::temp_dir().join("delve-planner-eval-data")
}

/// Reads models from where Delve Planner keeps them, as `eval_agent` does.
fn point_at_installed_models() {
    if env::var_os("OLLAMA_MODELS").is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    let app = env::var_os("HOME").map(|home| {
        PathBuf::from(home).join("Library/Application Support/com.vonvan.dayplan.desktop")
    });
    #[cfg(target_os = "windows")]
    let app =
        env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("com.vonvan.dayplan.desktop"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let app: Option<PathBuf> = None;
    if let Some(models) = app
        .map(|dir| dir.join("ai-models"))
        .filter(|path| path.is_dir())
    {
        env::set_var("OLLAMA_MODELS", models);
    }
}

fn options() -> Result<Options, String> {
    let usage = "Usage: a0_bench --input path --output path [--runs 3] [--endpoint http://127.0.0.1:PORT] [--only id,id]";
    let mut args = env::args().skip(1);
    let (mut input, mut output, mut runs, mut endpoint, mut only) =
        (None, None, 1usize, None, Vec::new());
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|runs| *runs > 0)
                    .ok_or("--runs requires a positive integer")?
            }
            "--endpoint" => {
                let value = args.next().ok_or(usage)?;
                // Same rule as the app: the model is only ever reached on this machine.
                if !value.starts_with("http://127.0.0.1:") {
                    return Err("--endpoint must be http://127.0.0.1:PORT".into());
                }
                endpoint = Some(value.trim_end_matches('/').to_string());
            }
            "--only" => {
                only = args
                    .next()
                    .ok_or("--only requires case ids")?
                    .split(',')
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty())
                    .collect()
            }
            _ => return Err(format!("Unknown argument: {argument}\n{usage}")),
        }
    }
    Ok(Options {
        input: input.ok_or(usage)?,
        output: output.ok_or(usage)?,
        runs,
        endpoint,
        only,
    })
}
