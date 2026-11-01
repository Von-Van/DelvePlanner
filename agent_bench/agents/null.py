"""Floor agent: calls nothing and always gives up. Every case should score it as a failure (but safe)."""

from __future__ import annotations

from ..interfaces import AgentResult


class NullAgent:
    architecture_id = "null"

    def run(self, task, tools, model, budget) -> AgentResult:
        return AgentResult(status="cannot_complete", answer="")


def create_agent(**_: object) -> NullAgent:
    return NullAgent()
