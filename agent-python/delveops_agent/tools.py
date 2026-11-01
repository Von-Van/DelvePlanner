"""Read-tool catalog and clients (overhaul plan, contract 1).

The catalog below mirrors the v0 table in docs/delveops/overhaul-plan.md. When piece C merges
`contracts/tools.v1.json`, `load_catalog` reads that file instead, so the agent and the Rust broker
share one definition. All tools are read-only; the agent has no write tool at all.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

from .types import ToolError, ToolResult


@dataclass(frozen=True)
class ToolSpec:
    name: str
    description: str
    # JSON-Schema-like description of arguments: {"type": "object", "properties": ..., "required": [...]}
    input_schema: dict[str, Any]

    def prompt_line(self) -> str:
        props = self.input_schema.get("properties", {})
        required = set(self.input_schema.get("required", []))
        args = ", ".join(f"{k}{'' if k in required else '?'}: {v.get('type', 'any')}" for k, v in props.items())
        return f"- {self.name}({args}): {self.description}"


def _obj(properties: dict[str, Any], required: list[str]) -> dict[str, Any]:
    return {"type": "object", "properties": properties, "required": required, "additionalProperties": False}


_DATE = {"type": "string", "description": "YYYY-MM-DD"}
_TZ = {"type": "string", "description": "IANA time zone"}

V0_CATALOG: tuple[ToolSpec, ...] = (
    ToolSpec(
        "get_day_context",
        "Events, scheduled tasks, task blocks, calendar busy times and free ranges for one day.",
        _obj({"date": _DATE, "timeZone": _TZ}, ["date", "timeZone"]),
    ),
    ToolSpec(
        "list_active_projects",
        "Active plans (projects) with id, revision, title, status, dates and milestone summaries.",
        _obj({"includePlanning": {"type": "boolean"}}, []),
    ),
    ToolSpec(
        "get_project_context",
        "One plan with its milestones, workstreams and tasks (ids and revisions included).",
        _obj({"planId": {"type": "string"}}, ["planId"]),
    ),
    ToolSpec(
        "search_tasks",
        "Find tasks by text, plan, status or due date. Returns ids, revisions, planId, milestoneId.",
        _obj(
            {
                "query": {"type": "string"},
                "planId": {"type": "string"},
                "status": {"type": "string"},
                "dueBefore": _DATE,
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
            },
            ["limit"],
        ),
    ),
    ToolSpec(
        "get_capacity",
        "Planned versus available hours for each day of a week.",
        _obj({"weekStart": _DATE, "timeZone": _TZ}, ["weekStart", "timeZone"]),
    ),
    ToolSpec(
        "get_recent_actions",
        "Recently applied proposals and failures.",
        _obj({"limit": {"type": "integer", "minimum": 1, "maximum": 20}}, ["limit"]),
    ),
)


def load_catalog(path: str | Path | None = None) -> tuple[ToolSpec, ...]:
    """Load tool specs from contracts/tools.v1.json if given and present, else the v0 table."""
    if path is None or not Path(path).is_file():
        return V0_CATALOG
    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    entries = raw.get("tools", raw) if isinstance(raw, dict) else raw
    specs = []
    for entry in entries:
        specs.append(
            ToolSpec(
                name=entry["name"],
                description=entry.get("description", ""),
                input_schema=entry.get("inputSchema") or entry.get("input_schema") or _obj({}, []),
            )
        )
    return tuple(specs)


_JSON_TYPES: dict[str, tuple[type, ...]] = {
    "string": (str,),
    "integer": (int,),
    "number": (int, float),
    "boolean": (bool,),
    "object": (dict,),
    "array": (list,),
}


def check_arguments(spec: ToolSpec, arguments: Any) -> ToolError | None:
    """Cheap local argument check so obviously bad calls don't spend tool budget.

    Rust validates again; this is a convenience, not the security boundary.
    """
    if not isinstance(arguments, dict):
        return ToolError("invalid_arguments", "arguments must be a JSON object")
    schema = spec.input_schema
    props: dict[str, Any] = schema.get("properties", {})
    for name in schema.get("required", []):
        if name not in arguments:
            return ToolError("invalid_arguments", f"missing required argument {name!r}")
    if schema.get("additionalProperties") is False:
        extra = sorted(set(arguments) - set(props))
        if extra:
            return ToolError("invalid_arguments", f"unknown argument(s): {', '.join(extra)}")
    for name, value in arguments.items():
        rule = props.get(name)
        if not rule:
            continue
        expected = _JSON_TYPES.get(rule.get("type", ""))
        if expected and (not isinstance(value, expected) or (isinstance(value, bool) and bool not in expected)):
            return ToolError("invalid_arguments", f"argument {name!r} must be {rule['type']}")
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            if "maximum" in rule and value > rule["maximum"]:
                return ToolError("invalid_arguments", f"argument {name!r} must be at most {rule['maximum']}")
            if "minimum" in rule and value < rule["minimum"]:
                return ToolError("invalid_arguments", f"argument {name!r} must be at least {rule['minimum']}")
    return None


class ToolClient(Protocol):
    """Runs one read tool. Implementations: `MockToolClient` here; the live client over the sidecar
    link arrives with pieces C and D.

    A call must return in bounded time: the agent cannot interrupt it. The live client relies on the
    Rust supervisor's per-call timeout (plan contract 3) and reports overruns as a `timeout` error.
    """

    def call(self, name: str, arguments: dict[str, Any]) -> ToolResult: ...


Handler = Callable[[dict[str, Any]], Any]


@dataclass
class MockToolClient:
    """Fixture-driven tool client for tests and the benchmark.

    Each entry in `responses` is a value (returned as data), a `ToolError`, a callable taking the
    arguments, or a list of those consumed one per call (the last one repeats).
    """

    responses: Mapping[str, Any]
    calls: list[tuple[str, dict[str, Any]]] = field(default_factory=list)

    def call(self, name: str, arguments: dict[str, Any]) -> ToolResult:
        self.calls.append((name, dict(arguments)))
        if name not in self.responses:
            return ToolResult(error=ToolError("unavailable", f"no fixture for tool {name}"))
        response = self.responses[name]
        if isinstance(response, list):
            index = sum(1 for called, _ in self.calls if called == name) - 1
            response = response[min(index, len(response) - 1)] if response else None
        if callable(response) and not isinstance(response, ToolError):
            response = response(arguments)
        if isinstance(response, ToolError):
            return ToolResult(error=response)
        if isinstance(response, ToolResult):
            return response
        return ToolResult(data=response)
