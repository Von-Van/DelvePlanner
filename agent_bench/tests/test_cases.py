"""The case set itself: valid, well split, and solvable."""

import json
from collections import Counter

import pytest

from agent_bench.agents.null import NullAgent
from agent_bench.agents.reference import ReferenceAgent
from agent_bench.cases import case_files, case_problems
from agent_bench.runner import run_case

ALL_FILES = case_files("all")


@pytest.mark.parametrize("path", ALL_FILES, ids=lambda p: p.stem)
def test_case_is_valid(path):
    assert case_problems(json.loads(path.read_text()), path) == []


def test_case_ids_are_unique(all_cases):
    counts = Counter(c["case_id"] for c in all_cases)
    assert [cid for cid, n in counts.items() if n > 1] == []


def test_reference_lookup_key_is_unique(all_cases):
    keys = Counter((c["goal"], c["now"], c["timeZone"]) for c in all_cases)
    assert max(keys.values()) == 1


def test_seed_set_covers_required_families(all_cases):
    families = {c["family"] for c in all_cases}
    assert {"tool_choice", "multi_step", "ambiguity", "tool_error", "stale_revision", "untrusted_input"} <= families
    assert len(all_cases) >= 20


def test_heldout_has_every_dev_family_and_is_a_minority(all_cases):
    dev = {c["family"] for c in all_cases if c["split"] == "dev"}
    held = {c["family"] for c in all_cases if c["split"] == "heldout"}
    assert dev == held
    n_held = sum(c["split"] == "heldout" for c in all_cases)
    assert 0 < n_held < len(all_cases) / 2


def test_every_case_has_a_reference(all_cases):
    assert all("reference" in c for c in all_cases)


@pytest.mark.parametrize("path", ALL_FILES, ids=lambda p: p.stem)
def test_reference_run_passes_and_is_safe(path, all_cases):
    case = json.loads(path.read_text())
    row = run_case(case, ReferenceAgent(all_cases))
    failed = {k: v["detail"] for k, v in row["checks"].items() if not v["passed"]}
    assert row["success"], failed
    assert row["safe"], row["safety"]["violations"]
    assert row["agentTraceProblems"] == []


@pytest.mark.parametrize("path", ALL_FILES, ids=lambda p: p.stem)
def test_null_agent_fails_but_stays_safe(path):
    row = run_case(json.loads(path.read_text()), NullAgent())
    assert not row["success"], "a do-nothing agent should never pass a case"
    assert row["safe"]
