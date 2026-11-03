"""The assistant check runs the assistant the way the app does, and scores it with the benchmark.

`agent_bench check` drives `AppAssistantBenchAgent`, which sets the agent up as `link_runner.run` does
in the app (self-check on, the conversation put in front of the request). Here the model is the naive
stand-in from the M4 tests, so a failure points at the wiring, not at a model's choices.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

import pytest

REPO = Path(__file__).resolve().parents[2]
if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))

from agent_bench.cases import load_cases  # noqa: E402
from agent_bench.check import AppAssistantBenchAgent  # noqa: E402
from agent_bench.runner import run_case  # noqa: E402
from test_bench_recovery import NaiveModel  # noqa: E402

CASES = load_cases("dev")


class RecordingModel(NaiveModel):
    """The naive model, keeping every request it was sent."""

    def __init__(self, case: dict) -> None:
        super().__init__(case)
        self.requests: list[Any] = []

    def complete(self, request: Any):  # noqa: ANN201
        self.requests.append(request)
        return super().complete(request)


@pytest.mark.parametrize("case", CASES, ids=[c["case_id"] for c in CASES])
def test_the_app_assistant_solves_every_dev_case_with_the_naive_model(case):
    row = run_case(case, AppAssistantBenchAgent(), NaiveModel(case))
    failed = {k: v for k, v in row["checks"].items() if not v["passed"]}
    assert row["safe"], row["safety"]
    assert row["success"], failed


def test_the_conversation_reaches_the_model_as_the_app_sends_it():
    case = next(c for c in CASES if c["case_id"] == "cont-thirty-minutes-later")
    assert case["conversation"], "the case should carry earlier turns"
    model = RecordingModel(case)
    run_case(case, AppAssistantBenchAgent(), model)
    first = "\n".join(m["content"] for m in model.requests[0].messages)
    assert case["conversation"][0]["text"] in first
    assert case["goal"] in first


def test_the_check_names_the_prompt_it_ran():
    agent = AppAssistantBenchAgent()
    from delveagent.memory import MEMORY_PROMPT_VERSION
    from delveagent.prompts import PROMPT_VERSION

    assert agent.prompt_version == f"{PROMPT_VERSION}+{MEMORY_PROMPT_VERSION}"
    assert agent.architecture_id == "assistant"
