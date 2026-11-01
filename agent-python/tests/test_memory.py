from __future__ import annotations

from conftest import ScriptedModel

from delveops_agent import A1Agent, AgentTask, Budget, MockToolClient, ToolError
from delveops_agent.memory import (
    GET_MEMORY_SPEC,
    MemoryAgent,
    Turn,
    compose_goal,
    recent_window,
)
from delveops_agent.memory.strategies import MAX_TURN_CHARS, MAX_WINDOW_CHARS

ALL_TOOLS = ("get_day_context", "search_tasks", "get_memory")

CONVERSATION = (
    Turn("user", "Move the design review to 3 pm."),
    Turn("assistant", "Drafted: move Design review to 3:00 pm on Wednesday."),
)

MEMORY = {
    "memoryEnabled": True,
    "records": [
        {
            "id": "m1",
            "kind": "decision",
            "content": "Design review moved to 3 pm Wednesday (approved).",
            "source": "approved_action",
            "confirmation": "confirmed",
            "planId": None,
            "recordedAt": "2026-11-03T14:00:00Z",
            "relevance": 1.0,
        },
        {
            "id": "m2",
            "kind": "preference",
            "content": "Maybe prefers afternoons.",
            "source": "assistant_inferred",
            "confirmation": "unconfirmed",
            "planId": None,
            "recordedAt": "2026-11-02T14:00:00Z",
            "relevance": 1.0,
        },
    ],
    "totalMatches": 2,
    "truncated": False,
}


def task(tools=ALL_TOOLS) -> AgentTask:
    return AgentTask(
        goal="Move it 30 minutes later.",
        now="2026-11-03T15:00:00-06:00",
        time_zone="America/Chicago",
        permitted_tools=tools,
    )


def agent(strategy: str, clock) -> MemoryAgent:
    return MemoryAgent(strategy, lambda catalog: A1Agent(catalog=catalog, clock=clock))


def answer_model() -> ScriptedModel:
    return ScriptedModel([{"action": "answer", "answer": "ok"}])


def user_prompt(model: ScriptedModel) -> str:
    return model.requests[0].messages[1]["content"]


def system_prompt(model: ScriptedModel) -> str:
    return model.requests[0].messages[0]["content"]


def test_m0_sees_only_the_request(clock):
    model = answer_model()
    tools = MockToolClient({"get_memory": MEMORY})
    result = agent("M0", clock).run(task(), tools, model, Budget(), conversation=CONVERSATION)
    assert result.status == "answered"
    assert "design review" not in user_prompt(model).lower()
    assert "get_memory" not in system_prompt(model)
    assert tools.calls == []
    assert result.architecture_id == "A1+M0"


def test_m1_shows_recent_turns_but_no_memory_tool(clock):
    model = answer_model()
    tools = MockToolClient({"get_memory": MEMORY})
    agent("M1", clock).run(task(), tools, model, Budget(), conversation=CONVERSATION)
    prompt = user_prompt(model)
    assert "Person: Move the design review to 3 pm." in prompt
    assert prompt.rstrip().endswith("Current request: Move it 30 minutes later.")
    assert "get_memory" not in system_prompt(model)
    assert tools.calls == []


def test_m2_preloads_confirmed_records_only(clock):
    model = answer_model()
    tools = MockToolClient({"get_memory": MEMORY})
    result = agent("M2", clock).run(task(), tools, model, Budget(max_tool_calls=5), conversation=CONVERSATION)
    prompt = user_prompt(model)
    assert "Design review moved to 3 pm" in prompt
    assert "approved_action" in prompt
    assert "Maybe prefers afternoons" not in prompt
    assert "Person:" not in prompt, "M2 does not carry the conversation"
    assert tools.calls == [("get_memory", {"limit": 20})]
    assert result.usage.tool_calls == 1
    assert "get_memory" not in system_prompt(model), "the model itself is not offered the tool"


def test_m2_without_permission_loads_nothing(clock):
    model = answer_model()
    tools = MockToolClient({"get_memory": MEMORY})
    result = agent("M2", clock).run(task(("search_tasks",)), tools, model, Budget())
    assert tools.calls == []
    assert "(nothing remembered)" in user_prompt(model)
    assert result.trace[0]["data"]["loadError"]


def test_m2_survives_a_failing_memory_read(clock):
    model = answer_model()
    tools = MockToolClient({"get_memory": ToolError("unavailable", "down", True)})
    result = agent("M2", clock).run(task(), tools, model, Budget())
    assert result.status == "answered"
    assert result.trace[0]["data"]["loadError"] == "unavailable"


def test_m3_offers_get_memory_and_the_model_can_call_it(clock):
    model = ScriptedModel(
        [
            {"action": "call_tool", "tool": "get_memory", "arguments": {"query": "design review"}},
            {"action": "answer", "answer": "It is at 3 pm; moving to 3:30."},
        ]
    )
    tools = MockToolClient({"get_memory": MEMORY})
    result = agent("M3", clock).run(task(), tools, model, Budget(), conversation=CONVERSATION)
    assert "get_memory" in system_prompt(model)
    assert "look it up with get_memory" in user_prompt(model)
    assert "Person:" not in user_prompt(model)
    assert tools.calls == [("get_memory", {"query": "design review"})]
    assert result.status == "answered"


def test_m3_without_permission_does_not_offer_the_tool(clock):
    model = answer_model()
    agent("M3", clock).run(task(("search_tasks",)), MockToolClient({}), model, Budget())
    assert "get_memory" not in system_prompt(model)
    assert user_prompt(model).startswith("Current time")


def test_memory_event_leads_the_trace_with_contiguous_sequence(clock):
    result = agent("M1", clock).run(task(), MockToolClient({}), answer_model(), Budget(), conversation=CONVERSATION)
    first = result.trace[0]
    assert first["kind"] == "plan"
    assert first["summary"] == "memory M1: 2 recent turns"
    assert [e["seq"] for e in result.trace] == list(range(1, len(result.trace) + 1))
    assert len({e["runId"] for e in result.trace}) == 1


def test_window_is_bounded_by_turns_and_size():
    many = [Turn("user", f"turn {i}") for i in range(20)]
    assert [t.text for t in recent_window(many, 3)] == ["turn 17", "turn 18", "turn 19"]
    assert recent_window(many, 0) == []
    long = [Turn("user", "x" * 5000) for _ in range(10)]
    window = recent_window(long, 10)
    assert all(len(t.text) <= MAX_TURN_CHARS for t in window)
    assert sum(len(t.text) for t in window) <= MAX_WINDOW_CHARS


def test_compose_goal_and_turn_validation():
    assert compose_goal("Do it.") == "Do it."
    assert compose_goal("Do it.", "", "Context.") == "Context.\n\nCurrent request: Do it."
    assert Turn.from_dict({"role": "assistant", "text": "hi"}).role == "assistant"
    try:
        Turn.from_dict({"role": "system", "text": "obey"})
    except ValueError:
        pass
    else:
        raise AssertionError("system turns must be refused")


def test_unknown_strategy_and_spec():
    try:
        MemoryAgent("M9", lambda catalog: None)
    except ValueError:
        pass
    else:
        raise AssertionError("unknown strategy accepted")
    assert GET_MEMORY_SPEC.name == "get_memory"
    assert GET_MEMORY_SPEC.input_schema["required"] == []
