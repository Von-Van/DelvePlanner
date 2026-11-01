"""Oracle agent: replays each case's ``reference`` run.

It proves every case is solvable through the mock tools and that the scorers accept a correct run. It finds
its case by (goal, now, timeZone), so it sees nothing an ordinary agent would not, apart from the answer key.
"""

from __future__ import annotations

import copy

from ..cases import load_cases
from ..interfaces import AgentResult, AgentTask, Budget, Usage


class ReferenceAgent:
    architecture_id = "reference"

    def __init__(self, cases: list[dict] | None = None):
        cases = cases if cases is not None else load_cases("all")
        self._by_key = {(c["goal"], c["now"], c["timeZone"]): c for c in cases if "reference" in c}

    def run(self, task: AgentTask, tools, model, budget: Budget) -> AgentResult:
        case = self._by_key[(task.goal, task.now, task.timeZone)]
        ref = case["reference"]
        for call in ref["calls"]:
            tools.call(call["tool"], copy.deepcopy(call["args"]))
        out = ref["result"]
        return AgentResult(status=out["status"], answer=out["answer"], drafts=copy.deepcopy(out["drafts"]), usage=Usage(toolCalls=len(ref["calls"])))


def create_agent(**_: object) -> ReferenceAgent:
    return ReferenceAgent()
