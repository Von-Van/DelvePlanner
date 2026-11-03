"""Trace events in the shared shape (overhaul plan, contract 5).

Routine traces hold metadata only. Full prompt and response text is attached only when the tracer is
created with `debug=True`, which is meant for local, opt-in debugging runs.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

TRACE_KINDS = frozenset(
    {"model_call", "tool_call", "tool_result", "plan", "verify", "repair", "draft", "final", "error"}
)


class Tracer:
    def __init__(self, run_id: str, debug: bool = False) -> None:
        self.run_id = run_id
        self.debug = debug
        self.events: list[dict[str, Any]] = []

    def emit(
        self,
        kind: str,
        summary: str,
        data: dict[str, Any] | None = None,
        duration_ms: int | None = None,
        debug_data: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        if kind not in TRACE_KINDS:
            raise ValueError(f"unknown trace kind {kind}")
        payload = dict(data or {})
        if self.debug and debug_data:
            payload["debug"] = debug_data
        event = {
            "runId": self.run_id,
            "seq": len(self.events) + 1,
            "kind": kind,
            "at": datetime.now(UTC).isoformat(timespec="milliseconds"),
            "summary": summary,
            "data": payload,
        }
        # The shared trace shape has no null duration: an untimed step simply leaves it out.
        if duration_ms is not None:
            event["durationMs"] = duration_ms
        self.events.append(event)
        return event
