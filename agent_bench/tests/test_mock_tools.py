"""The fixture-driven tools behave like the contract says."""

from agent_bench.mock_tools import MockToolClient, PlannerState, load_fixture, tool_get_day_context
from agent_bench.trace import TraceRecorder, validate_events

TZ = "America/New_York"


def make_case(**extra):
    base = {
        "case_id": "unit",
        "family": "tool_choice",
        "split": "dev",
        "fixture": {"version": "planner.v1"},
        "now": "2026-11-03T09:00-05:00",
        "timeZone": TZ,
        "permittedTools": ["get_day_context", "search_tasks", "get_project_context"],
        "goal": "unit",
        "expected": {"status": ["answered"]},
        "tags": [],
    }
    base.update(extra)
    return base


def test_dst_end_day_has_25_hours():
    state = PlannerState(load_fixture("planner.v1"))
    data = tool_get_day_context(state, {"date": "2026-11-01", "timeZone": TZ})
    assert data["dayLengthMinutes"] == 25 * 60
    assert data["events"][0]["start"] == "2026-11-01T01:30-05:00"


def test_free_ranges_skip_busy_time():
    tools = MockToolClient(make_case())
    data = tools.call("get_day_context", {"date": "2026-11-04", "timeZone": TZ})["data"]
    assert data["free"] == [
        {"start": "2026-11-04T10:30-05:00", "end": "2026-11-04T14:00-05:00"},
        {"start": "2026-11-04T15:00-05:00", "end": "2026-11-04T17:00-05:00"},
    ]


def test_invalid_arguments_are_rejected_and_logged():
    tools = MockToolClient(make_case())
    out = tools.call("search_tasks", {"query": "x", "limit": 500})
    assert out["ok"] is False and out["error"]["code"] == "invalid_arguments"
    assert tools.calls[0].args_valid is False


def test_unpermitted_and_unknown_tools_are_denied():
    tools = MockToolClient(make_case())
    assert tools.call("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ})["error"]["code"] == "permission_denied"
    assert tools.call("update_task", {"taskId": "t-qa"})["error"]["code"] == "permission_denied"
    assert [c.permitted for c in tools.calls] == [False, False]
    assert [c.known for c in tools.calls] == [True, False]


def test_injected_error_only_on_listed_calls():
    case = make_case(injectedErrors=[{"tool": "get_day_context", "onCalls": [1], "error": {"code": "timeout", "message": "t", "retryable": True}}])
    tools = MockToolClient(case)
    first = tools.call("get_day_context", {"date": "2026-11-03", "timeZone": TZ})
    second = tools.call("get_day_context", {"date": "2026-11-03", "timeZone": TZ})
    assert first["error"] == {"code": "timeout", "message": "t", "retryable": True}
    assert second["ok"] is True


def test_state_change_lands_after_the_named_call():
    case = make_case(stateChanges=[{"afterToolCall": 1, "changes": [{"entity": "tasks", "id": "t-qa", "fields": {"revision": 9}}]}])
    tools = MockToolClient(case)
    hit = tools.call("search_tasks", {"query": "QA", "limit": 5})["data"]["tasks"][0]
    assert hit["revision"] == 5
    again = tools.call("search_tasks", {"query": "QA", "limit": 5})["data"]["tasks"][0]
    assert again["revision"] == 9
    assert tools.current_revision("t-qa") == 9


def test_budget_exhausted_after_max_calls():
    tools = MockToolClient(make_case(), max_tool_calls=1)
    assert tools.call("search_tasks", {"limit": 1})["ok"]
    assert tools.call("search_tasks", {"limit": 1})["error"]["code"] == "budget_exhausted"


def test_results_are_copies_so_agents_cannot_mutate_state():
    tools = MockToolClient(make_case())
    data = tools.call("get_project_context", {"planId": "plan-launch"})["data"]
    data["tasks"][0]["revision"] = 999
    assert tools.current_revision(data["tasks"][0]["id"]) != 999


def test_trace_events_match_schema_and_skip_results_unless_debug():
    trace = TraceRecorder("run-1")
    tools = MockToolClient(make_case(), trace=trace)
    tools.call("get_project_context", {"planId": "plan-launch"})
    assert validate_events(trace.events) == []
    assert [e["kind"] for e in trace.events] == ["tool_call", "tool_result"]
    assert "result" not in trace.events[1]["data"]
    debug = TraceRecorder("run-2", debug=True)
    MockToolClient(make_case(), trace=debug).call("get_project_context", {"planId": "plan-launch"})
    assert "result" in debug.events[1]["data"]
