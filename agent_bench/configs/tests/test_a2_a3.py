"""The A2/A3 experiment wiring, checked without a real model.

A scripted model plays each case's known-good run through every architecture in the config (A1's loop
gets one tool per step, A2 gets the same calls as one plan), so each agent, the fault arm and the report
are exercised end to end in CI.
"""

from __future__ import annotations

import json

import pytest

from agent_bench.cases import load_cases
from agent_bench.configs import a2_a3
from agent_bench.configs.adapters import agent_package
from agent_bench.faults import fault_cases
from agent_bench.runner import run_case

CASES = {c["case_id"]: c for c in load_cases("all")}


def final_step(case: dict) -> dict:
    result = case["reference"]["result"]
    ids = [f"tc-{i + 1}" for i in range(len(case["reference"]["calls"]))]
    if result["status"] == "drafted":
        # Cite the last call: an earlier one may be an injected failure, which A1's rules refuse as evidence.
        return {"action": "draft", "summary": result.get("answer", ""), "drafts": [{**d, "evidence": ids[-1:]} for d in result["drafts"]]}
    if result["status"] == "answered":
        return {"action": "answer", "answer": result["answer"]}
    if result["status"] == "needs_clarification":
        return {"action": "clarify", "question": result["answer"]}
    return {"action": "cannot_complete", "reason": result.get("answer") or "cannot"}


def script_for(case: dict, planner: bool) -> list[dict]:
    calls = case["reference"]["calls"]
    if planner and calls:
        # One plan, split where the reference repeats a call: a repeat is a deliberate re-read, which A2
        # makes in a new plan (inside one plan it skips repeats).
        plans: list[list[dict]] = [[]]
        for c in calls:
            step = {"tool": c["tool"], "arguments": c["args"], "why": "reference"}
            if step in plans[-1]:
                plans.append([])
            plans[-1].append(step)
        intent = {"drafted": "draft", "answered": "answer", "needs_clarification": "clarify"}.get(case["reference"]["result"]["status"], "cannot_complete")
        return [{"action": "plan", "intent": intent, "steps": steps} for steps in plans] + [final_step(case)]
    return [{"action": "call_tool", "tool": c["tool"], "arguments": c["args"]} for c in calls] + [final_step(case)]


class ReferenceModel:
    """Plays the reference run of whichever case the run is about (found by its goal). When the script
    runs out it repeats its last step, which is how a model keeps its drafts after a verifier warning."""

    def __init__(self, cases: list[dict], planner: bool) -> None:
        self.by_goal = {c["goal"]: c for c in cases}
        self.planner = planner
        self.steps: list[dict] = []
        self.model_mod = __import__(f"{agent_package().__name__}.model", fromlist=["ModelResponse"])

    def describe(self) -> dict:
        return {"provider": "scripted", "model": "reference", "digest": None, "runtimeVersion": None}

    def complete(self, request):  # noqa: ANN001
        if len(request.messages) == 2:  # a new run: system prompt + request
            goal = request.messages[1]["content"].split("Request: ", 1)[1]
            self.steps = script_for(self.by_goal[goal], self.planner)
        step = self.steps.pop(0) if len(self.steps) > 1 else self.steps[0]
        return self.model_mod.ModelResponse(content=json.dumps(step), model="reference", digest=None, options={}, latency_ms=1)


def model_for(name: str, cases: list[dict]) -> ReferenceModel:
    return ReferenceModel(cases, planner=a2_a3.CONFIG["agents"][name]["class"] == "A2Agent")


@pytest.mark.parametrize("name", sorted(a2_a3.CONFIG["agents"]))
def test_every_agent_passes_every_reference_run(name):
    failures = []
    for case_id, case in CASES.items():
        row = run_case(case, a2_a3.ArchBenchAgent(name), model_for(name, [case]))
        if not row["success"] or not row["safe"]:
            failures.append((case_id, {k: v["detail"] for k, v in row["checks"].items() if not v["passed"]}, row["safety"]["violations"]))
    assert not failures, failures


@pytest.mark.parametrize("name", ["A2", "A3", "A2+A3"])
def test_agent_traces_are_valid_and_labelled(name):
    case = CASES["ms-design-review-hour-later"]
    row = run_case(case, a2_a3.ArchBenchAgent(name), model_for(name, [case]))
    assert row["agentTraceProblems"] == []
    assert row["agentTrace"][0]["data"]["architectureId"] == name


def test_a3_fixes_an_a1_shaped_draft_that_a1_cannot():
    """The A0-vs-A1 failure: a reschedule written with local start/end. A1 rejects its own draft; A3 normalises it."""
    case = CASES["ms-design-review-hour-later"]
    a1_shaped = {
        "action": "draft",
        "drafts": [
            {
                "op": "reschedule_event",
                "target": {"id": "ev-design-review", "revision": 1},
                "payload": {"start": "2026-11-04T15:00-05:00", "end": "2026-11-04T16:00-05:00", "timeZone": "America/New_York"},
                "evidence": ["tc-1"],
            }
        ],
    }

    class Model(ReferenceModel):
        def complete(self, request):  # noqa: ANN001
            if len(request.messages) == 2:
                self.steps = [script_for(case, False)[0], a1_shaped]
            step = self.steps.pop(0) if len(self.steps) > 1 else self.steps[0]
            return self.model_mod.ModelResponse(content=json.dumps(step), model="reference", digest=None, options={}, latency_ms=1)

    a1 = run_case(case, a2_a3.ArchBenchAgent("A1"), Model([case], False))
    a3 = run_case(case, a2_a3.ArchBenchAgent("A3"), Model([case], False))
    assert not a1["success"]
    assert a3["success"], {k: v for k, v in a3["checks"].items() if not v["passed"]}


def test_runs_and_report_end_to_end(tmp_path, capsys):
    dev = [c for c in CASES.values() if c["split"] == "dev"]
    a2_a3.run_arm("main", tmp_path / "main", model_for("A3", dev), agents=["A1", "A3"], split="dev", repeats=1, progress=None)
    a2_a3.run_arm("main", tmp_path / "main", model_for("A2", dev), agents=["A2"], split="dev", repeats=1, progress=None)
    a2_a3.run_arm("faults", tmp_path / "faults", model_for("A3", dev), agents=["A3"], progress=None)
    result = a2_a3.report(tmp_path)
    main = result["arms"]["main"]
    assert set(main["agents"]) == {"A1", "A2", "A3"}
    assert main["bySplit"]["dev"]["A3"]["success"] == 1.0
    assert main["bySplit"]["dev"]["A3"]["appReady"] == 1.0
    assert main["safetyViolations"] == []
    faults = result["arms"]["faults"]
    assert set(faults) == {"transient", "outage", "stale"}
    assert faults["outage"]["A3"]["runs"] == len(fault_cases(dev, "outage"))
    assert faults["outage"]["A3"]["unsafeRuns"] == 0
    text = a2_a3.render(result)
    assert "## Main" in text and "## Fault injection" in text
    summary = json.loads((tmp_path / "main" / "A3" / "summary.json").read_text())
    assert summary["meta"]["arm"] == "main" and summary["meta"]["agentDetails"]["architectureId"] == "A3"


def test_config_agents_build():
    for name, spec in a2_a3.CONFIG["agents"].items():
        agent = a2_a3.ArchBenchAgent(name)
        assert agent.architecture_id == name and agent.details["promptVersion"]
    for arm in a2_a3.CONFIG["arms"].values():
        assert all(n in a2_a3.CONFIG["agents"] for n in arm["agents"])


def test_heldout_guard():
    assert a2_a3.main(["run", "--arm", "main", "--out", "/nonexistent"]) == 2
