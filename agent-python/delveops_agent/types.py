"""Shared data shapes for DelveOps agents (overhaul plan, contract 4).

Field names on the wire are camelCase to match the Rust side; Python attributes are snake_case.
Every shape here is plain data so the benchmark (piece B) and the comparison runner (piece H) can
serialise results without knowing which architecture produced them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal

ResultStatus = Literal["answered", "drafted", "needs_clarification", "cannot_complete"]

# The tool error taxonomy from the overhaul plan (contract 1).
TOOL_ERROR_CODES = frozenset(
    {
        "invalid_arguments",
        "not_found",
        "ambiguous",
        "permission_denied",
        "timeout",
        "unavailable",
        "budget_exhausted",
        "internal",
    }
)


@dataclass(frozen=True)
class AgentTask:
    """One request for an agent. `now` is frozen so runs are reproducible."""

    goal: str
    now: str
    time_zone: str
    permitted_tools: tuple[str, ...]

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> AgentTask:
        return cls(
            goal=data["goal"],
            now=data["now"],
            time_zone=data["timeZone"],
            permitted_tools=tuple(data.get("permittedTools", ())),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "goal": self.goal,
            "now": self.now,
            "timeZone": self.time_zone,
            "permittedTools": list(self.permitted_tools),
        }


@dataclass(frozen=True)
class Budget:
    """Hard limits for one run. Exceeding any of them ends the run with `cannot_complete`."""

    max_model_calls: int = 8
    max_tool_calls: int = 12
    max_wall_seconds: float = 120.0
    max_context_tokens: int = 16_000

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Budget:
        default = cls()
        return cls(
            max_model_calls=int(data.get("maxModelCalls", default.max_model_calls)),
            max_tool_calls=int(data.get("maxToolCalls", default.max_tool_calls)),
            max_wall_seconds=float(data.get("maxWallSeconds", default.max_wall_seconds)),
            max_context_tokens=int(data.get("maxContextTokens", default.max_context_tokens)),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "maxModelCalls": self.max_model_calls,
            "maxToolCalls": self.max_tool_calls,
            "maxWallSeconds": self.max_wall_seconds,
            "maxContextTokens": self.max_context_tokens,
        }


@dataclass(frozen=True)
class ToolError:
    code: str
    message: str
    retryable: bool = False

    def __post_init__(self) -> None:
        if self.code not in TOOL_ERROR_CODES:
            raise ValueError(f"unknown tool error code: {self.code}")

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ToolError:
        code = data.get("code", "internal")
        if code not in TOOL_ERROR_CODES:
            code = "internal"
        return cls(code=code, message=str(data.get("message", "")), retryable=bool(data.get("retryable", False)))

    def to_dict(self) -> dict[str, Any]:
        return {"code": self.code, "message": self.message, "retryable": self.retryable}


@dataclass(frozen=True)
class ToolResult:
    """What a tool call returned: either `data` or an `error`, never both."""

    data: Any = None
    error: ToolError | None = None

    @property
    def ok(self) -> bool:
        return self.error is None


@dataclass(frozen=True)
class Draft:
    """A proposed planner change. Never applied by Python; Rust re-validates it (contract 2)."""

    op: str
    target: dict[str, Any] | None
    payload: dict[str, Any]
    evidence: tuple[str, ...]

    def to_dict(self) -> dict[str, Any]:
        return {
            "op": self.op,
            "target": self.target,
            "payload": self.payload,
            "evidence": list(self.evidence),
        }


@dataclass
class Usage:
    model_calls: int = 0
    tool_calls: int = 0
    prompt_tokens: int = 0
    completion_tokens: int = 0
    model_latency_ms: int = 0
    tool_latency_ms: int = 0
    wall_ms: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "modelCalls": self.model_calls,
            "toolCalls": self.tool_calls,
            "tokens": {"prompt": self.prompt_tokens, "completion": self.completion_tokens},
            "latencyMs": {"model": self.model_latency_ms, "tool": self.tool_latency_ms, "wall": self.wall_ms},
        }


@dataclass
class AgentResult:
    status: ResultStatus
    answer: str | None = None
    drafts: list[Draft] = field(default_factory=list)
    trace: list[dict[str, Any]] = field(default_factory=list)
    usage: Usage = field(default_factory=Usage)
    # Machine-readable reason for `cannot_complete` / `needs_clarification` (for benchmark scoring).
    stop_reason: str | None = None
    run_id: str = ""
    architecture_id: str = ""
    model: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "runId": self.run_id,
            "architectureId": self.architecture_id,
            "status": self.status,
            "answer": self.answer,
            "drafts": [d.to_dict() for d in self.drafts],
            "trace": list(self.trace),
            "usage": self.usage.to_dict(),
            "stopReason": self.stop_reason,
            "model": dict(self.model),
        }
