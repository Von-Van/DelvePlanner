"""The assistant check: skills, scorecard, comparison with the last check, and where results go."""

import json
from datetime import datetime
from pathlib import Path

from agent_bench.__main__ import main
from agent_bench.agents.null import NullAgent
from agent_bench.agents.reference import ReferenceAgent
from agent_bench.cases import load_cases
from agent_bench.check import (
    APP_RESULTS_FOLDER,
    app_data_folder,
    build_check,
    check_cases,
    compare,
    default_out,
    find_previous,
    render,
)
from agent_bench.runner import run_suite
from agent_bench.skills import SKILLS, skill_key, skill_label

REFERENCE = "agent_bench.agents.reference:create_agent"
NULL = "agent_bench.agents.null:create_agent"


def _check(tmp_path: Path, name: str, agent, cases=None) -> dict:
    folder = tmp_path / name
    summary = run_suite(cases or load_cases("dev"), agent, out_dir=folder, agent_spec=name)
    return build_check(summary, folder, name)


def test_every_case_checks_a_named_skill(all_cases):
    # A new family needs a plain name before the app can show it; this catches one added without.
    for case in all_cases:
        assert skill_key(case["case_id"], case["family"]) is not None, case["case_id"]
    keys = {skill_key(c["case_id"], c["family"]) for c in all_cases}
    assert {"two_hours", "follow_ups", "memory", "feedback", "recovering", "changed_records"} <= keys


def test_two_hours_cases_and_faulted_copies_have_their_own_skill():
    assert skill_label("wf-two-hours-tomorrow", "multi_step") == "Planning limited time"
    assert skill_label("h-wf-monday-after-clock-change", "multi_step") == "Planning limited time"
    assert skill_label("ms-least-busy-day", "multi_step") == "Combining several lookups"
    assert skill_label("cont-push-to-friday--fault-outage", "continuity") == "Coping when a lookup fails"
    assert len({s.label for s in SKILLS}) == len(SKILLS)


def test_result_rows_carry_skill_and_tags(tmp_path, all_cases):
    run_suite(load_cases("dev")[:3], ReferenceAgent(all_cases), out_dir=tmp_path)
    for line in (tmp_path / "results.jsonl").read_text().splitlines():
        row = json.loads(line)
        assert row["skill"] == skill_label(row["caseId"], row["family"])
        assert isinstance(row["tags"], list)


def test_scorecard_groups_by_skill_in_a_fixed_order(tmp_path, all_cases):
    check = _check(tmp_path, "check-a", ReferenceAgent(all_cases))
    labels = [line["label"] for line in check["skills"]]
    order = [s.label for s in SKILLS]
    assert labels == [label for label in order if label in labels]
    assert all(line["passed"] == line["runs"] and not line["failing"] for line in check["skills"])
    assert check["overall"] == {"runs": len(load_cases("dev")), "passed": len(load_cases("dev")), "unsafe": 0}
    follow_ups = next(line for line in check["skills"] if line["skill"] == "follow_ups")
    assert follow_ups["milestone"] == "M1" and follow_ups["cases"] == 3


def test_comparison_names_cases_that_got_better_or_worse(tmp_path, all_cases):
    good = _check(tmp_path, "check-a", ReferenceAgent(all_cases))
    bad = _check(tmp_path, "check-b", NullAgent())
    worse = compare(bad, good)
    assert worse["worse"] == sorted(c["case_id"] for c in load_cases("dev"))
    assert worse["better"] == [] and worse["stillFailing"] == []
    assert "agent" in worse["settingsChanged"]
    better = compare(good, bad)
    assert better["better"] == worse["worse"] and better["worse"] == []
    same = compare(good, good)
    assert same["better"] == same["worse"] == same["settingsChanged"] == []


def test_comparison_lists_cases_only_one_check_has(tmp_path, all_cases):
    cases = load_cases("dev")
    first = _check(tmp_path, "check-a", ReferenceAgent(all_cases), cases[:5])
    second = _check(tmp_path, "check-b", ReferenceAgent(all_cases), cases[3:8])
    result = compare(second, first)
    assert result["newCases"] == sorted(c["case_id"] for c in cases[5:8])
    assert result["droppedCases"] == sorted(c["case_id"] for c in cases[:3])
    assert "caseSetHash" in result["settingsChanged"]


def test_previous_check_is_the_latest_earlier_one_next_to_it(tmp_path):
    for name, started in (("check-1", "2026-11-01T10:00:00Z"), ("check-2", "2026-11-02T10:00:00Z")):
        (tmp_path / name).mkdir()
        (tmp_path / name / "check.json").write_text(
            json.dumps({"schema": "agent_bench.check.v1", "startedAt": started, "folder": name})
        )
    (tmp_path / "not-a-check").mkdir()
    (tmp_path / "broken").mkdir()
    (tmp_path / "broken" / "check.json").write_text("{")
    assert find_previous(tmp_path / "check-3")["folder"] == "check-2"
    assert find_previous(tmp_path / "missing" / "check-1") is None


def test_report_reads_plainly(tmp_path, all_cases):
    good = _check(tmp_path, "check-a", ReferenceAgent(all_cases))
    bad = _check(tmp_path, "check-b", NullAgent())
    bad["comparison"] = compare(bad, good)
    text = render(bad)
    assert "Follow-ups in a conversation (M1): 0/3" in text
    assert "Worse now: amb-move-the-review" in text
    assert "Changed in between: the assistant's prompt or setup" in text
    assert "Nothing to compare with yet" in render(good)


def test_faulted_copies_count_honest_stops_as_handled(tmp_path, all_cases):
    cases = [c for c in check_cases("dev", with_faults=True) if "--fault-outage" in c["case_id"]][:4]
    check = _check(tmp_path, "check-f", NullAgent(), cases)
    faults = check["skills"][0]
    assert faults["skill"] == "faults" and faults["passed"] == 0
    # The null agent gives up at once: never a success, but an honest stop under an outage.
    assert faults["handled"] == faults["runs"] and faults["failing"] == []


def test_app_data_folder_matches_tauri_on_each_platform(tmp_path):
    env = {"HOME": "/Users/jake"}
    assert app_data_folder("id.app", "darwin", env) == Path("/Users/jake/Library/Application Support/id.app")
    assert app_data_folder("id.app", "win32", {"APPDATA": "C:/Users/jake/AppData/Roaming"}) == Path(
        "C:/Users/jake/AppData/Roaming/id.app"
    )
    assert app_data_folder("id.app", "linux", env) == Path("/Users/jake/.local/share/id.app")
    assert app_data_folder("id.app", "linux", {**env, "XDG_DATA_HOME": "/data"}) == Path("/data/id.app")
    assert app_data_folder(platform="darwin", env=env).name == "com.vonvan.dayplan.desktop"


def test_default_out_uses_the_app_results_folder_when_the_app_has_run(tmp_path):
    when = datetime(2026, 11, 3, 9, 30, 0)
    out, seen = default_out(when, tmp_path)
    assert seen and out == tmp_path / APP_RESULTS_FOLDER / "check-20261103-093000"
    out, seen = default_out(when, tmp_path / "never-installed")
    assert not seen and out.parent.name == "results" and out.name == "check-20261103-093000"


def test_cli_saves_a_check_and_compares_the_next_one(tmp_path, capsys):
    first, second = tmp_path / "check-1", tmp_path / "check-2"
    assert main(["check", "--agent", REFERENCE, "--out", str(first), "--quiet"]) == 0
    saved = json.loads((first / "check.json").read_text())
    assert saved["comparison"] is None and (first / "check.md").is_file() and (first / "summary.json").is_file()
    # A worse run is reported, and --fail-on-worse turns it into a failing exit.
    assert main(["check", "--agent", NULL, "--out", str(second), "--quiet", "--fail-on-worse"]) == 1
    comparison = json.loads((second / "check.json").read_text())["comparison"]
    assert comparison["previous"] == "check-1" and comparison["worse"]
    assert "Worse now:" in capsys.readouterr().out


def test_cli_refuses_heldout_without_the_flag(tmp_path):
    assert main(["check", "--agent", REFERENCE, "--out", str(tmp_path / "c"), "--split", "all"]) == 2
