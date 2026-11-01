from __future__ import annotations

from conftest import FakeClock, ScriptedModel

from delveops_agent import A2Agent, AgentTask, Budget, MockToolClient, ToolError
from delveops_agent.tools import V0_CATALOG
from delveops_agent.verify import VerifierConfig

ALL_TOOLS = tuple(spec.name for spec in V0_CATALOG)
TZ = "America/New_York"
PROJECTS = {"plans": [{"id": "plan-launch", "revision": 4, "title": "Website relaunch"}]}
PROJECT = {"plan": {"id": "plan-launch", "revision": 4}, "tasks": [{"id": "t-qa", "revision": 5, "title": "QA pass"}]}
DAY = {"date": "2026-11-04", "events": [], "free": []}


def make_task(tools=ALL_TOOLS) -> AgentTask:
    return AgentTask(goal="Help.", now="2026-11-03T09:00:00-05:00", time_zone=TZ, permitted_tools=tools)


def plan(*steps, intent="answer"):
    return {"action": "plan", "intent": intent, "steps": [{"tool": t, "arguments": a, "why": "w"} for t, a in steps]}


def answer(text="Done."):
    return {"action": "answer", "answer": text}


def run(script, responses=None, task=None, budget=None, agent=None):
    model = ScriptedModel(script)
    tools = MockToolClient(responses if responses is not None else {"list_active_projects": PROJECTS})
    agent = agent or A2Agent(clock=FakeClock())
    return agent.run(task or make_task(), tools, model, budget or Budget()), tools, model


def test_plan_runs_every_step_then_finishes():
    responses = {"list_active_projects": PROJECTS, "get_day_context": DAY}
    script = [plan(("list_active_projects", {}), ("get_day_context", {"date": "2026-11-04", "timeZone": TZ})), answer()]
    result, tools, model = run(script, responses)
    assert result.status == "answered" and result.architecture_id == "A2"
    assert [name for name, _ in tools.calls] == ["list_active_projects", "get_day_context"]
    assert result.usage.model_calls == 2 and result.usage.tool_calls == 2
    plans = [e for e in result.trace if e["kind"] == "plan" and "intent" in e["data"]]
    assert plans[0]["data"]["intent"] == "answer"
    assert plans[0]["data"]["steps"][1] == {"tool": "get_day_context", "argumentNames": ["date", "timeZone"]}
    assert "Plan 1 has run" in model.requests[1].messages[-1]["content"]


def test_retryable_failure_is_retried_once_by_the_executor():
    flaky = [ToolError("timeout", "slow", retryable=True), PROJECTS]
    result, tools, _ = run([plan(("list_active_projects", {})), answer()], {"list_active_projects": flaky})
    assert result.status == "answered"
    assert len(tools.calls) == 2


def test_second_identical_failure_ends_the_run():
    down = ToolError("timeout", "slow", retryable=True)
    result, tools, _ = run([plan(("list_active_projects", {}))], {"list_active_projects": down})
    assert result.status == "cannot_complete" and result.stop_reason == "repeated_tool_error"
    assert len(tools.calls) == 2


def test_tool_outside_the_task_is_never_called():
    task = make_task(tools=("get_day_context",))
    result, tools, _ = run([plan(("list_active_projects", {}))], task=task)
    assert result.status == "cannot_complete" and result.stop_reason == "tool_not_permitted"
    assert tools.calls == []


def test_repeated_step_is_not_run_twice():
    script = [plan(("list_active_projects", {})), plan(("list_active_projects", {})), answer()]
    result, tools, model = run(script)
    assert len(tools.calls) == 1
    assert "repeats tc-1" in model.requests[2].messages[-1]["content"]


def test_question_before_reading_is_sent_back_once():
    ask = {"action": "clarify", "question": "Which one?"}
    result, tools, _ = run([ask, plan(("list_active_projects", {}), intent="clarify"), ask])
    assert result.status == "needs_clarification" and len(tools.calls) == 1


def test_plan_limit():
    script = [plan(("list_active_projects", {}))] * 3 + [
        plan(("get_day_context", {"date": "2026-11-04", "timeZone": TZ}))
    ]
    script += [answer()]
    result, tools, model = run(script, {"list_active_projects": PROJECTS, "get_day_context": DAY})
    assert result.status == "answered"
    assert "No more plans" in model.requests[4].messages[-1]["content"]


def test_lone_call_tool_is_a_one_step_plan():
    script = [{"action": "call_tool", "tool": "list_active_projects", "arguments": {}}, answer()]
    result, tools, _ = run(script)
    assert result.status == "answered" and len(tools.calls) == 1


def test_steps_past_the_tool_budget_are_skipped():
    responses = {"list_active_projects": PROJECTS, "get_project_context": PROJECT}
    script = [plan(("list_active_projects", {}), ("get_project_context", {"planId": "plan-launch"})), answer()]
    result, tools, model = run(script, responses, budget=Budget(max_tool_calls=1))
    assert result.status == "answered" and len(tools.calls) == 1
    assert "Tool budget used up" in model.requests[1].messages[-1]["content"]


def test_too_many_steps_are_trimmed():
    steps = [("get_day_context", {"date": f"2026-11-0{d}", "timeZone": TZ}) for d in range(1, 9)]
    result, tools, _ = run(
        [plan(*steps), answer()], {"get_day_context": DAY}, agent=A2Agent(max_steps=3, clock=FakeClock())
    )
    assert len(tools.calls) == 3


def test_a2_with_verifier_normalises_drafts():
    draft = {
        "action": "draft",
        "drafts": [
            {
                "op": "schedule_task",
                "target": {"id": "t-qa", "revision": 5},
                "payload": {"dueDate": "2026-11-20"},
                "evidence": ["tc-1"],
            }
        ],
    }
    agent = A2Agent(verifier=VerifierConfig(), clock=FakeClock())
    result, _, _ = run(
        [plan(("get_project_context", {"planId": "plan-launch"}), intent="draft"), draft],
        {"get_project_context": PROJECT},
        agent=agent,
    )
    assert result.status == "drafted" and result.architecture_id == "A2+A3"
    assert result.drafts[0].payload["scheduledDay"] == {"action": "unchanged"}


def test_a2_without_verifier_keeps_a1_draft_rules():
    draft = {
        "action": "draft",
        "drafts": [
            {
                "op": "schedule_task",
                "target": {"id": "t-qa", "revision": 4},
                "payload": {"dueDate": "x"},
                "evidence": ["tc-1"],
            }
        ],
    }
    result, _, _ = run(
        [plan(("get_project_context", {"planId": "plan-launch"})), draft, draft], {"get_project_context": PROJECT}
    )
    assert result.status == "cannot_complete" and result.stop_reason == "invalid_draft"
