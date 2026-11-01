"""The A0-vs-A1 wiring, checked without a real model.

A scripted model replays each case's known-good run through the real A1 loop, and a hand-written
``a0_bench`` reply goes through the A0 replay, so both paths are exercised end to end in CI.
"""

from __future__ import annotations

import copy
import json

import pytest

from agent_bench.cases import load_cases
from agent_bench.configs import experiment
from agent_bench.configs.adapters import (
    A0ReplayAgent,
    A1BenchAgent,
    a0_applicable,
    a0_input,
    agent_package,
    operation_to_draft,
    starting_state,
)
from agent_bench.runner import run_case, run_suite

CASES = {c["case_id"]: c for c in load_cases("all")}


class ScriptedModel:
    """Answers with a case's reference run, one A1 step per call."""

    def __init__(self, case: dict) -> None:
        ref = case["reference"]
        self.steps = [{"action": "call_tool", "tool": c["tool"], "arguments": c["args"]} for c in ref["calls"]]
        result = ref["result"]
        tool_ids = [f"tc-{i + 1}" for i in range(len(ref["calls"]))]
        if result["status"] == "drafted":
            drafts = [{**d, "evidence": tool_ids} for d in result["drafts"]]
            self.steps.append({"action": "draft", "summary": result.get("answer", ""), "drafts": drafts})
        elif result["status"] == "answered":
            self.steps.append({"action": "answer", "answer": result["answer"]})
        elif result["status"] == "needs_clarification":
            self.steps.append({"action": "clarify", "question": result["answer"]})
        else:
            self.steps.append({"action": "cannot_complete", "reason": result.get("answer") or "cannot"})
        self.model_mod = __import__(f"{agent_package().__name__}.model", fromlist=["ModelResponse"])

    def describe(self) -> dict:
        return {"provider": "scripted", "model": "reference", "digest": None}

    def complete(self, request):  # noqa: ANN001
        step = self.steps.pop(0) if self.steps else {"action": "cannot_complete", "reason": "script ended"}
        return self.model_mod.ModelResponse(content=json.dumps(step), model="reference", digest=None, options={}, latency_ms=1)


def test_a1_adapter_passes_reference_runs():
    """With a model that plays the known-good run, A1 must pass every case the harness can replay
    this way. Cases whose reference depends on harness behaviour A1's loop stops on are listed."""
    failures = []
    for case_id, case in CASES.items():
        if any(call["tool"] == "get_memory" for call in case["reference"]["calls"]):
            continue  # needs get_memory, which only the memory wrapper offers; replayed in test_memory_check.py
        row = run_case(case, A1BenchAgent(), ScriptedModel(case))
        if not row["success"] or not row["safe"]:
            failures.append((case_id, {k: v["detail"] for k, v in row["checks"].items() if not v["passed"]}, row["safety"]["violations"]))
    # A1 stops after the same tool error twice (by design); a case whose reference retries the
    # identical failing call more than that cannot be replayed step for step.
    assert all(not row[2] for row in failures), failures
    assert not failures, failures


def test_a1_adapter_reports_usage_and_valid_trace():
    case = CASES["tc-today-agenda"]
    row = run_case(case, A1BenchAgent(), ScriptedModel(case))
    assert row["metrics"]["agentUsage"]["modelCalls"] >= 2
    assert row["metrics"]["agentUsage"]["toolCalls"] == len(case["reference"]["calls"])
    assert row["agentTraceProblems"] == []


def test_a0_input_skips_cases_that_need_tool_calls():
    payload = a0_input(list(CASES.values()))
    ids = {c["caseId"] for c in payload["cases"]}
    for case_id, case in CASES.items():
        assert (case_id in ids) == a0_applicable(case)
    assert "stale-event-moved" not in ids and "err-timeout-then-retry" not in ids
    sample = payload["cases"][0]
    assert set(sample) == {"caseId", "goal", "day", "timeZone", "state"}
    assert sample["state"]["events"], "the starting state carries the planner records"


def test_starting_state_applies_overrides():
    case = next(c for c in CASES.values() if c["fixture"].get("overrides"))
    change = case["fixture"]["overrides"][0]
    state = starting_state(case)
    if "add" in change:
        assert change["add"] in state[change["entity"]]
    else:
        row = next(r for r in state[change["entity"]] if r["id"] == change["id"])
        assert all(row[k] == v for k, v in change["fields"].items())


def test_operation_to_draft_moves_target_and_revision():
    state = starting_state(CASES["tc-today-agenda"])
    draft = operation_to_draft(
        {"type": "reschedule_event", "eventId": "ev-lunch", "expectedRevision": 1, "title": None, "notes": None, "startAtUtc": "2026-11-03T19:00:00Z", "timeZone": "America/New_York", "durationMinutes": None, "reminderChange": {"action": "unchanged"}},
        state,
    )
    assert draft["op"] == "reschedule_event"
    assert draft["target"] == {"id": "ev-lunch", "revision": 2}
    assert "eventId" not in draft["payload"] and "expectedRevision" not in draft["payload"]
    create = operation_to_draft({"type": "create_task", "title": "x", "description": "", "plan": {"id": "plan-launch"}, "milestone": None, "scheduledDay": None, "dueDate": None, "status": "todo", "priority": "normal"}, state)
    assert create["target"] is None and create["payload"]["plan"] == {"id": "plan-launch"}


def _a0_output(cases: list[dict], repeats: int = 1) -> dict:
    """What a0_bench would write if A0 gave each case's reference reply (in A0's own shapes)."""
    runs = []
    for repeat in range(repeats):
        for case in cases:
            if not a0_applicable(case):
                continue
            ref = case["reference"]["result"]
            run = {"caseId": case["case_id"], "repeat": repeat, "latencyMs": 900, "operations": []}
            if ref["status"] == "drafted":
                run["kind"], run["summary"] = "proposal", ref.get("answer", "")
                for d in ref["drafts"]:
                    op = {"type": d["op"], **copy.deepcopy(d["payload"])}
                    if d["target"]:
                        field = {"event": "eventId", "task": "taskId", "plan": "planId", "milestone": "milestoneId"}
                        key = next(v for k, v in field.items() if k in d["op"])
                        op[key], op["expectedRevision"] = d["target"]["id"], 1
                    run["operations"].append(op)
            elif ref["status"] == "needs_clarification":
                run["kind"], run["question"] = "clarification", ref["answer"]
            else:
                # A0 can only propose or ask; an answer-type case gets a polite question back.
                run["kind"], run["question"] = "clarification", "Which change would you like to make?"
            runs.append(run)
    return {"architectureId": "A0", "generatedAt": "2026-11-01T12:00:00.000Z", "modelName": "qwen3:8b", "modelDigest": "d", "ollamaVersion": "0.32.0", "runs": runs}


def test_a0_replay_scores_through_the_harness(tmp_path):
    cases = list(CASES.values())
    output = _a0_output(cases, repeats=2)
    agent = A0ReplayAgent(output, cases)
    chosen = [c for c in cases if c["case_id"] in agent.case_ids()]
    summary = run_suite(chosen, agent, repeats=agent.repeats(), out_dir=tmp_path / "A0")
    rows = summary["results"]
    assert len(rows) == 2 * len(chosen)
    by_id = {r["caseId"]: r for r in rows if r["repeat"] == 0}
    # A drafting case A0 gets right passes on outcome even though A0 called no tools.
    drafting = [c for c in chosen if c["reference"]["result"]["status"] == "drafted"]
    assert drafting
    for case in drafting:
        assert experiment.outcome_success(by_id[case["case_id"]]), by_id[case["case_id"]]["checks"]
        # The harness alone calls every A0 draft ungrounded; the experiment's A0 rule does not.
        assert experiment.a0_safety(by_id[case["case_id"]], starting_state(case)) == []


def test_a0_safety_still_catches_invented_targets():
    case = CASES["tc-today-agenda"]
    row = {"safety": {"violations": []}, "drafts": [{"op": "delete_event", "target": {"id": "ev-nope", "revision": None}, "payload": {}}]}
    assert experiment.a0_safety(row, starting_state(case))[0]["rule"] == "ungrounded_draft"


def test_compare_end_to_end(tmp_path):
    cases = list(CASES.values())
    agent = A0ReplayAgent(_a0_output(cases), cases)
    chosen = [c for c in cases if c["case_id"] in agent.case_ids()]
    run_suite(chosen, agent, out_dir=tmp_path / "A0")
    summary_path = tmp_path / "A0" / "summary.json"
    data = json.loads(summary_path.read_text())
    data["meta"]["a0"] = {k: v for k, v in _a0_output([]).items() if k != "runs"}
    summary_path.write_text(json.dumps(data))

    class PerCaseModel:
        def __init__(self) -> None:
            self.current = None

        def describe(self) -> dict:
            return {"model": "reference", "digest": None}

    a1_rows = []
    for case in cases:
        a1_rows.append(run_case(case, A1BenchAgent(), ScriptedModel(case)))
    (tmp_path / "A1").mkdir()
    (tmp_path / "A1" / "results.jsonl").write_text("\n".join(json.dumps({k: v for k, v in r.items() if k not in ("trace", "agentTrace")}) for r in a1_rows))
    (tmp_path / "A1" / "summary.json").write_text(json.dumps({"meta": {"repeats": 1, "model": PerCaseModel().describe()}}))

    assert experiment.main(["compare", "--a0", str(tmp_path / "A0"), "--a1", str(tmp_path / "A1"), "--out", str(tmp_path / "cmp")]) == 0
    result = json.loads((tmp_path / "cmp" / "comparison.json").read_text())
    assert result["bySplit"]["dev"]["overlap"]["cases"] > 0
    assert {e["verdict"] for e in result["cases"]} <= {"both", "A1 only", "A0 only", "neither", "A1 only tested", "A1 only tested, not solved"}
    # Answer-type cases are ones A0 cannot do: it can only propose or ask.
    answered = [e for e in result["cases"] if CASES[e["caseId"]]["expected"]["status"] == ["answered"] and e["a0Applicable"]]
    assert answered and all(e["verdict"] in ("A1 only", "neither") for e in answered)
    assert "## Per case" in (tmp_path / "cmp" / "comparison.md").read_text()


def test_heldout_needs_confirmation(tmp_path, capsys):
    assert experiment.main(["a0-input", "--out", str(tmp_path / "x.json")]) == 2
    assert experiment.main(["a0-input", "--split", "dev", "--out", str(tmp_path / "x.json")]) == 0


@pytest.mark.parametrize("name", ["a0_vs_a1.json"])
def test_config_names_the_frozen_model(name):
    assert experiment.CONFIG["model"]["name"] == "qwen3:8b"
    assert set(experiment.CONFIG["scoring"]["processChecks"]) == {"required_tools", "tool_order", "required_calls", "call_limits"}
