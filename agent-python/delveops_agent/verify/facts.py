"""Fact checks: does a set of drafts agree with what the agent itself read in this run?

`drafts.parse_draft` already guarantees a draft has the shape the app accepts and targets a record (and
revision) a tool returned. These checks look one level up, at whether the change makes sense:

* **time**: an event placed on top of something on that day, outside working hours, or in the past;
* **dependencies**: a task scheduled or finished while a task it waits on is still open;
* **capacity**: tasks scheduled onto a day with less free time than they are estimated to take.

Every check uses only tool results from this run. A day, task or week the agent never read is not
judged, so a check never assumes something is free or done. Nothing here calls a tool or the model.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from datetime import date, datetime, timedelta
from typing import Any
from zoneinfo import ZoneInfo

from ..types import Draft
from .operations import normalize_payload, parse_utc, payload_problems


@dataclass(frozen=True)
class Observation:
    """One successful tool call the agent made in this run."""

    call_id: str
    tool: str
    arguments: dict[str, Any]
    data: Any


@dataclass(frozen=True)
class Finding:
    draft: int  # index into the drafts list
    code: str  # time_conflict | outside_hours | in_past | waiting_on | over_capacity
    message: str  # plain words, fit to show the person

    def to_dict(self) -> dict[str, Any]:
        return {"draft": self.draft, "code": self.code, "message": self.message}


# -- before parsing: safe rewrites and the full problem list ----------------------------------------


def normalize_drafts(raw: list[Any], time_zone: str) -> tuple[list[Any], list[str]]:
    """Meaning-preserving rewrites of each draft's payload (see `operations.normalize_payload`)."""
    out, notes = [], []
    for index, item in enumerate(raw):
        if isinstance(item, dict) and isinstance(item.get("payload"), dict) and isinstance(item.get("op"), str):
            payload, changed = normalize_payload(item["op"], item["payload"], time_zone)
            if changed:
                item = {**item, "payload": payload}
                notes.extend(f"draft {index}: {c}" for c in changed)
        out.append(item)
    return out, notes


def shape_problems(raw: list[Any]) -> list[str]:
    """Every payload problem in every draft, where the parser would stop at the first."""
    problems = []
    for index, item in enumerate(raw):
        if isinstance(item, dict):
            problems.extend(f"draft {index}: {p}" for p in payload_problems(str(item.get("op")), item.get("payload")))
    return problems


# -- helpers --------------------------------------------------------------------------------------


def _instant(value: Any) -> datetime | None:
    if not isinstance(value, str) or "T" not in value:
        return None
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return moment if moment.tzinfo is not None else None


def _zone(name: Any, fallback: str) -> ZoneInfo | None:
    for candidate in (name, fallback):
        if isinstance(candidate, str) and candidate:
            try:
                return ZoneInfo(candidate)
            except Exception:  # noqa: BLE001 - an unknown zone is a payload problem, reported elsewhere
                continue
    return None


def _find(value: Any, record_id: str) -> dict[str, Any] | None:
    if isinstance(value, dict):
        if value.get("id") == record_id:
            return value
        children: Iterable[Any] = value.values()
    elif isinstance(value, list):
        children = value
    else:
        return None
    for child in children:
        hit = _find(child, record_id)
        if hit is not None:
            return hit
    return None


def latest_record(record_id: str, observations: list[Observation]) -> dict[str, Any] | None:
    """The most recent tool result's copy of a record, or None if no tool showed it."""
    for obs in reversed(observations):
        hit = _find(obs.data, record_id)
        if hit is not None:
            return hit
    return None


def _name(record: dict[str, Any] | None, fallback: str) -> str:
    if record and isinstance(record.get("title"), str) and record["title"]:
        return f'"{record["title"]}"'
    if record and isinstance(record.get("taskTitle"), str) and record["taskTitle"]:
        return f'"{record["taskTitle"]}"'
    return fallback


def _hhmm(moment: datetime, zone: ZoneInfo) -> str:
    return f"{moment.astimezone(zone):%H:%M}"


def _set_day(change: Any) -> str | None:
    if isinstance(change, dict) and change.get("action") == "set" and isinstance(change.get("day"), str):
        return change["day"]
    return None


# -- time -----------------------------------------------------------------------------------------


def _latest_day(observations: list[Observation], day: str) -> Observation | None:
    for obs in reversed(observations):
        if obs.tool == "get_day_context" and isinstance(obs.data, dict) and obs.data.get("date") == day:
            return obs
    return None


def _working_hours(observations: list[Observation]) -> dict[str, Any] | None:
    for obs in reversed(observations):
        if obs.tool == "get_capacity" and isinstance(obs.data, dict) and isinstance(obs.data.get("workingHours"), dict):
            return obs.data["workingHours"]
    return None


def _event_window(
    draft: Draft, observations: list[Observation], time_zone: str
) -> tuple[datetime, datetime, ZoneInfo] | None:
    start = parse_utc(draft.payload.get("startAtUtc"))
    zone = _zone(draft.payload.get("timeZone"), time_zone)
    if start is None or zone is None:
        return None
    minutes = draft.payload.get("durationMinutes")
    if minutes is None and draft.target:
        existing = latest_record(draft.target["id"], observations) or {}
        minutes = existing.get("durationMinutes")
    if not isinstance(minutes, int) or isinstance(minutes, bool) or minutes <= 0:
        return None
    return start, start + timedelta(minutes=minutes), zone


def time_findings(drafts: list[Draft], observations: list[Observation], now: str, time_zone: str) -> list[Finding]:
    findings: list[Finding] = []
    current = _instant(now)
    hours = _working_hours(observations)
    for index, draft in enumerate(drafts):
        if draft.op not in ("create_event", "reschedule_event"):
            continue
        window = _event_window(draft, observations, time_zone)
        if window is None:
            continue
        start, end, zone = window
        target_id = (draft.target or {}).get("id")
        what = _name(
            draft.payload if draft.op == "create_event" else latest_record(target_id or "", observations), "the event"
        )
        span = f"{_hhmm(start, zone)}-{_hhmm(end, zone)}"
        if current is not None and start < current:
            findings.append(
                Finding(
                    index,
                    "in_past",
                    f"{what} would start at {span} on {start.astimezone(zone):%b %d}, which is in the past.",
                )
            )
        day = _latest_day(observations, start.astimezone(zone).date().isoformat())
        if day is not None:
            clashes = []
            for kind in ("events", "taskBlocks", "externalBusy"):
                for item in day.data.get(kind) or []:
                    if not isinstance(item, dict) or (target_id and item.get("id") == target_id):
                        continue
                    s, e = _instant(item.get("startAtUtc")), _instant(item.get("endAtUtc"))
                    if s and e and s < end and e > start:
                        label = _name(item, "busy time on a calendar")
                        clashes.append(f"{label} ({_hhmm(s, zone)}-{_hhmm(e, zone)})")
            if clashes:
                free = ", ".join(
                    f"{_hhmm(s, zone)}-{_hhmm(e, zone)}"
                    for s, e in (
                        (_instant(r.get("startAtUtc")), _instant(r.get("endAtUtc")))
                        for r in day.data.get("free") or []
                        if isinstance(r, dict)
                    )
                    if s and e
                )
                findings.append(
                    Finding(
                        index,
                        "time_conflict",
                        f"{what} at {span} overlaps {', '.join(sorted(set(clashes)))}"
                        + (f"; free that day: {free}." if free else "; the day has no free time left."),
                    )
                )
        if hours is not None:
            local_start, local_end = start.astimezone(zone), end.astimezone(zone)
            start_minute = local_start.hour * 60 + local_start.minute
            end_minute = start_minute + int((end - start).total_seconds() // 60)
            on_day = local_start.isoweekday() in (hours.get("days") or [])
            within = hours.get("startMinute", 0) <= start_minute and end_minute <= hours.get("endMinute", 24 * 60)
            if not on_day or not within or local_end.date() != local_start.date():
                hs, he = hours.get("startMinute", 0), hours.get("endMinute", 0)
                findings.append(
                    Finding(
                        index,
                        "outside_hours",
                        f"{what} at {span} on {local_start:%A} falls outside working hours "
                        f"({hs // 60:02d}:{hs % 60:02d}-{he // 60:02d}:{he % 60:02d}).",
                    )
                )
    return findings


# -- dependencies ---------------------------------------------------------------------------------


def dependency_findings(drafts: list[Draft], observations: list[Observation]) -> list[Finding]:
    findings: list[Finding] = []
    for index, draft in enumerate(drafts):
        if not draft.target:
            continue
        scheduled = _set_day(draft.payload.get("scheduledDay")) if draft.op == "schedule_task" else None
        starting = draft.op == "update_task" and draft.payload.get("status") in ("in_progress", "done")
        if scheduled is None and not starting:
            continue
        task = latest_record(draft.target["id"], observations) or {}
        for waiting_id in task.get("waitingOn") or []:
            other = latest_record(waiting_id, observations)
            if other is None or other.get("status") == "done":
                continue  # never assume an unseen task is open
            other_day = other.get("scheduledDay") or other.get("dueDate")
            if scheduled is not None:
                if isinstance(other_day, str) and other_day <= scheduled:
                    continue  # it is planned to be done first
                when = f", planned for {other_day}" if isinstance(other_day, str) else ""
                message = (
                    f"{_name(task, 'This task')} waits on {_name(other, waiting_id)}, which is not done yet{when}."
                )
            else:
                message = f"{_name(task, 'This task')} waits on {_name(other, waiting_id)}, which is not done yet."
            findings.append(Finding(index, "waiting_on", message))
    return findings


# -- capacity -------------------------------------------------------------------------------------


def _available(observations: list[Observation], day: str) -> int | None:
    for obs in reversed(observations):
        if not isinstance(obs.data, dict):
            continue
        if obs.tool == "get_day_context" and obs.data.get("date") == day:
            value = obs.data.get("availableMinutes")
            return value if isinstance(value, int) else None
        if obs.tool == "get_capacity":
            for entry in obs.data.get("days") or []:
                if (
                    isinstance(entry, dict)
                    and entry.get("day") == day
                    and isinstance(entry.get("availableMinutes"), int)
                ):
                    return entry["availableMinutes"]
    return None


def capacity_findings(drafts: list[Draft], observations: list[Observation]) -> list[Finding]:
    """Tasks scheduled onto one day that together need more time than the day has free."""
    by_day: dict[str, list[tuple[int, dict[str, Any], int]]] = {}
    for index, draft in enumerate(drafts):
        day = _set_day(draft.payload.get("scheduledDay")) if draft.op == "schedule_task" and draft.target else None
        if day is None:
            continue
        task = latest_record(draft.target["id"], observations) or {}
        if task.get("scheduledDay") == day:
            continue  # already counted in that day's planned time
        minutes = task.get("estimatedMinutes")
        if isinstance(minutes, int) and minutes > 0:
            by_day.setdefault(day, []).append((index, task, minutes))
    findings = []
    for day, items in by_day.items():
        free = _available(observations, day)
        total = sum(m for _, _, m in items)
        if free is None or total <= free:
            continue
        names = ", ".join(f"{_name(t, 'a task')} ({m} min)" for _, t, m in items)
        weekday = date.fromisoformat(day).strftime("%A %b %d")
        message = f"{names} need {total} min on {weekday}, but only {free} min are free that day."
        findings.append(Finding(items[0][0], "over_capacity", message))
    return findings


def fact_findings(drafts: list[Draft], observations: list[Observation], now: str, time_zone: str) -> list[Finding]:
    return (
        time_findings(drafts, observations, now, time_zone)
        + dependency_findings(drafts, observations)
        + capacity_findings(drafts, observations)
    )
