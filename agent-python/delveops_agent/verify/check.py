"""The self-check: what the agent does with a draft step before it may finish (milestone M4).

`_Run.parse_drafts` hands every draft step here when the agent's self-check is on (the default). In
order:

1. **Fix what is certain.** Meaning-preserving rewrites (`operations.normalize_payload`), such as a
   local start/end pair turned into `startAtUtc` + `durationMinutes`. No model call needed.
2. **Shape and grounding.** Every payload problem at once, plus A1's grounding rules (target and
   revision seen in this run, evidence from real calls). A failure goes back to the model as a normal
   repair: one attempt, then the run stops and says what was wrong.
3. **Freshness.** Re-read each target from its source. A record that changed goes back to the model
   once with the fresh data. A record that disappeared can't be changed: a later draft against it is a
   shape problem, so the run stops rather than hand over a change Rust would refuse.
4. **Facts.** Time clashes, working hours, the past, open dependencies, capacity (`facts.py`), checked
   against everything read so far, re-reads included. These go back to the model once. If the next
   drafts still trip them, the drafts are handed over with the problems spelled out for the person: a
   clash can be deliberate, and the person decides.

Each round is traced as a `verify` event whose `outcome` is `passed`, `fixed`, `sent_back`, `noted` or
`stopped`, so the app can show "checked → fixed" or "checked → stopped". Problem text (record titles)
only goes into debug data, like every other piece of planner text in the trace.
"""

from __future__ import annotations

from typing import Any

from ..drafts import DraftError, check_time_blocks, observed_records, observed_references, parse_draft
from ..types import Draft
from .facts import Finding, Observation, fact_findings, normalize_drafts, shape_problems
from .freshness import Staleness, plan_rereads, stale_targets


def _bullets(lines: list[str]) -> str:
    return "\n".join(f"- {line}" for line in lines)


class SelfCheck:
    """Per-run state: which one-time rounds are used, and what the person should be told."""

    def __init__(self, run: Any) -> None:
        self.run = run
        self.facts_round_used = False
        self.reread_done = False
        self.sent_back = False
        self.changed: list[Staleness] = []
        self.missing: set[str] = set()  # targets a fresh read no longer shows

    # -- helpers --------------------------------------------------------------------------------

    def observations(self) -> list[Observation]:
        return [
            Observation(call_id, name, arguments, self.run.tool_results[call_id])
            for call_id, (name, arguments) in self.run.call_args.items()
            if call_id in self.run.tool_results
        ]

    def model_calls_left(self) -> int:
        return self.run.budget.max_model_calls - self.run.usage.model_calls

    def trace(
        self,
        outcome: str,
        check: str,
        problems: list[str] | None = None,
        fixed: list[str] | None = None,
        **data: Any,
    ) -> None:
        count = len(problems or [])
        summary = f"self-check {check}: {outcome}" + (f" ({count} problem{'s' if count != 1 else ''})" if count else "")
        self.run.tracer.emit(
            "verify",
            summary,
            {"outcome": outcome, "check": check, "problemCount": count, "fixedCount": len(fixed or []), **data},
            debug_data={"problems": problems or [], "fixed": fixed or []},
        )

    # -- the draft step -------------------------------------------------------------------------

    def drafts(self, raw: Any) -> list[Draft] | None:
        run = self.run
        if not isinstance(raw, list) or not raw:
            run.repair("invalid_draft", 'A draft step needs a non-empty "drafts" list.')
            return None

        raw, fixed = normalize_drafts(raw, run.task.time_zone)
        problems = shape_problems(raw)
        records = observed_records(run.tool_results.values())
        references = observed_references(run.tool_results.values())
        drafts: list[Draft] = []
        try:
            drafts = [parse_draft(item, set(run.tool_results), records, references) for item in raw]
            check_time_blocks(drafts, run.tool_results.values())  # M3: blocks must sit in free time
        except DraftError as error:
            # The parser stops at its first problem. When the full list already names it, skip the repeat.
            if not (problems and "payload" in str(error)):
                problems.append(str(error))
        for index, draft in enumerate(drafts):
            if draft.target and draft.target["id"] in self.missing:
                problems.append(f"draft {index}: {draft.target['id']} no longer exists, so it can't be changed")
        if problems:
            stopping = run.repairs_in_a_row + 1 >= run.repair_limit
            self.trace("stopped" if stopping else "sent_back", "shape", problems, fixed)
            self.sent_back = True
            run.repair(
                "invalid_draft",
                "Draft rejected:\n" + _bullets(problems) + "\n",
                explain="I couldn't prepare a change the planner would accept:\n" + _bullets(problems),
            )
            return None

        if not self.reread_done and self.recheck_freshness(drafts) is False:
            return None

        findings = fact_findings(drafts, self.observations(), run.task.now, run.task.time_zone)
        if findings and not self.facts_round_used and self.model_calls_left() > 0:
            self.facts_round_used = True
            self.sent_back = True
            self.trace("sent_back", "facts", self._for_model(findings), fixed, codes=sorted({f.code for f in findings}))
            run.messages.append(
                {
                    "role": "user",
                    "content": "Before the person sees these drafts, check them against what you read:\n"
                    + _bullets(self._for_model(findings))
                    + "\nIf they are right anyway (for example the person asked for exactly this), send the same "
                    "drafts again. Otherwise send corrected drafts or another action. Reply with one JSON step.",
                }
            )
            return None

        notes = [f.message for f in findings]
        targets = {d.target["id"] for d in drafts if d.target}
        notes += [s.for_person() for s in self.changed if s.record_id in targets]
        run.notes.extend(notes)
        if findings:
            outcome = "noted"
        elif self.sent_back or fixed:
            outcome = "fixed"
        else:
            outcome = "passed"
        self.trace(outcome, "all", [f.message for f in findings], fixed, draftCount=len(drafts))
        return drafts

    @staticmethod
    def _for_model(findings: list[Finding]) -> list[str]:
        return [f"draft {f.draft}: {f.message}" for f in findings]

    # -- freshness ------------------------------------------------------------------------------

    def recheck_freshness(self, drafts: list[Draft]) -> bool:
        """Re-read the drafts' targets. False when something changed and the model was asked to redraft."""
        run = self.run
        observations = self.observations()
        reads = plan_rereads(drafts, observations, set(run.offered_names), run.task.time_zone)
        self.reread_done = True
        if not reads:
            return True
        if run.usage.tool_calls + len(reads) > run.budget.max_tool_calls:
            self.trace("passed", "freshness", skipped="tool budget")
            return True
        before = set(run.tool_results)
        repairs = run.repairs_in_a_row
        reread_ids: set[str] = set()
        for read in reads:
            known = set(run.tool_results)
            run.call_tool(read.tool, read.arguments)
            if set(run.tool_results) - known:
                reread_ids.update(read.record_ids)
        run.repairs_in_a_row = repairs  # the re-reads are the check's, not a fresh start for the model
        fresh = [obs for obs in self.observations() if obs.call_id not in before]
        stale = stale_targets(drafts, fresh, reread_ids, observations)
        if not stale:
            self.trace("passed", "freshness", reads=len(reads))
            return True
        self.changed = stale
        self.missing = {s.record_id for s in stale if s.current_revision is None}
        self.sent_back = True
        problems = [s.for_model() for s in stale]
        if self.model_calls_left() <= 0:
            from ..agent import _Stop  # local import: agent.py imports this module

            self.trace("stopped", "freshness", problems, reads=len(reads))
            raise _Stop(
                "cannot_complete",
                "invalid_draft",
                "Something changed while I was working, and I ran out of steps to redo the change:\n"
                + _bullets([s.for_person() for s in stale]),
            )
        self.trace("sent_back", "freshness", problems, reads=len(reads))
        run.messages.append(
            {
                "role": "user",
                "content": "Before handing the drafts over I re-read their records:\n"
                + _bullets(problems)
                + "\nThe fresh data is in the tool results above. Reply with drafts against the current "
                "revisions, or another action if the change means the drafts are no longer right. One JSON step.",
            }
        )
        return False
