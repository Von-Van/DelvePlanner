"""The shapes the benchmark hands to an agent and expects back.

These follow section 4 of the overhaul plan (kept outside the repo). The Python agent package (piece E) owns the
real implementation; the runner accepts either these dataclasses or any object/dict with the same field
names, so the two packages do not need to import each other.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol

FINAL_STATUSES = ("answered", "drafted", "needs_clarification", "cannot_complete")


@dataclass(frozen=True)
class AgentTask:
    goal: str
    now: str  # frozen ISO timestamp with offset
    timeZone: str
    permittedTools: tuple[str, ...]


@dataclass(frozen=True)
class Budget:
    maxModelCalls: int = 12
    maxToolCalls: int = 12
    maxWallSeconds: float = 120.0
    maxContextTokens: int = 16384


@dataclass
class Usage:
    modelCalls: int = 0
    toolCalls: int = 0
    tokens: int = 0
    latencyMs: float = 0.0


@dataclass
class AgentResult:
    status: str
    answer: str = ""
    drafts: list[dict[str, Any]] = field(default_factory=list)
    trace: list[dict[str, Any]] = field(default_factory=list)
    usage: Usage = field(default_factory=Usage)


class ToolClient(Protocol):
    """Read-only tool access. Every call returns ``{"ok": True, "data": ...}`` or
    ``{"ok": False, "error": {"code", "message", "retryable"}}``; it never raises for tool failures."""

    def list_tools(self) -> list[dict[str, Any]]: ...

    def call(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]: ...


class ModelClient(Protocol):
    def describe(self) -> dict[str, Any]: ...


class Agent(Protocol):
    architecture_id: str

    def run(self, task: AgentTask, tools: ToolClient, model: ModelClient | None, budget: Budget) -> Any: ...


def _as_list(value: Any) -> list:
    return list(value) if isinstance(value, (list, tuple)) else ([] if value is None else [value])


def _as_dict(value: Any) -> dict:
    """Malformed items are kept, wrapped, so scorers can fail them instead of the harness crashing."""
    return dict(value) if isinstance(value, dict) else {"invalid": repr(value)}


def coerce_result(raw: Any) -> AgentResult:
    """Turn whatever an agent returned (dataclass, object or dict) into an AgentResult."""

    def get(name: str, default: Any) -> Any:
        if isinstance(raw, dict):
            return raw.get(name, default)
        return getattr(raw, name, default)

    usage_raw = get("usage", None) or {}
    if not isinstance(usage_raw, dict) and not hasattr(usage_raw, "modelCalls"):
        usage_raw = {}
    if not isinstance(usage_raw, dict):
        usage_raw = {k: getattr(usage_raw, k) for k in ("modelCalls", "toolCalls", "tokens", "latencyMs") if hasattr(usage_raw, k)}
    return AgentResult(
        status=str(get("status", "")),
        answer=str(get("answer", "") or ""),
        drafts=[_as_dict(d) for d in _as_list(get("drafts", None))],
        trace=[_as_dict(e) for e in _as_list(get("trace", None))],
        usage=Usage(**{k: usage_raw[k] for k in ("modelCalls", "toolCalls", "tokens", "latencyMs") if k in usage_raw}),
    )
