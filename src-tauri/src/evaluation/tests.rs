use super::*;
use rusqlite::params;
use std::path::Path;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/evaluation");

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// An app data folder holding the fixture result files and no database.
fn app_data() -> (tempfile::TempDir, Sources) {
    let directory = tempfile::tempdir().unwrap();
    let sources = Sources::in_app_data(directory.path());
    copy_dir(Path::new(FIXTURES), &sources.results);
    (directory, sources)
}

#[test]
fn an_empty_app_data_folder_lists_nothing_without_problems() {
    let directory = tempfile::tempdir().unwrap();
    let overview = overview(&Sources::in_app_data(directory.path()));
    assert!(overview.runs.is_empty());
    assert!(overview.problems.is_empty());
    assert!(overview.results_folder.ends_with(RESULTS_FOLDER));
}

#[test]
fn lists_benchmark_and_regression_runs_newest_first() {
    let (_dir, sources) = app_data();
    let overview = overview(&sources);
    assert!(overview.problems.is_empty(), "{:?}", overview.problems);
    let ids: Vec<&str> = overview.runs.iter().map(|r| r.id.as_str()).collect();
    // notes.json is not a result file and is skipped quietly.
    assert_eq!(
        ids,
        [
            "benchmark:bench-a1-sample",
            "regression:regression-2026-11.json"
        ]
    );
}

#[test]
fn a_benchmark_run_keeps_safety_failures_out_of_the_success_rate() {
    let (_dir, sources) = app_data();
    let run = &overview(&sources).runs[0];
    assert_eq!(run.source, RunSource::Benchmark);
    assert_eq!(run.architecture.as_deref(), Some("A1"));
    assert_eq!(run.model_name.as_deref(), Some("qwen3:8b"));
    assert!(run.model_digest.as_deref().unwrap().starts_with("500a1f06"));
    assert_eq!(
        run.hardware.as_deref(),
        Some("chip: Apple M2, memory: 16 GB")
    );
    assert_eq!((run.cases, run.attempts, run.passed_attempts), (3, 3, 1));
    assert_eq!(run.safety_failures, 1);
    assert!((run.success_rate.unwrap() - 1.0 / 3.0).abs() < 1e-9);
    assert_eq!(run.latency_p50_ms, Some(1504.25));
    assert_eq!(run.latency_p95_ms, Some(2310.5));
}

#[test]
fn a_benchmark_run_lists_every_case_with_its_checks() {
    let (_dir, sources) = app_data();
    let detail = run_detail(&sources, "benchmark:bench-a1-sample").unwrap();
    assert_eq!(detail.cases.len(), 3);
    let failed = &detail.cases[1];
    assert_eq!(failed.case_id, "ms-least-busy-day");
    assert_eq!(failed.family.as_deref(), Some("multi_step"));
    assert_eq!(failed.tags, ["fixture"]);
    assert_eq!(failed.skill.as_deref(), Some("Combining several lookups"));
    // A run written before skills were recorded leaves it out; the screen falls back to the family.
    assert_eq!(detail.cases[0].skill, None);
    assert!(!failed.success);
    assert_eq!(failed.safe, Some(true));
    assert_eq!(failed.failed_checks[0].name, "drafts");
    let unsafe_case = &detail.cases[2];
    assert_eq!(unsafe_case.safe, Some(false));
    assert_eq!(
        unsafe_case.safety_violations[0].rule,
        "followed_untrusted_instruction"
    );
    assert!(detail.cases.iter().all(|c| c.has_trace));
}

#[test]
fn a_trace_merges_the_agents_own_steps_without_duplicating_the_harness() {
    let (_dir, sources) = app_data();
    let trace = case_trace(&sources, "benchmark:bench-a1-sample", "tc-today-agenda#0").unwrap();
    let kinds: Vec<&str> = trace
        .events
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[0], "model_call");
    // The agent's own tool_call copy is dropped; the harness's record stays.
    assert_eq!(kinds.iter().filter(|k| **k == "tool_call").count(), 1);
    assert_eq!(*kinds.last().unwrap(), "final");
    let seqs: Vec<u64> = trace
        .events
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    assert_eq!(trace.status.as_deref(), Some("answered"));
}

#[test]
fn a_regression_report_counts_exact_results_and_lists_failures() {
    let (_dir, sources) = app_data();
    let detail = run_detail(&sources, "regression:regression-2026-11.json").unwrap();
    let run = &detail.run;
    assert_eq!(run.architecture.as_deref(), Some("A0"));
    assert_eq!((run.cases, run.repeats, run.attempts), (126, 3, 378));
    assert_eq!(run.passed_attempts, 375);
    assert_eq!(run.safety_failures, 0);
    assert_eq!(run.latency_p50_ms, None);
    assert_eq!(detail.cases.len(), 3);
    assert_eq!(detail.cases[0].case_id, "relative-followup");
    assert_eq!(detail.cases[0].repeat, 2);
    // The report has no per-case safety, so the case must not claim to be safe.
    assert_eq!(detail.cases[0].safe, None);
    assert!(detail.note.unwrap().contains("124 of 126"));
}

#[test]
fn ids_cannot_reach_outside_the_results_folder() {
    let (dir, sources) = app_data();
    fs::create_dir_all(dir.path().join("outside")).unwrap();
    fs::copy(
        sources.results.join("bench-a1-sample/summary.json"),
        dir.path().join("outside/summary.json"),
    )
    .unwrap();
    for id in [
        "benchmark:../outside",
        "benchmark:bench-a1-sample/../../outside",
        "benchmark:",
        "nonsense:bench-a1-sample",
        "bench-a1-sample",
    ] {
        assert!(
            matches!(run_detail(&sources, id), Err(EvaluationError::NotFound)),
            "{id}"
        );
    }
}

#[test]
fn a_broken_result_file_is_reported_not_hidden() {
    let (_dir, sources) = app_data();
    fs::write(sources.results.join("broken.json"), "{ not json").unwrap();
    let overview = overview(&sources);
    assert_eq!(overview.runs.len(), 2);
    assert_eq!(overview.problems.len(), 1);
    assert!(overview.problems[0].starts_with("broken.json"));
}

fn saved_runs_database(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE experiment_run (id TEXT PRIMARY KEY, kind TEXT, benchmark_version TEXT,
                architecture_id TEXT, model_name TEXT, model_digest TEXT, hardware TEXT,
                status TEXT, started_at TEXT);
             CREATE TABLE agent_run (id TEXT PRIMARY KEY, source TEXT, experiment_run_id TEXT,
                case_id TEXT, case_family TEXT, architecture_id TEXT, model_name TEXT,
                model_digest TEXT, status TEXT, failure_reason TEXT, passed INTEGER,
                safety_failure INTEGER, latency_ms REAL, started_at TEXT);
             CREATE TABLE agent_step (run_id TEXT, seq INTEGER, kind TEXT, at TEXT,
                duration_ms REAL, summary TEXT, data_json TEXT, PRIMARY KEY (run_id, seq));",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO experiment_run VALUES ('exp-1', 'benchmark', '0.1.0', 'A1', 'qwen3:8b',
                'abc123', 'Apple M2', 'completed', '2026-11-02T10:00:00Z')",
            [],
        )
        .unwrap();
    let mut insert = connection
        .prepare("INSERT INTO agent_run VALUES (?1, ?2, ?3, ?4, 'tool_choice', 'A1', 'qwen3:8b', 'abc123', ?5, ?6, ?7, ?8, ?9, ?10)")
        .unwrap();
    insert
        .execute(params![
            "run-1",
            "benchmark",
            "exp-1",
            "tc-today-agenda",
            "answered",
            None::<String>,
            1,
            0,
            900.0,
            "2026-11-02T10:00:01Z"
        ])
        .unwrap();
    insert
        .execute(params![
            "run-2",
            "benchmark",
            "exp-1",
            "untrusted-event-notes",
            "drafted",
            Some("drafted a delete"),
            0,
            1,
            1500.0,
            "2026-11-02T10:00:03Z"
        ])
        .unwrap();
    insert
        .execute(params![
            "run-3",
            "assistant",
            None::<String>,
            None::<String>,
            "answered",
            None::<String>,
            None::<i64>,
            0,
            700.0,
            "2026-11-02T12:00:00Z"
        ])
        .unwrap();
    connection
        .execute(
            "INSERT INTO agent_step VALUES ('run-1', 1, 'tool_call', '2026-11-02T10:00:01Z', NULL,
                'get_day_context', '{\"tool\":\"get_day_context\"}'),
             ('run-1', 2, 'final', '2026-11-02T10:00:02Z', 12.5, 'status answered', '{}')",
            [],
        )
        .unwrap();
}

#[test]
fn saved_runs_are_read_from_the_database_without_changing_it() {
    let directory = tempfile::tempdir().unwrap();
    let sources = Sources::in_app_data(directory.path());
    saved_runs_database(&sources.database);
    let before = fs::read(&sources.database).unwrap();

    let overview = overview(&sources);
    assert!(overview.problems.is_empty(), "{:?}", overview.problems);
    let ids: Vec<&str> = overview.runs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["recorded:assistant", "recorded:exp-1"]);
    let experiment = &overview.runs[1];
    assert_eq!(experiment.label, "Saved benchmark run (0.1.0)");
    assert_eq!(experiment.hardware.as_deref(), Some("Apple M2"));
    assert_eq!((experiment.attempts, experiment.passed_attempts), (2, 1));
    assert_eq!(experiment.safety_failures, 1);

    let detail = run_detail(&sources, "recorded:exp-1").unwrap();
    assert_eq!(detail.cases[1].family.as_deref(), Some("tool_choice"));
    assert_eq!(detail.cases[1].safe, Some(false));
    assert_eq!(detail.cases[1].failed_checks[0].detail, "drafted a delete");

    let trace = case_trace(&sources, "recorded:exp-1", "run-1").unwrap();
    assert_eq!(trace.events.len(), 2);
    assert_eq!(trace.events[0]["data"]["tool"], "get_day_context");
    assert_eq!(trace.events[1]["durationMs"], 12.5);

    assert_eq!(fs::read(&sources.database).unwrap(), before);
}

#[test]
fn a_database_from_before_run_storage_lists_no_saved_runs() {
    let directory = tempfile::tempdir().unwrap();
    let sources = Sources::in_app_data(directory.path());
    Connection::open(&sources.database)
        .unwrap()
        .execute_batch("CREATE TABLE plan (id TEXT)")
        .unwrap();
    let overview = overview(&sources);
    assert!(overview.runs.is_empty());
    assert!(overview.problems.is_empty());
}

/// The panel's component tests read what these commands return for the fixture files. This keeps
/// those snapshots in step with the Rust output: run with `UPDATE_EVALUATION_SNAPSHOTS=1` to rewrite.
#[test]
fn ui_snapshots_match_what_the_commands_return() {
    let (_dir, sources) = app_data();
    let mut overview = overview(&sources);
    overview.results_folder = "/app-data/evaluation-results".into();
    let snapshots = [
        ("overview.json", serde_json::to_value(&overview).unwrap()),
        (
            "benchmark-run.json",
            serde_json::to_value(run_detail(&sources, "benchmark:bench-a1-sample").unwrap())
                .unwrap(),
        ),
        (
            "regression-run.json",
            serde_json::to_value(
                run_detail(&sources, "regression:regression-2026-11.json").unwrap(),
            )
            .unwrap(),
        ),
        (
            "trace.json",
            serde_json::to_value(
                case_trace(&sources, "benchmark:bench-a1-sample", "tc-today-agenda#0").unwrap(),
            )
            .unwrap(),
        ),
    ];
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/evaluation-ui");
    let update = std::env::var_os("UPDATE_EVALUATION_SNAPSHOTS").is_some();
    for (name, value) in snapshots {
        let path = folder.join(name);
        let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
        if update {
            fs::create_dir_all(&folder).unwrap();
            fs::write(&path, &text).unwrap();
        } else {
            let saved = fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                saved, text,
                "{name} is out of date; rerun with UPDATE_EVALUATION_SNAPSHOTS=1"
            );
        }
    }
}
