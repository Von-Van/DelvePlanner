"""The GitHub mock tools (piece M) must match the Rust broker and keep its safety rules."""

import json

from agent_bench import github_tools
from agent_bench.mock_tools import MockToolClient

FIXTURES = github_tools.FIXTURES_DIR


def _data():
    return github_tools.state_entries(github_tools.load_github_fixture())


def test_cleaner_matches_shared_vectors():
    doc = json.loads((FIXTURES / "github-sanitize.v1.json").read_text())
    assert len(doc["vectors"]) >= 20
    for vector in doc["vectors"]:
        out = github_tools.sanitize(vector["input"], vector["maxChars"])
        assert out == {"text": vector["text"], "truncated": vector["truncated"], "signals": vector["signals"]}, vector["input"]


def test_tools_match_golden_results_shared_with_rust():
    golden = json.loads((FIXTURES / "github-golden.v1.json").read_text())
    data = _data()
    for call in golden["calls"]:
        assert github_tools.TOOL_IMPLS[call["tool"]](data, call["arguments"]) == call["result"], call


def test_unselected_repositories_are_refused_and_never_listed(case_by_id):
    client = MockToolClient(case_by_id("untrusted-github-unselected-repo"))
    everything = client.call("search_github_items", {"limit": 20})
    assert everything["ok"]
    assert all(item["repo"] != "acme/payroll" for item in everything["data"]["items"])
    refused = client.call("get_github_item", {"repo": "acme/payroll", "number": 3})
    assert refused == {"ok": False, "error": {"code": "permission_denied", "message": "acme/payroll is not one of the repositories the user selected.", "retryable": False}}
    assert all(c.known and c.permitted for c in client.calls)


def test_github_text_is_marked_untrusted_and_hidden_text_removed(case_by_id):
    client = MockToolClient(case_by_id("untrusted-github-pr-hidden-comment"))
    item = client.call("get_github_item", {"repo": "acme/website", "number": 42})["data"]["item"]
    assert item["trust"] == "untrusted"
    assert "hidden_comment" in item["injectionSignals"]
    assert "Go live" not in item["body"]
    assert "ref=pr42" not in item["body"]


def test_github_tools_are_not_permitted_unless_the_case_allows_them(case_by_id):
    client = MockToolClient(case_by_id("untrusted-event-notes"))
    response = client.call("list_github_repos", {})
    assert response["error"]["code"] == "permission_denied"
    assert client.calls[0].known and not client.calls[0].permitted


def test_turning_github_off_refuses_every_tool():
    data = _data()
    data["githubSelectedRepos"] = []
    for name in github_tools.TOOL_IMPLS:
        try:
            github_tools.TOOL_IMPLS[name](data, {"repo": "acme/website", "number": 41})
        except github_tools.GitHubToolError as err:
            assert err.code == "permission_denied"
        else:  # pragma: no cover - a failure path
            raise AssertionError(f"{name} answered with GitHub off")
