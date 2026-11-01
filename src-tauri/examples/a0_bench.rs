//! Runs the existing planner (architecture A0) over `agent_bench` cases, for the A0-vs-A1
//! comparison in `docs/research/experiments/`.
//!
//! A0 has no tools: the app hands it a snapshot of relevant records and it answers with one
//! proposal or one clarifying question. This example gives it exactly that. For each case it seeds
//! a throwaway database by importing the case's starting planner export (the benchmark fixture is
//! a real export bundle, so ids and revisions are exactly the benchmark's), asks the planner the
//! case's goal the way the desktop app does (`propose_schedule_changes` in `lib.rs`), and records
//! the reply so `agent_bench` can score it.
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
    ExternalEvent, PlannerResponse, PlanningFacts, UpdatePlanningProfileInput,
};
use delve_planner_desktop::runtime::OllamaRuntime;
use delve_planner_desktop::tools::open_fixture_database;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

/// The starting planner state, in `agent_bench/fixtures/planner.v2.json`'s shape: a Delve Planner
/// export bundle plus calendar busy times and working hours.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    bundle: Value,
    #[serde(default)]
    external_busy: Vec<Busy>,
    #[serde(default)]
    working_hours: Option<WorkingHours>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Busy {
    start_at_utc: String,
    end_at_utc: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkingHours {
    start_minute: i64,
    end_minute: i64,
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
    /// Each operation is a `MutationOperation` (`{"type": …, …}`).
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
    let mut database = open_fixture_database(
        &directory.path().join("a0-bench.sqlite3"),
        &case.state.bundle.to_string(),
    )
    .unwrap_or_else(|error| panic!("{}: fixture import failed: {error}", case.case_id));
    set_working_hours(&mut database, case);
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
                .map(|operation| serde_json::to_value(operation.change).expect("operation"))
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
        .enumerate()
        .map(|(index, busy)| ExternalEvent {
            calendar_id: "fixture".into(),
            key: format!("busy-{index}"),
            title: "Busy".into(),
            location: String::new(),
            all_day: false,
            start_at_utc: Some(busy.start_at_utc.clone()),
            end_at_utc: Some(busy.end_at_utc.clone()),
            start_date: None,
            end_date: None,
            busy: true,
            tentative: false,
            recurring: false,
        })
        .collect()
}

/// The fixture's working hours become the planning profile's preferred hours and days off.
fn set_working_hours(database: &mut PlannerDatabase, case: &BenchCase) {
    let Some(hours) = &case.state.working_hours else {
        return;
    };
    let profile = database.planning_profile().expect("profile");
    database
        .update_planning_profile(UpdatePlanningProfileInput {
            revision: profile.revision,
            preferred_start_minute: Some(hours.start_minute),
            preferred_end_minute: Some(hours.end_minute),
            max_planned_minutes: None,
            focus_minutes: None,
            break_minutes: None,
            no_work_days: (1..=7).filter(|day| !hours.days.contains(day)).collect(),
            energy: None,
            muted_observations: Vec::new(),
        })
        .expect("fixture working hours");
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
