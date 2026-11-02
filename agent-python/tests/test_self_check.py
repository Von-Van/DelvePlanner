"""Milestone M4: the agent notices its own mistakes and recovers, or stops and says what is wrong.

Tool results use the tools.v1 shapes (UTC instants, revisions, waitingOn) so the checks see what the
real Rust tools return.
"""

from __future__ import annotations

from conftest import FakeClock, ScriptedModel

from delveagent import A1Agent, AgentTask, Budget, MockToolClient, ToolError
from delveagent.tools import default_catalog

ALL_TOOLS = tuple(spec.name for spec in default_catalog() if spec.name != "get_memory")
TZ = "America/New_York"

REVIEW = {
    "id": "ev-review",
    "revision": 1,
    "title": "Design review",
    "startAtUtc": "2026-11-04T19:00:00Z",
    "endAtUtc": "2026-11-04T20:00:00Z",
    "durationMinutes": 60,
    "timeZone": TZ,
    "planId": None,
    "workstreamId": None,
    "location": "",
}
DAY = {
    "date": "2026-11-04",
    "timeZone": TZ,
    "events": [REVIEW],
    "taskBlocks": [],
    "externalBusy": [{"startAtUtc": "2026-11-04T16:00:00Z", "endAtUtc": "2026-11-04T17:00:00Z", "allDay": False}],
    "free": [
        {"startAtUtc": "2026-11-04T14:00:00Z", "endAtUtc": "2026-11-04T16:00:00Z"},
        {"startAtUtc": "2026-11-04T17:00:00Z", "endAtUtc": "2026-11-04T19:00:00Z"},
        {"startAtUtc": "2026-11-04T20:00:00Z", "endAtUtc": "2026-11-04T22:00:00Z"},
    ],
    "availableMinutes": 360,
}
CAPACITY = {
    "workingHours": {"days": [1, 2, 3, 4, 5], "startMinute": 540, "endMinute": 1020, "revision": 1, "updatedAt": "x"},
    "days": [{"day": "2026-11-05", "availableMinutes": 30, "free": []}],
}


def task(id_, revision, title, **fields):
    return {"id": id_, "revision": revision, "title": title, "planId": "plan-launch", "status": "todo", **fields}


QA = task("t-qa", 5, "QA pass", dueDate="2026-11-18", estimatedMinutes=90)
DNS = task("t-dns", 1, "Switch DNS", waitingOn=["t-qa"], estimatedMinutes=45)
PROJECT = {"plan": {"id": "plan-launch", "revision": 4, "title": "Launch"}, "tasks": [QA, DNS]}


def make_task(goal="Do the thing.", tools=ALL_TOOLS) -> AgentTask:
    return AgentTask(goal=goal, now="2026-11-03T09:00:00-05:00", time_zone=TZ, permitted_tools=tools)


def call(tool, **arguments):
    return {"action": "call_tool", "tool": tool, "arguments": arguments}


def day_call():
    return call("get_day_context", date="2026-11-04", timeZone=TZ)


def project_call():
    return call("get_project_context", planId="plan-launch")


def draft(op, payload, target=None, evidence=("tc-1",), summary="Here is the change."):
    return {
        "action": "draft",
        "summary": summary,
        "drafts": [{"op": op, "target": target, "payload": payload, "evidence": list(evidence)}],
    }


def event(start, minutes=60, title="Mockups", evidence=("tc-1",)):
    return draft(
        "create_event",
        {"title": title, "notes": "", "startAtUtc": start, "timeZone": TZ, "durationMinutes": minutes},
        evidence=evidence,
    )


def run(script, responses=None, budget=None, self_check=True):
    model = ScriptedModel(script)
    tools = MockToolClient(responses if responses is not None else {"get_day_context": DAY})
    agent = A1Agent(clock=FakeClock(), self_check=self_check)
    return agent.run(make_task(), tools, model, budget or Budget()), tools, model


def checks(result):
    return [e["data"] for e in result.trace if e["kind"] == "verify"]


# -- fix what is certain --------------------------------------------------------------------------


def test_local_start_and_end_are_rewritten_without_asking_the_model():
    step = draft(
        "create_event",
        {"title": "Mockups", "notes": "", "start": "2026-11-04T09:00-05:00", "end": "2026-11-04T10:00-05:00"},
    )
    result, _, model = run([day_call(), step])
    assert result.status == "drafted"
    assert result.drafts[0].payload["startAtUtc"] == "2026-11-04T14:00:00Z"
    assert result.drafts[0].payload["durationMinutes"] == 60
    assert len(model.requests) == 2
    assert checks(result)[-1]["outcome"] == "fixed"


def test_end_at_utc_from_the_day_tool_becomes_a_duration():
    step = draft(
        "reschedule_event",
        {"startAtUtc": "2026-11-04T20:00:00Z", "endAtUtc": "2026-11-04T21:00:00Z", "timeZone": TZ},
        target={"id": "ev-review", "revision": 1},
    )
    result, _, _ = run([day_call(), step])
    assert result.status == "drafted"
    assert result.drafts[0].payload["durationMinutes"] == 60
    assert "endAtUtc" not in result.drafts[0].payload


# -- shape: one repair, then a clear stop ---------------------------------------------------------


def test_every_shape_problem_is_listed_at_once():
    bad = draft("create_event", {"title": "Mockups", "date": "2026-11-04", "duration": 60})
    result, _, model = run([day_call(), bad, event("2026-11-04T14:00:00Z")])
    assert result.status == "drafted"
    message = model.requests[2].messages[-1]["content"]
    assert "unknown field 'date'" in message
    assert "unknown field 'duration' (the field is durationMinutes)" in message
    assert "missing 'startAtUtc'" in message
    assert [c["outcome"] for c in checks(result)] == ["sent_back", "fixed"]


def test_second_bad_draft_stops_and_says_what_is_wrong():
    bad = draft("create_event", {"title": "Mockups", "date": "2026-11-04"})
    result, _, _ = run([day_call(), bad, bad])
    assert result.status == "cannot_complete"
    assert result.stop_reason == "invalid_draft"
    assert result.drafts == []
    assert result.answer.startswith("I couldn't prepare a change the planner would accept")
    assert "unknown field 'date'" in result.answer
    assert [c["outcome"] for c in checks(result)] == ["sent_back", "stopped"]


def test_stop_explains_even_with_the_self_check_off():
    bad = draft("schedule_task", {"scheduledDay": {"action": "set", "day": "2026-11-05"}}, target={"id": "t-x"})
    result, _, _ = run([project_call(), bad, bad], responses={"get_project_context": PROJECT}, self_check=False)
    assert result.stop_reason == "invalid_draft"
    assert "was not seen in any tool result" in result.answer


# -- facts: sent back once, then handed over with the problem spelled out --------------------------


def test_overlap_goes_back_once_and_a_moved_event_passes():
    clash = event("2026-11-04T19:30:00Z")  # 14:30, on top of Design review
    result, _, model = run([day_call(), clash, event("2026-11-04T20:00:00Z")])
    assert result.status == "drafted"
    sent = model.requests[2].messages[-1]["content"]
    assert 'overlaps "Design review" (14:00-15:00)' in sent
    assert "free that day: 09:00-11:00, 12:00-14:00, 15:00-17:00" in sent
    assert result.answer == "Here is the change."
    assert [c["outcome"] for c in checks(result)] == ["sent_back", "fixed"]


def test_overlap_kept_on_purpose_is_handed_over_with_a_heads_up():
    clash = event("2026-11-04T16:30:00Z")  # 11:30, during calendar busy time
    result, _, _ = run([day_call(), clash, clash])
    assert result.status == "drafted"
    assert "Before you approve:" in result.answer
    assert "overlaps busy time on a calendar (11:00-12:00)" in result.answer
    assert [c["outcome"] for c in checks(result)] == ["sent_back", "noted"]


def test_outside_working_hours_and_in_the_past_are_flagged():
    responses = {"get_day_context": DAY, "get_capacity": CAPACITY}
    evening = event("2026-11-04T23:00:00Z")  # 18:00
    script = [day_call(), call("get_capacity", weekStart="2026-11-02", timeZone=TZ), evening, evening]
    result, _, model = run(script, responses=responses)
    assert "falls outside working hours (09:00-17:00)" in model.requests[3].messages[-1]["content"]
    assert result.status == "drafted"

    past = event("2026-11-02T15:00:00Z")
    result, _, model = run([day_call(), past, past])
    assert "which is in the past" in model.requests[2].messages[-1]["content"]


def test_a_task_waiting_on_open_work_is_flagged():
    early = draft(
        "schedule_task",
        {"scheduledDay": {"action": "set", "day": "2026-11-10"}},
        target={"id": "t-dns", "revision": 1},
    )
    after = draft(
        "schedule_task",
        {"scheduledDay": {"action": "set", "day": "2026-11-19"}},
        target={"id": "t-dns", "revision": 1},
    )
    result, _, model = run([project_call(), early, after], responses={"get_project_context": PROJECT})
    assert result.status == "drafted"
    assert (
        '"Switch DNS" waits on "QA pass", which is not done yet, planned for 2026-11-18'
        in (model.requests[2].messages[-1]["content"])
    )
    assert result.drafts[0].payload["scheduledDay"]["day"] == "2026-11-19"


def test_a_day_without_room_is_flagged():
    responses = {"get_project_context": PROJECT, "get_capacity": CAPACITY}
    step = draft(
        "schedule_task",
        {"scheduledDay": {"action": "set", "day": "2026-11-05"}},
        target={"id": "t-qa", "revision": 5},
        evidence=("tc-1",),
    )
    script = [project_call(), call("get_capacity", weekStart="2026-11-02", timeZone=TZ), step, step]
    result, _, model = run(script, responses=responses)
    assert (
        '"QA pass" (90 min) need 90 min on Thursday Nov 05, but only 30 min are free'
        in (model.requests[3].messages[-1]["content"])
    )
    assert result.status == "drafted"
    assert "only 30 min are free" in result.answer


def test_no_facts_round_when_no_model_call_is_left():
    clash = event("2026-11-04T19:30:00Z")
    result, _, _ = run([day_call(), clash], budget=Budget(max_model_calls=2))
    assert result.status == "drafted"
    assert "overlaps" in result.answer


# -- freshness: re-read, redraft, and say so ------------------------------------------------------


def test_stale_task_is_reread_and_redrafted_against_the_new_revision():
    search = call("search_tasks", query="QA", limit=5)
    fresh = {**PROJECT, "tasks": [{**QA, "revision": 6}, DNS]}
    responses = {"search_tasks": {"tasks": [QA], "totalMatches": 1, "truncated": False}, "get_project_context": fresh}

    def due(revision, evidence):
        return draft(
            "schedule_task",
            {"dueDate": {"action": "set", "day": "2026-11-20"}},
            target={"id": "t-qa", "revision": revision},
            evidence=(evidence,),
            summary="Moved QA pass to Nov 20.",
        )

    responses["get_day_context"] = DAY
    script = [search, day_call(), due(5, "tc-1"), due(6, "tc-3")]
    result, tools, model = run(script, responses=responses)
    assert result.status == "drafted"
    assert result.drafts[0].target == {"id": "t-qa", "revision": 6}
    assert [name for name, _ in tools.calls] == ["search_tasks", "get_day_context", "get_project_context"]
    assert '"QA pass" (t-qa) is now revision 6 (you drafted against 5)' in model.requests[3].messages[-1]["content"]
    assert '"QA pass" was changed while I was working on this' in result.answer
    assert [c["outcome"] for c in checks(result)] == ["sent_back", "fixed"]


def test_an_unchanged_target_passes_after_one_reread():
    step = draft(
        "schedule_task",
        {"dueDate": {"action": "set", "day": "2026-11-20"}},
        target={"id": "t-qa", "revision": 5},
    )
    result, tools, _ = run([project_call(), step], responses={"get_project_context": PROJECT})
    assert result.status == "drafted"
    assert tools.calls == [("get_project_context", {"planId": "plan-launch"})] * 2
    assert [(c["check"], c["outcome"]) for c in checks(result)] == [("freshness", "passed"), ("all", "passed")]


def test_stale_event_with_no_model_call_left_stops_instead_of_handing_over():
    renamed = {**DAY, "events": [{**REVIEW, "revision": 2}]}
    responses = {
        "get_day_context": [DAY, renamed],
        "search_tasks": {"tasks": [], "totalMatches": 0, "truncated": False},
    }
    step = draft("update_event", {"title": "Design review (moved room)"}, target={"id": "ev-review", "revision": 1})
    script = [day_call(), call("search_tasks", query="x", limit=5), step]
    result, tools, _ = run(script, responses=responses, budget=Budget(max_model_calls=3))
    assert [name for name, _ in tools.calls][-1] == "get_day_context"
    assert result.status == "cannot_complete"
    assert result.drafts == []
    assert '"Design review" was changed while I was working on this' in result.answer
    assert checks(result)[-1]["outcome"] == "stopped"


def test_reread_is_skipped_without_tool_budget():
    search = call("search_tasks", query="QA", limit=5)
    step = draft(
        "schedule_task",
        {"dueDate": {"action": "set", "day": "2026-11-20"}},
        target={"id": "t-qa", "revision": 5},
    )
    responses = {"search_tasks": {"tasks": [QA], "totalMatches": 1, "truncated": False}, "get_day_context": DAY}
    result, tools, _ = run([search, step], responses=responses, budget=Budget(max_tool_calls=1))
    assert result.status == "drafted"
    assert len(tools.calls) == 1
    assert checks(result)[0].get("skipped") == "tool budget"


# -- tools: one sensible retry --------------------------------------------------------------------


def test_a_timeout_is_retried_once_by_the_loop():
    timeout = ToolError("timeout", "calendar read timed out", retryable=True)
    result, tools, model = run(
        [day_call(), {"action": "answer", "answer": "Design review at 14:00."}],
        responses={"get_day_context": [timeout, DAY]},
    )
    assert result.status == "answered"
    assert len(tools.calls) == 2
    assert len(model.requests) == 2
    assert "Tool result tc-2 (get_day_context)" in model.requests[1].messages[-1]["content"]
    retries = [e for e in result.trace if e["kind"] == "repair" and e["data"]["reason"] == "tool_retry"]
    assert len(retries) == 1


def test_a_lasting_outage_is_reported_after_one_retry_and_then_stops():
    down = ToolError("unavailable", "calendar is down", retryable=True)
    result, tools, model = run([day_call(), day_call()], responses={"get_day_context": down})
    assert "retried once automatically" in model.requests[1].messages[-1]["content"]
    assert result.status == "cannot_complete"
    assert result.stop_reason == "repeated_tool_error"
    assert len(tools.calls) == 3  # first call and its retry, then the model's own retry


def test_errors_that_are_not_retryable_are_not_retried():
    missing = ToolError("not_found", "no such plan", retryable=False)
    result, tools, _ = run(
        [project_call(), {"action": "cannot_complete", "reason": "No such project."}],
        responses={"get_project_context": missing},
    )
    assert len(tools.calls) == 1
    assert result.status == "cannot_complete"


# -- off switch -----------------------------------------------------------------------------------


def test_self_check_off_is_the_loop_as_it_was():
    timeout = ToolError("timeout", "slow", retryable=True)
    clash = event("2026-11-04T19:30:00Z", evidence=("tc-2",))
    result, tools, _ = run(
        [day_call(), day_call(), clash], responses={"get_day_context": [timeout, DAY]}, self_check=False
    )
    assert result.status == "drafted"
    assert len(tools.calls) == 2
    assert checks(result) == []
    assert result.answer == "Here is the change."


def test_a_clash_that_appears_in_the_reread_is_caught():
    # The slot was free on the first read; by the re-read someone has put a meeting there.
    standup = {**REVIEW, "id": "ev-standup", "title": "Standup", "startAtUtc": "2026-11-04T20:00:00Z"}
    standup["endAtUtc"] = "2026-11-04T20:30:00Z"
    later = {**DAY, "events": [REVIEW, standup]}
    move = draft(
        "reschedule_event",
        {"startAtUtc": "2026-11-04T20:00:00Z", "timeZone": TZ},
        target={"id": "ev-review", "revision": 1},
    )
    result, tools, model = run([day_call(), move, move], responses={"get_day_context": [DAY, later]})
    assert len(tools.calls) == 2
    assert 'overlaps "Standup" (15:00-15:30)' in model.requests[2].messages[-1]["content"]
    assert result.status == "drafted"
    assert 'overlaps "Standup"' in result.answer
    assert [(c["check"], c["outcome"]) for c in checks(result)][-2:] == [("facts", "sent_back"), ("all", "noted")]


def test_a_target_that_disappeared_cannot_be_drafted_again():
    gone = {**DAY, "events": []}
    rename = draft("update_event", {"title": "Design review (room 4)"}, target={"id": "ev-review", "revision": 1})
    result, _, model = run([day_call(), rename, rename, rename], responses={"get_day_context": [DAY, gone]})
    assert "no longer appears in a fresh read" in model.requests[2].messages[-1]["content"]
    assert "ev-review no longer exists, so it can't be changed" in model.requests[3].messages[-1]["content"]
    assert result.status == "cannot_complete"
    assert result.drafts == []
    assert "no longer exists" in result.answer
