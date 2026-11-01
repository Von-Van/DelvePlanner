"""M4 against the benchmark: the agent recovers from injected faults, or stops honestly.

Every dev case is re-asked under each fault profile from `agent_bench/faults` (a lookup that times out
once, a lookup that stays down, the target edited mid-run). The model is a *naive* stand-in: it replays
the case's known-good lookups once, never retries and never re-reads by itself, and follows whatever the
agent's self-check tells it. So any recovery here comes from the agent loop, not from a careful model.

Scored by agent_bench's own scorers, through its mock tools (proven identical to the Rust tools).
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

import pytest

REPO = Path(__file__).resolve().parents[2]
if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))

from agent_bench.cases import load_cases  # noqa: E402
from agent_bench.faults import PROFILES, fault_cases, graceful, original_id  # noqa: E402
from agent_bench.runner import run_case  # noqa: E402

from delveops_agent import A1Agent, AgentTask, Budget, ToolError, ToolResult  # noqa: E402
from delveops_agent.model import ModelResponse  # noqa: E402

CASES = {c["case_id"]: c for c in load_cases("dev")}
FAULTED = [(p, c) for p in PROFILES for c in fault_cases(list(CASES.values()), p)]
TOOL_RESULT = re.compile(r"^Tool result (tc-\d+) ")


class _Tools:
    """agent_bench's tool client (the tools.v1 envelope) behind the agent's ToolClient."""

    def __init__(self, bench_tools: Any) -> None:
        self.bench = bench_tools

    def call(self, name: str, arguments: dict[str, Any]) -> ToolResult:
        reply = self.bench.call(name, arguments)
        if reply.get("ok"):
            return ToolResult(data=reply.get("result", reply.get("data")))
        error = reply.get("error") or {}
        return ToolResult(
            error=ToolError(error.get("code", "internal"), error.get("message", ""), bool(error.get("retryable")))
        )


class NaiveModel:
    """Replays a case's reference lookups once, then its reference result; obeys the self-check."""

    def __init__(self, case: dict) -> None:
        reference = case["reference"]
        self.lookups = [{"action": "call_tool", "tool": c["tool"], "arguments": c["args"]} for c in reference["calls"]]
        self.result = reference["result"]
        self.last_drafts: list[dict] | None = None

    def describe(self) -> dict[str, Any]:
        return {"provider": "scripted", "model": "naive", "digest": "sha256:test", "options": {}}

    def complete(self, request: Any) -> ModelResponse:
        step = self.next_step(request.messages)
        return ModelResponse(json.dumps(step), "naive", "sha256:test", {}, 1, 10, 10)

    def next_step(self, messages: list[dict[str, str]]) -> dict[str, Any]:
        last = messages[-1]["content"]
        if last.startswith("Tool error"):
            return {"action": "cannot_complete", "reason": "A lookup I need is failing."}
        if self.last_drafts is not None and ("re-read their records" in last or "Draft rejected" in last):
            return self.draft_step(messages, self.last_drafts, refresh=True)  # redraft on what the tools show now
        if self.last_drafts is not None and "check them against what you read" in last:
            return self.draft_step(messages, self.last_drafts, refresh=False)  # keep the reference choice
        if self.lookups:
            return self.lookups.pop(0)
        status = self.result["status"]
        if status == "drafted":
            self.last_drafts = self.result["drafts"]
            return self.draft_step(messages, self.last_drafts, refresh=False)
        if status == "answered":
            return {"action": "answer", "answer": self.result.get("answer") or "Done."}
        if status == "needs_clarification":
            return {"action": "clarify", "question": self.result.get("answer") or "Which one?"}
        return {"action": "cannot_complete", "reason": self.result.get("answer") or "I can't."}

    def draft_step(self, messages: list[dict[str, str]], drafts: list[dict], refresh: bool) -> dict[str, Any]:
        seen = [m.group(1) for m in (TOOL_RESULT.match(x["content"]) for x in messages if x["role"] == "user") if m]
        out = []
        for draft in drafts:
            draft = json.loads(json.dumps(draft))
            target = draft.get("target")
            if refresh and isinstance(target, dict):
                target["revision"] = self.revision_shown(messages, target["id"], target["revision"])
            draft["evidence"] = seen[-1:]
            out.append(draft)
        return {"action": "draft", "summary": self.result.get("answer") or "Drafted.", "drafts": out}

    @staticmethod
    def revision_shown(messages: list[dict[str, str]], record_id: str, default: int) -> int:
        """The revision of a record in the latest tool result that shows it (what a model would read)."""
        for message in reversed(messages):
            if TOOL_RESULT.match(message["content"]) and record_id in message["content"]:
                data = json.loads(message["content"].split("\n", 1)[1])
                found = _find_revision(data, record_id)
                if found is not None:
                    return found
        return default


def _find_revision(value: Any, record_id: str) -> int | None:
    if isinstance(value, dict):
        if value.get("id") == record_id and isinstance(value.get("revision"), int):
            return value["revision"]
        value = list(value.values())
    if isinstance(value, list):
        for child in value:
            hit = _find_revision(child, record_id)
            if hit is not None:
                return hit
    return None


class BenchAgent:
    """The assistant agent speaking agent_bench's interface."""

    def __init__(self, self_check: bool = True) -> None:
        self.agent = A1Agent(self_check=self_check)

    def run(self, task: Any, tools: Any, model: NaiveModel, budget: Any) -> dict[str, Any]:
        agent_task = AgentTask(task.goal, task.now, task.timeZone, tuple(task.permittedTools))
        agent_budget = Budget.from_dict(
            {
                "maxModelCalls": budget.maxModelCalls,
                "maxToolCalls": budget.maxToolCalls,
                "maxWallSeconds": budget.maxWallSeconds,
                "maxContextTokens": budget.maxContextTokens,
            }
        )
        result = self.agent.run(agent_task, _Tools(tools), model, agent_budget)
        return {
            "status": result.status,
            "answer": result.answer or "",
            "drafts": [d.to_dict() for d in result.drafts],
            "trace": [{k: v for k, v in e.items() if not (k == "durationMs" and v is None)} for e in result.trace],
            "stopReason": result.stop_reason,
        }


def run_naive(case: dict, self_check: bool = True) -> dict:
    return run_case(case, BenchAgent(self_check), NaiveModel(CASES[original_id(case["case_id"])]))


def failed_checks(row: dict) -> dict:
    return {k: v for k, v in row["checks"].items() if not v["passed"]}


@pytest.mark.parametrize("case", list(CASES.values()), ids=list(CASES))
def test_the_naive_model_solves_every_unfaulted_case(case):
    row = run_naive(case)
    assert row["safe"], row["safety"]
    assert row["success"], failed_checks(row)


@pytest.mark.parametrize("profile,case", FAULTED, ids=[c["case_id"] for _, c in FAULTED])
def test_faults_are_recovered_or_stopped_honestly(profile, case):
    row = run_naive(case)
    assert row["safe"], row["safety"]
    if PROFILES[profile].recoverable:
        assert row["success"], failed_checks(row)
    else:
        assert graceful(row), (row["status"], failed_checks(row))


def test_without_the_self_check_the_same_model_does_not_recover():
    stale = [c for p, c in FAULTED if p == "stale"]
    transient = [c for p, c in FAULTED if p == "transient"]
    assert not any(run_naive(c, self_check=False)["success"] for c in transient)
    assert not all(run_naive(c, self_check=False)["success"] for c in stale)
