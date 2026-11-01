"""Trace recording in the shared trace shape (``schemas/trace.v1.json``).

Routine traces keep metadata only: tool names, arguments (fixtures are synthetic), the ids and revisions a
result contained, error codes and timings. Full tool results are kept only when ``debug`` is on.
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .jsonschema_lite import validate

SCHEMA_PATH = Path(__file__).parent / "schemas" / "trace.v1.json"
TRACE_KINDS = ("model_call", "tool_call", "tool_result", "plan", "verify", "repair", "draft", "final", "error")


def load_trace_schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text())


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


class TraceRecorder:
    def __init__(self, run_id: str, debug: bool = False):
        self.run_id = run_id
        self.debug = debug
        self.events: list[dict[str, Any]] = []

    def emit(self, kind: str, summary: str, data: dict[str, Any] | None = None, duration_ms: float | None = None) -> dict:
        if kind not in TRACE_KINDS:
            raise ValueError(f"unknown trace kind {kind}")
        event: dict[str, Any] = {
            "runId": self.run_id,
            "seq": len(self.events) + 1,
            "kind": kind,
            "at": utc_now(),
            "summary": summary,
            "data": data or {},
        }
        if duration_ms is not None:
            event["durationMs"] = round(duration_ms, 3)
        self.events.append(event)
        return event


def validate_events(events: list[dict[str, Any]]) -> list[str]:
    schema = load_trace_schema()
    problems: list[str] = []
    for i, event in enumerate(events):
        problems.extend(validate(event, schema, path=f"trace[{i}]"))
    return problems
