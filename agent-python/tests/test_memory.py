from __future__ import annotations

from conftest import ScriptedModel

from delveops_agent import A1Agent, AgentTask, Budget, MockToolClient, ToolError
from delveops_agent.memory import (
    GET_MEMORY_SPEC,
    MEMORY_KINDS,
    MemoryAgent,
    Turn,
    compose_goal,
    recent_window,
)
from delveops_agent.memory.agent import MAX_TURN_CHARS, MAX_WINDOW_CHARS

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
            "trust": "verified",
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
            "trust": "inferred",
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


def agent(clock, window_turns: int = 6) -> MemoryAgent:
    return MemoryAgent(lambda catalog: A1Agent(catalog=catalog, clock=clock), window_turns=window_turns)


def answer_model() -> ScriptedModel:
    return ScriptedModel([{"action": "answer", "answer": "ok"}])


def user_prompt(model: ScriptedModel) -> str:
    return model.requests[0].messages[1]["content"]


def system_prompt(model: ScriptedModel) -> str:
    return model.requests[0].messages[0]["content"]


def test_sees_recent_turns_and_is_offered_get_memory(clock):
    model = answer_model()
    tools = MockToolClient({"get_memory": MEMORY})
    result = agent(clock).run(task(), tools, model, Budget(), conversation=CONVERSATION)
    prompt = user_prompt(model)
    assert "Person: Move the design review to 3 pm." in prompt
    assert "look it up with get_memory" in prompt
    assert "never present it as confirmed" in prompt
    assert prompt.rstrip().endswith("Current request: Move it 30 minutes later.")
    assert "get_memory" in system_prompt(model)
    assert tools.calls == [], "nothing is pre-loaded: retrieval is the model's choice"
    assert result.status == "answered"
    assert result.architecture_id == "A1+memory"


def test_the_model_retrieves_memory_selectively(clock):
    model = ScriptedModel(
        [
            {"action": "call_tool", "tool": "get_memory", "arguments": {"query": "design review"}},
            {"action": "answer", "answer": "It is at 3 pm; moving to 3:30."},
        ]
    )
    tools = MockToolClient({"get_memory": MEMORY})
    result = agent(clock).run(task(), tools, model, Budget(), conversation=CONVERSATION)
    assert tools.calls == [("get_memory", {"query": "design review"})]
    assert result.status == "answered"
    assert result.usage.tool_calls == 1


def test_window_setting_zero_shows_no_conversation(clock):
    model = answer_model()
    agent(clock, window_turns=0).run(task(), MockToolClient({}), model, Budget(), conversation=CONVERSATION)
    assert "Person:" not in user_prompt(model)


def test_without_permission_get_memory_is_not_offered(clock):
    model = answer_model()
    result = agent(clock).run(task(("search_tasks",)), MockToolClient({}), model, Budget())
    assert "get_memory" not in system_prompt(model)
    assert user_prompt(model).startswith("Current time")
    assert result.trace[0]["summary"] == "memory: 0 recent turns, get_memory not permitted"


def test_a_failing_memory_read_is_survivable(clock):
    model = ScriptedModel(
        [
            {"action": "call_tool", "tool": "get_memory", "arguments": {}},
            {"action": "answer", "answer": "I couldn't read memory, so I'm going from the planner."},
        ]
    )
    tools = MockToolClient({"get_memory": ToolError("unavailable", "down", True)})
    result = agent(clock).run(task(), tools, model, Budget())
    assert result.status == "answered"


def test_memory_event_leads_the_trace_with_contiguous_sequence(clock):
    result = agent(clock).run(task(), MockToolClient({}), answer_model(), Budget(), conversation=CONVERSATION)
    first = result.trace[0]
    assert first["kind"] == "plan"
    assert first["summary"] == "memory: 2 recent turns, get_memory offered"
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


def test_a_long_reply_keeps_the_person_s_feedback_note():
    note = (
        ' [The person said this reply was not helpful because: "I\'m out after 3 tomorrow".'
        " This applies to this conversation only.]"
    )
    reply = Turn("assistant", "Drafted: " + "a long plan " * 100 + note)
    (clipped,) = recent_window([reply], 1)
    assert len(clipped.text) <= MAX_TURN_CHARS
    assert clipped.text.endswith(note)
    assert "…" in clipped.text
    # A note longer than half the budget (a long change description and reason) is still kept.
    long_note = (
        " [The person turned down the drafted change. " + "y" * 300 + " This applies to this conversation only.]"
    )
    (kept,) = recent_window([Turn("assistant", "Drafted: " + "z" * 2000 + long_note)], 1)
    assert len(kept.text) <= MAX_TURN_CHARS and kept.text.endswith(long_note)
    assert kept.text.startswith("Drafted: z")
    # A note that can't fit keeps its end, where the newest feedback is, and some of the reply.
    huge = " [" + "old feedback " * 80 + "newest: no deep work after 4.]"
    (cut,) = recent_window([Turn("assistant", "Drafted: " + "z" * 2000 + huge)], 1)
    assert len(cut.text) <= MAX_TURN_CHARS
    assert cut.text.startswith("Drafted: z") and cut.text.endswith("newest: no deep work after 4.]")
    # Text with no closing note is clipped at the end as before.
    (plain,) = recent_window([Turn("assistant", "x" * 5000)], 1)
    assert len(plain.text) <= MAX_TURN_CHARS and plain.text.endswith("…")


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


def test_spec_matches_the_contract_kinds():
    import json
    from pathlib import Path

    contract = json.loads((Path(__file__).resolve().parents[2] / "contracts" / "tools.v1.json").read_text())
    kinds = contract["$defs"]["memoryKind"]["enum"]
    assert list(MEMORY_KINDS) == kinds
    assert GET_MEMORY_SPEC.name == "get_memory"
    assert GET_MEMORY_SPEC.input_schema["required"] == []
