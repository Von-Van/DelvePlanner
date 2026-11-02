"""The "I have two hours tomorrow, help me prioritize" workflow (milestone M3).

The agent may write a short plan of its reads, looks up free time, preferences and open work, and
drafts time blocks. Blocks must sit inside free time the tools reported and must not overlap; a block
that doesn't fit is sent back to the model with the free ranges, once.
"""

from __future__ import annotations

import json

from conftest import FakeClock, ScriptedModel

from delveops_agent import A1Agent, AgentTask, Budget, MockToolClient, mutation_operation
from delveops_agent.drafts import DraftError, check_time_blocks, observed_free_ranges, parse_draft
from delveops_agent.prompts import STEP_SCHEMA, task_message
from delveops_agent.tools import default_catalog

TOOLS = ("get_day_context", "list_active_projects", "get_project_context", "get_preferences")
LEASE, MOVERS, DNS = "task-lease", "task-movers", "task-dns"

DAY = {
    "date": "2026-11-05",
    "timeZone": "America/New_York",
    "events": [],
    "free": [
        {"startAtUtc": "2026-11-05T17:00:00.000Z", "endAtUtc": "2026-11-05T18:00:00.000Z"},
        {"startAtUtc": "2026-11-05T19:00:00.000Z", "endAtUtc": "2026-11-05T21:00:00.000Z"},
    ],
}
PREFERENCES = {
    "profile": {"trust": "user_confirmed", "isSet": True, "focusMinutes": 45, "breakMinutes": 10},
    "workingHours": {"days": [1, 2, 3, 4, 5], "startMinute": 540, "endMinute": 1020},
    "observations": [],
}
PROJECTS = {"plans": [{"id": "plan-move", "revision": 2, "title": "Apartment move"}]}
PROJECT = {
    "plan": {"id": "plan-move", "revision": 2, "title": "Apartment move"},
    "tasks": [
        {"id": LEASE, "revision": 3, "title": "Review lease", "dueDate": "2026-11-07", "estimatedMinutes": 45},
        {"id": MOVERS, "revision": 2, "title": "Book movers", "dueDate": "2026-11-10", "estimatedMinutes": 30},
        {"id": DNS, "revision": 1, "title": "Switch DNS", "waitingOn": ["task-qa"], "estimatedMinutes": 30},
    ],
}
RESPONSES = {
    "get_day_context": DAY,
    "get_preferences": PREFERENCES,
    "list_active_projects": PROJECTS,
    "get_project_context": PROJECT,
}

PLAN = {
    "action": "plan",
    "steps": [
        {"tool": "get_day_context", "why": "free time tomorrow"},
        {"tool": "get_preferences", "why": "focus and break lengths"},
        {"tool": "list_active_projects", "why": "find the projects"},
        {"tool": "get_project_context", "why": "open tasks"},
    ],
}
READS = [
    {
        "action": "call_tool",
        "tool": "get_day_context",
        "arguments": {"date": "2026-11-05", "timeZone": "America/New_York"},
    },
    {"action": "call_tool", "tool": "get_preferences", "arguments": {}},
    {"action": "call_tool", "tool": "list_active_projects", "arguments": {}},
    {"action": "call_tool", "tool": "get_project_context", "arguments": {"planId": "plan-move"}},
]


def block(task: str, revision: int, start: str, minutes: int) -> dict:
    return {
        "op": "create_task_block",
        "target": {"id": task, "revision": revision},
        "payload": {"startAtUtc": start, "timeZone": "America/New_York", "durationMinutes": minutes},
        "evidence": ["tc-1", "tc-4"],
    }


def blocks_step(*drafts: dict) -> dict:
    return {
        "action": "draft",
        "summary": "Review lease first (due Friday), then book movers; Switch DNS waits on QA.",
        "drafts": list(drafts),
    }


GOOD = blocks_step(
    block(LEASE, 3, "2026-11-05T19:00:00Z", 45),
    block(MOVERS, 2, "2026-11-05T19:55:00Z", 30),
)


def run(script, budget=None, debug=False):
    model = ScriptedModel(script)
    tools = MockToolClient(RESPONSES)
    task = AgentTask(
        goal="I have two hours tomorrow afternoon. Look at my projects and help me decide what to prioritize.",
        now="2026-11-04T17:00:00-05:00",
        time_zone="America/New_York",
        permitted_tools=TOOLS,
    )
    agent = A1Agent(catalog=default_catalog(), debug_trace=debug, clock=FakeClock())
    return agent.run(task, tools, model, budget or Budget()), tools, model


def test_plans_reads_and_drafts_two_blocks_that_fit_the_free_time():
    result, tools, model = run([PLAN, *READS, GOOD])
    assert result.status == "drafted"
    assert [name for name, _ in tools.calls] == [step["tool"] for step in PLAN["steps"]]
    assert [draft.op for draft in result.drafts] == ["create_task_block", "create_task_block"]
    assert mutation_operation(result.drafts[0]) == {
        "type": "create_task_block",
        "taskId": LEASE,
        "expectedRevision": 3,
        "startAtUtc": "2026-11-05T19:00:00Z",
        "timeZone": "America/New_York",
        "durationMinutes": 45,
    }
    plan_events = [event for event in result.trace if event["kind"] == "plan" and event["data"].get("steps")]
    assert plan_events[0]["data"] == {"plan": 1, "steps": [{"tool": step["tool"]} for step in PLAN["steps"]]}
    # The model's reasons stay out of the routine trace.
    assert "focus and break lengths" not in json.dumps(result.trace)


def test_a_simple_request_needs_no_plan():
    one = block(LEASE, 3, "2026-11-05T17:00:00Z", 45) | {"evidence": ["tc-1", "tc-2"]}
    result, tools, _ = run([READS[0], READS[3], blocks_step(one)])
    assert result.status == "drafted"
    assert len(tools.calls) == 2
    assert not [event for event in result.trace if event["kind"] == "plan" and "steps" in event["data"]]


def test_a_block_outside_the_free_time_is_sent_back_with_the_free_ranges():
    clash = blocks_step(block(LEASE, 3, "2026-11-05T18:00:00Z", 45))  # 1 pm: busy
    result, _, model = run([*READS, clash, GOOD])
    assert result.status == "drafted"
    feedback = model.requests[-1].messages[-1]["content"]
    assert "not inside any free range" in feedback
    assert "2026-11-05T19:00:00Z to 2026-11-05T21:00:00Z" in feedback
    assert any(event["kind"] == "repair" for event in result.trace)


def test_more_time_than_is_free_is_refused():
    too_long = blocks_step(block(LEASE, 3, "2026-11-05T19:00:00Z", 150))
    result, _, _ = run([*READS, too_long, too_long])
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_draft"


def test_overlapping_blocks_are_refused():
    overlap = blocks_step(
        block(LEASE, 3, "2026-11-05T19:00:00Z", 45),
        block(MOVERS, 2, "2026-11-05T19:30:00Z", 30),
    )
    result, _, model = run([*READS, overlap, GOOD])
    assert result.status == "drafted"
    assert "overlap" in model.requests[-1].messages[-1]["content"]


def test_a_plan_may_only_name_offered_tools_and_is_bounded():
    bad = {"action": "plan", "steps": [{"tool": "delete_everything"}]}
    result, tools, model = run([bad, PLAN, *READS, GOOD])
    assert result.status == "drafted"
    assert "can't use" in model.requests[1].messages[-1]["content"]
    assert tools.calls and all(name != "delete_everything" for name, _ in tools.calls)

    result, _, model = run([PLAN, PLAN, PLAN, *READS, GOOD])
    assert result.status == "drafted"
    assert "already have a plan" in model.requests[3].messages[-1]["content"]


def test_the_step_format_offers_a_plan():
    assert "plan" in STEP_SCHEMA["properties"]["action"]["enum"]


def test_the_request_says_what_today_and_tomorrow_are():
    task = AgentTask(goal="x", now="2026-11-04T23:30:00-05:00", time_zone="America/New_York", permitted_tools=())
    assert "Today is Wednesday 2026-11-04; tomorrow is Thursday 2026-11-05." in task_message(task)
    # Across the clock change: Saturday night before the end of daylight saving time.
    task = AgentTask(goal="x", now="2026-11-01T03:30:00Z", time_zone="America/New_York", permitted_tools=())
    assert "Today is Saturday 2026-10-31; tomorrow is Sunday 2026-11-01." in task_message(task)
    task = AgentTask(goal="x", now="not a time", time_zone="Nowhere/Else", permitted_tools=())
    assert task_message(task) == "Current time: not a time (time zone Nowhere/Else).\nRequest: x"


def test_free_ranges_come_from_day_context_and_capacity():
    capacity = {
        "days": [
            {"day": "2026-11-06", "free": [{"startAtUtc": "2026-11-06T14:00:00Z", "endAtUtc": "2026-11-06T15:00:00Z"}]}
        ]
    }
    ranges = observed_free_ranges([DAY, capacity])
    assert len(ranges) == 3
    seen = {"task-lease": 3}
    draft = parse_draft(block(LEASE, 3, "2026-11-06T14:00:00Z", 60) | {"evidence": ["tc-1"]}, {"tc-1"}, seen)
    check_time_blocks([draft], [capacity])
    try:
        check_time_blocks([draft], [])
    except DraftError as error:
        assert "look up the free time first" in str(error)
    else:
        raise AssertionError("a block without any free time seen must be refused")
