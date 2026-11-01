"""Freshness: re-read the records a draft changes before handing the draft over.

Rust discards a draft whose target revision is no longer current, so a draft built on a stale read is
wasted work at best. Before finishing, the verifier plans one re-read per target from the record's
canonical source, the agent runs those reads through its normal (budgeted, traced) tool path, and
`stale_targets` compares what the draft says with what the fresh read shows.

A target is skipped when the run's most recent tool call already returned it: nothing has happened since,
and re-reading would only spend budget.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

from ..drafts import observed_records
from ..types import Draft
from .review import Observation

# Which kind of record each targeted operation changes.
TARGET_KIND = {
    "update_event": "event",
    "delete_event": "event",
    "reschedule_event": "event",
    "set_event_plan": "event",
    "update_plan": "plan",
    "update_milestone": "milestone",
    "delete_milestone": "milestone",
    "update_task": "task",
    "schedule_task": "task",
    "set_task_plan": "task",
    "delete_task": "task",
    "set_task_workstream": "task",
}


@dataclass(frozen=True)
class ReRead:
    tool: str
    arguments: dict[str, Any]
    record_ids: tuple[str, ...]

    def key(self) -> str:
        return json.dumps([self.tool, self.arguments], sort_keys=True)


def _find(value: Any, record_id: str) -> dict[str, Any] | None:
    if isinstance(value, dict):
        if value.get("id") == record_id:
            return value
        children = list(value.values())
    elif isinstance(value, list):
        children = value
    else:
        return None
    for child in children:
        hit = _find(child, record_id)
        if hit is not None:
            return hit
    return None


def _latest(record_id: str, observations: list[Observation]) -> tuple[int, Observation, dict[str, Any]] | None:
    for index in range(len(observations) - 1, -1, -1):
        record = _find(observations[index].data, record_id)
        if record is not None:
            return index, observations[index], record
    return None


def plan_rereads(
    drafts: list[Draft],
    observations: list[Observation],
    permitted: set[str],
    time_zone: str,
    last_call_id: str | None,
) -> list[ReRead]:
    """One read per stale-able target, from the record's canonical source where the task permits it."""
    planned: dict[str, ReRead] = {}
    for draft in drafts:
        if not draft.target:
            continue
        record_id = draft.target["id"]
        latest = _latest(record_id, observations)
        if latest is None:
            continue  # grounding already refused drafts on unseen targets
        _, source, record = latest
        if source.call_id == last_call_id:
            continue
        kind = TARGET_KIND.get(draft.op)
        tool, arguments = source.tool, dict(source.arguments)
        plan_id = record.get("planId")
        if kind in ("task", "milestone") and isinstance(plan_id, str) and "get_project_context" in permitted:
            tool, arguments = "get_project_context", {"planId": plan_id}
        elif kind == "plan" and "get_project_context" in permitted:
            tool, arguments = "get_project_context", {"planId": record_id}
        elif kind == "event" and "get_day_context" in permitted and isinstance(record.get("start"), str):
            tool, arguments = "get_day_context", {"date": record["start"][:10], "timeZone": time_zone}
        read = ReRead(tool, arguments, (record_id,))
        existing = planned.get(read.key())
        planned[read.key()] = read if existing is None else ReRead(tool, arguments, existing.record_ids + (record_id,))
    return list(planned.values())


@dataclass(frozen=True)
class Staleness:
    record_id: str
    drafted_revision: Any
    current_revision: Any  # None when the fresh read no longer shows the record

    def describe(self) -> str:
        if self.current_revision is None:
            return (
                f"{self.record_id} no longer appears in a fresh read "
                f"(you drafted against revision {self.drafted_revision})"
            )
        return f"{self.record_id} is now revision {self.current_revision} (you drafted against {self.drafted_revision})"


def stale_targets(drafts: list[Draft], fresh: list[Observation], reread_ids: set[str]) -> list[Staleness]:
    """Targets whose fresh read disagrees with the draft. Records that were not re-read are not judged."""
    current = observed_records(obs.data for obs in fresh)
    out = []
    for draft in drafts:
        if not draft.target or draft.target["id"] not in reread_ids:
            continue
        record_id = draft.target["id"]
        if record_id not in current:
            out.append(Staleness(record_id, draft.target.get("revision"), None))
        elif current[record_id] != draft.target.get("revision"):
            out.append(Staleness(record_id, draft.target.get("revision"), current[record_id]))
    return out
