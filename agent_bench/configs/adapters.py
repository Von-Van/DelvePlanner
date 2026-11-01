"""Plug the two architectures of the A0-vs-A1 experiment into agent_bench.

* **A1** lives in ``agent-python/``. ``A1BenchAgent`` translates between the benchmark's shapes and the
  agent package's own (they share field meanings but not Python types, on purpose: neither package
  imports the other).
* **A0** is the existing Rust planner. It has no tools; it gets one snapshot of records and replies with
  one proposal or one question. ``a0_input`` writes each case's starting state for
  ``src-tauri/examples/a0_bench.rs``, which runs the real planner; ``A0ReplayAgent`` then hands those
  recorded replies to the benchmark, turning each planner operation into a draft.
"""

from __future__ import annotations

import copy
import importlib
import json
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

from ..cases import case_set_hash
from ..interfaces import AgentResult, Usage
from ..mock_tools import PlannerState, load_fixture

REPO = Path(__file__).resolve().parents[2]
AGENT_PYTHON = REPO / "agent-python"

# The agent package is renamed delveops_agent -> delveagent before wave 2; accept either.
AGENT_PACKAGES = ("delveagent", "delveops_agent")


def agent_package() -> Any:
    """Import the A1 agent package from the repository checkout (no install needed)."""
    if str(AGENT_PYTHON) not in sys.path:
        sys.path.insert(0, str(AGENT_PYTHON))
    for name in AGENT_PACKAGES:
        try:
            return importlib.import_module(name)
        except ModuleNotFoundError:
            continue
    raise ModuleNotFoundError(f"no agent package ({' or '.join(AGENT_PACKAGES)}) under {AGENT_PYTHON}")


def _submodule(name: str) -> Any:
    return importlib.import_module(f"{agent_package().__name__}.{name}")


# ---------------------------------------------------------------------------------------------------------
# A1


class _ToolBridge:
    """The benchmark's tool client (``{"ok", "result" | "error"}``, the tools.v1 envelope) behind the
    agent's ``ToolClient``."""

    def __init__(self, bench_tools: Any) -> None:
        self.bench_tools = bench_tools
        types = _submodule("types")
        self._ToolResult, self._ToolError = types.ToolResult, types.ToolError

    def call(self, name: str, arguments: dict[str, Any]) -> Any:
        reply = self.bench_tools.call(name, arguments)
        if reply.get("ok"):
            return self._ToolResult(data=reply["result"] if "result" in reply else reply.get("data"))
        return self._ToolResult(error=self._ToolError.from_dict(reply.get("error") or {}))


class A1BenchAgent:
    """The agent package's ``A1Agent``, speaking agent_bench's interface."""

    architecture_id = "A1"

    def __init__(self, debug_trace: bool = False) -> None:
        agent_mod = _submodule("agent")
        self._types = _submodule("types")
        self.agent = agent_mod.A1Agent(debug_trace=debug_trace)
        self.prompt_version = _submodule("prompts").PROMPT_VERSION

    def run(self, task: Any, tools: Any, model: Any, budget: Any) -> dict[str, Any]:
        if model is None:
            raise RuntimeError("A1 needs a model client")
        types = self._types
        agent_task = types.AgentTask.from_dict(
            {"goal": task.goal, "now": task.now, "timeZone": task.timeZone, "permittedTools": list(task.permittedTools)}
        )
        agent_budget = types.Budget.from_dict(
            {
                "maxModelCalls": budget.maxModelCalls,
                "maxToolCalls": budget.maxToolCalls,
                "maxWallSeconds": budget.maxWallSeconds,
                "maxContextTokens": budget.maxContextTokens,
            }
        )
        result = self.agent.run(agent_task, _ToolBridge(tools), model, agent_budget)
        usage = result.usage
        return {
            "status": result.status,
            "answer": result.answer or "",
            "drafts": [d.to_dict() for d in result.drafts],
            # A1 writes "durationMs": null on events without a duration; the shared trace schema wants the
            # key left out instead. Dropping it loses nothing.
            "trace": [{k: v for k, v in event.items() if not (k == "durationMs" and v is None)} for event in result.trace],
            "usage": {
                "modelCalls": usage.model_calls,
                "toolCalls": usage.tool_calls,
                "tokens": usage.prompt_tokens + usage.completion_tokens,
                "latencyMs": usage.wall_ms,
            },
            "stopReason": result.stop_reason,
        }


def a1_model(port: int = 11434, model: str = "qwen3:8b", options: dict[str, Any] | None = None) -> Any:
    """The loopback-only Ollama client from the agent package, with the experiment's settings."""
    model_mod = _submodule("model")
    return model_mod.OllamaModelClient(f"http://127.0.0.1:{port}", model=model, options=options, think=False)


# ---------------------------------------------------------------------------------------------------------
# A0


def starting_state(case: dict) -> dict:
    """The planner state a case starts from: the fixture plus the case's overrides.

    ``bundle`` is a Delve Planner export bundle, which ``a0_bench`` imports as is, so A0 sees the
    benchmark's own ids and revisions.
    """
    state = PlannerState(load_fixture(case["fixture"]["version"]))
    for change in case["fixture"].get("overrides", []):
        state.apply(change)
    return {"bundle": state.bundle, "externalBusy": state.external_busy, "workingHours": state.working_hours}


def a0_applicable(case: dict) -> bool:
    """Whether a case can be put to A0 at all.

    Injected tool errors or responses and mid-run state changes all happen *between tool calls*. A0
    makes no tool calls, so it would never meet the condition the case is testing.
    """
    return not (case.get("injectedErrors") or case.get("injectedToolResponses") or case.get("stateChanges"))


def a0_input(cases: list[dict]) -> dict:
    """Input for ``a0_bench``: each applicable case's goal, day and starting state."""
    rows = []
    for case in cases:
        if not a0_applicable(case):
            continue
        rows.append(
            {
                "caseId": case["case_id"],
                "goal": case["goal"],
                "day": datetime.fromisoformat(case["now"]).date().isoformat(),
                "timeZone": case["timeZone"],
                "state": starting_state(case),
            }
        )
    return {"caseSetHash": case_set_hash(cases), "cases": rows}


# MutationOperation field holding the target record's id, by operation.
TARGET_FIELDS = {
    "update_event": "eventId",
    "delete_event": "eventId",
    "reschedule_event": "eventId",
    "set_event_plan": "eventId",
    "update_plan": "planId",
    "update_milestone": "milestoneId",
    "delete_milestone": "milestoneId",
    "update_task": "taskId",
    "schedule_task": "taskId",
    "set_task_plan": "taskId",
    "delete_task": "taskId",
    "set_task_workstream": "taskId",
}

_ENTITIES = ("events", "plans", "milestones", "tasks", "workstreams", "taskBlocks")


def known_records(state: dict) -> dict[str, int | None]:
    """Record id -> revision for every record in a starting state."""
    bundle = state["bundle"]
    return {row["id"]: row.get("revision") for entity in _ENTITIES for row in bundle.get(entity, [])}


def _revision_of(state: dict, record_id: str) -> int | None:
    for entity in _ENTITIES:
        for row in state["bundle"].get(entity, []):
            if row.get("id") == record_id:
                return row.get("revision")
    return None


def operation_to_draft(operation: dict, state: dict) -> dict:
    """One A0 ``MutationOperation`` as an agent_bench draft.

    The target's revision is the one the record had in the case's starting state, which is what A0's
    snapshot showed it (the operation's own ``expectedRevision`` is the same number, since the database
    was imported from that state).
    """
    payload = copy.deepcopy(operation)
    op = payload.pop("type", None)
    payload.pop("expectedRevision", None)
    field = TARGET_FIELDS.get(op)
    target = None
    if field is not None:
        record_id = payload.pop(field, None)
        target = {"id": record_id, "revision": _revision_of(state, str(record_id))}
    return {"op": op, "target": target, "payload": payload, "evidence": ["a0-snapshot"]}


def a0_result(run: dict, state: dict) -> AgentResult:
    """A recorded ``a0_bench`` reply in agent_bench's result shape."""
    kind = run.get("kind")
    usage = Usage(modelCalls=1, toolCalls=0, latencyMs=float(run.get("latencyMs") or 0))
    trace = [
        {
            "runId": f"a0-{run['caseId']}-{run.get('repeat', 0)}",
            "seq": 1,
            "kind": "model_call",
            "at": run.get("at") or "1970-01-01T00:00:00Z",
            "durationMs": run.get("latencyMs") or 0,
            "summary": f"A0 planner reply: {kind}",
            "data": {"kind": kind, "error": run.get("error")},
        }
    ]
    if kind == "proposal":
        drafts = [operation_to_draft(op, state) for op in run.get("operations", [])]
        return AgentResult(status="drafted", answer=run.get("summary") or "", drafts=drafts, trace=trace, usage=usage)
    if kind == "clarification":
        return AgentResult(status="needs_clarification", answer=run.get("question") or "", trace=trace, usage=usage)
    # The app shows the person an error; nothing is proposed. Scored as declining.
    return AgentResult(status="cannot_complete", answer=run.get("error") or "", trace=trace, usage=usage)


class A0ReplayAgent:
    """Replays ``a0_bench`` output through the harness so A0 is scored by the same code as A1.

    The harness hands an agent only the task, so a recorded reply is found by the case's goal and
    frozen time, and repeats are taken in the order the harness runs them.
    """

    architecture_id = "A0"

    def __init__(self, output: dict, cases: list[dict]) -> None:
        self.meta = {k: v for k, v in output.items() if k != "runs"}
        self.runs = {(r["caseId"], r.get("repeat", 0)): r for r in output.get("runs", [])}
        self.cases = {(c["goal"], c["now"]): c["case_id"] for c in cases}
        self.states = {c["case_id"]: starting_state(c) for c in cases}
        self.seen: dict[str, int] = {}

    @classmethod
    def from_file(cls, path: Path, cases: list[dict]) -> A0ReplayAgent:
        return cls(json.loads(Path(path).read_text()), cases)

    def repeats(self) -> int:
        return 1 + max((repeat for _, repeat in self.runs), default=-1)

    def case_ids(self) -> set[str]:
        return {case_id for case_id, _ in self.runs}

    def run(self, task: Any, tools: Any, model: Any, budget: Any) -> AgentResult:
        case_id = self.cases[(task.goal, task.now)]
        repeat = self.seen.get(case_id, 0)
        self.seen[case_id] = repeat + 1
        if (case_id, repeat) not in self.runs:
            raise KeyError(f"no recorded A0 reply for {case_id} repeat {repeat}")
        run = {"at": self.meta.get("generatedAt"), **self.runs[(case_id, repeat)]}
        return a0_result(run, self.states[case_id])
