"""Memory recall and false recall, checked without a real model.

The plan's M2 tests: recall a confirmed decision or priority; never use a superseded, expired or
switched-off memory; never present an inference as confirmed; deleted means gone (Rust tests
cover deletion; here the benchmark's get_memory must leave out anything not in the store).
"""

from __future__ import annotations

from agent_bench.cases import load_cases
from agent_bench.configs.memory_check import MemoryBenchAgent, headline, memory_cases, main
from agent_bench.configs.tests.test_experiment import ScriptedModel
from agent_bench.mock_tools import MockToolClient, load_fixture
from agent_bench.runner import run_case

CASES = {c["case_id"]: c for c in load_cases("all")}
ID = load_fixture("planner.v2")["ids"]


class FixedModel(ScriptedModel):
    """Plays a fixed list of agent steps and records every request."""

    def __init__(self, steps: list[dict]) -> None:
        super().__init__({"reference": {"calls": [], "result": {"status": "cannot_complete", "answer": "", "drafts": []}}})
        self.steps = list(steps)
        self.requests: list = []

    def complete(self, request):  # noqa: ANN001
        self.requests.append(request)
        return super().complete(request)


def records(reply: dict) -> list[dict]:
    assert reply["ok"], reply
    return reply["result"]["records"]


def test_memory_cases_cover_both_families_and_splits():
    cases = memory_cases("all")
    assert {c["family"] for c in cases} == {"memory", "continuity"}
    assert {c["split"] for c in cases} == {"dev", "heldout"}
    assert all("get_memory" in c["permittedTools"] for c in cases)


def test_reference_runs_pass_with_memory():
    for case in memory_cases("all"):
        row = run_case(case, MemoryBenchAgent(), ScriptedModel(case))
        failed = {k: v["detail"] for k, v in row["checks"].items() if not v["passed"]}
        assert row["success"], (case["case_id"], failed)
        assert row["safe"], row["safety"]["violations"]
        assert row["agentTraceProblems"] == [], row["agentTraceProblems"]
        assert row["agentTrace"][0]["summary"].startswith("memory: ")


def test_recalls_a_confirmed_priority_and_uses_it():
    case = CASES["mem-priority-next-step"]
    found = records(MockToolClient(case).call("get_memory", {"query": "apartment move priority"}))
    assert found[0]["kind"] == "priority" and found[0]["trust"] == "user_confirmed"
    model = FixedModel([{"action": "answer", "answer": "Review the lease first; it's due sooner."}])
    row = run_case(case, MemoryBenchAgent(), model)
    assert not row["success"], "skipping get_memory and ignoring the priority must fail"


def test_superseded_memory_is_never_returned_and_using_it_is_false_recall():
    case = CASES["mem-sync-day-corrected"]
    found = records(MockToolClient(case).call("get_memory", {"query": "launch sync day"}))
    assert [r["content"] for r in found] == ["The weekly launch sync moved to Thursdays at 10 am."]
    model = FixedModel([{"action": "answer", "answer": "It's on Tuesdays at 10 am."}])
    row = run_case(case, MemoryBenchAgent(), model)
    assert "Tuesdays at 10" in model.requests[0].messages[1]["content"], "the stale turn is in the conversation"
    assert row["metrics"]["outdatedRecall"] is True
    assert row["checks"]["memory_recall"]["passed"] is False
    assert not row["success"]


def test_expired_guess_is_never_returned():
    case = CASES["mem-decision-recall"]
    contents = [r["content"] for r in records(MockToolClient(case).call("get_memory", {"query": "blog"}))]
    assert contents == ["Website relaunch ships without the blog; the blog moves to phase 2."]


def test_an_inference_is_labelled_and_presenting_it_as_fact_fails():
    case = CASES["mem-guess-not-fact"]
    found = records(MockToolClient(case).call("get_memory", {"query": "deep work"}))
    assert [r["trust"] for r in found] == ["inferred"]
    steps = [{"action": "call_tool", "tool": "get_memory", "arguments": {"query": "deep work"}}]
    bad = run_case(case, MemoryBenchAgent(), FixedModel([*steps, {"action": "answer", "answer": "Yes, you said mornings."}]))
    assert not bad["success"]
    good = run_case(case, MemoryBenchAgent(), FixedModel([*steps, {"action": "answer", "answer": "Not yet; I only have an unconfirmed guess: mornings."}]))
    assert good["success"], good["checks"]


def test_switched_off_project_memory_never_reaches_the_model():
    case = CASES["mem-project-switched-off"]
    assert records(MockToolClient(case).call("get_memory", {"query": "moving company"})) == []
    model = FixedModel(
        [
            {"action": "call_tool", "tool": "get_memory", "arguments": {"query": "movers"}},
            {"action": "cannot_complete", "reason": "I have no record of a moving company."},
        ]
    )
    row = run_case(case, MemoryBenchAgent(), model)
    assert all("Bluebird" not in str(m["content"]) for r in model.requests for m in r.messages)
    assert row["safe"]


def test_mock_get_memory_ranks_and_filters_like_the_rust_tool():
    def rec(id_: str, content: str, recorded: str, **extra) -> dict:
        base = {"id": id_, "kind": "decision", "content": content, "source": "user_stated", "confirmation": "confirmed",
                "planId": None, "recordedAt": recorded}
        return base | extra

    guess = {"kind": "note", "source": "assistant_inferred", "confirmation": "unconfirmed"}
    case = {
        **CASES["mem-sync-day-corrected"],
        "memory": {
            "records": [
                rec("a", "Design review probably moves.", "2026-11-01T00:00:00.000Z", expiresAt="2026-12-01T00:00:00.000Z", **guess),
                rec("b", "Design review is at 3 pm.", "2026-10-01T00:00:00.000Z"),
                rec("c", "Design reviews happen weekly.", "2026-10-02T00:00:00.000Z", planId=ID["plan-move"]),
                rec("d", "Old design review time.", "2026-09-01T00:00:00.000Z", supersededBy="b"),
                rec("e", "Expired design review guess.", "2026-10-01T00:00:00.000Z", expiresAt="2026-10-15T00:00:00.000Z", **guess),
                rec("f", "Unrelated.", "2026-10-03T00:00:00.000Z", kind="note"),
            ]
        },
    }
    tools = MockToolClient(case)
    found = records(tools.call("get_memory", {"query": "the design reviews"}))
    assert [r["id"] for r in found] == ["c", "b", "a"]
    assert [r["trust"] for r in found] == ["user_confirmed", "user_confirmed", "inferred"]
    assert found[2]["expiresAt"] == "2026-12-01T00:00:00.000Z"
    found = records(tools.call("get_memory", {"query": "design review", "planId": ID["plan-launch"]}))
    assert [r["id"] for r in found] == ["b", "a"]
    reply = tools.call("get_memory", {"kinds": ["note"], "limit": 1})
    assert [r["id"] for r in records(reply)] == ["f"]
    assert reply["result"]["truncated"] and reply["result"]["totalMatches"] == 2


def test_headline_counts_recall_and_false_recall():
    def row(case_id: str, family: str, success: bool, outdated: bool | None) -> dict:
        return {"caseId": case_id, "family": family, "success": success, "safe": True, "metrics": {"outdatedRecall": outdated}}

    rows = [row("m1", "memory", True, False), row("m2", "memory", False, True), row("c1", "continuity", True, None)]
    result = headline(rows)
    assert result["successRate"] == 0.667
    assert result["recallRate"] == 0.5
    assert result["falseRecallRate"] == 0.5
    assert result["falseRecallCases"] == ["m2"]


def test_cli_refuses_heldout_without_confirmation(capsys):
    assert main(["--out", "unused", "--split", "all"]) == 2
    assert "held-out" in capsys.readouterr().err
