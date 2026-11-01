"""The M0–M3 memory comparison wiring, checked without a real model."""

from __future__ import annotations

import json

from agent_bench.cases import load_cases
from agent_bench.configs import memory_experiment
from agent_bench.configs.memory_experiment import MemoryBenchAgent, compare, memory_cases, render
from agent_bench.configs.tests.test_experiment import ScriptedModel
from agent_bench.mock_tools import MockToolClient
from agent_bench.runner import run_case, run_suite

CASES = {c["case_id"]: c for c in load_cases("all")}


class FixedModel(ScriptedModel):
    """Plays a fixed list of A1 steps and records every request."""

    def __init__(self, steps: list[dict]) -> None:
        super().__init__({"reference": {"calls": [], "result": {"status": "cannot_complete", "answer": "", "drafts": []}}})
        self.steps = list(steps)
        self.requests: list = []

    def complete(self, request):  # noqa: ANN001
        self.requests.append(request)
        return super().complete(request)


def test_memory_cases_cover_both_families_and_splits():
    cases = memory_cases("all")
    assert {c["family"] for c in cases} == {"memory", "continuity"}
    assert {c["split"] for c in cases} == {"dev", "heldout"}
    assert all("get_memory" in c["permittedTools"] for c in cases)


def test_reference_runs_pass_under_the_strategy_that_can_replay_them():
    """M3 can replay references that read memory; M1 those that rely on the conversation."""
    for case in memory_cases("all"):
        uses_memory = any(c["tool"] == "get_memory" for c in case["reference"]["calls"])
        strategy = "M3" if uses_memory else "M1"
        row = run_case(case, MemoryBenchAgent(strategy), ScriptedModel(case))
        failed = {k: v["detail"] for k, v in row["checks"].items() if not v["passed"]}
        assert row["success"], (case["case_id"], strategy, failed)
        assert row["safe"], row["safety"]["violations"]
        assert row["agentTraceProblems"] == [], row["agentTraceProblems"]
        assert row["agentTrace"][0]["summary"].startswith(f"memory {strategy}")


def test_outdated_conversation_is_scored_as_false_recall():
    case = CASES["mem-sync-day-corrected"]
    model = FixedModel([{"action": "answer", "answer": "It's on Tuesdays at 10 am."}])
    row = run_case(case, MemoryBenchAgent("M1"), model)
    assert "Tuesdays at 10" in model.requests[0].messages[1]["content"]
    assert row["metrics"]["outdatedRecall"] is True
    assert row["checks"]["memory_recall"]["passed"] is False
    assert not row["success"]


def test_m2_preloads_only_current_confirmed_records():
    case = CASES["mem-decision-recall"]
    model = FixedModel([{"action": "answer", "answer": "Yes, the blog moves to phase 2."}])
    row = run_case(case, MemoryBenchAgent("M2"), model)
    prompt = model.requests[0].messages[1]["content"]
    assert "blog moves to phase 2" in prompt
    assert "dropped entirely" not in prompt, "an expired, unconfirmed guess must not be loaded"
    assert [c["tool"] for c in row["calls"]] == ["get_memory"]
    assert row["success"], row["checks"]


def test_switched_off_project_memory_never_reaches_the_model():
    case = CASES["mem-project-switched-off"]
    tools = MockToolClient(case)
    reply = tools.call("get_memory", {"query": "moving company"})
    assert reply["ok"] and reply["data"]["records"] == []
    model = FixedModel([{"action": "cannot_complete", "reason": "I have no record of a moving company."}])
    row = run_case(case, MemoryBenchAgent("M2"), model)
    assert "Bluebird" not in model.requests[0].messages[1]["content"]
    assert row["safe"]


def test_mock_get_memory_ranks_and_filters_like_the_rust_tool():
    case = {
        **CASES["mem-sync-day-corrected"],
        "memory": {
            "records": [
                {"id": "a", "kind": "note", "content": "Design review probably moves.", "source": "assistant_inferred", "confirmation": "unconfirmed", "planId": None, "recordedAt": "2026-11-01T00:00:00Z", "expiresAt": "2026-12-01T00:00:00Z"},
                {"id": "b", "kind": "decision", "content": "Design review is at 3 pm.", "source": "user_stated", "confirmation": "confirmed", "planId": None, "recordedAt": "2026-10-01T00:00:00Z"},
                {"id": "c", "kind": "decision", "content": "Design reviews happen weekly.", "source": "user_stated", "confirmation": "confirmed", "planId": "plan-move", "recordedAt": "2026-10-02T00:00:00Z"},
                {"id": "d", "kind": "decision", "content": "Old design review time.", "source": "user_stated", "confirmation": "confirmed", "planId": None, "recordedAt": "2026-09-01T00:00:00Z", "supersededBy": "b"},
                {"id": "e", "kind": "note", "content": "Expired design review guess.", "source": "assistant_inferred", "confirmation": "unconfirmed", "planId": None, "recordedAt": "2026-10-01T00:00:00Z", "expiresAt": "2026-10-15T00:00:00Z"},
                {"id": "f", "kind": "note", "content": "Unrelated.", "source": "user_stated", "confirmation": "confirmed", "planId": None, "recordedAt": "2026-10-03T00:00:00Z"},
            ]
        },
    }
    tools = MockToolClient(case)
    data = tools.call("get_memory", {"query": "the design reviews"})["data"]
    assert [r["id"] for r in data["records"]] == ["c", "b", "a"]
    assert data["records"][2]["expiresAt"] == "2026-12-01T00:00:00Z"
    data = tools.call("get_memory", {"query": "design review", "planId": "plan-launch"})["data"]
    assert [r["id"] for r in data["records"]] == ["b", "a"]
    data = tools.call("get_memory", {"kinds": ["note"], "limit": 1})["data"]
    assert [r["id"] for r in data["records"]] == ["f"] and data["truncated"] and data["totalMatches"] == 2


def test_compare_reports_success_and_false_recall_per_strategy(tmp_path):
    cases = [CASES["mem-sync-day-corrected"], CASES["cont-thirty-minutes-later"]]

    class Script:
        def __init__(self, strategy: str) -> None:
            self.strategy = strategy

        def model_for(self, case: dict):
            if self.strategy == "M1" and case["case_id"] == "mem-sync-day-corrected":
                return FixedModel([{"action": "answer", "answer": "Tuesdays."}])
            if self.strategy == "M0":
                return FixedModel([{"action": "clarify", "question": "Which one do you mean?"}])
            return ScriptedModel(case)

    for strategy in ("M0", "M1", "M3"):
        script = Script(strategy)
        agent = MemoryBenchAgent(strategy)

        class PerCase:
            architecture_id = agent.architecture_id

            def run(self, task, tools, model, budget):  # noqa: ANN001
                case = next(c for c in cases if c["goal"] == task.goal)
                return agent.run(task, tools, script.model_for(case), budget)

        run_suite(cases, PerCase(), model=FixedModel([]), out_dir=tmp_path / strategy, agent_spec=strategy)
    result = compare(tmp_path)
    assert result["strategies"] == ["M0", "M1", "M3"]
    assert result["overall"]["M0"]["successRate"] == 0
    assert result["overall"]["M1"]["falseRecallRate"] == 0.5
    assert result["overall"]["M3"]["falseRecallRate"] == 0
    by_case = {e["caseId"]: e for e in result["cases"]}
    assert by_case["mem-sync-day-corrected"]["M1"]["outdatedRecall"] == 1
    assert by_case["cont-thirty-minutes-later"]["M1"]["solved"]
    assert by_case["cont-thirty-minutes-later"]["M1"]["targetResolved"] == 1
    assert by_case["cont-thirty-minutes-later"]["M0"]["targetResolved"] == 0
    assert by_case["mem-sync-day-corrected"]["M1"]["targetResolved"] is None
    assert result["overall"]["M1"]["targetResolvedRate"] == 1
    text = render(result)
    assert "| M1 |" in text and "⚠" in text
    json.dumps(result)


def test_cli_refuses_heldout_without_confirmation(capsys):
    assert memory_experiment.main(["run", "--out", "x"]) == 2
    assert "held-out" in capsys.readouterr().err
