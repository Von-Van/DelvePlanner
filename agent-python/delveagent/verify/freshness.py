"""Freshness: re-read the records a draft changes before handing the draft over.

Rust discards a draft whose target revision is no longer current, so a draft built on a stale read is
wasted work at best. Before finishing, the self-check plans one re-read per target from the record's
canonical source, the agent runs those reads through its normal (budgeted, traced) tool path, and
`stale_targets` compares what the draft says with what the fresh read shows.

The re-read happens even when the last call returned the target: a local model takes seconds to write
its drafts, and an edit in that window is exactly what this check is for. Reads are local and cheap; if
the tool budget cannot cover them, the check is skipped (and Rust still refuses a stale change).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime
from typing import Any
from zoneinfo import ZoneInfo

from ..drafts import observed_records
from ..types import Draft
from .facts import Observation, latest_record

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


def _latest(record_id: str, observations: list[Observation]) -> tuple[Observation, dict[str, Any]] | None:
    """The latest full copy of a record (one with a revision, as grounding used), else the latest copy."""
    fallback = None
    for obs in reversed(observations):
        record = latest_record(record_id, [obs])
        if record is not None and "revision" in record:
            return obs, record
        if record is not None and fallback is None:
            fallback = obs, record
    return fallback


def _local_day(instant: Any, time_zone: str) -> str | None:
    """The day-context date that shows an event, in the task's own time zone (as the day tool reads)."""
    if not isinstance(instant, str):
        return None
    try:
        moment = datetime.fromisoformat(instant.replace("Z", "+00:00"))
        zone = ZoneInfo(time_zone)
    except Exception:  # noqa: BLE001 - an unreadable time just means no canonical re-read
        return None
    return moment.astimezone(zone).date().isoformat()


def plan_rereads(
    drafts: list[Draft],
    observations: list[Observation],
    permitted: set[str],
    time_zone: str,
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
        source, record = latest
        kind = TARGET_KIND.get(draft.op)
        tool, arguments = source.tool, dict(source.arguments)
        plan_id = record.get("planId")
        if kind in ("task", "milestone") and isinstance(plan_id, str) and "get_project_context" in permitted:
            tool, arguments = "get_project_context", {"planId": plan_id}
        elif kind == "plan" and "get_project_context" in permitted:
            tool, arguments = "get_project_context", {"planId": record_id}
        elif kind == "event" and "get_day_context" in permitted:
            day = _local_day(record.get("startAtUtc"), time_zone)
            if day is not None:
                tool, arguments = "get_day_context", {"date": day, "timeZone": time_zone}
        read = ReRead(tool, arguments, (record_id,))
        existing = planned.get(read.key())
        planned[read.key()] = read if existing is None else ReRead(tool, arguments, existing.record_ids + (record_id,))
    return list(planned.values())


@dataclass(frozen=True)
class Staleness:
    record_id: str
    name: str  # the record's title in quotes, or its id
    drafted_revision: Any
    current_revision: Any  # None when the fresh read no longer shows the record

    def for_model(self) -> str:
        if self.current_revision is None:
            return (
                f"{self.name} ({self.record_id}) no longer appears in a fresh read "
                f"(you drafted against revision {self.drafted_revision})"
            )
        return (
            f"{self.name} ({self.record_id}) is now revision {self.current_revision} "
            f"(you drafted against {self.drafted_revision})"
        )

    def for_person(self) -> str:
        if self.current_revision is None:
            return f"{self.name} disappeared while I was working on this."
        return f"{self.name} was changed while I was working on this, so I re-read it and used the latest version."


def stale_targets(
    drafts: list[Draft], fresh: list[Observation], reread_ids: set[str], earlier: list[Observation]
) -> list[Staleness]:
    """Targets whose fresh read disagrees with the draft. Records that were not re-read are not judged."""
    current = observed_records(obs.data for obs in fresh)
    out = []
    for draft in drafts:
        if not draft.target or draft.target["id"] not in reread_ids:
            continue
        record_id = draft.target["id"]
        record = latest_record(record_id, fresh) or latest_record(record_id, earlier) or {}
        title = record.get("title")
        name = f'"{title}"' if isinstance(title, str) and title else record_id
        if record_id not in current:
            out.append(Staleness(record_id, name, draft.target.get("revision"), None))
        elif current[record_id] is None:
            continue  # a summary view without revisions shows the record exists, not which version
        elif current[record_id] != draft.target.get("revision"):
            out.append(Staleness(record_id, name, draft.target.get("revision"), current[record_id]))
    return out
