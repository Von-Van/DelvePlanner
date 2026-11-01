"""The mock tools give the same answers as the real Rust tools on the benchmark fixture.

Runs only when the ``tool_server`` example is built (``cargo build --example tool_server`` in
``src-tauri/``, or ``AGENT_BENCH_TOOL_SERVER=/path/to/tool_server``); otherwise it is skipped, unless
``AGENT_BENCH_REQUIRE_TOOL_SERVER`` is set (CI's parity job), when a missing server is an error.
The real server has no read-only calendars, so both sides run without calendar busy times.
"""

import json
import os

import pytest

from agent_bench import mock_tools
from agent_bench.cases import load_cases
from agent_bench.live_tools import LiveToolClient, find_tool_server
from agent_bench.mock_tools import MockToolClient, load_contract

if os.environ.get("AGENT_BENCH_REQUIRE_TOOL_SERVER") and find_tool_server() is None:
    raise RuntimeError("AGENT_BENCH_REQUIRE_TOOL_SERVER is set but tool_server is not built")
pytestmark = pytest.mark.skipif(find_tool_server() is None, reason="tool_server is not built")

TZ = "America/New_York"
ID = mock_tools.load_fixture("planner.v2")["ids"]
ALL_TOOLS = [t["name"] for t in load_contract()["tools"]]

PROBES = [
    ("get_day_context", {"date": d, "timeZone": TZ}) for d in ("2026-11-01", "2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05", "2026-11-06", "2026-11-07")
] + [
    ("list_active_projects", {}),
    ("list_active_projects", {"includePlanning": True}),
    ("get_project_context", {"planId": ID["plan-launch"]}),
    ("get_project_context", {"planId": ID["plan-move"]}),
    ("get_project_context", {"planId": ID["plan-garage"]}),
    ("get_project_context", {"planId": "00000000-0000-4000-8000-000000000999"}),
    ("get_project_context", {"planId": "plan-launch"}),
    ("search_tasks", {}),
    ("search_tasks", {"query": "review"}),
    ("search_tasks", {"query": "REVIEW lease"}),
    ("search_tasks", {"dueBefore": "2026-11-06", "limit": 20}),
    ("search_tasks", {"status": "done"}),
    ("search_tasks", {"planId": ID["plan-move"], "limit": 2}),
    ("search_tasks", {"query": "x", "limit": 500}),
    ("search_tasks", {"bogus": 1}),
    ("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ}),
    ("get_capacity", {"weekStart": "2026-10-26", "timeZone": TZ}),
    ("get_recent_actions", {}),
    ("get_memory", {}),
    ("get_preferences", {}),
    ("get_preferences", {"extra": True}),
    ("update_task", {"taskId": ID["t-qa"]}),
    ("get_day_context", {"date": "2026-13-01", "timeZone": TZ}),
    ("get_day_context", {"date": "2026-13-01", "timeZone": TZ}),
    ("get_day_context", {"date": "2026-13-01", "timeZone": TZ}),
]


@pytest.fixture(autouse=True)
def no_calendars(monkeypatch):
    original = mock_tools.load_fixture
    monkeypatch.setattr(mock_tools, "load_fixture", lambda version: {**original(version), "externalBusy": []})


def comparable(envelope: dict) -> dict:
    """Drop what legitimately differs: timings, byte counts, error wording, and the import time."""
    out = {k: v for k, v in envelope.items() if k not in ("durationMs", "resultBytes")}
    if "error" in out:
        out["error"] = {k: v for k, v in out["error"].items() if k != "message"}
    hours = (out.get("result") or {}).get("workingHours")
    if isinstance(hours, dict):
        hours.pop("updatedAt", None)
    return json.loads(json.dumps(out, sort_keys=True))


def run_both(case: dict, calls: list) -> list[tuple]:
    mock = MockToolClient(case)
    mismatches = []
    with LiveToolClient(case) as live:
        for name, args in calls:
            a, b = comparable(mock.call(name, args)), comparable(live.call(name, args))
            if a != b:
                mismatches.append((name, args, a, b))
    return mismatches


@pytest.mark.parametrize("privacy", [False, True])
def test_probe_calls_match(privacy):
    case = {"case_id": "parity", "fixture": {"version": "planner.v2"}, "now": "2026-11-03T09:00-05:00", "timeZone": TZ,
            "permittedTools": ALL_TOOLS, "privacy": {"includeDescriptions": privacy}}
    assert run_both(case, PROBES) == []


def _task(n: int, **fields) -> dict:
    """A planner-export task row for boundary probes."""
    row = {"id": f"00000000-0000-4000-8000-{900000 + n:012d}", "title": f"Probe task {n:03d}", "description": "", "planId": None,
           "milestoneId": None, "workstreamId": None, "ownerId": None, "dueDate": None, "scheduledDay": None, "plannedWeek": None,
           "estimatedMinutes": 10, "status": "todo", "priority": "normal", "completedAt": None, "recurrence": None, "checklist": [],
           "waitingOn": [], "sortOrder": 100 + n, "revision": 1, "createdAt": "2026-10-20T15:00:00.000Z", "updatedAt": "2026-10-20T15:00:00.000Z"}
    return row | fields


def _plan(n: int) -> dict:
    return {"id": f"00000000-0000-4000-8000-{800000 + n:012d}", "title": f"Probe plan {n:03d}", "description": "", "status": "active",
            "startDate": None, "targetDate": None, "color": None, "archived": False, "links": [], "revision": 1,
            "createdAt": "2026-10-20T15:00:00.000Z", "updatedAt": "2026-10-20T15:00:00.000Z"}


def test_list_caps_and_pooled_work_match():
    """Boundaries the small fixture can't reach: day-list and project caps, search totals, pooled weekly work."""
    overrides = [{"entity": "tasks", "add": _task(n, scheduledDay="2026-11-06", dueDate="2026-11-06")} for n in range(101)]
    overrides += [{"entity": "tasks", "add": _task(200 + n, plannedWeek="2026-11-02", estimatedMinutes=None if n % 2 else 15)} for n in range(4)]
    overrides += [{"entity": "plans", "add": _plan(n)} for n in range(51)]
    overrides += [{"entity": "tasks", "id": ID["t-lease"], "fields": {"plannedWeek": "2026-11-02"}}]
    case = {"case_id": "parity-bounds", "fixture": {"version": "planner.v2", "overrides": overrides}, "now": "2026-11-03T09:00-05:00",
            "timeZone": TZ, "permittedTools": ALL_TOOLS}
    calls = [
        ("get_day_context", {"date": "2026-11-06", "timeZone": TZ}),
        ("list_active_projects", {}),
        ("search_tasks", {"query": "probe", "limit": 50}),
        ("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ}),
    ]
    assert run_both(case, calls) == []
    capacity = MockToolClient(case).call("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ})["result"]
    assert capacity["pooledMinutes"] == 45 + 30 and capacity["pooledUnestimatedTasks"] == 2


PLAIN_CASES = [c for c in load_cases("all") if not (c.get("stateChanges") or c.get("injectedErrors") or c.get("injectedToolResponses"))]


@pytest.mark.parametrize("case", PLAIN_CASES, ids=lambda c: c["case_id"])
def test_reference_calls_match(case):
    calls = [(c["tool"], c["args"]) for c in case["reference"]["calls"]]
    assert run_both(case, calls) == []


def _block(n: int, task: str, start: str, minutes: int, zone: str = TZ) -> dict:
    return {"id": f"00000000-0000-4000-8000-{700000 + n:012d}", "taskId": task, "startAtUtc": start, "timeZone": zone,
            "durationMinutes": minutes, "revision": 1, "createdAt": "2026-10-20T15:00:00.000Z", "updatedAt": "2026-10-20T15:00:00.000Z"}


@pytest.mark.parametrize("muted", [[], ["usual_start", "typical_daily_load"]])
def test_preferences_profile_and_observations_match(muted):
    """A set profile (days off, daily limit, preferred hours) and observations computed from time blocks."""
    # Six finished tasks with estimates and blocks on six days; one block late in the evening counts on
    # its own local day, not the next day in UTC.
    overrides = []
    for n in range(6):
        task = _task(300 + n, status="done", estimatedMinutes=30, completedAt="2026-10-28T20:00:00.000Z", dueDate="2026-10-30")
        overrides.append({"entity": "tasks", "add": task})
        start = f"2026-10-{20 + n:02d}T{13 + n % 3:02d}:15:00.000Z" if n < 5 else "2026-10-26T03:30:00.000Z"
        overrides.append({"entity": "taskBlocks", "add": _block(n, task["id"], start, 45)})
    profile = {"preferredStartMinute": 540, "preferredEndMinute": 900, "maxPlannedMinutes": 240, "focusMinutes": 45,
               "breakMinutes": 10, "noWorkDays": [5, 3], "energy": "morning", "mutedObservations": muted}
    case = {"case_id": "parity-preferences", "fixture": {"version": "planner.v2", "overrides": overrides}, "now": "2026-11-03T09:00-05:00",
            "timeZone": TZ, "permittedTools": ALL_TOOLS, "planningProfile": profile}
    calls = [
        ("get_preferences", {}),
        ("get_capacity", {"weekStart": "2026-11-02", "timeZone": TZ}),
        ("get_day_context", {"date": "2026-11-04", "timeZone": TZ}),
    ]
    assert run_both(case, calls) == []
    result = MockToolClient(case).call("get_preferences", {})["result"]
    assert result["profile"]["noWorkDays"] == ["wednesday", "friday"]
    kinds = [o["kind"] for o in result["observations"]]
    assert kinds == (["estimate_accuracy"] if muted else ["estimate_accuracy", "usual_start", "typical_daily_load"])
