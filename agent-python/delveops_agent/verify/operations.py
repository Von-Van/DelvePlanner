"""Payload checks and safe rewrites for drafts, on top of A1's `MutationOperation` table.

`drafts.OP_SPECS` (piece E) is the single description of every operation's fields and value types, and
`drafts.parse_draft` refuses a draft at its first problem. The verifier adds two things on top:

* `payload_problems(op, payload)`: *every* problem at once, each worded so a model can fix it, with hints
  for the field names models reach for ("start" → "startAtUtc"), plus checks the type table leaves to
  Rust (a real IANA zone, a positive duration).
* `normalize_payload(op, payload, time_zone)`: rewrites that keep the meaning exactly, such as turning a
  local `start`/`end` pair into `startAtUtc` + `durationMinutes`, or a bare date into a day change.
  Anything that would need a guess is left for the model.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from ..drafts import _CHECKS, OP_SPECS, TYPE_HELP, UNCHANGED_DEFAULT

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
    "id": "the record id goes in target, not payload",
    "expectedRevision": "the revision goes in target, not payload",
}
_POSITIVE = frozenset({"durationMinutes"})


def _zone_ok(value: Any) -> bool:
    if not isinstance(value, str) or not value:
        return False
    try:
        ZoneInfo(value)
    except (ZoneInfoNotFoundError, ValueError):
        return False
    return True


def payload_problems(op: str, payload: Any) -> list[str]:
    """Every reason the app would refuse this payload for `op`, worded for the model."""
    spec = OP_SPECS.get(op)
    if spec is None:
        return [f"unknown operation {op!r}"]
    if not isinstance(payload, dict):
        return [f"{op}: payload must be an object"]
    problems: list[str] = []
    for name in payload:
        if name not in spec:
            hint = _HINTS.get(name)
            allowed = ", ".join(spec) or "no fields"
            problems.append(f"{op}: unknown field {name!r}" + (f" ({hint})" if hint else f"; allowed: {allowed}"))
    for name, (kind, required) in spec.items():
        value = payload.get(name)
        if value is None:
            if required and kind not in UNCHANGED_DEFAULT:
                problems.append(f"{op}: missing {name!r} ({TYPE_HELP[kind]})")
            continue
        if not _CHECKS[kind](value):
            problems.append(f"{op}: {name} must be {TYPE_HELP[kind]}")
        elif kind == "tz" and not _zone_ok(value):
            problems.append(f'{op}: {name} "{value}" is not an IANA time zone')
        elif name in _POSITIVE and value <= 0:
            problems.append(f"{op}: {name} must be above 0")
    return problems


def _moment(value: Any) -> datetime | None:
    if not isinstance(value, str) or "T" not in value:
        return None
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return moment if moment.tzinfo is not None else None


def parse_utc(value: Any) -> datetime | None:
    """A UTC timestamp as Rust's `validate_time` accepts it (RFC 3339, zero offset)."""
    moment = _moment(value)
    if moment is None or moment.utcoffset() != UTC.utcoffset(None):
        return None
    return moment


def canonical_utc(moment: datetime) -> str:
    return moment.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def normalize_payload(op: str, payload: dict[str, Any], time_zone: str) -> tuple[dict[str, Any], list[str]]:
    """Rewrite a payload without changing its meaning. Returns the new payload and what was changed.

    Only rewrites that need no judgement are made: a `start`/`end` pair with explicit offsets (the form
    the day-context tool reports) becomes `startAtUtc` + `durationMinutes`; any other spelling of a UTC
    time becomes the canonical one; a bare date where a day change is meant becomes "set the day"; a
    whole number where a reminder or estimate change is meant becomes "set it"; a bare id where a record
    reference is meant becomes `{"id": ...}`; and left-out "no change" fields are filled in.
    """
    out = dict(payload)
    notes: list[str] = []
    spec = OP_SPECS.get(op, {})

    if "startAtUtc" in spec and "startAtUtc" not in out:
        start, end = _moment(out.get("start")), _moment(out.get("end"))
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

    for name, (kind, _) in spec.items():
        value = out.get(name)
        if kind == "utc" and isinstance(value, str):
            moment = _moment(value)
            if moment is not None and canonical_utc(moment) != value:
                out[name] = canonical_utc(moment)
                notes.append(f"{name} spelled as UTC")
        elif kind == "day_change" and isinstance(value, str) and _CHECKS["date"](value):
            out[name] = {"action": "set", "day": value}
            notes.append(f"{name} date -> day change")
        elif kind == "reminder_change" and _CHECKS["int"](value):
            out[name] = {"action": "set", "minutesBefore": value}
            notes.append(f"{name} number -> reminder change")
        elif kind == "estimate_change" and _CHECKS["int"](value) and value > 0:
            out[name] = {"action": "set", "minutes": value}
            notes.append(f"{name} number -> estimate change")
        elif kind == "record_ref" and isinstance(value, str) and value:
            out[name] = {"id": value}
            notes.append(f"{name} id -> record reference")
        elif kind in UNCHANGED_DEFAULT and value is None:
            out[name] = {"action": "unchanged"}
            notes.append(f"{name} unchanged")
    return out, notes
