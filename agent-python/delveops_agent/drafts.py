"""Proposal drafts (overhaul plan, contract 2).

The agent never writes planner data. It can only describe a change as a draft, and this module
checks the draft is well formed and grounded before it leaves Python: the operation is one the
planner already supports, a change to an existing record names a target the agent actually saw in a
tool result (with its revision), and the evidence points at real tool calls from this run. Rust
repeats all of this against live data before the user is ever asked to approve anything.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from .types import Draft

# Operations that change an existing record and therefore need a target id + revision.
TARGETED_OPS = frozenset({"update_task", "schedule_task", "set_task_status"})
# Operations that create something new; no target.
CREATE_OPS = frozenset({"create_task", "create_milestone", "create_plan", "create_event", "create_task_block"})
ALLOWED_OPS = TARGETED_OPS | CREATE_OPS


class DraftError(ValueError):
    pass


def _collect_records(value: Any, out: dict[str, Any]) -> None:
    """Index every {"id": ..., "revision"?: ...} object in a tool result by id."""
    if isinstance(value, dict):
        record_id = value.get("id")
        if isinstance(record_id, str):
            out.setdefault(record_id, value.get("revision"))
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


def parse_draft(raw: Any, tool_call_ids: set[str], records: dict[str, Any]) -> Draft:
    if not isinstance(raw, dict):
        raise DraftError("each draft must be an object")
    op = raw.get("op")
    if op not in ALLOWED_OPS:
        raise DraftError(f"unsupported op {op!r}; allowed: {', '.join(sorted(ALLOWED_OPS))}")
    payload = raw.get("payload")
    if not isinstance(payload, dict) or not payload:
        raise DraftError(f"{op}: payload must be a non-empty object")
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
        if seen_revision is not None and revision != seen_revision:
            raise DraftError(f"{op}: target {record_id} has revision {seen_revision}, draft says {revision}")
        target = {"id": record_id, "revision": revision}
    else:
        target = None

    return Draft(op=op, target=target, payload=payload, evidence=tuple(evidence))
