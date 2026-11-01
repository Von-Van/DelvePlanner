"""The runner writes reproducible outputs and guards the held-out split."""

import json

from agent_bench.__main__ import main
from agent_bench.agents.reference import ReferenceAgent
from agent_bench.cases import load_cases
from agent_bench.jsonschema_lite import validate
from agent_bench.runner import run_suite


def test_suite_writes_results_traces_and_summary(tmp_path, all_cases):
    cases = load_cases("dev")
    summary = run_suite(cases, ReferenceAgent(all_cases), repeats=2, out_dir=tmp_path, agent_spec="reference")
    assert summary["summary"]["overall"]["runs"] == 2 * len(cases)
    assert summary["summary"]["overall"]["successRate"] == 1.0
    assert summary["summary"]["overall"]["unsafeRuns"] == 0
    rows = [json.loads(line) for line in (tmp_path / "results.jsonl").read_text().splitlines()]
    assert len(rows) == 2 * len(cases)
    assert len(list((tmp_path / "traces").glob("*.json"))) == 2 * len(cases)
    meta = json.loads((tmp_path / "summary.json").read_text())["meta"]
    for key in ("benchVersion", "caseSetHash", "toolDefinitionsHash", "python", "platform", "repeats"):
        assert meta[key] is not None
    assert "Safety" in (tmp_path / "report.md").read_text()


def test_routine_output_has_no_answer_text(tmp_path, all_cases):
    run_suite(load_cases("dev")[:1], ReferenceAgent(all_cases), out_dir=tmp_path)
    row = json.loads((tmp_path / "results.jsonl").read_text().splitlines()[0])
    assert row["answer"] is None
    assert all("data" not in c for c in row["calls"])


def test_cli_refuses_heldout_without_flag(tmp_path, capsys):
    assert main(["run", "--split", "heldout", "--out", str(tmp_path)]) == 2
    assert "Held-out" in capsys.readouterr().err


def test_cli_validate_and_run(tmp_path):
    assert main(["validate"]) == 0
    assert main(["run", "--split", "dev", "--out", str(tmp_path / "ref")]) == 0
    assert main(["run", "--agent", "agent_bench.agents.null:create_agent", "--out", str(tmp_path / "null"), "--require-success", "0.5"]) == 1


def test_jsonschema_lite_basics():
    schema = {"type": "object", "required": ["a"], "additionalProperties": False, "properties": {"a": {"type": "integer", "minimum": 1}}}
    assert validate({"a": 2}, schema) == []
    assert validate({"a": 0}, schema)
    assert validate({"a": True}, schema), "booleans are not integers"
    assert validate({"a": 1, "b": 2}, schema)
    assert validate({}, schema)
