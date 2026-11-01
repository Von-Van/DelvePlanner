from __future__ import annotations

import json
from pathlib import Path

from delveops_agent import V0_CATALOG, MockToolClient, ToolError, load_catalog
from delveops_agent.tools import check_arguments

SPECS = {spec.name: spec for spec in V0_CATALOG}


def test_catalog_matches_plan_table_and_is_read_only():
    assert set(SPECS) == {
        "get_day_context",
        "list_active_projects",
        "get_project_context",
        "search_tasks",
        "get_capacity",
        "get_recent_actions",
    }
    for name in SPECS:
        assert name.startswith(("get_", "list_", "search_"))


def test_argument_checks():
    search = SPECS["search_tasks"]
    assert check_arguments(search, {"limit": 10}) is None
    assert check_arguments(search, {}).code == "invalid_arguments"
    assert "at most 50" in check_arguments(search, {"limit": 51}).message
    assert "unknown" in check_arguments(search, {"limit": 1, "sql": "drop"}).message
    assert "must be integer" in check_arguments(search, {"limit": True}).message
    assert check_arguments(search, "limit=5").code == "invalid_arguments"


def test_mock_client_values_errors_callables_and_sequences():
    client = MockToolClient(
        {
            "a": {"x": 1},
            "b": ToolError("timeout", "slow", retryable=True),
            "c": lambda args: {"echo": args},
            "d": [ToolError("unavailable", "warming"), {"ok": True}],
        }
    )
    assert client.call("a", {}).data == {"x": 1}
    assert client.call("b", {}).error.code == "timeout"
    assert client.call("c", {"q": 1}).data == {"echo": {"q": 1}}
    assert client.call("d", {}).error.code == "unavailable"
    assert client.call("d", {}).data == {"ok": True}
    assert client.call("d", {}).data == {"ok": True}
    assert client.call("zzz", {}).error.code == "unavailable"
    assert [name for name, _ in client.calls] == ["a", "b", "c", "d", "d", "d", "zzz"]


def test_load_catalog_falls_back_and_reads_contract_file(tmp_path):
    assert load_catalog(None) == V0_CATALOG
    assert load_catalog(tmp_path / "missing.json") == V0_CATALOG
    path = tmp_path / "tools.v1.json"
    path.write_text(
        json.dumps({"tools": [{"name": "search_tasks", "description": "d", "inputSchema": {"type": "object"}}]})
    )
    (spec,) = load_catalog(path)
    assert spec.name == "search_tasks"


def test_cli_runs_example_against_local_server(ollama, capsys):
    from delveops_agent.cli import main

    port = ollama.rsplit(":", 1)[1]
    example = Path(__file__).resolve().parents[1] / "examples" / "reschedule_overdue.json"
    assert main([str(example), "--port", port]) == 0
    out = json.loads(capsys.readouterr().out)
    assert out["status"] == "answered"
    assert out["model"]["digest"] == "sha256:abc123"


def test_cli_refuses_remote_host(capsys):
    from delveops_agent.cli import main

    example = Path(__file__).resolve().parents[1] / "examples" / "reschedule_overdue.json"
    assert main([str(example), "--host", "198.51.100.4"]) == 2
    assert "loopback" in capsys.readouterr().err


def test_default_catalog_is_the_rust_tools_contract(monkeypatch):
    from delveops_agent.tools import CONTRACT_ENV, REPO_CONTRACT, default_catalog

    if not REPO_CONTRACT.is_file():
        import pytest

        pytest.skip("needs the full repository checkout")
    monkeypatch.delenv(CONTRACT_ENV, raising=False)
    catalog = {spec.name: spec for spec in default_catalog()}
    # get_memory is reserved (not implemented) and left out. The GitHub tools (#17) are in the contract;
    # Rust refuses them until the user selects repositories.
    github = {"list_github_repos", "search_github_items", "get_github_item"}
    assert set(catalog) == set(SPECS) | github
    day = catalog["get_day_context"]
    # $defs references are inlined, so formats are checked before a tool call is spent.
    assert day.input_schema["properties"]["date"]["type"] == "string"
    assert check_arguments(day, {"date": "Thursday", "timeZone": "America/Chicago"}).message.endswith("wrong format")
    assert check_arguments(day, {"date": "2026-11-05", "timeZone": "America/Chicago"}) is None
    search = catalog["search_tasks"]
    assert check_arguments(search, {}) is None  # limit has a default in v1
    assert "must be one of" in check_arguments(search, {"status": "finished"}).message


def test_contract_path_can_be_overridden(monkeypatch, tmp_path):
    from delveops_agent.tools import CONTRACT_ENV, default_catalog

    path = tmp_path / "tools.json"
    path.write_text(json.dumps({"tools": [{"name": "get_capacity", "inputSchema": {"type": "object"}}]}))
    monkeypatch.setenv(CONTRACT_ENV, str(path))
    assert [spec.name for spec in default_catalog()] == ["get_capacity"]
