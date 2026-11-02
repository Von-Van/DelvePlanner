from __future__ import annotations

import pytest

from agent_bench.agents.null import NullAgent
from agent_bench.agents.reference import ReferenceAgent
from agent_bench.cases import case_problems, load_cases
from agent_bench.faults import PROFILES, applicable, fault_cases, graceful, inject, original_id, profile_of, summarize_faults
from agent_bench.runner import run_case

CASES = load_cases("all")
FAULTED = [(p, c) for p in PROFILES for c in fault_cases(CASES, p)]


def test_every_profile_applies_somewhere():
    for profile in PROFILES:
        assert fault_cases(CASES, profile), profile


@pytest.mark.parametrize("profile,case", FAULTED, ids=[c["case_id"] for _, c in FAULTED])
def test_faulted_cases_are_valid_and_solvable(profile, case):
    assert case_problems(case) == []
    assert profile_of(case["case_id"]) == profile and original_id(case["case_id"]) in {c["case_id"] for c in CASES}
    row = run_case(case, ReferenceAgent([case]))
    assert row["safe"], row["safety"]
    if PROFILES[profile].recoverable:
        assert row["success"], {k: v for k, v in row["checks"].items() if not v["passed"]}
    else:
        assert graceful(row) and row["status"] == "cannot_complete"
    assert any(c["injected"] for c in row["calls"]) or profile == "stale"


@pytest.mark.parametrize("profile,case", FAULTED, ids=[c["case_id"] for _, c in FAULTED])
def test_null_agent_is_never_successful_but_stops_honestly(profile, case):
    row = run_case(case, NullAgent())
    assert not row["success"] and row["safe"] and graceful(row)


def test_ignoring_a_transient_fault_fails():
    case = next(c for c in CASES if c["case_id"] == "tc-today-agenda")
    faulted = inject(case, "transient")
    row = run_case(faulted, ReferenceAgent([case]))  # the original replay never retries
    assert not row["success"]


def test_stale_profile_bumps_the_expected_revision():
    case = next(c for c in CASES if applicable(c, "stale"))
    faulted = inject(case, "stale")
    old = case["expected"]["drafts"][0]["target"]["revision"]
    assert faulted["expected"]["drafts"][0]["target"]["revision"] == old + 1
    stale_row = run_case(faulted, ReferenceAgent([case]))  # drafting against the first read
    assert not stale_row["success"] and not stale_row["checks"]["fresh_revisions"]["passed"]


def test_inject_refuses_where_it_does_not_apply():
    case = next(c for c in CASES if c["case_id"] == "err-timeout-then-retry")
    assert not applicable(case, "transient")
    with pytest.raises(ValueError):
        inject(case, "transient")


def test_summary_counts_graceful_runs():
    rows = [
        {"caseId": "a--fault-outage", "success": False, "safe": True, "status": "cannot_complete", "drafts": []},
        {"caseId": "b--fault-outage", "success": False, "safe": True, "status": "answered", "drafts": []},
        {"caseId": "c--fault-transient", "success": True, "safe": True, "status": "answered", "drafts": []},
        {"caseId": "d--fault-transient", "success": True, "safe": False, "status": "drafted", "drafts": [{}]},
    ]
    summary = summarize_faults(rows)
    assert summary["byProfile"]["outage"] == {"runs": 2, "success": 0.0, "graceful": 0.5, "honestStops": 1, "unsafeRuns": 0}
    assert summary["byProfile"]["transient"]["graceful"] == 0.5
