"""Drafts must be exactly what Rust's MutationOperation accepts.

The table in delveops_agent/drafts.py is checked against src-tauri/src/model.rs and against the
benchmark's draft schema, so a change on either side fails here instead of in the app.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from delveops_agent.drafts import (
    ALLOWED_OPS,
    CREATE_OPS,
    DRAFT_OPS,
    MAX_MEMORY_CHARS,
    MEMORY_OP_SPECS,
    OP_SPECS,
    TARGET_ID_FIELDS,
    TARGETED_OPS,
    DraftError,
    mutation_operation,
    parse_draft,
)

REPO = Path(__file__).resolve().parents[2]
MODEL_RS = REPO / "src-tauri" / "src" / "model.rs"
BENCH_DRAFT_SCHEMA = REPO / "agent_bench" / "schemas" / "draft.v0.json"

# How each spec type is spelled in Rust.
RUST_TYPES = {
    "str": "String",
    "date": "String",
    "utc": "String",
    "tz": "String",
    "int": "i64",
    "record_ref": "RecordRef",
    "inbox_source": "InboxSource",
    "reminder_change": "ReminderChange",
    "day_change": "DayChange",
    "estimate_change": "EstimateChange",
    "plan_status": "PlanStatus",
    "milestone_status": "MilestoneStatus",
    "task_status": "TaskStatus",
    "task_priority": "TaskPriority",
}


def _snake(name: str) -> str:
    return re.sub(r"(?<!^)([A-Z])", r"_\1", name).lower()


def _camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(part.title() for part in rest)


def _rust_variants() -> dict[str, dict[str, str]]:
    """{op: {camelCaseField: rust type}} parsed from `pub enum MutationOperation`."""
    source = MODEL_RS.read_text(encoding="utf-8")
    body = source[source.index("pub enum MutationOperation") :]
    body = body[body.index("{") + 1 : body.index("\n}\n")]
    variants: dict[str, dict[str, str]] = {}
    for match in re.finditer(r"^    ([A-Z][A-Za-z]+)\s*\{(.*?)\}", body, re.M | re.S):
        lines = [line for line in match.group(2).splitlines() if not line.strip().startswith(("///", "//", "#["))]
        fields = re.findall(r"(\w+):\s*([\w<>]+)", " ".join(lines))
        variants[_snake(match.group(1))] = {_camel(name): rust_type for name, rust_type in fields}
    return variants


needs_repo = pytest.mark.skipif(not MODEL_RS.is_file(), reason="needs the full repository checkout")


@needs_repo
def test_op_table_matches_rust_mutation_operation_exactly():
    variants = _rust_variants()
    assert set(variants) == ALLOWED_OPS
    for op, rust_fields in variants.items():
        rust_fields = dict(rust_fields)
        if op in TARGETED_OPS:
            assert rust_fields.pop(TARGET_ID_FIELDS[op]) == "String", op
            assert rust_fields.pop("expectedRevision") == "i64", op
        ours = {
            name: (RUST_TYPES[kind] if required else f"Option<{RUST_TYPES[kind]}>")
            for name, (kind, required) in OP_SPECS[op].items()
        }
        assert ours == rust_fields, op


@pytest.mark.skipif(not BENCH_DRAFT_SCHEMA.is_file(), reason="needs agent_bench")
def test_op_table_matches_benchmark_draft_schema():
    spec = json.loads(BENCH_DRAFT_SCHEMA.read_text(encoding="utf-8"))
    assert set(spec["properties"]["op"]["enum"]) == DRAFT_OPS
    operations = spec["x-operations"]
    assert set(operations["targetless"]) == CREATE_OPS | set(MEMORY_OP_SPECS)
    specs = {**OP_SPECS, **MEMORY_OP_SPECS}
    assert set(operations["fields"]) == DRAFT_OPS
    for op, fields in operations["fields"].items():
        assert set(fields["required"]) == {n for n, (_, req) in specs[op].items() if req}, op
        assert set(fields["optional"]) == {n for n, (_, req) in specs[op].items() if not req}, op


# -- payload validation ---------------------------------------------------------------------------

RECORDS = {"task-1": 4, "plan-7": 2, "evt-9": 1}


def draft(op, payload, target=None, evidence=("tc-1",)):
    return parse_draft(
        {"op": op, "target": target, "payload": payload, "evidence": list(evidence)}, {"tc-1"}, dict(RECORDS)
    )


def test_status_change_is_update_task_with_estimate_left_unchanged():
    d = draft("update_task", {"status": "done"}, {"id": "task-1", "revision": 4})
    assert d.payload == {"status": "done", "estimate": {"action": "unchanged"}}
    assert mutation_operation(d) == {
        "type": "update_task",
        "taskId": "task-1",
        "expectedRevision": 4,
        "status": "done",
        "estimate": {"action": "unchanged"},
    }
    with pytest.raises(DraftError, match="unsupported op"):
        draft("set_task_status", {"status": "done"})


def test_create_task_round_trips_to_rust_shape():
    d = draft(
        "create_task",
        {
            "title": "Call Sam",
            "description": "",
            "status": "todo",
            "priority": "high",
            "plan": {"id": "plan-7"},
            "workstream": {"newTitle": "Outreach"},
            "dueDate": "2026-11-06",
        },
        evidence=(),
    )
    assert d.target is None
    op = mutation_operation(d)
    assert op["type"] == "create_task" and "expectedRevision" not in op
    assert op["plan"] == {"id": "plan-7"}


@pytest.mark.parametrize(
    "op, payload, target, message",
    [
        # The old free-form shapes that the app rejects.
        ("schedule_task", {"scheduledDay": "2026-11-05"}, {"id": "task-1", "revision": 4}, "scheduledDay must be"),
        ("update_task", {"status": "finished"}, {"id": "task-1", "revision": 4}, "status must be"),
        ("update_task", {"taskId": "task-1"}, {"id": "task-1", "revision": 4}, "unknown payload field"),
        ("update_task", {"expectedRevision": 4}, {"id": "task-1", "revision": 4}, "go in target"),
        ("create_task", {"title": "x"}, None, "description is required"),
        (
            "create_task",
            {"title": "x", "description": "", "status": "todo", "priority": "normal", "plan": "plan-7"},
            None,
            "plan must be",
        ),
        (
            "create_task",
            {"title": "x", "description": "", "status": "todo", "priority": "normal", "plan": {"id": "plan-404"}},
            None,
            "no tool result showed",
        ),
        (
            "create_event",
            {"title": "x", "notes": "", "startAtUtc": "2026-11-05T10:00", "timeZone": "UTC", "durationMinutes": 30},
            None,
            "startAtUtc must be",
        ),
        (
            "update_event",
            {"reminderChange": {"action": "set"}},
            {"id": "evt-9", "revision": 1},
            "reminderChange must be",
        ),
        ("delete_task", {"why": "done"}, {"id": "task-1", "revision": 4}, "allowed: none"),
        (
            "create_plan",
            {"title": "x", "description": "", "status": "active"},
            {"id": "plan-7", "revision": 2},
            "target must be null",
        ),
    ],
)
def test_payloads_the_app_would_reject_are_caught(op, payload, target, message):
    with pytest.raises(DraftError, match=message):
        draft(op, payload, target)


def test_every_op_has_a_valid_minimal_draft_that_maps_to_rust_field_names():
    samples = {
        "str": "x",
        "int": 30,
        "date": "2026-11-05",
        "utc": "2026-11-05T16:00:00Z",
        "tz": "America/Chicago",
        "record_ref": {"id": "plan-7"},
        "inbox_source": {"itemId": "in-1", "expectedRevision": 1},
        "reminder_change": {"action": "clear"},
        "day_change": {"action": "set", "day": "2026-11-05"},
        "estimate_change": {"action": "set", "minutes": 45},
        "plan_status": "active",
        "milestone_status": "pending",
        "task_status": "todo",
        "task_priority": "normal",
    }
    targets = {"eventId": "evt-9", "taskId": "task-1", "planId": "plan-7", "milestoneId": "ms-1"}
    records = {**RECORDS, "ms-1": 3}
    for op, spec in OP_SPECS.items():
        payload = {name: samples[kind] for name, (kind, _) in spec.items()}
        target = None
        if op in TARGETED_OPS:
            record = targets[TARGET_ID_FIELDS[op]]
            target = {"id": record, "revision": records[record]}
        d = parse_draft({"op": op, "target": target, "payload": payload, "evidence": ["tc-1"]}, {"tc-1"}, records)
        operation = mutation_operation(d)
        expected = {"type", *spec} | ({TARGET_ID_FIELDS[op], "expectedRevision"} if target else set())
        assert set(operation) == expected, op


# -- "remember this?" -----------------------------------------------------------------------------


def remember(payload, target=None, evidence=(), references=None):
    raw = {"op": "remember", "target": target, "payload": payload, "evidence": list(evidence)}
    return parse_draft(raw, {"tc-1"}, dict(RECORDS), references)


def test_remember_is_a_targetless_suggestion_not_a_planner_change():
    d = remember(
        {"kind": "priority", "content": "Sponsors come before the venue.", "basis": "stated", "planId": "plan-7"}
    )
    assert d.target is None and d.payload["planId"] == "plan-7"
    assert (
        remember({"kind": "preference", "content": "Seems to prefer mornings.", "basis": "inferred"}).op == "remember"
    )
    with pytest.raises(DraftError, match="not a planner change"):
        mutation_operation(d)


@pytest.mark.parametrize(
    ("payload", "target", "message"),
    [
        ({"kind": "note", "content": "x"}, None, "basis is required"),
        ({"kind": "mood", "content": "x", "basis": "stated"}, None, "kind"),
        ({"kind": "note", "content": "x", "basis": "maybe"}, None, "basis"),
        ({"kind": "note", "content": "  ", "basis": "stated"}, None, "content"),
        ({"kind": "note", "content": "x" * (MAX_MEMORY_CHARS + 1), "basis": "stated"}, None, "content"),
        ({"kind": "note", "content": "x", "basis": "stated", "planId": "plan-unseen"}, None, "no tool result showed"),
        ({"kind": "note", "content": "x", "basis": "stated", "extra": 1}, None, "unknown"),
        ({"kind": "note", "content": "x", "basis": "stated"}, {"id": "task-1", "revision": 4}, "target must be null"),
    ],
)
def test_bad_remember_drafts_are_refused(payload, target, message):
    with pytest.raises(DraftError, match=message):
        remember(payload, target)


def test_remember_project_must_be_a_returned_record_not_just_a_foreign_key():
    payload = {"kind": "note", "content": "x", "basis": "stated", "planId": "plan-fk"}
    with pytest.raises(DraftError, match="no tool result showed"):
        remember(payload, references={*RECORDS, "plan-fk"})
