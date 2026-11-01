"""The verifier wired into an agent loop.

`VerifyingRun` extends A1's run loop (`agent._Run`) without changing A1: it keeps a log of tool calls,
optionally swaps in another system prompt, and, when a verifier
config is given, sends each draft step through the checks in `review.py` and `freshness.py` before the
run may finish. A failed check goes back to the model as a normal repair message; a run whose model
cannot fix its drafts within the repair limit ends in `cannot_complete`, as A1's does.

A3 is `VerifyingRun` with everything on; A2 builds its planner-executor loop on top of it, with the
verifier optional.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ..agent import _Run, _Stop
from ..drafts import OP_SPECS, TYPE_HELP, DraftError, observed_records, observed_references, parse_draft, payload_help
from ..types import AgentResult, Draft, ToolError
from .freshness import plan_rereads, stale_targets
from .review import Observation, VerifierConfig, format_findings, normalize_drafts, shape_findings, time_findings

CONVERSION_EXAMPLE = """Example, moving an event to 3 pm New York time on Nov 4 (20:00 UTC):
{"op": "reschedule_event", "target": {"id": "ev-1", "revision": 2}, "payload": {"startAtUtc": \
"2026-11-04T20:00:00Z", "timeZone": "America/New_York", "reminderChange": {"action": "unchanged"}}, \
"evidence": ["tc-1"]}
Day-context tools show local "start"/"end" times; convert them to startAtUtc (UTC, ending in Z) plus \
durationMinutes."""


VERIFY_NOTE = (
    "Before your drafts reach the person they are checked against the planner's rules and their records are "
    "re-read; if a check fails you will be told what to fix."
)


def draft_reference() -> str:
    """The payload table from A1's prompt (same source: `drafts.OP_SPECS`), for prompts that are not A1's."""
    ops = "\n".join(payload_help(op) for op in OP_SPECS)
    used = sorted({kind for spec in OP_SPECS.values() for kind, _ in spec.values()})
    types = "\n".join(f"- {kind}: {TYPE_HELP[kind]}" for kind in used if kind not in ("str", "int"))
    return (
        'Draft ops and their payload fields. "?" = optional; a left-out *_change field means unchanged.\n'
        f"Use no other fields:\n{ops}\nField types:\n{types}"
    )


@dataclass(frozen=True)
class CallInfo:
    call_id: str
    tool: str
    arguments: dict[str, Any]
    ok: bool
    error: ToolError | None = None


class VerifyingRun(_Run):
    """A1's loop plus a call log, an optional replacement system prompt, and an optional verifier."""

    def __init__(
        self,
        agent: Any,
        task: Any,
        tools: Any,
        model: Any,
        budget: Any,
        *,
        system_prompt: str | None = None,
        verifier: VerifierConfig | None = None,
        max_repairs: int = 2,
    ) -> None:
        super().__init__(agent, task, tools, model, budget)
        if system_prompt is not None:
            self.messages[0] = {"role": "system", "content": system_prompt}
        self.verifier = verifier
        self.max_repairs = max_repairs
        self.calls_log: list[CallInfo] = []
        self.last_error: ToolError | None = None
        self.warning_rounds = 0
        self.fresh_rounds = 0

    # -- instrumentation ------------------------------------------------------------------------

    def call_tool(self, name: Any, arguments: Any) -> None:
        before = self.usage.tool_calls
        self.last_error = None
        try:
            super().call_tool(name, arguments)
        finally:
            if self.usage.tool_calls > before:
                call_id = f"tc-{self.usage.tool_calls}"
                args = dict(arguments) if isinstance(arguments, dict) else {}
                ok = call_id in self.tool_results
                self.calls_log.append(CallInfo(call_id, str(name), args, ok, None if ok else self.last_error))

    def handle_tool_error(self, name: str, arguments: Any, error: ToolError, call_id: str | None) -> None:
        self.last_error = error
        super().handle_tool_error(name, arguments, error, call_id)

    def observations(self) -> list[Observation]:
        return [
            Observation(c.call_id, c.tool, c.arguments, self.tool_results[c.call_id])
            for c in self.calls_log
            if c.ok and c.call_id in self.tool_results
        ]

    def repair(self, reason: str, message: str) -> None:
        # A1's repair with a configurable limit (A1 itself allows two bad replies in a row).
        self.repairs_in_a_row += 1
        self.tracer.emit("repair", message, {"reason": reason, "attempt": self.repairs_in_a_row})
        if self.repairs_in_a_row >= self.max_repairs:
            raise _Stop("cannot_complete", reason)
        self.messages.append({"role": "user", "content": f"{message} Reply with one corrected JSON step."})

    # -- drafts ---------------------------------------------------------------------------------

    def parse_drafts(self, raw: Any) -> list[Draft] | None:
        if self.verifier is None:
            return super().parse_drafts(raw)
        config = self.verifier
        if not isinstance(raw, list) or not raw:
            self.repair("invalid_draft", 'A draft step needs a non-empty "drafts" list.')
            return None

        notes: list[str] = []
        if config.normalize:
            raw, notes = normalize_drafts(raw, self.task.time_zone)
        findings = shape_findings(raw) if config.check_payloads else []
        records = observed_records(self.tool_results.values())
        references = observed_references(self.tool_results.values())
        drafts: list[Draft] = []
        grounding: str | None = None
        try:
            drafts = [parse_draft(item, set(self.tool_results), records, references) for item in raw]
        except DraftError as error:
            grounding = str(error)
        if findings or grounding:
            # A1's parser stops at the first problem; when the full list already names it, skip the repeat.
            messages = [f.message for f in findings]
            if grounding and not (findings and "payload" in grounding):
                messages.append(grounding)
            self.tracer.emit(
                "verify",
                f"drafts rejected ({len(messages)} problem(s))",
                {
                    "passed": False,
                    "normalized": notes,
                    "findings": [f.to_dict() for f in findings],
                    "grounding": grounding,
                },
            )
            self.repair("invalid_draft", "Draft rejected:\n" + "\n".join(f"- {m}" for m in messages) + "\n")
            return None

        if config.check_times and self.warning_rounds == 0:
            warnings = time_findings(drafts, self.observations(), self.task.now, self.task.time_zone)
            # Warnings go back once. A model that keeps its choice after seeing them is allowed to.
            if warnings and self.repairs_in_a_row + 1 < self.max_repairs:
                self.warning_rounds += 1
                self.tracer.emit(
                    "verify",
                    f"{len(warnings)} warning(s) sent back",
                    {"passed": False, "normalized": notes, "findings": [w.to_dict() for w in warnings]},
                )
                self.repairs_in_a_row += 1
                self.messages.append(
                    {
                        "role": "user",
                        "content": "Check before the person sees these drafts:\n"
                        + format_findings(warnings)
                        + "\nIf the time is right anyway, send the same drafts again; otherwise send corrected "
                        "drafts. Reply with one JSON step.",
                    }
                )
                return None

        if config.check_freshness and self.fresh_rounds == 0:
            if self.check_freshness(drafts) is False:
                return None

        self.tracer.emit("verify", "drafts passed", {"passed": True, "normalized": notes, "draftCount": len(drafts)})
        for draft in drafts:
            self.tracer.emit(
                "draft",
                f"draft {draft.op}",
                {"op": draft.op, "target": draft.target, "evidence": list(draft.evidence)},
                debug_data={"payload": draft.payload},
            )
        return drafts

    def check_freshness(self, drafts: list[Draft]) -> bool:
        """Re-read targets. False when something changed and the model has been asked to redo its drafts."""
        last = self.calls_log[-1].call_id if self.calls_log else None
        reads = plan_rereads(drafts, self.observations(), set(self.offered_names), self.task.time_zone, last)
        if not reads:
            return True
        if self.usage.tool_calls + len(reads) > self.budget.max_tool_calls:
            self.tracer.emit("verify", "freshness re-read skipped: not enough tool budget", {"reads": len(reads)})
            return True
        self.fresh_rounds += 1
        self.tracer.emit(
            "verify",
            f"re-reading {len(reads)} source(s) before finishing",
            {"reads": [{"tool": r.tool, "records": list(r.record_ids)} for r in reads]},
        )
        start = len(self.calls_log)
        reread_ids: set[str] = set()
        for read in reads:
            self.call_tool(read.tool, read.arguments)
            if self.calls_log and len(self.calls_log) > start and self.calls_log[-1].ok:
                reread_ids.update(read.record_ids)
        fresh = [obs for obs in self.observations() if obs.call_id in {c.call_id for c in self.calls_log[start:]}]
        stale = stale_targets(drafts, fresh, reread_ids)
        if not stale:
            self.tracer.emit("verify", "targets are current", {"passed": True, "records": sorted(reread_ids)})
            return True
        self.tracer.emit(
            "verify",
            f"{len(stale)} target(s) changed since they were read",
            {
                "passed": False,
                "stale": [
                    {"id": s.record_id, "drafted": s.drafted_revision, "current": s.current_revision} for s in stale
                ],
            },
        )
        self.messages.append(
            {
                "role": "user",
                "content": "Before handing the drafts over I re-read their records:\n"
                + "\n".join(f"- {s.describe()}" for s in stale)
                + "\nThe fresh data is in the tool results above. Reply with drafts against the current revisions, "
                "or another action if the change means the drafts are no longer right. One JSON step.",
            }
        )
        return False


def relabel(result: AgentResult, architecture_id: str, prompt_version: str, details: dict[str, Any]) -> AgentResult:
    """A1's run loop labels its result and opening trace event "A1"; give them the real architecture."""
    result.architecture_id = architecture_id
    for event in result.trace:
        if event.get("kind") == "plan" and event.get("data", {}).get("architectureId") == "A1":
            event["data"]["architectureId"] = architecture_id
            event["data"]["promptVersion"] = prompt_version
            event["data"].update(details)
            break
    return result
