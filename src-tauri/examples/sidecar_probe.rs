//! Packaging probe for the Python agent link (DelveOps piece D). Starts a packaged or source
//! agent through the real supervisor, runs the link's scenarios against it, and prints a JSON
//! report with timings. Exits non-zero if any scenario fails.
//!
//! ```text
//! cargo run --example sidecar_probe -- --frozen dist/delveops-agent/delveops-agent
//! cargo run --example sidecar_probe -- --python python3 --agent-dir agent-python
//! ```

use delve_planner_desktop::sidecar::{
    LinkError, RunBudget, RunRequest, Sidecar, SidecarCommand, SidecarError, SidecarHost,
    SidecarLimits, StopReason,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

struct ProbeHost;

impl SidecarHost for ProbeHost {
    async fn call_tool(
        &self,
        _run_id: &str,
        name: &str,
        _arguments: &Value,
    ) -> Result<Value, LinkError> {
        Ok(
            json!({ "callId": "call-1", "tool": name, "ok": true, "result": { "events": [] }, "durationMs": 0 }),
        )
    }

    async fn call_model(&self, _run_id: &str, _request: &Value) -> Result<Value, LinkError> {
        Ok(
            json!({ "model": "probe", "digest": "0".repeat(64), "ollamaVersion": "probe",
                   "message": { "role": "assistant", "content": "ok" } }),
        )
    }

    fn trace(&self, _run_id: &str, _event: &Value) {}
}

fn task(goal: &str) -> RunRequest {
    RunRequest {
        goal: goal.into(),
        now: "2026-11-02T09:00:00Z".into(),
        time_zone: "UTC".into(),
        permitted_tools: vec!["get_day_context".into()],
        architecture: None,
        budget: RunBudget::default(),
        conversation: Vec::new(),
        conversation_summary: None,
    }
}

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 10_000.0).round() / 10.0
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let (command, mode) = if let Some(frozen) = value("--frozen") {
        (
            SidecarCommand::frozen(PathBuf::from(frozen), "diagnostic"),
            "frozen",
        )
    } else {
        let python = value("--python").unwrap_or_else(|| "python3".into());
        let agent_dir = value("--agent-dir").unwrap_or_else(|| "agent-python".into());
        let agent_dir = std::path::absolute(&agent_dir).unwrap_or_else(|_| agent_dir.into());
        (
            SidecarCommand::python_source(python.into(), agent_dir, "diagnostic"),
            "source",
        )
    };
    let limits = SidecarLimits {
        handshake_timeout: Duration::from_secs(60),
        cancel_grace: Duration::from_secs(2),
        max_restarts: 10,
        ..SidecarLimits::default()
    };
    let sidecar = Sidecar::new(command, limits);
    let host = ProbeHost;
    let mut checks: Vec<Value> = Vec::new();
    let mut failed = false;
    let mut record = |name: &str, ok: bool, elapsed: Duration, detail: Value| {
        failed |= !ok;
        checks.push(json!({ "check": name, "ok": ok, "ms": millis(elapsed), "detail": detail }));
    };

    // Cold start: spawn plus handshake.
    let started = Instant::now();
    let info = sidecar.ensure_started().await;
    let agent = match &info {
        Ok(info) => json!({ "agent": info.agent, "runtime": info.runtime }),
        Err(error) => json!({ "error": error.to_string(), "stderr": sidecar.diagnostics() }),
    };
    record("cold_start", info.is_ok(), started.elapsed(), agent.clone());
    if info.is_err() {
        println!(
            "{}",
            json!({ "mode": mode, "agent": agent, "checks": checks })
        );
        return ExitCode::FAILURE;
    }

    let none = CancellationToken::new();
    for (name, goal, expect) in [
        ("echo", "echo:probe", "probe"),
        ("tool_round_trip", "tool:get_day_context", "call-1"),
        ("model_round_trip", "model", "ok"),
        ("trace_events", "trace", "traced"),
        ("stray_stdout", "stdout", "printed"),
    ] {
        let started = Instant::now();
        let outcome = sidecar.run(&task(goal), &host, &none).await;
        let answer = outcome
            .as_ref()
            .ok()
            .and_then(|outcome| outcome.result["answer"].as_str().map(str::to_string))
            .unwrap_or_default();
        record(
            name,
            answer.contains(expect),
            started.elapsed(),
            json!({ "answer": answer }),
        );
    }

    // Warm round trips, to see the steady-state cost of one message pair.
    let mut samples = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        let ok = sidecar.run(&task("echo:x"), &host, &none).await.is_ok();
        samples.push((ok, started.elapsed()));
    }
    samples.sort_by_key(|(_, elapsed)| *elapsed);
    record(
        "warm_run_median",
        samples.iter().all(|(ok, _)| *ok),
        samples[samples.len() / 2].1,
        json!({ "p90Ms": millis(samples[samples.len() * 9 / 10].1) }),
    );

    // Cancel: how quickly a cooperative run reports back.
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let outcome = sidecar.run(&task("sleep:30"), &host, &cancel).await;
    let stopped = outcome.as_ref().ok().and_then(|outcome| outcome.stopped);
    record(
        "cancel",
        stopped == Some(StopReason::Cancelled),
        started.elapsed().saturating_sub(Duration::from_millis(200)),
        json!({}),
    );

    // Crash, then the next run restarts the child.
    let crash = sidecar.run(&task("crash"), &host, &none).await;
    let started = Instant::now();
    let restarted = sidecar.run(&task("echo:again"), &host, &none).await;
    record(
        "crash_then_restart",
        matches!(crash, Err(SidecarError::Crashed { .. })) && restarted.is_ok(),
        started.elapsed(),
        json!({}),
    );

    // A message over 1 MiB ends the child instead of being buffered.
    let started = Instant::now();
    let oversize = sidecar.run(&task("oversize"), &host, &none).await;
    record(
        "oversize_refused",
        matches!(oversize, Err(SidecarError::Protocol(_))),
        started.elapsed(),
        json!({}),
    );

    // No environment leaks into the child.
    let outcome = sidecar.run(&task("env:PATH"), &host, &none).await;
    let answer = outcome
        .ok()
        .and_then(|outcome| outcome.result["answer"].as_str().map(str::to_string))
        .unwrap_or_default();
    record(
        "environment_isolated",
        answer == "absent",
        Duration::ZERO,
        json!({}),
    );

    sidecar.shutdown().await;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "mode": mode,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "agent": agent,
            "checks": checks,
        }))
        .unwrap()
    );
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
