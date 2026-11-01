"""A3: A1's tool loop with a draft verifier in front of the exit.

The model works exactly as in A1 (one read tool per step, then an answer, drafts, a question, or
`cannot_complete`). What changes is what happens to drafts:

1. The system prompt is A1's (which lists every operation's payload fields), plus a note that drafts are
   checked and one worked example of turning a day-context time into `startAtUtc` (`prompt_notes`).
2. Drafts are normalised where the meaning is certain (see `verify.operations.normalize_payload`).
3. Each payload is checked the way Rust will deserialize it; problems go back to the model, which gets
   up to `max_repairs - 1` chances to fix them.
4. Events are checked against the days the agent read: overlaps, working hours, the past. These are
   warnings the model sees once and may keep.
5. Targets read before the latest tool call are re-read from their canonical source; if one changed,
   the model sees the fresh data and redrafts once.

Every switch is in `VerifierConfig` plus `prompt_notes`, so each piece can be turned off for ablations.
A3 never writes and never calls a tool A1 could not call; re-reads go through the same budgeted,
permission-checked path as the model's own calls.
"""

from __future__ import annotations

import time
from collections.abc import Callable

from .agent import A1Agent
from .model import ModelClient
from .prompts import PROMPT_VERSION as A1_PROMPT_VERSION
from .prompts import system_prompt
from .tools import ToolClient, ToolSpec
from .types import AgentResult, AgentTask, Budget
from .verify.loop import CONVERSION_EXAMPLE, VERIFY_NOTE, VerifyingRun, relabel
from .verify.review import VerifierConfig

PROMPT_VERSION = "a3-v1"  # A1's prompt plus VERIFY_NOTE and CONVERSION_EXAMPLE


def a3_system_prompt(tools: tuple[ToolSpec, ...] | list[ToolSpec], notes: bool) -> str:
    base = system_prompt(tools)
    return f"{base}\n- {VERIFY_NOTE}\n\n{CONVERSION_EXAMPLE}" if notes else base


class A3Agent:
    """A1 loop + draft verifier (+ a short prompt note). `architecture_id` may be overridden for ablation labels."""

    def __init__(
        self,
        verifier: VerifierConfig | None = None,
        use_verifier: bool = True,
        prompt_notes: bool = True,
        max_repairs: int = 3,
        catalog: tuple[ToolSpec, ...] | None = None,
        debug_trace: bool = False,
        clock: Callable[[], float] = time.monotonic,
        architecture_id: str = "A3",
    ) -> None:
        # use_verifier=False keeps A1's own draft handling, so "A3 minus everything" is exactly A1.
        self.verifier = (VerifierConfig() if verifier is None else verifier) if use_verifier else None
        self.prompt_notes = prompt_notes
        self.max_repairs = max_repairs
        self.architecture_id = architecture_id
        # A1Agent holds the catalog, debug flag and clock that A1's run loop reads.
        self._base = A1Agent(catalog=catalog, debug_trace=debug_trace, clock=clock)

    def describe(self) -> dict:
        return {
            "architectureId": self.architecture_id,
            "promptVersion": PROMPT_VERSION if self.prompt_notes else A1_PROMPT_VERSION,
            "promptNotes": self.prompt_notes,
            "verifier": self.verifier.to_dict() if self.verifier else None,
            "maxRepairs": self.max_repairs,
        }

    def run(self, task: AgentTask, tools: ToolClient, model: ModelClient, budget: Budget) -> AgentResult:
        offered = [self._base.catalog[n] for n in task.permitted_tools if n in self._base.catalog]
        run = VerifyingRun(
            self._base,
            task,
            tools,
            model,
            budget,
            system_prompt=a3_system_prompt(offered, self.prompt_notes),
            verifier=self.verifier,
            max_repairs=self.max_repairs,
        )
        details = self.describe()
        return relabel(run.execute(), self.architecture_id, details.pop("promptVersion"), details)
