"""The payload each draft operation must carry, checked the way Rust will deserialize it.

Mirrors `MutationOperation` in src-tauri/src/model.rs (serde camelCase, `deny_unknown_fields`) and the
`x-operations` table in agent_bench/schemas/draft.v0.json, with the value types added: Rust rejects a
draft whose field has the wrong shape just as surely as one with a missing field. The target id and
`expectedRevision` are not in the payload; they live in the draft's `target`.

Three things live here:

* `payload_problems(op, payload)`: every reason Rust would refuse the payload, each phrased so a model
  can fix it.
* `normalize_payload(op, payload, time_zone)`: deterministic rewrites that keep the meaning exactly, such
  as filling a missing "no change" field or turning a local `start`/`end` pair into `startAtUtc`,
  `timeZone` and `durationMinutes`. Anything that would need a guess is left for the model.
* `payload_guide()`: the same table as prompt text, so the model knows the fields before it writes.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, date, datetime
from typing import Any
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

Check = Callable[[Any], str | None]  # None when the value is fine, else what is wrong with it

_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")

PLAN_STATUSES = ("planning", "active", "on_hold", "complete", "cancelled")
TASK_STATUSES = ("todo", "in_progress", "blocked", "done")
TASK_PRIORITIES = ("low", "normal", "high", "critical")
MILESTONE_STATUSES = ("pending", "complete", "skipped")


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _string(value: Any) -> str | None:
    return None if isinstance(value, str) else "must be a string"


def _text(value: Any) -> str | None:
    if not isinstance(value, str) or not value.strip():
        return "must be a non-empty string"
    return None


def _integer(value: Any) -> str | None:
    return None if _is_int(value) else "must be a whole number"


def _positive(value: Any) -> str | None:
    return None if _is_int(value) and value > 0 else "must be a whole number above 0"


def _day(value: Any) -> str | None:
    if isinstance(value, str) and _DAY.match(value):
        try:
            date.fromisoformat(value)
            return None
        except ValueError:
            pass
    return 'must be a date like "2026-11-20"'


def parse_utc(value: Any) -> datetime | None:
    """A timestamp Rust's `validate_time` accepts: RFC 3339 with a zero UTC offset."""
    if not isinstance(value, str) or "T" not in value:
        return None
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if moment.tzinfo is None or moment.utcoffset() != UTC.utcoffset(None):
        return None
    return moment


def _utc(value: Any) -> str | None:
    if parse_utc(value) is None:
        return 'must be a UTC time ending in Z, like "2026-11-04T20:00:00Z"'
    return None


def _zone(value: Any) -> str | None:
    if not isinstance(value, str) or not value:
        return 'must be an IANA time zone like "America/New_York"'
    try:
        ZoneInfo(value)
    except (ZoneInfoNotFoundError, ValueError):
        return f'"{value}" is not an IANA time zone'
    return None


def _enum(options: tuple[str, ...]) -> Check:
    def check(value: Any) -> str | None:
        return None if value in options else f"must be one of {', '.join(options)}"

    return check


def _optional(check: Check) -> Check:
    def inner(value: Any) -> str | None:
        return None if value is None else check(value)

    return inner


def _record_ref(value: Any) -> str | None:
    """`RecordRef`: an existing record `{"id": ...}` or one made in the same proposal `{"newTitle": ...}`."""
    if isinstance(value, dict) and set(value) == {"id"} and isinstance(value["id"], str) and value["id"]:
        return None
    if isinstance(value, dict) and set(value) == {"newTitle"} and isinstance(value["newTitle"], str):
        return None
    return 'must be {"id": "<record id>"} (or {"newTitle": "..."} for a record created in the same proposal)'


def _change(extra: dict[str, Check], example: str) -> Check:
    """Tagged "no change / clear / set" edits: `DayChange`, `ReminderChange`, `EstimateChange`."""

    def check(value: Any) -> str | None:
        hint = f'must be {{"action": "unchanged"}}, {{"action": "clear"}} or {example}'
        if not isinstance(value, dict):
            return hint
        action = value.get("action")
        if action in ("unchanged", "clear"):
            return None if set(value) == {"action"} else hint
        if action == "set" and set(value) == {"action", *extra}:
            for key, inner in extra.items():
                if inner(value[key]) is not None:
                    return hint
            return None
        return hint

    return check


_DAY_CHANGE = _change({"day": _day}, '{"action": "set", "day": "2026-11-20"}')
_REMINDER_CHANGE = _change({"minutesBefore": _integer}, '{"action": "set", "minutesBefore": 15}')
_ESTIMATE_CHANGE = _change({"minutes": _positive}, '{"action": "set", "minutes": 60}')


def _inbox(value: Any) -> str | None:
    if (
        isinstance(value, dict)
        and set(value) == {"itemId", "expectedRevision"}
        and isinstance(value["itemId"], str)
        and _is_int(value["expectedRevision"])
    ):
        return None
    return 'must be {"itemId": "...", "expectedRevision": N}'


@dataclass(frozen=True)
class Field:
    name: str
    check: Check
    describe: str
    required: bool = True
    # The value check without the "or null" wrapper optional fields get; normalisation keys off it.
    base: Check | None = None


def _req(name: str, check: Check, describe: str) -> Field:
    return Field(name, check, describe, True, check)


def _opt(name: str, check: Check, describe: str) -> Field:
    return Field(name, _optional(check), describe, False, check)


_DAY_CHANGE_TEXT = "DayChange"
_REMINDER_TEXT = "ReminderChange"
_ESTIMATE_TEXT = "EstimateChange"
_REF_TEXT = "Ref"

GUIDE_LEGEND = """Value shapes:
- Ref: {"id": "<record id>"}
- DayChange: {"action": "unchanged"} | {"action": "clear"} | {"action": "set", "day": "YYYY-MM-DD"}
- ReminderChange: {"action": "unchanged"} | {"action": "clear"} | {"action": "set", "minutesBefore": N}
- EstimateChange: {"action": "unchanged"} | {"action": "clear"} | {"action": "set", "minutes": N}
- UTC: a time ending in Z, like "2026-11-04T20:00:00Z"; zone: an IANA name like America/New_York"""

OPERATION_FIELDS: dict[str, tuple[Field, ...]] = {
    "create_event": (
        _req("title", _text, "string"),
        _req("notes", _string, 'string, "" if none'),
        _req("startAtUtc", _utc, "UTC"),
        _req("timeZone", _zone, "zone"),
        _req("durationMinutes", _positive, "minutes"),
        _opt("reminderMinutesBefore", _integer, "minutes"),
        _opt("plan", _record_ref, _REF_TEXT),
        _opt("fromInbox", _inbox, "inbox item"),
    ),
    "update_event": (
        _req("reminderChange", _REMINDER_CHANGE, _REMINDER_TEXT),
        _opt("title", _text, "string"),
        _opt("notes", _string, "string"),
        _opt("durationMinutes", _positive, "minutes"),
    ),
    "delete_event": (),
    "reschedule_event": (
        _req("startAtUtc", _utc, "UTC"),
        _req("timeZone", _zone, "zone"),
        _req("reminderChange", _REMINDER_CHANGE, _REMINDER_TEXT),
        _opt("title", _text, "string"),
        _opt("notes", _string, "string"),
        _opt("durationMinutes", _positive, "minutes"),
    ),
    "set_event_plan": (_opt("plan", _record_ref, "Ref or null to remove"),),
    "create_plan": (
        _req("title", _text, "string"),
        _req("description", _string, 'string, "" if none'),
        _req("status", _enum(PLAN_STATUSES), "|".join(PLAN_STATUSES)),
        _opt("startDate", _day, "YYYY-MM-DD"),
        _opt("targetDate", _day, "YYYY-MM-DD"),
        _opt("fromInbox", _inbox, "inbox item"),
    ),
    "update_plan": (
        _opt("title", _text, "string"),
        _opt("description", _string, "string"),
        _opt("status", _enum(PLAN_STATUSES), "|".join(PLAN_STATUSES)),
        _opt("startDate", _day, "YYYY-MM-DD"),
        _opt("targetDate", _day, "YYYY-MM-DD"),
    ),
    "create_milestone": (
        _req("plan", _record_ref, _REF_TEXT),
        _req("title", _text, "string"),
        _req("description", _string, 'string, "" if none'),
        _opt("targetDate", _day, "YYYY-MM-DD"),
    ),
    "update_milestone": (
        _opt("title", _text, "string"),
        _opt("description", _string, "string"),
        _opt("targetDate", _day, "YYYY-MM-DD"),
        _opt("status", _enum(MILESTONE_STATUSES), "|".join(MILESTONE_STATUSES)),
    ),
    "delete_milestone": (),
    "create_task": (
        _req("title", _text, "string"),
        _req("description", _string, 'string, "" if none'),
        _req("status", _enum(TASK_STATUSES), "|".join(TASK_STATUSES)),
        _req("priority", _enum(TASK_PRIORITIES), "|".join(TASK_PRIORITIES)),
        _opt("plan", _record_ref, _REF_TEXT),
        _opt("milestone", _record_ref, _REF_TEXT),
        _opt("scheduledDay", _day, "YYYY-MM-DD"),
        _opt("dueDate", _day, "YYYY-MM-DD"),
        _opt("workstream", _record_ref, _REF_TEXT),
        _opt("fromInbox", _inbox, "inbox item"),
    ),
    "update_task": (
        _req("estimate", _ESTIMATE_CHANGE, _ESTIMATE_TEXT),
        _opt("title", _text, "string"),
        _opt("description", _string, "string"),
        _opt("status", _enum(TASK_STATUSES), "|".join(TASK_STATUSES)),
        _opt("priority", _enum(TASK_PRIORITIES), "|".join(TASK_PRIORITIES)),
    ),
    "schedule_task": (
        _req("scheduledDay", _DAY_CHANGE, _DAY_CHANGE_TEXT),
        _req("dueDate", _DAY_CHANGE, _DAY_CHANGE_TEXT),
        _req("plannedWeek", _DAY_CHANGE, "DayChange, any day in the week"),
    ),
    "set_task_plan": (
        _opt("plan", _record_ref, "Ref or null to remove"),
        _opt("milestone", _record_ref, _REF_TEXT),
    ),
    "delete_task": (),
    "create_workstream": (_req("plan", _record_ref, _REF_TEXT), _req("name", _text, "string")),
    "set_task_workstream": (_opt("workstream", _record_ref, "Ref or null to remove"),),
}

# Fields that may be left out of a model's draft and filled with "no change" without guessing: the
# person asked for something else, so leaving these alone is exactly what they meant.
UNCHANGED_DEFAULTS: dict[str, dict[str, Any]] = {
    "update_event": {"reminderChange": {"action": "unchanged"}},
    "reschedule_event": {"reminderChange": {"action": "unchanged"}},
    "update_task": {"estimate": {"action": "unchanged"}},
    "schedule_task": {
        "scheduledDay": {"action": "unchanged"},
        "dueDate": {"action": "unchanged"},
        "plannedWeek": {"action": "unchanged"},
    },
}

# Field names models reach for that the planner calls something else.
_HINTS = {
    "start": "use startAtUtc (UTC, ending in Z) with timeZone instead of start",
    "end": "use durationMinutes instead of end",
    "startTime": "use startAtUtc (UTC, ending in Z) with timeZone instead of startTime",
    "endTime": "use durationMinutes instead of endTime",
    "duration": "the field is durationMinutes",
    "date": "events take startAtUtc; tasks take scheduledDay/dueDate",
    "due": "the field is dueDate",
    "description": "events use notes, not description",
}


def payload_problems(op: str, payload: Any) -> list[str]:
    """Every reason Rust would refuse this payload for `op`, worded for the model."""
    fields = OPERATION_FIELDS.get(op)
    if fields is None:
        return [f"unknown operation {op!r}"]
    if not isinstance(payload, dict):
        return [f"{op}: payload must be an object"]
    known = {f.name: f for f in fields}
    problems: list[str] = []
    for name in payload:
        if name not in known:
            hint = _HINTS.get(name)
            allowed = ", ".join(known) or "no fields"
            problems.append(f"{op}: unknown field {name!r}" + (f" ({hint})" if hint else f"; allowed: {allowed}"))
    for spec in fields:
        if spec.name not in payload:
            if spec.required:
                problems.append(f"{op}: missing {spec.name!r} ({spec.describe})")
            continue
        issue = spec.check(payload[spec.name])
        if issue is not None:
            problems.append(f"{op}: {spec.name} {issue}")
    return problems


def canonical_utc(moment: datetime) -> str:
    return moment.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _local_moment(value: Any) -> datetime | None:
    if not isinstance(value, str) or "T" not in value:
        return None
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return moment if moment.tzinfo is not None else None


def normalize_payload(op: str, payload: dict[str, Any], time_zone: str) -> tuple[dict[str, Any], list[str]]:
    """Rewrite a payload without changing its meaning. Returns the new payload and what was changed.

    Only rewrites that need no judgement are made: "no change" for edit fields the model left out,
    a bare date where a day change was meant (`"dueDate": "2026-11-20"`), a whole number where a reminder
    or estimate change was meant, a UTC time in another spelling, and a `start`/`end` pair that carries
    explicit offsets (the form the day-context tool reports) turned into `startAtUtc` plus
    `durationMinutes`. Anything else is left for the model to fix from the problem list.
    """
    out = dict(payload)
    notes: list[str] = []
    fields = {f.name: f for f in OPERATION_FIELDS.get(op, ())}

    if op in ("create_event", "reschedule_event") and "startAtUtc" not in out:
        start, end = _local_moment(out.get("start")), _local_moment(out.get("end"))
        if start is not None and ("end" not in out or end is not None):
            out.pop("start")
            out["startAtUtc"] = canonical_utc(start)
            notes.append("start -> startAtUtc")
            if end is not None:
                out.pop("end")
                minutes = int((end - start).total_seconds() // 60)
                if minutes > 0 and "durationMinutes" not in out:
                    out["durationMinutes"] = minutes
                    notes.append("end -> durationMinutes")
            out.setdefault("timeZone", time_zone)

    if "startAtUtc" in out and isinstance(out["startAtUtc"], str):
        moment = _local_moment(out["startAtUtc"])
        if moment is not None:
            canonical = canonical_utc(moment)
            if canonical != out["startAtUtc"]:
                out["startAtUtc"] = canonical
                notes.append("startAtUtc spelled as UTC")

    for name, spec in fields.items():
        value = out.get(name)
        if spec.base is _DAY_CHANGE and isinstance(value, str) and _day(value) is None:
            out[name] = {"action": "set", "day": value}
            notes.append(f"{name} date -> day change")
        elif spec.base is _REMINDER_CHANGE and _is_int(value):
            out[name] = {"action": "set", "minutesBefore": value}
            notes.append(f"{name} number -> reminder change")
        elif spec.base is _ESTIMATE_CHANGE and _is_int(value) and value > 0:
            out[name] = {"action": "set", "minutes": value}
            notes.append(f"{name} number -> estimate change")
        elif spec.base is _record_ref and isinstance(value, str) and value:
            out[name] = {"id": value}
            notes.append(f"{name} id -> record reference")

    for name, default in UNCHANGED_DEFAULTS.get(op, {}).items():
        if name not in out:
            out[name] = dict(default)
            notes.append(f"{name} unchanged")
    return out, notes


def payload_guide() -> str:
    """The operation table as prompt text: the value shapes, then one line per operation."""
    lines = [GUIDE_LEGEND, "Operations (required fields first):"]
    for op, fields in OPERATION_FIELDS.items():
        if not fields:
            lines.append(f"- {op}: payload {{}} (the target says which record)")
            continue
        required = ", ".join(f"{f.name} ({f.describe})" for f in fields if f.required)
        optional = ", ".join(f"{f.name} ({f.describe})" for f in fields if not f.required)
        parts = [f"- {op}: " + (required or "no required fields")]
        if optional:
            parts.append(f"optional: {optional}")
        lines.append("; ".join(parts))
    return "\n".join(lines)
