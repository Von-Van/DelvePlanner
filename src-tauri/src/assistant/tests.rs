use super::*;
use crate::db::PlannerDatabase;
use crate::sidecar::{RunBudget, RunRequest, StopReason};
use crate::tools::{open_fixture_database, NoCalendars, ToolSources, SAMPLE_FIXTURE};
use std::ffi::OsString;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MOCKUPS: &str = "00000000-0000-4000-8000-000000000401";

fn observed() -> Observed {
    let mut observed = Observed::default();
    observed.note_tool_result(&json!({
        "callId": "call-1",
        "tool": "search_tasks",
        "ok": true,
        "result": { "tasks": [
            { "id": "00000000-0000-4000-8000-0000000000a1", "revision": 4, "title": "Draft budget", "planId": "00000000-0000-4000-8000-0000000000b1" },
            { "id": "00000000-0000-4000-8000-0000000000a2", "revision": 1, "title": "Call venue" }
        ] }
    }));
    observed.note_tool_result(&json!({
        "callId": "call-2",
        "tool": "get_project_context",
        "ok": true,
        "result": { "plan": { "id": "00000000-0000-4000-8000-0000000000b1", "revision": 2, "title": "Grant" } }
    }));
    observed.note_tool_result(&json!({
        "callId": "call-3",
        "tool": "get_project_context",
        "ok": false,
        "error": { "code": "not_found", "message": "no", "retryable": false }
    }));
    observed
}

fn schedule(target: Value, evidence: Value) -> Value {
    json!({
        "op": "schedule_task",
        "target": target,
        "payload": {
            "scheduledDay": { "action": "set", "day": "2026-11-05" },
            "dueDate": { "action": "unchanged" },
            "plannedWeek": { "action": "unchanged" }
        },
        "evidence": evidence,
    })
}

#[test]
fn observed_records_come_only_from_successful_calls() {
    let observed = observed();
    assert!(observed.calls.contains("call-1") && observed.calls.contains("call-2"));
    assert!(!observed.calls.contains("call-3"));
    assert_eq!(
        observed.records["00000000-0000-4000-8000-0000000000a1"].revision,
        Some(4)
    );
    assert_eq!(
        observed.title("00000000-0000-4000-8000-0000000000b1"),
        Some("Grant")
    );
}

#[test]
fn a_grounded_draft_becomes_the_planners_own_operation() {
    let operation = draft_operation(
        &schedule(
            json!({ "id": "00000000-0000-4000-8000-0000000000a1", "revision": 4 }),
            json!(["call-1"]),
        ),
        &observed(),
    )
    .unwrap();
    match operation {
        MutationOperation::ScheduleTask {
            task_id,
            expected_revision,
            ..
        } => {
            assert_eq!(task_id, "00000000-0000-4000-8000-0000000000a1");
            assert_eq!(expected_revision, 4);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn ungrounded_or_malformed_drafts_are_refused() {
    let observed = observed();
    let target = json!({ "id": "00000000-0000-4000-8000-0000000000a1", "revision": 4 });
    let cases = [
        (
            "unseen record",
            schedule(
                json!({ "id": "00000000-0000-4000-8000-0000000000a9", "revision": 1 }),
                json!(["call-1"]),
            ),
        ),
        (
            "stale revision",
            schedule(
                json!({ "id": "00000000-0000-4000-8000-0000000000a1", "revision": 3 }),
                json!(["call-1"]),
            ),
        ),
        ("no evidence", schedule(target.clone(), json!([]))),
        ("failed call", schedule(target.clone(), json!(["call-3"]))),
        (
            "invented call",
            schedule(target.clone(), json!(["call-77"])),
        ),
        ("no target", schedule(Value::Null, json!(["call-1"]))),
        (
            "unknown op",
            json!({ "op": "set_time_block", "payload": { "x": 1 }, "evidence": ["call-1"] }),
        ),
        (
            "two records",
            json!({ "op": "delete_task", "target": target, "payload": { "taskId": "00000000-0000-4000-8000-0000000000a2" },
                    "evidence": ["call-1"] }),
        ),
        (
            "unknown field",
            json!({ "op": "delete_task", "target": target, "payload": { "force": true },
                    "evidence": ["call-1"] }),
        ),
        (
            "type override",
            json!({ "op": "delete_task", "target": target, "payload": { "type": "delete_event" },
                    "evidence": ["call-1"] }),
        ),
        (
            "create with a target",
            json!({ "op": "create_workstream", "target": target,
                    "payload": { "plan": { "id": "00000000-0000-4000-8000-0000000000b1" }, "name": "Ops" }, "evidence": [] }),
        ),
    ];
    for (name, draft) in cases {
        assert!(
            draft_operation(&draft, &observed).is_err(),
            "{name} should be refused"
        );
    }
}

#[test]
fn one_bad_draft_holds_back_the_whole_set() {
    let observed = observed();
    let good = schedule(
        json!({ "id": "00000000-0000-4000-8000-0000000000a1", "revision": 4 }),
        json!(["call-1"]),
    );
    let bad = schedule(
        json!({ "id": "00000000-0000-4000-8000-0000000000a2", "revision": 9 }),
        json!(["call-1"]),
    );
    let (check, operations) = check_drafts(&[good.clone(), bad], &observed);
    assert_eq!(
        (check.drafted, check.passed, check.problems.len()),
        (2, 1, 1)
    );
    assert!(operations.is_empty());
    let (check, operations) = check_drafts(&[good], &observed);
    assert_eq!((check.drafted, check.passed), (1, 1));
    assert_eq!(operations.len(), 1);
}

fn start() -> RunStart {
    RunStart {
        run_key: "key-1".into(),
        goal: "Move the budget to Thursday".into(),
        started_at: "2026-11-02T15:00:00Z".parse().unwrap(),
        model: None,
    }
}

fn outcome(result: Value) -> RunOutcome {
    RunOutcome {
        run_id: "run-1".into(),
        result,
        tool_calls: 1,
        model_calls: 2,
        duration: Duration::from_millis(1_250),
        stopped: None,
    }
}

#[test]
fn a_drafted_run_offers_its_changes_in_the_planners_review_shape() {
    let trace = vec![
        json!({ "kind": "tool_result", "data": { "ok": true } }),
        json!({ "kind": "tool_result", "data": { "ok": false } }),
    ];
    let (record, response) = finish_run(
        start(),
        Ok(outcome(json!({
            "status": "drafted",
            "answer": "Schedule the budget for Thursday.",
            "drafts": [schedule(json!({ "id": "00000000-0000-4000-8000-0000000000a1", "revision": 4 }), json!(["call-1"]))],
            "usage": { "promptTokens": 300, "completionTokens": 40 }
        }))),
        &observed(),
        trace,
        "2026-11-02T15:00:02Z".parse().unwrap(),
    );
    assert_eq!(record.status, "drafted");
    assert_eq!(record.review, ReviewState::Pending);
    assert_eq!(
        record.usage,
        RunUsage {
            model_calls: 2,
            tool_calls: 1,
            tool_failures: 1,
            prompt_tokens: 300,
            completion_tokens: 40,
            duration_ms: 1_250,
        }
    );
    assert_eq!(record.draft_check.as_ref().unwrap().passed, 1);
    let Some(PlannerResponse::Proposal {
        summary,
        operations,
        references,
        ..
    }) = &record.proposal
    else {
        panic!("expected a proposal");
    };
    assert_eq!(summary, "Schedule the budget for Thursday.");
    assert_eq!(operations.len(), 1);
    assert_eq!(references[0].title, "Draft budget");
    assert!(matches!(response, Some(ModelResponse::Proposal { .. })));
}

#[test]
fn a_run_that_never_finished_is_recorded_as_failed() {
    let (record, response) = finish_run(
        start(),
        Err(SidecarError::Cancelled),
        &Observed::default(),
        Vec::new(),
        "2026-11-02T15:00:01Z".parse().unwrap(),
    );
    assert_eq!(record.status, "failed");
    assert!(record.cancelled);
    assert_eq!(record.usage.duration_ms, 1_000);
    assert!(response.is_none());

    let (record, _) = finish_run(
        start(),
        Ok(RunOutcome {
            stopped: Some(StopReason::Cancelled),
            ..outcome(json!({ "status": "cannot_complete", "stopReason": "cancelled" }))
        }),
        &Observed::default(),
        Vec::new(),
        "2026-11-02T15:00:01Z".parse().unwrap(),
    );
    assert_eq!(record.status, "cannot_complete");
    assert!(record.cancelled);
}

fn runtime() -> AssistantRuntime {
    AssistantRuntime {
        sidecar: None,
        source: None,
        active: Mutex::new(None),
        runs: Mutex::new(VecDeque::new()),
        pending: Mutex::new(None),
    }
}

fn drafted(run_key: &str) -> (AgentRunRecord, Option<ModelResponse>) {
    finish_run(
        RunStart {
            run_key: run_key.into(),
            ..start()
        },
        Ok(outcome(json!({
            "status": "drafted",
            "drafts": [schedule(json!({ "id": "00000000-0000-4000-8000-0000000000a1", "revision": 4 }), json!(["call-1"]))],
        }))),
        &observed(),
        Vec::new(),
        Utc::now(),
    )
}

fn proposal_id(record: &AgentRunRecord) -> String {
    match &record.proposal {
        Some(PlannerResponse::Proposal { proposal_id, .. }) => proposal_id.clone(),
        _ => panic!("no proposal"),
    }
}

#[test]
fn a_finished_run_becomes_the_conversations_reply() {
    let (record, _) = drafted("key-reply");
    let (text, status) = conversation_reply(&record);
    assert_eq!(status, MessageRunStatus::Drafted);
    assert!(!text.is_empty());

    let (answered, _) = finish_run(
        start(),
        Ok(outcome(
            json!({ "status": "answered", "answer": "  Two meetings today.  " }),
        )),
        &Observed::default(),
        Vec::new(),
        "2026-11-02T15:00:01Z".parse().unwrap(),
    );
    assert_eq!(
        conversation_reply(&answered),
        (
            "Two meetings today.".to_string(),
            MessageRunStatus::Answered
        )
    );

    let (stopped, _) = finish_run(
        start(),
        Err(SidecarError::Cancelled),
        &Observed::default(),
        Vec::new(),
        "2026-11-02T15:00:01Z".parse().unwrap(),
    );
    assert_eq!(
        conversation_reply(&stopped),
        (
            "Stopped before finishing.".to_string(),
            MessageRunStatus::Cancelled
        )
    );
}

#[test]
fn deleting_a_conversation_forgets_its_runs_and_pending_changes() {
    let runtime = runtime();
    let (mut kept, response) = drafted("key-kept");
    kept.thread_id = Some("thread-kept".into());
    runtime.record(kept, response);
    let (mut gone, response) = drafted("key-gone");
    gone.thread_id = Some("thread-gone".into());
    runtime.record(gone, response);
    assert!(runtime.pending.lock().unwrap().is_some());

    runtime.forget_thread("thread-gone");
    let keys: Vec<_> = runtime.runs().into_iter().map(|run| run.run_key).collect();
    assert_eq!(keys, ["key-kept"]);
    // The newest changes belonged to the deleted conversation, so nothing is left to apply.
    assert!(runtime.pending.lock().unwrap().is_none());
}

#[test]
fn changes_can_be_applied_once_and_newer_changes_replace_older_ones() {
    let runtime = runtime();
    let (record, response) = drafted("first");
    let first = proposal_id(&runtime.record(record, response));
    let (record, response) = drafted("second");
    let second = proposal_id(&runtime.record(record, response));

    assert!(runtime.claim(&first).is_err(), "older changes are spent");
    let runs = runtime.runs();
    assert_eq!(runs[0].run_key, "second");
    assert_eq!(runs[1].review, ReviewState::Expired);

    assert!(runtime.claim(&second).is_ok());
    assert!(runtime.claim(&second).is_err(), "applied only once");
    assert!(
        runtime.discard(&second).is_err(),
        "a claimed set can't be discarded"
    );
    runtime.finish_review(&second, Ok(1));
    assert_eq!(
        runtime.runs()[0].review,
        ReviewState::Applied { changes: 1 }
    );
    assert!(runtime.runs()[0].proposal.is_none());
}

#[test]
fn changes_the_planner_would_refuse_are_never_offered() {
    // Well formed and grounded, but the planner's own checks reject an id that isn't one of its
    // record ids, so nothing reaches review.
    let mut observed = Observed::default();
    observed.note_tool_result(&json!({
        "callId": "call-1", "ok": true,
        "result": { "tasks": [{ "id": "task-1", "revision": 1, "title": "Odd" }] }
    }));
    let (record, response) = finish_run(
        start(),
        Ok(outcome(json!({
            "status": "drafted",
            "drafts": [schedule(json!({ "id": "task-1", "revision": 1 }), json!(["call-1"]))],
        }))),
        &observed,
        Vec::new(),
        Utc::now(),
    );
    assert!(response.is_none() && record.proposal.is_none());
    let check = record.draft_check.unwrap();
    assert_eq!(check.passed, 0);
    assert_eq!(check.problems.len(), 1);
}

#[test]
fn only_one_run_at_a_time() {
    let runtime = runtime();
    let token = runtime.begin().unwrap();
    assert!(runtime.begin().is_err());
    runtime.cancel();
    assert!(token.is_cancelled());
    runtime.end();
    assert!(runtime.begin().is_ok());
}

// ------------------------------------------------------------------------------------------------
// End to end: the real Python agent, the real supervisor, tools on a fixture database, and a
// scripted stand-in for Ollama behind the real model broker.

fn python() -> Option<PathBuf> {
    let candidates: Vec<OsString> = match std::env::var_os("DELVEOPS_PYTHON") {
        Some(path) => vec![path],
        None => vec!["python3".into(), "python".into()],
    };
    candidates.into_iter().find_map(|candidate| {
        let output = std::process::Command::new(&candidate)
            .args(["-c", "import sys; print(sys.version_info >= (3, 11))"])
            .output()
            .ok()?;
        (String::from_utf8_lossy(&output.stdout).trim() == "True").then(|| candidate.into())
    })
}

/// Answers each model request with the next scripted step, as Ollama's chat endpoint would.
async fn scripted_ollama(steps: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for step in steps {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 8192];
            let body_start = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..body_start]).to_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|value| value.trim().parse().unwrap())
                .unwrap_or(0);
            while request.len() < body_start + length {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
            }
            let body = json!({
                "model": "test-model",
                "message": { "role": "assistant", "content": step.to_string() },
                "done": true,
                "done_reason": "stop",
                "prompt_eval_count": 120,
                "eval_count": 30
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    endpoint
}

struct Fixture {
    _dir: TempDir,
    database: Mutex<PlannerDatabase>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let database =
        open_fixture_database(&dir.path().join("fixture.sqlite3"), SAMPLE_FIXTURE).unwrap();
    Fixture {
        _dir: dir,
        database: Mutex::new(database),
    }
}

fn read_from(
    fixture: &Fixture,
) -> impl Fn(&mut ToolSession, &str, Value) -> ToolOutcome + Sync + '_ {
    move |session, tool, arguments| {
        let database = fixture.database.lock().unwrap();
        let sources = ToolSources {
            database: &database,
            calendars: &NoCalendars,
        };
        session.call(&sources, tool, arguments)
    }
}

#[tokio::test]
async fn the_python_agent_reads_drafts_and_its_change_applies_through_the_planner() {
    let Some(python) = python() else {
        eprintln!("skipping: no Python 3.11+ found (set DELVEOPS_PYTHON)");
        return;
    };
    let agent_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-python");
    let sidecar = Sidecar::new(
        SidecarCommand::python_source(python, agent_dir, RUNNER),
        SidecarLimits::default(),
    );
    let endpoint = scripted_ollama(vec![
        json!({ "action": "call_tool", "tool": "search_tasks",
                "arguments": { "query": "homepage mockups", "limit": 5 } }),
        json!({ "action": "draft", "summary": "Mark the mockups done.", "drafts": [{
            "op": "update_task",
            "target": { "id": MOCKUPS, "revision": 1 },
            "payload": { "status": "done", "estimate": { "action": "unchanged" } },
            "evidence": ["tc-1"]
        }] }),
    ])
    .await;
    let identity = ModelIdentity {
        name: "test-model".into(),
        digest: "b".repeat(64),
        ollama_version: "0.0.0".into(),
    };
    let broker = ModelBroker::new(&endpoint, identity.clone()).unwrap();
    let fixture = fixture();
    let now: DateTime<Utc> = "2026-11-02T15:00:00Z".parse().unwrap();
    let streamed = Mutex::new(Vec::new());
    let host = RunHost::new(
        SessionConfig::new(now, "America/Chicago".parse().unwrap()),
        read_from(&fixture),
        Some(broker),
        |event: &Value| streamed.lock().unwrap().push(event.clone()),
    );
    let request = RunRequest {
        goal: "I finished the homepage mockups.".into(),
        now: "2026-11-02T09:00:00-06:00".into(),
        time_zone: "America/Chicago".into(),
        permitted_tools: permitted_tools(),
        architecture: Some(ARCHITECTURE.into()),
        budget: RunBudget::default(),
        conversation: Vec::new(),
        conversation_summary: None,
    };
    let outcome = sidecar
        .run(&request, &host, &CancellationToken::new())
        .await;
    sidecar.shutdown().await;
    let (observed, trace) = host.into_parts();
    let (mut record, response) = finish_run(
        RunStart {
            run_key: "key".into(),
            goal: request.goal.clone(),
            started_at: now,
            model: Some(identity),
        },
        outcome,
        &observed,
        trace,
        Utc::now(),
    );

    assert_eq!(record.status, "drafted", "{record:?}");
    assert_eq!(record.usage.tool_calls, 1);
    assert_eq!(record.usage.model_calls, 2);
    assert_eq!(record.usage.prompt_tokens, 240);
    let kinds: Vec<&str> = record
        .trace
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "plan",
            "model_call",
            "tool_call",
            "tool_result",
            "model_call",
            "draft",
            "final"
        ]
    );
    assert_eq!(streamed.lock().unwrap().len(), record.trace.len());
    let Some(PlannerResponse::Proposal { references, .. }) = &record.proposal else {
        panic!("expected changes to review: {:?}", record.draft_check);
    };
    assert_eq!(references[0].title, "Finalize homepage mockups");

    // The run, every step and the offered draft go into the run history.
    store_run(
        &mut fixture.database.lock().unwrap(),
        &mut record,
        &json!({}),
    )
    .unwrap();
    let stored_id = record.stored_run_id.clone().unwrap();
    let detail = fixture
        .database
        .lock()
        .unwrap()
        .agent_run_detail(&stored_id)
        .unwrap();
    assert_eq!(detail.steps.len(), record.trace.len());
    assert_eq!(detail.run.usage.model_calls, 2);
    assert_eq!(detail.run.model_name, "test-model");
    assert_eq!(detail.drafts.len(), 1);
    assert_eq!(record.stored_drafts.len(), 1);

    // Nothing has changed yet; applying goes through the planner's own code.
    let applied = fixture
        .database
        .lock()
        .unwrap()
        .apply_proposal(&response.unwrap())
        .unwrap();
    assert_eq!(applied.task_ids, [MOCKUPS]);
    store_decisions(
        &mut fixture.database.lock().unwrap(),
        &record.stored_drafts,
        None,
        Some(&Ok(serde_json::to_value(&applied).unwrap())),
    )
    .unwrap();
    let detail = fixture
        .database
        .lock()
        .unwrap()
        .agent_run_detail(&stored_id)
        .unwrap();
    assert_eq!(detail.drafts[0].state, crate::db::DraftState::Approved);
    assert_eq!(
        detail.drafts[0].result.as_ref().unwrap().execution_state,
        crate::db::ExecutionState::Applied
    );
    let mut session = ToolSession::new(SessionConfig::new(now, "America/Chicago".parse().unwrap()));
    let after = read_from(&fixture)(
        &mut session,
        "search_tasks",
        json!({ "query": "homepage mockups", "limit": 5 }),
    );
    let task = &after.result.unwrap()["tasks"][0];
    assert_eq!(task["status"], "done");
    assert_eq!(task["revision"], 2);
}
