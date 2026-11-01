"""Proposal drafts (overhaul plan, contract 2).

The agent never writes planner data. It can only describe a change as a draft, and this module makes
sure the draft is something the app will accept and is grounded in what the agent actually saw:

- `op` is exactly one variant of `MutationOperation` in src-tauri/src/model.rs (snake_case).
- `payload` holds that variant's fields, camelCase, with the same names, required fields and value
  shapes as the Rust type (which is `deny_unknown_fields`, so an extra key would be rejected there).
- A change to an existing record names a `target` id and revision that appeared in a tool result
  during this run; existing records referenced inside the payload (`{"id": ...}`) must also have
  been seen. Evidence must point at real tool calls.

`Draft.mutation_operation()` turns a draft into the exact JSON the Rust enum deserialises. Rust still
re-validates everything against live data before the person is ever asked to approve.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Iterable
from typing import Any

from .types import Draft

# -- MutationOperation table ------------------------------------------------------------------------
#
# field -> (value type, required). "Required" means a non-Option Rust field: it must be present.
# Option fields may be omitted or null. `tests/test_drafts.py` checks this table against model.rs.

_EVENT, _PLAN, _MILESTONE, _TASK = "eventId", "planId", "milestoneId", "taskId"

OP_SPECS: dict[str, dict[str, tuple[str, bool]]] = {
    "create_event": {
        "title": ("str", True),
        "notes": ("str", True),
        "startAtUtc": ("utc", True),
        "timeZone": ("tz", True),
        "durationMinutes": ("int", True),
        "reminderMinutesBefore": ("int", False),
        "plan": ("record_ref", False),
        "fromInbox": ("inbox_source", False),
    },
    "update_event": {
        "title": ("str", False),
        "notes": ("str", False),
        "durationMinutes": ("int", False),
        "reminderChange": ("reminder_change", True),
    },
    "delete_event": {},
    "reschedule_event": {
        "title": ("str", False),
        "notes": ("str", False),
        "startAtUtc": ("utc", True),
        "timeZone": ("tz", True),
        "durationMinutes": ("int", False),
        "reminderChange": ("reminder_change", True),
    },
    "set_event_plan": {"plan": ("record_ref", False)},
    "create_plan": {
        "title": ("str", True),
        "description": ("str", True),
        "status": ("plan_status", True),
        "startDate": ("date", False),
        "targetDate": ("date", False),
        "fromInbox": ("inbox_source", False),
    },
    "update_plan": {
        "title": ("str", False),
        "description": ("str", False),
        "status": ("plan_status", False),
        "startDate": ("date", False),
        "targetDate": ("date", False),
    },
    "create_milestone": {
        "plan": ("record_ref", True),
        "title": ("str", True),
        "description": ("str", True),
        "targetDate": ("date", False),
    },
    "update_milestone": {
        "title": ("str", False),
        "description": ("str", False),
        "targetDate": ("date", False),
        "status": ("milestone_status", False),
    },
    "delete_milestone": {},
    "create_task": {
        "title": ("str", True),
        "description": ("str", True),
        "plan": ("record_ref", False),
        "milestone": ("record_ref", False),
        "scheduledDay": ("date", False),
        "dueDate": ("date", False),
        "status": ("task_status", True),
        "priority": ("task_priority", True),
        "workstream": ("record_ref", False),
        "fromInbox": ("inbox_source", False),
    },
    "update_task": {
        "title": ("str", False),
        "description": ("str", False),
        "status": ("task_status", False),
        "priority": ("task_priority", False),
        "estimate": ("estimate_change", True),
    },
    "schedule_task": {
        "scheduledDay": ("day_change", True),
        "dueDate": ("day_change", True),
        "plannedWeek": ("day_change", True),
    },
    "set_task_plan": {"plan": ("record_ref", False), "milestone": ("record_ref", False)},
    "delete_task": {},
    "create_workstream": {"plan": ("record_ref", True), "name": ("str", True)},
    "set_task_workstream": {"workstream": ("record_ref", False)},
}

# Operations on an existing record, and the Rust field that carries the record's id.
TARGET_ID_FIELDS: dict[str, str] = {
    "update_event": _EVENT,
    "delete_event": _EVENT,
    "reschedule_event": _EVENT,
    "set_event_plan": _EVENT,
    "update_plan": _PLAN,
    "update_milestone": _MILESTONE,
    "delete_milestone": _MILESTONE,
    "update_task": _TASK,
    "schedule_task": _TASK,
    "set_task_plan": _TASK,
    "delete_task": _TASK,
    "set_task_workstream": _TASK,
}
TARGETED_OPS = frozenset(TARGET_ID_FIELDS)
CREATE_OPS = frozenset(OP_SPECS) - TARGETED_OPS
ALLOWED_OPS = frozenset(OP_SPECS)

ENUMS: dict[str, tuple[str, ...]] = {
    "plan_status": ("planning", "active", "on_hold", "complete", "cancelled"),
    "milestone_status": ("pending", "complete", "skipped"),
    "task_status": ("todo", "in_progress", "blocked", "done"),
    "task_priority": ("low", "normal", "high", "critical"),
}

# How each value type is written, for the prompt and for error messages.
TYPE_HELP: dict[str, str] = {
    "str": "text",
    "int": "whole number",
    "date": '"YYYY-MM-DD"',
    "utc": '"YYYY-MM-DDTHH:MM:SSZ" (UTC)',
    "tz": 'IANA time zone like "America/Chicago"',
    "record_ref": '{"id": ID} for an existing record or {"newTitle": TEXT} for one created in the same proposal',
    "inbox_source": '{"itemId": ID, "expectedRevision": N}',
    "reminder_change": '{"action": "unchanged"} | {"action": "clear"} | {"action": "set", "minutesBefore": N}',
    "day_change": '{"action": "unchanged"} | {"action": "clear"} | {"action": "set", "day": "YYYY-MM-DD"}',
    "estimate_change": '{"action": "unchanged"} | {"action": "clear"} | {"action": "set", "minutes": N}',
    **{name: " | ".join(f'"{v}"' for v in values) for name, values in ENUMS.items()},
}

_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_UTC = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|\+00:00)$")


# Required Rust "change" fields that default to {"action": "unchanged"} when a draft leaves them out.
UNCHANGED_DEFAULT = frozenset({"reminder_change", "day_change", "estimate_change"})


class DraftError(ValueError):
    pass


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _nonempty_str(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _tagged(value: Any, set_fields: dict[str, Callable[[Any], bool]]) -> bool:
    """A Rust `#[serde(tag = "action")]` change: unchanged, clear, or set with exactly `set_fields`."""
    if not isinstance(value, dict):
        return False
    action = value.get("action")
    if action in ("unchanged", "clear"):
        return set(value) == {"action"}
    if action == "set":
        return set(value) == {"action", *set_fields} and all(check(value[k]) for k, check in set_fields.items())
    return False


def _one_of(allowed: tuple[str, ...]) -> Callable[[Any], bool]:
    return lambda v: v in allowed


_CHECKS: dict[str, Callable[[Any], bool]] = {
    "str": lambda v: isinstance(v, str),
    "int": _is_int,
    "date": lambda v: isinstance(v, str) and bool(_DATE.match(v)),
    "utc": lambda v: isinstance(v, str) and bool(_UTC.match(v)),
    "tz": _nonempty_str,
    "record_ref": lambda v: (
        isinstance(v, dict)
        and ((set(v) == {"id"} and _nonempty_str(v["id"])) or (set(v) == {"newTitle"} and _nonempty_str(v["newTitle"])))
    ),
    "inbox_source": lambda v: (
        isinstance(v, dict)
        and set(v) == {"itemId", "expectedRevision"}
        and _nonempty_str(v["itemId"])
        and _is_int(v["expectedRevision"])
    ),
    "reminder_change": lambda v: _tagged(v, {"minutesBefore": _is_int}),
    "day_change": lambda v: _tagged(v, {"day": lambda d: isinstance(d, str) and bool(_DATE.match(d))}),
    "estimate_change": lambda v: _tagged(v, {"minutes": _is_int}),
    **{name: _one_of(allowed) for name, allowed in ENUMS.items()},
}


def payload_help(op: str) -> str:
    """One line describing an op's payload, for the prompt."""
    spec = OP_SPECS[op]
    target = f" target {TARGET_ID_FIELDS[op][:-2]}" if op in TARGETED_OPS else " target null"
    if not spec:
        return f"- {op} ({target.strip()}): payload {{}}"
    fields = ", ".join(
        f"{name}{'' if required and kind not in UNCHANGED_DEFAULT else '?'}: {kind}"
        for name, (kind, required) in spec.items()
    )
    return f"- {op} ({target.strip()}): {fields}"


# -- grounding ------------------------------------------------------------------------------------


def _collect_records(value: Any, out: dict[str, Any]) -> None:
    """Index every {"id": ..., "revision"?: ...} object in a tool result by id."""
    if isinstance(value, dict):
        record_id = value.get("id")
        # Later observations win, so a re-read refreshes the revision a draft must cite. A later
        # result that omits the revision (a summary view) does not erase one seen earlier.
        if isinstance(record_id, str) and (record_id not in out or "revision" in value):
            out[record_id] = value.get("revision")
        for child in value.values():
            _collect_records(child, out)
    elif isinstance(value, list):
        for child in value:
            _collect_records(child, out)


def observed_records(tool_data: Iterable[Any]) -> dict[str, Any]:
    records: dict[str, Any] = {}
    for data in tool_data:
        _collect_records(data, records)
    return records


# Foreign-key fields in tool results whose values name real records (a task's planId, ...).
REFERENCE_FIELDS = frozenset({"planId", "milestoneId", "taskId", "workstreamId", "eventId"})


def _collect_references(value: Any, out: set[str]) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key in REFERENCE_FIELDS and isinstance(child, str) and child:
                out.add(child)
            _collect_references(child, out)
    elif isinstance(value, list):
        for child in value:
            _collect_references(child, out)


def observed_references(tool_data: Iterable[Any]) -> set[str]:
    """Every record id a tool result showed: records themselves plus foreign keys pointing at them.

    Good enough for naming a record in a payload (`{"id": ...}`); a target still needs the record
    itself and its revision.
    """
    data = list(tool_data)
    refs = set(observed_records(data))
    for item in data:
        _collect_references(item, refs)
    return refs


# -- parsing --------------------------------------------------------------------------------------


def _check_payload(op: str, payload: dict[str, Any], references: set[str]) -> None:
    spec = OP_SPECS[op]
    unknown = sorted(set(payload) - set(spec))
    if unknown:
        hint = " (the record id and revision go in target)" if {"expectedRevision", "id"} & set(unknown) else ""
        allowed = ", ".join(spec) or "none"
        raise DraftError(f"{op}: unknown payload field(s) {', '.join(unknown)}{hint}; allowed: {allowed}")
    for name, (kind, required) in spec.items():
        if (name not in payload or payload[name] is None) and kind in UNCHANGED_DEFAULT:
            # Rust requires these change fields; leaving one out can only mean "leave it as it is".
            payload[name] = {"action": "unchanged"}
        if name not in payload or payload[name] is None:
            if required:
                raise DraftError(f"{op}: payload.{name} is required ({TYPE_HELP[kind]})")
            continue
        value = payload[name]
        if not _CHECKS[kind](value):
            raise DraftError(f"{op}: payload.{name} must be {TYPE_HELP[kind]}")
        if kind == "record_ref" and "id" in value and value["id"] not in references:
            raise DraftError(f"{op}: payload.{name} refers to {value['id']}, which no tool result showed")


def parse_draft(
    raw: Any, tool_call_ids: set[str], records: dict[str, Any], references: set[str] | None = None
) -> Draft:
    """Validate one draft. `records` maps seen record ids to revisions (for targets); `references`
    is every id seen, including foreign keys (for ids named in the payload; defaults to `records`)."""
    if not isinstance(raw, dict):
        raise DraftError("each draft must be an object")
    op = raw.get("op")
    if op not in ALLOWED_OPS:
        raise DraftError(f"unsupported op {op!r}; allowed: {', '.join(sorted(ALLOWED_OPS))}")
    payload = raw.get("payload")
    if payload is None:
        payload = {}
    if not isinstance(payload, dict):
        raise DraftError(f"{op}: payload must be an object")
    evidence = raw.get("evidence") or []
    if not isinstance(evidence, list) or not all(isinstance(e, str) for e in evidence):
        raise DraftError(f"{op}: evidence must be a list of tool call ids")
    unknown = [e for e in evidence if e not in tool_call_ids]
    if unknown:
        raise DraftError(f"{op}: evidence refers to tool calls that did not happen: {', '.join(unknown)}")

    target = raw.get("target")
    if op in TARGETED_OPS:
        if not isinstance(target, dict) or not isinstance(target.get("id"), str):
            raise DraftError(f"{op}: target.id is required")
        if not evidence:
            raise DraftError(f"{op}: changing an existing record needs evidence")
        record_id = target["id"]
        if record_id not in records:
            raise DraftError(f"{op}: target {record_id} was not seen in any tool result")
        seen_revision = records[record_id]
        revision = target.get("revision")
        if not _is_int(seen_revision):
            raise DraftError(f"{op}: no revision was seen for {record_id}; look it up before changing it")
        if revision != seen_revision:
            raise DraftError(f"{op}: target {record_id} has revision {seen_revision}, draft says {revision}")
        target = {"id": record_id, "revision": revision}
    else:
        if target not in (None, {}):
            raise DraftError(f"{op}: creates a new record, so target must be null")
        target = None

    payload = dict(payload)
    _check_payload(op, payload, set(records) if references is None else references)
    return Draft(op=op, target=target, payload=payload, evidence=tuple(evidence))


def mutation_operation(draft: Draft) -> dict[str, Any]:
    """The exact JSON for Rust's `MutationOperation` (`tag = "type"`, camelCase fields)."""
    operation: dict[str, Any] = {"type": draft.op}
    if draft.op in TARGETED_OPS:
        assert draft.target is not None
        operation[TARGET_ID_FIELDS[draft.op]] = draft.target["id"]
        operation["expectedRevision"] = draft.target["revision"]
    operation.update({k: v for k, v in draft.payload.items() if v is not None})
    return operation
