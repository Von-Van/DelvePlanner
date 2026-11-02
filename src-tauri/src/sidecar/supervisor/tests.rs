//! End-to-end tests of the link: the real Python protocol package, started by the real
//! supervisor, driven by the diagnostic runner. Skipped (with a note) where no Python 3 is found.

use super::*;
use std::path::Path;
use std::sync::Mutex as StdMutex;

/// A Python 3.10+ interpreter: `DELVEAGENT_TEST_PYTHON` if set, otherwise `python3` or `python`.
fn python() -> Option<PathBuf> {
    let candidates: Vec<OsString> = match std::env::var_os("DELVEAGENT_TEST_PYTHON") {
        Some(path) => vec![path],
        None => vec!["python3".into(), "python".into()],
    };
    candidates.into_iter().find_map(|candidate| {
        let output = std::process::Command::new(&candidate)
            .args(["-c", "import sys; print(sys.version_info >= (3, 10))"])
            .output()
            .ok()?;
        (String::from_utf8_lossy(&output.stdout).trim() == "True").then(|| candidate.into())
    })
}

fn agent_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("agent-python")
}

fn sidecar_with(limits: SidecarLimits) -> Option<Sidecar> {
    let Some(python) = python() else {
        eprintln!("skipping: no Python 3.10+ found (set DELVEAGENT_TEST_PYTHON)");
        return None;
    };
    Some(Sidecar::new(
        SidecarCommand::python_source(python, agent_dir(), "diagnostic"),
        limits,
    ))
}

fn sidecar() -> Option<Sidecar> {
    sidecar_with(fast_limits())
}

fn fast_limits() -> SidecarLimits {
    SidecarLimits {
        tool_timeout: Duration::from_millis(500),
        model_timeout: Duration::from_millis(500),
        cancel_grace: Duration::from_millis(500),
        ..SidecarLimits::default()
    }
}

fn task(goal: &str) -> RunRequest {
    RunRequest {
        goal: goal.into(),
        now: "2026-11-02T09:00:00-06:00".into(),
        time_zone: "America/Chicago".into(),
        permitted_tools: vec!["get_day_context".into(), "slow_tool".into()],
        architecture: None,
        budget: RunBudget::default(),
        conversation: Vec::new(),
        conversation_summary: None,
    }
}

#[derive(Default)]
struct TestHost {
    traces: StdMutex<Vec<Value>>,
    tool_calls: StdMutex<Vec<(String, Value)>>,
}

impl SidecarHost for TestHost {
    async fn call_tool(
        &self,
        _run_id: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<Value, LinkError> {
        self.tool_calls
            .lock()
            .unwrap()
            .push((name.to_string(), arguments.clone()));
        if name == "slow_tool" {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let calls = self.tool_calls.lock().unwrap().len();
        Ok(json!({
            "callId": format!("call-{calls}"),
            "tool": name,
            "ok": true,
            "result": { "events": [{ "id": "evt-1", "revision": 3 }] },
            "durationMs": 1,
            "callsRemaining": 23,
        }))
    }

    async fn call_model(&self, run_id: &str, request: &Value) -> Result<Value, LinkError> {
        assert_eq!(request["runId"], run_id);
        Ok(json!({
            "model": "qwen3:8b",
            "digest": "f".repeat(64),
            "ollamaVersion": "0.32.0",
            "message": { "role": "assistant", "content": "ok" },
            "usage": { "promptTokens": 9, "completionTokens": 1 },
        }))
    }

    fn trace(&self, _run_id: &str, event: &Value) {
        self.traces.lock().unwrap().push(event.clone());
    }
}

async fn run(sidecar: &Sidecar, goal: &str) -> Result<RunOutcome, SidecarError> {
    sidecar
        .run(&task(goal), &TestHost::default(), &CancellationToken::new())
        .await
}

fn answer(outcome: &RunOutcome) -> &str {
    outcome.result["answer"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn handshakes_runs_and_reuses_one_child() {
    let Some(sidecar) = sidecar() else { return };
    assert!(!sidecar.running().await);
    let info = sidecar.ensure_started().await.unwrap();
    assert_eq!(info.protocol_version, 1);
    assert_eq!(info.agent["runner"], "diagnostic");
    let pid = sidecar.process_id().await;

    let outcome = run(&sidecar, "echo:hello").await.unwrap();
    assert_eq!(outcome.result["status"], "answered");
    assert_eq!(answer(&outcome), "hello");
    assert!(outcome.run_id.starts_with("run-"));
    let outcome = run(&sidecar, "echo:again").await.unwrap();
    assert_eq!(answer(&outcome), "again");
    assert_eq!(sidecar.process_id().await, pid);

    sidecar.shutdown().await;
    assert!(!sidecar.running().await);
}

#[tokio::test]
async fn routes_tool_and_model_calls_through_the_host() {
    let Some(sidecar) = sidecar() else { return };
    let host = TestHost::default();
    let cancel = CancellationToken::new();

    let outcome = sidecar
        .run(&task("tool:get_day_context"), &host, &cancel)
        .await
        .unwrap();
    assert_eq!(outcome.tool_calls, 1);
    let reply: Value = serde_json::from_str(answer(&outcome)).unwrap();
    assert_eq!(reply["callId"], "call-1");
    assert_eq!(reply["result"]["events"][0]["id"], "evt-1");
    assert_eq!(
        host.tool_calls.lock().unwrap()[0],
        ("get_day_context".to_string(), json!({}))
    );

    let outcome = sidecar.run(&task("model"), &host, &cancel).await.unwrap();
    assert_eq!(answer(&outcome), "ok");
    assert_eq!(outcome.model_calls, 1);
    assert_eq!(outcome.result["usage"]["promptTokens"], 9);
}

#[tokio::test]
async fn refuses_tools_outside_the_runs_permissions_and_budget() {
    let Some(sidecar) = sidecar() else { return };
    let host = TestHost::default();
    let outcome = run(&sidecar, "tool:delete_everything").await.unwrap();
    assert_eq!(outcome.result["status"], "cannot_complete");
    assert_eq!(outcome.result["error"]["code"], "permission_denied");
    assert!(host.tool_calls.lock().unwrap().is_empty());

    let mut request = task("tool:get_day_context");
    request.budget.max_tool_calls = 0;
    let outcome = sidecar
        .run(&request, &host, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.result["error"]["code"], "budget_exhausted");
    assert!(host.tool_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_slow_tool_times_out_without_ending_the_run() {
    let Some(sidecar) = sidecar() else { return };
    let outcome = run(&sidecar, "tool:slow_tool").await.unwrap();
    assert_eq!(outcome.result["status"], "cannot_complete");
    assert_eq!(outcome.result["error"]["code"], "timeout");
    assert_eq!(outcome.result["error"]["retryable"], true);
    // The child is still fine.
    assert_eq!(
        answer(&run(&sidecar, "echo:still here").await.unwrap()),
        "still here"
    );
}

#[tokio::test]
async fn passes_trace_events_to_the_host() {
    let Some(sidecar) = sidecar() else { return };
    let host = TestHost::default();
    sidecar
        .run(&task("trace"), &host, &CancellationToken::new())
        .await
        .unwrap();
    let traces = host.traces.lock().unwrap();
    let kinds: Vec<&str> = traces.iter().map(|t| t["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["plan", "verify", "final"]);
    assert_eq!(traces[2]["seq"], 3);
    assert!(traces[0]["at"].as_str().unwrap().ends_with("+00:00"));
}

#[tokio::test]
async fn a_cancelled_run_reports_back_and_the_child_stays() {
    let Some(sidecar) = sidecar() else { return };
    sidecar.ensure_started().await.unwrap();
    let pid = sidecar.process_id().await;
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let outcome = sidecar
        .run(&task("sleep:30"), &TestHost::default(), &cancel)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(outcome.stopped, Some(StopReason::Cancelled));
    assert_eq!(outcome.result["status"], "cannot_complete");
    assert_eq!(outcome.result["stopReason"], "cancelled");
    assert_eq!(sidecar.process_id().await, pid);
}

#[tokio::test]
async fn a_run_that_ignores_cancel_is_killed_and_the_next_one_restarts() {
    let Some(sidecar) = sidecar() else { return };
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let error = sidecar
        .run(&task("hang"), &TestHost::default(), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, SidecarError::Cancelled), "{error:?}");
    assert!(!sidecar.running().await);
    assert_eq!(answer(&run(&sidecar, "echo:back").await.unwrap()), "back");
}

#[tokio::test]
async fn a_run_past_its_wall_clock_is_stopped() {
    let Some(sidecar) = sidecar() else { return };
    let mut request = task("sleep:30");
    request.budget.max_wall_seconds = 1;
    let outcome = sidecar
        .run(&request, &TestHost::default(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.stopped, Some(StopReason::TimedOut));
    assert_eq!(outcome.result["stopReason"], "timeout");

    let mut request = task("hang");
    request.budget.max_wall_seconds = 1;
    let error = sidecar
        .run(&request, &TestHost::default(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, SidecarError::TimedOut), "{error:?}");
}

#[tokio::test]
async fn the_run_clock_keeps_running_during_a_slow_call() {
    let Some(sidecar) = sidecar_with(SidecarLimits {
        tool_timeout: Duration::from_secs(30),
        ..fast_limits()
    }) else {
        return;
    };
    let mut request = task("tool:slow_tool");
    request.budget.max_wall_seconds = 1;
    let started = Instant::now();
    let outcome = sidecar
        .run(&request, &TestHost::default(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(outcome.stopped, Some(StopReason::TimedOut));
    assert_eq!(outcome.result["stopReason"], "timeout");
}

#[tokio::test]
async fn a_flood_of_stderr_without_newlines_is_drained() {
    let Some(sidecar) = sidecar() else { return };
    assert_eq!(
        answer(&run(&sidecar, "stderr_flood").await.unwrap()),
        "flooded"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let diagnostics = sidecar.diagnostics();
    assert!(
        diagnostics.iter().any(|line| line == "after the flood"),
        "{diagnostics:?}"
    );
    assert!(diagnostics
        .iter()
        .all(|line| line.len() <= STDERR_LINE_BYTES + 4));
}

#[tokio::test]
async fn restarts_after_a_crash_but_not_in_a_loop() {
    let Some(sidecar) = sidecar_with(SidecarLimits {
        max_restarts: 2,
        ..fast_limits()
    }) else {
        return;
    };
    sidecar.ensure_started().await.unwrap();
    let first = sidecar.process_id().await;
    let error = run(&sidecar, "crash").await.unwrap_err();
    assert!(matches!(error, SidecarError::Crashed { .. }), "{error:?}");

    assert_eq!(
        answer(&run(&sidecar, "echo:restarted").await.unwrap()),
        "restarted"
    );
    assert_ne!(sidecar.process_id().await, first);

    assert!(matches!(
        run(&sidecar, "crash").await.unwrap_err(),
        SidecarError::Crashed { .. }
    ));
    assert!(matches!(
        run(&sidecar, "echo:x").await.unwrap_err(),
        SidecarError::CrashLoop
    ));
}

#[tokio::test]
async fn an_oversized_message_ends_the_child() {
    let Some(sidecar) = sidecar() else { return };
    let error = run(&sidecar, "oversize").await.unwrap_err();
    assert!(
        matches!(&error, SidecarError::Protocol(problem) if problem.contains("limit")),
        "{error:?}"
    );
    assert!(!sidecar.running().await);
    assert_eq!(answer(&run(&sidecar, "echo:fresh").await.unwrap()), "fresh");
}

#[tokio::test]
async fn stray_prints_go_to_diagnostics_not_the_protocol() {
    let Some(sidecar) = sidecar() else { return };
    assert_eq!(answer(&run(&sidecar, "stdout").await.unwrap()), "printed");
    // The stderr reader runs on its own; give it a moment.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(sidecar
        .diagnostics()
        .iter()
        .any(|line| line.contains("not the protocol")));
}

#[tokio::test]
async fn an_agent_bug_is_a_normal_cannot_complete() {
    let Some(sidecar) = sidecar() else { return };
    let outcome = run(&sidecar, "fail").await.unwrap();
    assert_eq!(outcome.result["status"], "cannot_complete");
    assert_eq!(outcome.result["error"]["code"], "internal");
    assert!(sidecar.running().await);
}

#[tokio::test]
async fn the_child_gets_none_of_the_apps_environment() {
    let Some(sidecar) = sidecar() else { return };
    // PATH is set for every test process; the child must not inherit it.
    assert!(std::env::var_os("PATH").is_some());
    assert_eq!(answer(&run(&sidecar, "env:PATH").await.unwrap()), "absent");
    assert_eq!(
        answer(&run(&sidecar, "env:PYTHONUTF8").await.unwrap()),
        "present"
    );
}

#[tokio::test]
async fn a_child_that_never_handshakes_is_refused() {
    let Some(python) = python() else { return };
    let sidecar = Sidecar::new(
        SidecarCommand {
            program: python,
            args: vec!["-c".into(), "import time; time.sleep(30)".into()],
            cwd: None,
            env: Vec::new(),
        },
        SidecarLimits {
            handshake_timeout: Duration::from_millis(500),
            ..fast_limits()
        },
    );
    let error = sidecar.ensure_started().await.unwrap_err();
    assert!(matches!(error, SidecarError::Handshake(_)), "{error:?}");
    assert!(!sidecar.running().await);
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let sidecar = Sidecar::new(
        SidecarCommand::frozen("/definitely/not/here/delveagent".into(), "diagnostic"),
        fast_limits(),
    );
    assert!(matches!(
        sidecar.ensure_started().await.unwrap_err(),
        SidecarError::Spawn(_)
    ));
}

#[tokio::test]
async fn an_idle_child_is_stopped() {
    let Some(sidecar) = sidecar() else { return };
    run(&sidecar, "echo:x").await.unwrap();
    assert!(!sidecar.stop_if_idle(Duration::from_secs(60)).await);
    assert!(sidecar.stop_if_idle(Duration::ZERO).await);
    assert!(!sidecar.running().await);
}

/// The real read-tool broker (piece C) as the host, against its built-in sample week: the
/// payloads both sides agreed on travel through unchanged.
struct BrokerHost {
    inner: StdMutex<(crate::db::PlannerDatabase, crate::tools::ToolSession)>,
}

impl SidecarHost for BrokerHost {
    async fn call_tool(
        &self,
        _run_id: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<Value, LinkError> {
        let mut inner = self.inner.lock().unwrap();
        let (database, session) = &mut *inner;
        let sources = crate::tools::ToolSources {
            database,
            calendars: &crate::tools::NoCalendars,
        };
        let outcome = session.call(&sources, name, arguments.clone());
        Ok(serde_json::to_value(outcome).unwrap())
    }

    async fn call_model(&self, _run_id: &str, _request: &Value) -> Result<Value, LinkError> {
        Err(LinkError::new(
            ErrorCode::Unavailable,
            "no model here",
            true,
        ))
    }

    fn trace(&self, _run_id: &str, _event: &Value) {}
}

#[tokio::test]
async fn speaks_the_read_tool_brokers_contract() {
    let Some(sidecar) = sidecar() else { return };
    let folder = tempfile::tempdir().unwrap();
    let database = crate::tools::open_fixture_database(
        &folder.path().join("fixture.sqlite3"),
        crate::tools::SAMPLE_FIXTURE,
    )
    .unwrap();
    let config =
        crate::tools::SessionConfig::new(chrono::Utc::now(), "America/Chicago".parse().unwrap());
    let host = BrokerHost {
        inner: StdMutex::new((database, crate::tools::ToolSession::new(config))),
    };
    let mut request = task("tool:list_active_projects");
    request.permitted_tools.push("list_active_projects".into());
    let outcome = sidecar
        .run(&request, &host, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.result["status"], "answered", "{}", outcome.result);
    let reply: Value = serde_json::from_str(answer(&outcome)).unwrap();
    assert_eq!(reply["callId"], "call-1");
    assert!(reply["result"]["plans"].is_array(), "{reply}");
}
