from __future__ import annotations

import socket
from pathlib import Path

from agent_bench.agents.null import NullAgent
from agent_bench.agents.reference import ReferenceAgent
from agent_bench.faults import PROFILES
from agent_bench.faults.__main__ import main, report, suite
from agent_bench.runner import run_suite


def test_suite_is_every_dev_case_plus_its_faulted_copies():
    cases = suite()
    plain = [c for c in cases if "--fault-" not in c["case_id"]]
    assert plain and all(c["split"] == "dev" for c in plain)
    for profile in PROFILES:
        assert any(c["case_id"].endswith(f"--fault-{profile}") for c in cases)


def test_report_reads_run_folders(tmp_path: Path):
    cases = suite()
    run_suite(cases, ReferenceAgent(cases), out_dir=tmp_path / "ref", agent_spec="reference")
    run_suite(cases, NullAgent(), out_dir=tmp_path / "null", agent_spec="null")
    table = report([tmp_path / "ref", tmp_path / "null"])
    assert "| reference | transient |" in table and "| null | stale |" in table
    assert "| null | not graceful |" not in table  # doing nothing is an honest stop, never a success


def test_run_refuses_without_a_model(tmp_path: Path, capsys):
    with socket.socket() as probe:  # a loopback port nothing listens on
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    assert main(["run", "--port", str(port), "--out", str(tmp_path / "x")]) == 2
    assert "Cannot reach" in capsys.readouterr().err
