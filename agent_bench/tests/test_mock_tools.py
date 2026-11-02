"""The fixture-driven tools follow contracts/tools.v1.json and the Rust broker's rules."""

import pytest

from agent_bench.jsonschema_lite import validate
from agent_bench.mock_tools import MockToolClient, PlannerState, Session, load_contract, load_fixture, tool_get_day_context, tool_schema
from agent_bench.trace import TraceRecorder, validate_events

TZ = "America/New_York"
ID = load_fixture("planner.v2")["ids"]
CONTRACT = load_contract()
ALL_TOOLS = [t["name"] for t in CONTRACT["tools"]]


def make_case(**extra):
    base = {
        "case_id": "unit",
        "family": "tool_choice",
        "split": "dev",
        "fixture": {"version": "planner.v2"},
        "now": "2026-11-03T09:00-05:00",
        "timeZone": TZ,
        "permittedTools": ["get_day_context", "search_tasks", "get_project_context"],
        "goal": "unit",
        "expected": {"status": ["answered"]},
        "tags": [],
    }
    base.update(extra)
    return base


CALLS = [
    ("get_day_context", {"date": d, "timeZone": TZ}) for d in ("2026-11-01", "2026-11-03", "2026-11-04", "2026-11-05", "2026-11-06")
] + [
    ("list_active_projects", {}),
    ("list_active_projects", {"includePlanning": True}),
    ("get_project_context", {"planId": ID["plan-launch"]}),
    ("get_project_context", {"planId": ID["plan-move"]}),
    ("search_tasks", {"query": "review"}),
    ("search_tasks", {"dueBefore": "2026-11-06", "limit": 20}),
    ("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ}),
    ("get_recent_actions", {}),
]


@pytest.mark.parametrize("privacy", [False, True])
@pytest.mark.parametrize("name,args", CALLS, ids=[f"{n}-{i}" for i, (n, _) in enumerate(CALLS)])
def test_results_match_the_contract(name, args, privacy):
    tools = MockToolClient(make_case(permittedTools=ALL_TOOLS, privacy={"includeDescriptions": privacy}))
    envelope = tools.call(name, args)
    assert validate(envelope, tool_schema(CONTRACT, {"$ref": "#/$defs/toolResult"})) == []
    assert envelope["ok"], envelope
    spec = next(t for t in CONTRACT["tools"] if t["name"] == name)
    assert validate(envelope["result"], tool_schema(CONTRACT, spec["outputSchema"])) == []


def test_descriptions_only_with_privacy_allowed():
    hidden = MockToolClient(make_case()).call("get_project_context", {"planId": ID["plan-launch"]})["result"]
    shown = MockToolClient(make_case(privacy={"includeDescriptions": True})).call("get_project_context", {"planId": ID["plan-launch"]})["result"]
    assert "description" not in hidden["plan"]
    assert shown["plan"]["description"] == "Move the marketing site to the new design."


def test_dst_end_day_event_is_reported_in_utc():
    state = PlannerState(load_fixture("planner.v2"))
    data = tool_get_day_context(state, {"date": "2026-11-01", "timeZone": TZ}, Session(now=None, time_zone=TZ, privacy={}))
    assert data["events"][0]["startAtUtc"] == "2026-11-01T06:30:00.000Z"  # 1:30 AM EST, after the clocks fall back


def test_time_blocks_are_blocked_not_busy():
    tools = MockToolClient(make_case())
    data = tools.call("get_day_context", {"date": "2026-11-04", "timeZone": TZ})["result"]
    assert data["availableMinutes"] == 420  # only Design review is busy
    assert data["free"] == [
        {"startAtUtc": "2026-11-04T15:30:00.000Z", "endAtUtc": "2026-11-04T19:00:00.000Z"},
        {"startAtUtc": "2026-11-04T20:00:00.000Z", "endAtUtc": "2026-11-04T22:00:00.000Z"},
    ]


def test_task_scheduled_on_its_due_day_is_not_also_listed_as_due():
    data = MockToolClient(make_case()).call("get_day_context", {"date": "2026-11-04", "timeZone": TZ})["result"]
    assert [t["title"] for t in data["scheduledTasks"]] == ["Review mockups"]
    assert data["dueTasks"] == []


def test_invalid_arguments_are_rejected_and_logged():
    tools = MockToolClient(make_case())
    out = tools.call("search_tasks", {"query": "x", "limit": 500})
    assert out["ok"] is False and out["error"]["code"] == "invalid_arguments"
    assert tools.calls[0].args_valid is False
    assert tools.call("get_project_context", {"planId": "plan-launch"})["error"]["code"] == "invalid_arguments"


def test_admission_order_matches_the_broker():
    tools = MockToolClient(make_case(permittedTools=["get_day_context", "get_memory"]))
    assert tools.call("update_task", {})["error"]["code"] == "invalid_arguments"  # no such tool
    assert tools.call("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ})["error"]["code"] == "permission_denied"
    assert tools.call("get_memory", {"limit": 0})["error"]["code"] == "invalid_arguments"  # permitted, bad args
    assert tools.call("get_memory", {})["ok"] is True
    assert [c.known for c in tools.calls] == [False, True, True, True]


def test_refused_calls_do_not_use_the_budget():
    tools = MockToolClient(make_case(), max_tool_calls=1)
    tools.call("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ})  # not permitted
    first = tools.call("search_tasks", {"limit": 1})
    assert first["ok"] and first["callsRemaining"] == 0
    assert tools.call("search_tasks", {"limit": 1})["error"]["code"] == "budget_exhausted"


def test_identical_failures_are_cut_off():
    tools = MockToolClient(make_case())
    bad = {"date": "2026-13-01", "timeZone": TZ}
    codes = [tools.call("get_day_context", bad)["error"]["code"] for _ in range(3)]
    assert codes == ["invalid_arguments", "invalid_arguments", "budget_exhausted"]


def test_injected_error_only_on_listed_calls():
    case = make_case(injectedErrors=[{"tool": "get_day_context", "onCalls": [1], "error": {"code": "timeout", "message": "t", "retryable": True}}])
    tools = MockToolClient(case)
    first = tools.call("get_day_context", {"date": "2026-11-03", "timeZone": TZ})
    second = tools.call("get_day_context", {"date": "2026-11-03", "timeZone": TZ})
    assert first["error"] == {"code": "timeout", "message": "t", "retryable": True}
    assert second["ok"] is True


def test_state_change_lands_after_the_named_call():
    case = make_case(stateChanges=[{"afterToolCall": 1, "changes": [{"entity": "tasks", "id": ID["t-qa"], "fields": {"revision": 9}}]}])
    tools = MockToolClient(case)
    assert tools.call("search_tasks", {"query": "QA"})["result"]["tasks"][0]["revision"] == 5
    assert tools.call("search_tasks", {"query": "QA"})["result"]["tasks"][0]["revision"] == 9
    assert tools.current_revision(ID["t-qa"]) == 9


def test_results_are_copies_so_agents_cannot_mutate_state():
    tools = MockToolClient(make_case())
    data = tools.call("get_project_context", {"planId": ID["plan-launch"]})["result"]
    data["tasks"][0]["revision"] = 999
    assert tools.current_revision(data["tasks"][0]["id"]) != 999


def test_trace_events_match_schema_and_skip_results_unless_debug():
    trace = TraceRecorder("run-1")
    MockToolClient(make_case(), trace=trace).call("get_project_context", {"planId": ID["plan-launch"]})
    assert validate_events(trace.events) == []
    assert [e["kind"] for e in trace.events] == ["tool_call", "tool_result"]
    assert "result" not in trace.events[1]["data"]
    debug = TraceRecorder("run-2", debug=True)
    MockToolClient(make_case(), trace=debug).call("get_project_context", {"planId": ID["plan-launch"]})
    assert "result" in debug.events[1]["data"]
