from __future__ import annotations

from conftest import FakeClock, ScriptedModel

from delveops_agent import A1Agent, A3Agent, AgentTask, Budget, MockToolClient
from delveops_agent.prompts import system_prompt
from delveops_agent.tools import V0_CATALOG
from delveops_agent.verify import VerifierConfig

ALL_TOOLS = tuple(spec.name for spec in V0_CATALOG)
TZ = "America/New_York"

DAY = {
    "date": "2026-11-04",
    "timeZone": TZ,
    "workingHours": {"start": "2026-11-04T09:00-05:00", "end": "2026-11-04T17:00-05:00"},
    "events": [
        {
            "id": "ev-review",
            "revision": 1,
            "title": "Design review",
            "start": "2026-11-04T14:00-05:00",
            "end": "2026-11-04T15:00-05:00",
        }
    ],
    "taskBlocks": [],
    "externalBusy": [],
    "free": [
        {"start": "2026-11-04T09:00-05:00", "end": "2026-11-04T14:00-05:00"},
        {"start": "2026-11-04T15:00-05:00", "end": "2026-11-04T17:00-05:00"},
    ],
}
SEARCH = {"tasks": [{"id": "t-qa", "revision": 5, "planId": "plan-launch", "title": "QA pass"}]}
PROJECT_FRESH = {"plan": {"id": "plan-launch", "revision": 4}, "tasks": [{"id": "t-qa", "revision": 6}]}


def make_task(goal="Do the thing.") -> AgentTask:
    return AgentTask(goal=goal, now="2026-11-03T09:00:00-05:00", time_zone=TZ, permitted_tools=ALL_TOOLS)


def call(tool, **arguments):
    return {"action": "call_tool", "tool": tool, "arguments": arguments}


def day_call():
    return call("get_day_context", date="2026-11-04", timeZone=TZ)


def event_draft(payload, evidence=("tc-1",)):
    return {
        "action": "draft",
        "summary": "s",
        "drafts": [{"op": "create_event", "target": None, "payload": payload, "evidence": list(evidence)}],
    }


def run(script, responses=None, agent=None):
    model = ScriptedModel(script)
    tools = MockToolClient(responses if responses is not None else {"get_day_context": DAY})
    agent = agent or A3Agent(clock=FakeClock())
    return agent.run(make_task(), tools, model, Budget()), tools, model


def kinds(result, kind):
    return [e for e in result.trace if e["kind"] == kind]


def test_a1_shaped_event_is_normalised_and_passes():
    draft = event_draft(
        {"title": "Mockups", "notes": "", "start": "2026-11-04T10:00-05:00", "end": "2026-11-04T11:00-05:00"}
    )
    result, _, model = run([day_call(), draft])
    assert result.status == "drafted"
    assert result.drafts[0].payload["startAtUtc"] == "2026-11-04T15:00:00Z"
    assert result.drafts[0].payload["durationMinutes"] == 60
    assert result.architecture_id == "A3"
    assert result.trace[0]["data"]["architectureId"] == "A3"
    assert result.trace[0]["data"]["promptVersion"] == "a3-v1"
    assert "Value shapes" in model.requests[0].messages[0]["content"]
    assert kinds(result, "verify")[-1]["data"]["passed"] is True


def test_payload_problems_go_back_to_the_model_and_can_be_fixed():
    bad = event_draft({"title": "Mockups", "startAtUtc": "2026-11-04T15:00:00Z", "durationMinutes": 60})
    good = event_draft(
        {"title": "Mockups", "notes": "", "startAtUtc": "2026-11-04T15:00:00Z", "timeZone": TZ, "durationMinutes": 60}
    )
    result, _, model = run([day_call(), bad, good])
    assert result.status == "drafted"
    feedback = model.requests[2].messages[-1]["content"]
    assert "missing 'notes'" in feedback and "missing 'timeZone'" in feedback
    assert kinds(result, "verify")[0]["data"]["passed"] is False


def test_drafts_that_never_get_fixed_end_in_cannot_complete():
    bad = event_draft({"title": "Mockups"})
    result, _, model = run([day_call(), bad, bad, bad])
    assert result.status == "cannot_complete" and result.stop_reason == "invalid_draft"
    assert len(model.requests) == 4  # max_repairs=3: three bad replies


def test_overlap_is_a_warning_the_model_may_keep():
    clash = event_draft(
        {"title": "Mockups", "notes": "", "startAtUtc": "2026-11-04T19:30:00Z", "timeZone": TZ, "durationMinutes": 60}
    )
    result, _, model = run([day_call(), clash, clash])
    assert result.status == "drafted"
    warning = model.requests[2].messages[-1]["content"]
    assert "overlaps Design review" in warning and "15:00-17:00" in warning


def test_overlap_warning_can_lead_to_a_better_time():
    clash = event_draft(
        {"title": "Mockups", "notes": "", "startAtUtc": "2026-11-04T19:30:00Z", "timeZone": TZ, "durationMinutes": 60}
    )
    moved = event_draft(
        {"title": "Mockups", "notes": "", "startAtUtc": "2026-11-04T20:00:00Z", "timeZone": TZ, "durationMinutes": 60}
    )
    result, _, _ = run([day_call(), clash, moved])
    assert result.drafts[0].payload["startAtUtc"] == "2026-11-04T20:00:00Z"


def test_reschedule_uses_the_events_own_duration_for_the_overlap_check():
    move = {
        "action": "draft",
        "drafts": [
            {
                "op": "reschedule_event",
                "target": {"id": "ev-review", "revision": 1},
                "payload": {"startAtUtc": "2026-11-04T13:30:00Z", "timeZone": TZ},
                "evidence": ["tc-1"],
            }
        ],
    }
    result, _, model = run([day_call(), move, move])
    assert result.status == "drafted"
    # 8:30 plus the event's own 60 minutes: before working hours, and its old slot is not a clash.
    warning = model.requests[2].messages[-1]["content"]
    assert "outside working hours (09:00-17:00)" in warning and "overlaps" not in warning


def test_stale_target_is_reread_and_redrafted():
    def draft(revision):
        return {
            "action": "draft",
            "drafts": [
                {
                    "op": "schedule_task",
                    "target": {"id": "t-qa", "revision": revision},
                    "payload": {"dueDate": "2026-11-20"},
                    "evidence": ["tc-1"],
                }
            ],
        }

    responses = {"search_tasks": SEARCH, "get_day_context": DAY, "get_project_context": PROJECT_FRESH}
    script = [call("search_tasks", query="QA", limit=5), day_call(), draft(5), draft(6)]
    result, tools, model = run(script, responses)
    assert result.status == "drafted"
    assert result.drafts[0].target == {"id": "t-qa", "revision": 6}
    assert result.drafts[0].payload["plannedWeek"] == {"action": "unchanged"}
    assert [name for name, _ in tools.calls] == ["search_tasks", "get_day_context", "get_project_context"]
    assert "t-qa is now revision 6" in model.requests[3].messages[-1]["content"]


def test_no_reread_when_the_last_call_returned_the_target():
    draft = {
        "action": "draft",
        "drafts": [{"op": "delete_task", "target": {"id": "t-qa", "revision": 5}, "payload": {}, "evidence": ["tc-1"]}],
    }
    result, tools, _ = run([call("search_tasks", query="QA", limit=5), draft], {"search_tasks": SEARCH})
    assert result.status == "drafted" and result.drafts[0].payload == {}
    assert len(tools.calls) == 1


def test_reread_skipped_without_budget():
    draft = {
        "action": "draft",
        "drafts": [{"op": "delete_task", "target": {"id": "t-qa", "revision": 5}, "payload": {}, "evidence": ["tc-1"]}],
    }
    model = ScriptedModel([call("search_tasks", query="QA", limit=5), day_call(), draft])
    tools = MockToolClient({"search_tasks": SEARCH, "get_day_context": DAY})
    result = A3Agent(clock=FakeClock()).run(make_task(), tools, model, Budget(max_tool_calls=2))
    assert result.status == "drafted" and len(tools.calls) == 2
    assert any("skipped" in e["summary"] for e in kinds(result, "verify"))


def test_ablation_switches_turn_checks_off():
    config = VerifierConfig(normalize=False, check_payloads=False, check_times=False, check_freshness=False)
    a1_shaped = event_draft({"title": "Mockups", "start": "2026-11-04T10:00-05:00"})
    result, _, _ = run([day_call(), a1_shaped], agent=A3Agent(verifier=config, clock=FakeClock()))
    assert result.status == "drafted" and "start" in result.drafts[0].payload


def test_everything_off_is_a1():
    script = [day_call(), event_draft({"title": "Mockups", "start": "x"})]
    a3 = A3Agent(use_verifier=False, payload_guide=False, max_repairs=2, clock=FakeClock())
    a3_result, _, a3_model = run(list(script), agent=a3)
    a1_result, _, a1_model = run(list(script), agent=A1Agent(clock=FakeClock()))
    assert a3_model.requests[0].messages == a1_model.requests[0].messages
    assert a3_model.requests[0].messages[0]["content"] == system_prompt(V0_CATALOG)
    assert (a3_result.status, a3_result.drafts) == (a1_result.status, a1_result.drafts)
    assert [e["kind"] for e in a3_result.trace] == [e["kind"] for e in a1_result.trace]
