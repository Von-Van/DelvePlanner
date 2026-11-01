from __future__ import annotations

import json

from conftest import FakeClock, ScriptedModel

from delveops_agent import V0_CATALOG, A1Agent, AgentTask, Budget, MockToolClient, ToolError, mutation_operation
from delveops_agent.model import ModelError
from delveops_agent.trace import TRACE_KINDS

ALL_TOOLS = (
    "get_day_context",
    "list_active_projects",
    "get_project_context",
    "search_tasks",
    "get_capacity",
    "get_recent_actions",
)

TASK_SEARCH = {
    "tasks": [
        {"id": "task-42", "revision": 3, "planId": "plan-7", "milestoneId": "ms-2", "title": "Draft grant budget"}
    ]
}


def make_task(tools=ALL_TOOLS, goal="Schedule my grant budget task for Thursday.") -> AgentTask:
    return AgentTask(goal=goal, now="2026-11-03T09:00:00-06:00", time_zone="America/Chicago", permitted_tools=tools)


def search(query="grant"):
    return {"action": "call_tool", "tool": "search_tasks", "arguments": {"query": query, "limit": 5}}


def run(script, responses=None, task=None, budget=None, clock=None, debug=False):
    model = ScriptedModel(script)
    tools = MockToolClient(responses if responses is not None else {"search_tasks": TASK_SEARCH})
    agent = A1Agent(catalog=V0_CATALOG, debug_trace=debug, clock=clock or FakeClock())
    result = agent.run(task or make_task(), tools, model, budget or Budget())
    return result, tools, model


SCHEDULE_DRAFT = {
    "action": "draft",
    "summary": "Schedule Draft grant budget for Thursday.",
    "drafts": [
        {
            "op": "schedule_task",
            "target": {"id": "task-42", "revision": 3},
            "payload": {"scheduledDay": {"action": "set", "day": "2026-11-05"}},
            "evidence": ["tc-1"],
        }
    ],
}


# -- happy paths ----------------------------------------------------------------------------------


def test_answers_after_reading_a_tool():
    result, tools, model = run([search(), {"action": "answer", "answer": "It is due 2026-10-31."}])
    assert result.status == "answered"
    assert result.answer == "It is due 2026-10-31."
    assert tools.calls == [("search_tasks", {"query": "grant", "limit": 5})]
    assert result.usage.model_calls == 2 and result.usage.tool_calls == 1
    assert result.usage.prompt_tokens == 200
    # The real tool output was shown to the model before its second step.
    assert "task-42" in model.requests[1].messages[-1]["content"]


def test_grounded_draft_is_returned_not_applied():
    result, tools, _ = run([search(), SCHEDULE_DRAFT])
    assert result.status == "drafted"
    assert result.stop_reason is None
    assert [d.to_dict() for d in result.drafts] == [
        {
            "op": "schedule_task",
            "target": {"id": "task-42", "revision": 3},
            "payload": {
                "scheduledDay": {"action": "set", "day": "2026-11-05"},
                "dueDate": {"action": "unchanged"},
                "plannedWeek": {"action": "unchanged"},
            },
            "evidence": ["tc-1"],
        }
    ]
    # The exact JSON Rust's MutationOperation deserialises.
    assert mutation_operation(result.drafts[0]) == {
        "type": "schedule_task",
        "taskId": "task-42",
        "expectedRevision": 3,
        "scheduledDay": {"action": "set", "day": "2026-11-05"},
        "dueDate": {"action": "unchanged"},
        "plannedWeek": {"action": "unchanged"},
    }
    # Only read tools were ever invoked; drafting did not call anything.
    assert [name for name, _ in tools.calls] == ["search_tasks"]


def test_create_draft_needs_no_target():
    draft = {
        "action": "draft",
        "summary": "Add a task.",
        "drafts": [
            {
                "op": "create_task",
                "target": None,
                "payload": {"title": "Call Sam", "description": "", "status": "todo", "priority": "normal"},
                "evidence": [],
            }
        ],
    }
    result, tools, _ = run([draft])
    assert result.status == "drafted"
    assert result.drafts[0].target is None
    assert tools.calls == []


def test_result_serialises_to_contract_shape():
    result, _, _ = run([search(), SCHEDULE_DRAFT])
    data = json.loads(json.dumps(result.to_dict()))
    assert set(data) >= {"status", "answer", "drafts", "trace", "usage", "runId", "architectureId", "model"}
    assert data["architectureId"] == "A1"
    assert data["model"]["digest"] == "sha256:test"
    assert set(data["usage"]) == {"modelCalls", "toolCalls", "tokens", "latencyMs"}


# -- draft grounding ------------------------------------------------------------------------------


def _bad_draft(**overrides):
    draft = json.loads(json.dumps(SCHEDULE_DRAFT))
    draft["drafts"][0].update(overrides)
    return draft


def test_invented_target_is_repaired_then_accepted():
    result, _, model = run([search(), _bad_draft(target={"id": "task-999", "revision": 1}), SCHEDULE_DRAFT])
    assert result.status == "drafted"
    assert "was not seen in any tool result" in model.requests[2].messages[-1]["content"]
    assert [e["kind"] for e in result.trace].count("repair") == 1


def test_stale_revision_is_rejected():
    result, _, _ = run(
        [search(), _bad_draft(target={"id": "task-42", "revision": 2}), _bad_draft(target={"id": "task-42"})]
    )
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_draft"
    assert result.drafts == []


def test_evidence_must_be_a_real_tool_call():
    result, _, _ = run([search(), _bad_draft(evidence=["tc-7"]), _bad_draft(evidence=[])])
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_draft"


def test_unsupported_op_is_rejected():
    result, _, _ = run([search(), _bad_draft(op="set_task_status"), _bad_draft(op="create_task_block")])
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_draft"


def test_target_without_a_seen_revision_is_rejected():
    responses = {"search_tasks": {"tasks": [{"id": "task-42", "title": "no revision here"}]}}
    bad = _bad_draft(target={"id": "task-42", "revision": 3})
    result, _, model = run([search(), bad, bad], responses=responses)
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_draft"
    assert "no revision was seen" in model.requests[2].messages[-1]["content"]


def test_reread_refreshes_the_revision_a_draft_must_cite():
    responses = {
        "search_tasks": [
            {"tasks": [{"id": "task-42", "revision": 3}]},
            {"tasks": [{"id": "task-42", "revision": 4}]},
        ]
    }
    fresh = _bad_draft(target={"id": "task-42", "revision": 4}, evidence=["tc-2"])
    result, _, _ = run([search(), search("again"), fresh], responses=responses)
    assert result.status == "drafted"
    assert result.drafts[0].target == {"id": "task-42", "revision": 4}


def test_summary_without_revision_does_not_erase_a_seen_revision():
    responses = {
        "search_tasks": TASK_SEARCH,
        "list_active_projects": {"plans": [{"id": "plan-7", "tasks": [{"id": "task-42"}]}]},
    }
    projects = {"action": "call_tool", "tool": "list_active_projects", "arguments": {}}
    result, _, _ = run([search(), projects, SCHEDULE_DRAFT], responses=responses)
    assert result.status == "drafted"


# -- stop conditions ------------------------------------------------------------------------------


def test_tool_outside_permitted_set_is_never_called():
    task = make_task(tools=("search_tasks",))
    step = {"action": "call_tool", "tool": "get_capacity", "arguments": {"weekStart": "2026-11-02", "timeZone": "UTC"}}
    result, tools, _ = run([step], task=task, responses={"get_capacity": {}})
    assert result.status == "cannot_complete"
    assert result.stop_reason == "tool_not_permitted"
    assert tools.calls == []


def test_unknown_tool_is_never_called():
    result, tools, _ = run([{"action": "call_tool", "tool": "run_shell", "arguments": {"cmd": "rm -rf /"}}])
    assert result.stop_reason == "tool_not_permitted"
    assert tools.calls == []


def test_only_permitted_tools_are_offered_in_the_prompt():
    _, _, model = run([{"action": "answer", "answer": "hi"}], task=make_task(tools=("search_tasks",)))
    system = model.requests[0].messages[0]["content"]
    assert "search_tasks(" in system
    assert "get_capacity(" not in system


def test_repeated_identical_tool_error_stops():
    err = ToolError("not_found", "no such plan", retryable=False)
    step = {"action": "call_tool", "tool": "get_project_context", "arguments": {"planId": "plan-x"}}
    result, tools, _ = run([step, step, step], responses={"get_project_context": err})
    assert result.status == "cannot_complete"
    assert result.stop_reason == "repeated_tool_error"
    assert len(tools.calls) == 2


def test_different_errors_feed_back_and_continue():
    responses = {"get_project_context": [ToolError("not_found", "no such plan"), {"plan": {"id": "plan-7"}}]}
    a = {"action": "call_tool", "tool": "get_project_context", "arguments": {"planId": "plan-x"}}
    b = {"action": "call_tool", "tool": "get_project_context", "arguments": {"planId": "plan-7"}}
    result, _, model = run([a, b, {"action": "answer", "answer": "Found it."}], responses=responses)
    assert result.status == "answered"
    assert "not_found" in model.requests[1].messages[-1]["content"]


def test_permission_denied_stops():
    result, _, _ = run([search()], responses={"search_tasks": ToolError("permission_denied", "nope")})
    assert result.status == "cannot_complete"
    assert result.stop_reason == "permission_denied"


def test_ambiguous_target_asks_for_clarification():
    err = ToolError("ambiguous", "Two tasks match 'grant': Draft grant budget, Grant report.")
    result, _, _ = run([search()], responses={"search_tasks": err})
    assert result.status == "needs_clarification"
    assert result.stop_reason == "ambiguous_target"
    assert "Two tasks match" in result.answer


def test_invalid_arguments_are_rejected_locally_without_spending_tool_budget():
    bad = {"action": "call_tool", "tool": "search_tasks", "arguments": {"query": "grant", "limit": 500}}
    result, tools, model = run([bad, search(), {"action": "answer", "answer": "ok"}])
    assert result.status == "answered"
    assert tools.calls == [("search_tasks", {"query": "grant", "limit": 5})]
    assert result.usage.tool_calls == 1
    assert "at most 50" in model.requests[1].messages[-1]["content"]


def test_model_call_budget():
    result, _, _ = run([search(), search("b"), search("c")], budget=Budget(max_model_calls=2))
    assert result.status == "cannot_complete"
    assert result.stop_reason == "model_call_budget"
    assert result.usage.model_calls == 2


def test_tool_call_budget():
    result, tools, _ = run([search(), search("b"), search("c")], budget=Budget(max_tool_calls=2))
    assert result.stop_reason == "tool_call_budget"
    assert len(tools.calls) == 2


def test_wall_clock_budget(clock):
    def tick():
        clock.now += 40

    model = ScriptedModel([search(), search("b"), search("c"), search("d")], on_call=tick)
    agent = A1Agent(catalog=V0_CATALOG, clock=clock)
    result = agent.run(make_task(), MockToolClient({"search_tasks": TASK_SEARCH}), model, Budget(max_wall_seconds=100))
    assert result.status == "cannot_complete"
    assert result.stop_reason == "wall_clock_budget"
    assert len(model.requests) == 3
    assert result.usage.wall_ms == 120_000


def test_context_budget():
    big = {"tasks": [{"id": f"task-{i}", "title": "x" * 200} for i in range(50)]}
    result, _, _ = run([search(), search("b")], responses={"search_tasks": big}, budget=Budget(max_context_tokens=1500))
    assert result.stop_reason == "context_budget"


def test_unparseable_output_gets_one_repair_then_stops():
    result, _, model = run(["not json", "{still not"])
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_model_output"
    assert "not one valid JSON step" in model.requests[1].messages[-1]["content"]


def test_repair_then_success():
    result, _, _ = run(["oops", {"action": "answer", "answer": "Done."}])
    assert result.status == "answered"


def test_model_failure_is_cannot_complete():
    result, _, _ = run([ModelError("Ollama is not reachable")])
    assert result.status == "cannot_complete"
    assert result.stop_reason == "model_error"


def test_model_can_ask_or_decline():
    asked, _, _ = run([{"action": "clarify", "question": "Which Thursday?"}])
    assert (asked.status, asked.answer, asked.stop_reason) == ("needs_clarification", "Which Thursday?", "model_asked")
    declined, _, _ = run([{"action": "cannot_complete", "reason": "No tool can see email."}])
    assert (declined.status, declined.stop_reason) == ("cannot_complete", "model_declined")


def test_crashing_tool_client_is_a_tool_error_not_a_crash():
    def boom(_args):
        raise RuntimeError("socket closed")

    result, _, model = run([search(), {"action": "answer", "answer": "Sorry."}], responses={"search_tasks": boom})
    assert result.status == "answered"
    assert "internal" in model.requests[1].messages[-1]["content"]


# -- tracing --------------------------------------------------------------------------------------


def test_trace_shape_and_order():
    result, _, _ = run([search(), SCHEDULE_DRAFT])
    kinds = [e["kind"] for e in result.trace]
    assert kinds == ["plan", "model_call", "tool_call", "tool_result", "model_call", "draft", "final"]
    assert [e["seq"] for e in result.trace] == list(range(1, len(kinds) + 1))
    for event in result.trace:
        assert set(event) == {"runId", "seq", "kind", "at", "durationMs", "summary", "data"}
        assert event["runId"] == result.run_id
        assert event["kind"] in TRACE_KINDS
    assert result.trace[1]["data"]["digest"] == "sha256:test"
    assert result.trace[-1]["data"]["status"] == "drafted"


def test_routine_trace_has_no_prompt_or_result_text():
    result, _, _ = run([search(), {"action": "answer", "answer": "secret answer"}])
    dumped = json.dumps(result.trace)
    assert "Draft grant budget" not in dumped
    assert "Schedule my grant budget" not in dumped
    assert "debug" not in dumped


def test_routine_trace_has_no_tool_argument_values_or_error_messages():
    private = "dentist appointment for Riley"
    step = {"action": "call_tool", "tool": "search_tasks", "arguments": {"query": private, "limit": 5}}
    err = ToolError("not_found", f"nothing matches {private}")
    result, _, _ = run([step, {"action": "answer", "answer": "none"}], responses={"search_tasks": err})
    dumped = json.dumps(result.trace)
    assert private not in dumped
    call = next(e for e in result.trace if e["kind"] == "tool_call")
    assert call["data"]["argumentNames"] == ["limit", "query"]


def test_model_calls_get_the_remaining_wall_clock_budget(clock):
    def tick():
        clock.now += 30

    model = ScriptedModel([search(), search("b"), {"action": "answer", "answer": "ok"}], on_call=tick)
    agent = A1Agent(catalog=V0_CATALOG, clock=clock)
    agent.run(make_task(), MockToolClient({"search_tasks": TASK_SEARCH}), model, Budget(max_wall_seconds=100))
    assert [r.timeout_seconds for r in model.requests] == [100, 70, 40]


def test_debug_trace_includes_text_when_opted_in():
    result, _, _ = run([search(), {"action": "answer", "answer": "ok"}], debug=True)
    dumped = json.dumps(result.trace)
    assert "Draft grant budget" in dumped
