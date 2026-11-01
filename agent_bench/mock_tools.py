"""Fixture-driven mock of the read tools.

``MockToolClient`` answers the v0 read tools from an in-memory copy of a planner fixture. Nothing here
touches a real database. A case can inject errors, replace responses, and change state mid-run (to make a
record go stale between two reads). Every call is logged so scorers judge what the agent actually did,
not what it says it did.
"""

from __future__ import annotations

import copy
import json
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

from . import github_tools
from .jsonschema_lite import validate
from .trace import TraceRecorder

HERE = Path(__file__).parent
TOOLS_PATH = HERE / "schemas" / "tools.v0.json"
FIXTURES_DIR = HERE / "fixtures"

# A call to an unknown tool whose name starts like this is counted as an attempt to write.
WRITE_PREFIXES = ("create", "update", "delete", "remove", "schedule", "apply", "approve", "complete", "mark", "set", "move", "write", "send", "cancel", "reschedule")


def load_tool_catalog() -> dict:
    catalog = json.loads(TOOLS_PATH.read_text())
    catalog["tools"].update(copy.deepcopy(github_tools.CATALOG))  # piece M: read-only GitHub context
    return catalog


def load_fixture(version: str) -> dict:
    path = FIXTURES_DIR / f"{version}.json"
    return json.loads(path.read_text())


def parse_utc(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def _local_iso(moment: datetime, tz: ZoneInfo) -> str:
    return moment.astimezone(tz).isoformat(timespec="minutes")


# ---------------------------------------------------------------------------------------------------------
# Planner state


class PlannerState:
    """The fixture plus the queries the tools need. Times are computed with zoneinfo, so DST is real."""

    def __init__(self, fixture: dict):
        self.data = copy.deepcopy(fixture)

    # -- mutation (fixture overrides and mid-run state changes)

    def apply(self, change: dict) -> None:
        entity = change["entity"]
        rows = self.data.setdefault(entity, [])
        if "add" in change:
            rows.append(copy.deepcopy(change["add"]))
            return
        for row in rows:
            if row["id"] == change["id"]:
                row.update(copy.deepcopy(change["fields"]))
                return
        raise KeyError(f"{entity} {change['id']} not in fixture")

    def find(self, entity: str, record_id: str) -> dict | None:
        return next((r for r in self.data.get(entity, []) if r["id"] == record_id), None)

    # -- time helpers

    def intervals_for_day(self, day: date, tz: ZoneInfo) -> dict[str, list[dict]]:
        start = datetime.combine(day, datetime.min.time(), tz)
        end = datetime.combine(day + timedelta(days=1), datetime.min.time(), tz)

        def within(rows: list[dict]) -> list[dict]:
            out = []
            for row in rows:
                s = parse_utc(row["startAtUtc"])
                e = s + timedelta(minutes=row["durationMinutes"])
                if s < end and e > start:
                    out.append({**row, "_start": s, "_end": e})
            return sorted(out, key=lambda r: r["_start"])

        return {
            "events": within(self.data.get("events", [])),
            "taskBlocks": within(self.data.get("taskBlocks", [])),
            "externalBusy": within(self.data.get("externalBusy", [])),
        }

    def working_window(self, day: date, tz: ZoneInfo) -> tuple[datetime, datetime] | None:
        hours = self.data["workingHours"]
        if day.isoweekday() not in hours["days"]:
            return None
        sh, sm = map(int, hours["start"].split(":"))
        eh, em = map(int, hours["end"].split(":"))
        return (
            datetime(day.year, day.month, day.day, sh, sm, tzinfo=tz),
            datetime(day.year, day.month, day.day, eh, em, tzinfo=tz),
        )

    def busy_ranges(self, day: date, tz: ZoneInfo) -> list[tuple[datetime, datetime]]:
        found = self.intervals_for_day(day, tz)
        ranges = [(r["_start"], r["_end"]) for group in found.values() for r in group]
        ranges.sort()
        merged: list[tuple[datetime, datetime]] = []
        for s, e in ranges:
            if merged and s <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], e))
            else:
                merged.append((s, e))
        return merged

    def free_ranges(self, day: date, tz: ZoneInfo) -> list[tuple[datetime, datetime]]:
        window = self.working_window(day, tz)
        if window is None:
            return []
        ws, we = window
        free, cursor = [], ws
        for s, e in self.busy_ranges(day, tz):
            s, e = max(s, ws), min(e, we)
            if e <= s:
                continue
            if s > cursor:
                free.append((cursor, s))
            cursor = max(cursor, e)
        if cursor < we:
            free.append((cursor, we))
        return free


def _minutes(ranges: list[tuple[datetime, datetime]]) -> int:
    return int(sum((e - s).total_seconds() for s, e in ranges) // 60)


# ---------------------------------------------------------------------------------------------------------
# Tool implementations: pure functions of (state, args) -> data


class ToolError(Exception):
    def __init__(self, code: str, message: str, retryable: bool = False):
        super().__init__(message)
        self.code, self.message, self.retryable = code, message, retryable


def _zone(name: str) -> ZoneInfo:
    try:
        return ZoneInfo(name)
    except Exception as exc:  # noqa: BLE001 - any lookup failure is a bad argument
        raise ToolError("invalid_arguments", f"unknown time zone {name!r}") from exc


def _task_view(t: dict) -> dict:
    keys = ("id", "revision", "title", "status", "priority", "planId", "milestoneId", "dueDate", "scheduledDay", "estimatedMinutes", "waitingOn", "description")
    return {k: copy.deepcopy(t.get(k)) for k in keys}


def tool_get_day_context(state: PlannerState, args: dict) -> dict:
    tz = _zone(args["timeZone"])
    day = date.fromisoformat(args["date"])
    found = state.intervals_for_day(day, tz)
    day_start = datetime.combine(day, datetime.min.time(), tz)
    day_end = datetime.combine(day + timedelta(days=1), datetime.min.time(), tz)
    window = state.working_window(day, tz)
    return {
        "date": args["date"],
        "timeZone": args["timeZone"],
        "dayLengthMinutes": int((day_end.astimezone(timezone.utc) - day_start.astimezone(timezone.utc)).total_seconds() // 60),
        "workingHours": None if window is None else {"start": _local_iso(window[0], tz), "end": _local_iso(window[1], tz)},
        "events": [
            {"id": r["id"], "revision": r["revision"], "title": r["title"], "notes": r.get("notes", ""), "start": _local_iso(r["_start"], tz), "end": _local_iso(r["_end"], tz)}
            for r in found["events"]
        ],
        "taskBlocks": [
            {"id": r["id"], "revision": r["revision"], "taskId": r["taskId"], "start": _local_iso(r["_start"], tz), "end": _local_iso(r["_end"], tz)}
            for r in found["taskBlocks"]
        ],
        "externalBusy": [{"calendar": r["calendar"], "start": _local_iso(r["_start"], tz), "end": _local_iso(r["_end"], tz)} for r in found["externalBusy"]],
        "scheduledTasks": [_task_view(t) for t in state.data["tasks"] if t.get("scheduledDay") == args["date"]],
        "free": [{"start": _local_iso(s, tz), "end": _local_iso(e, tz)} for s, e in state.free_ranges(day, tz)],
    }


def tool_list_active_projects(state: PlannerState, args: dict) -> dict:
    allowed = {"active", "on_hold"} | ({"planning"} if args.get("includePlanning") else set())
    plans = []
    for p in state.data["plans"]:
        if p.get("archived") or p["status"] not in allowed:
            continue
        milestones = [
            {
                "id": m["id"],
                "revision": m["revision"],
                "title": m["title"],
                "status": m["status"],
                "targetDate": m.get("targetDate"),
                "openTasks": sum(1 for t in state.data["tasks"] if t.get("milestoneId") == m["id"] and t["status"] != "done"),
            }
            for m in state.data["milestones"]
            if m["planId"] == p["id"]
        ]
        plans.append({k: p.get(k) for k in ("id", "revision", "title", "status", "startDate", "targetDate")} | {"milestones": milestones})
    return {"plans": plans}


def tool_get_project_context(state: PlannerState, args: dict) -> dict:
    plan = state.find("plans", args["planId"])
    if plan is None:
        raise ToolError("not_found", f"no plan {args['planId']}")
    return {
        "plan": {k: plan.get(k) for k in ("id", "revision", "title", "description", "status", "startDate", "targetDate", "archived")},
        "milestones": [copy.deepcopy(m) for m in state.data["milestones"] if m["planId"] == plan["id"]],
        "workstreams": [copy.deepcopy(w) for w in state.data.get("workstreams", []) if w.get("planId") == plan["id"]],
        "tasks": [_task_view(t) for t in state.data["tasks"] if t.get("planId") == plan["id"]],
    }


def tool_search_tasks(state: PlannerState, args: dict) -> dict:
    words = [w for w in args.get("query", "").lower().split() if w]
    hits = []
    for t in state.data["tasks"]:
        text = f"{t['title']} {t.get('description', '')}".lower()
        if words and not all(w in text for w in words):
            continue
        if args.get("planId") and t.get("planId") != args["planId"]:
            continue
        if args.get("status") and t["status"] != args["status"]:
            continue
        if args.get("dueBefore") and not (t.get("dueDate") and t["dueDate"] < args["dueBefore"]):
            continue
        hits.append({k: t.get(k) for k in ("id", "revision", "title", "status", "planId", "milestoneId", "dueDate", "scheduledDay")})
    limit = args["limit"]
    return {"tasks": hits[:limit], "truncated": len(hits) > limit}


def tool_get_capacity(state: PlannerState, args: dict) -> dict:
    tz = _zone(args["timeZone"])
    start = date.fromisoformat(args["weekStart"])
    if start.isoweekday() != 1:
        raise ToolError("invalid_arguments", "weekStart must be a Monday")
    days = []
    for offset in range(7):
        day = start + timedelta(days=offset)
        window = state.working_window(day, tz)
        working = 0 if window is None else int((window[1] - window[0]).total_seconds() // 60)
        free = state.free_ranges(day, tz)
        available = _minutes(free)
        open_tasks = [t for t in state.data["tasks"] if t.get("scheduledDay") == day.isoformat() and t["status"] != "done"]
        days.append(
            {
                "day": day.isoformat(),
                "workingMinutes": working,
                "busyMinutes": working - available,
                "availableMinutes": available,
                "plannedMinutes": sum(t.get("estimatedMinutes") or 0 for t in open_tasks),
                "unestimatedTasks": sum(1 for t in open_tasks if t.get("estimatedMinutes") is None),
                "blockedMinutes": 0,
                "free": [{"start": _local_iso(s, tz), "end": _local_iso(e, tz)} for s, e in free],
            }
        )
    hours = state.data["workingHours"]
    return {
        "workingHours": hours,
        "plannedLimitMinutes": None,
        "days": days,
        "pooledMinutes": 0,
        "pooledUnestimatedTasks": 0,
        "calendarsIncomplete": False,
    }


def tool_get_recent_actions(state: PlannerState, args: dict) -> dict:
    return {"actions": copy.deepcopy(state.data.get("recentActions", []))[: args["limit"]]}


TOOL_IMPLS = {
    "get_day_context": tool_get_day_context,
    "list_active_projects": tool_list_active_projects,
    "get_project_context": tool_get_project_context,
    "search_tasks": tool_search_tasks,
    "get_capacity": tool_get_capacity,
    "get_recent_actions": tool_get_recent_actions,
}


def _github_tool(fn):
    """Adapts a GitHub mock tool (piece M) to the (state, args) signature and this module's errors."""

    def run(state: PlannerState, args: dict) -> dict:
        try:
            return fn(state.data, args)
        except github_tools.GitHubToolError as err:
            raise ToolError(err.code, err.message, err.retryable) from err

    return run


TOOL_IMPLS.update({name: _github_tool(fn) for name, fn in github_tools.TOOL_IMPLS.items()})


def collect_records(value: Any, out: dict[str, int | None] | None = None) -> dict[str, int | None]:
    """Every ``{"id": ..., "revision": ...}`` found anywhere in a tool result."""
    out = {} if out is None else out
    if isinstance(value, dict):
        if isinstance(value.get("id"), str):
            out[value["id"]] = value.get("revision")
        for item in value.values():
            collect_records(item, out)
    elif isinstance(value, list):
        for item in value:
            collect_records(item, out)
    return out


# ---------------------------------------------------------------------------------------------------------
# The client the agent talks to


@dataclass
class CallRecord:
    seq: int
    tool: str
    args: Any
    ok: bool
    error_code: str | None
    injected: bool
    args_valid: bool
    permitted: bool
    known: bool
    records: dict[str, int | None] = field(default_factory=dict)
    data: Any = None

    def to_json(self, debug: bool = False) -> dict:
        out = {
            "seq": self.seq,
            "tool": self.tool,
            "args": self.args,
            "ok": self.ok,
            "errorCode": self.error_code,
            "injected": self.injected,
            "argsValid": self.args_valid,
            "permitted": self.permitted,
            "known": self.known,
            "records": self.records,
        }
        if debug:
            out["data"] = self.data
        return out


def _matches(rule: dict, tool: str, args: Any, nth: int) -> bool:
    if rule["tool"] != tool:
        return False
    on = rule.get("onCalls", "all")
    if on != "all" and nth not in on:
        return False
    want = rule.get("match") or {}
    return isinstance(args, dict) and all(args.get(k) == v for k, v in want.items())


class MockToolClient:
    def __init__(self, case: dict, trace: TraceRecorder | None = None, max_tool_calls: int | None = None):
        self.case = case
        self.catalog = load_tool_catalog()
        self.state = PlannerState(load_fixture(case["fixture"]["version"]))
        self.state.data.update(github_tools.state_entries(github_tools.load_github_fixture()))
        for change in case["fixture"].get("overrides", []):
            self.state.apply(change)
        self.permitted = set(case["permittedTools"])
        self.trace = trace
        self.max_tool_calls = max_tool_calls
        self.calls: list[CallRecord] = []
        self._per_tool: dict[str, int] = {}
        self._pending_changes = sorted(case.get("stateChanges", []), key=lambda c: c["afterToolCall"])

    def list_tools(self) -> list[dict]:
        return [
            {"name": name, "description": spec["description"], "inputSchema": spec["input"]}
            for name, spec in self.catalog["tools"].items()
            if name in self.permitted
        ]

    def _error(self, code: str, message: str, retryable: bool = False) -> dict:
        return {"ok": False, "error": {"code": code, "message": message, "retryable": retryable}}

    def call(self, name: str, arguments: dict | None = None) -> dict:
        started = time.perf_counter()
        args = {} if arguments is None else arguments
        seq = len(self.calls) + 1
        nth = self._per_tool[name] = self._per_tool.get(name, 0) + 1
        spec = self.catalog["tools"].get(name)
        known, permitted = spec is not None, name in self.permitted
        args_valid = known and isinstance(args, dict) and not validate(args, spec["input"])
        injected = False
        if self.trace:
            self.trace.emit("tool_call", f"{name} (call {nth})", {"tool": name, "args": args, "callSeq": seq})

        if self.max_tool_calls is not None and seq > self.max_tool_calls:
            response = self._error("budget_exhausted", "tool call budget used up")
        elif not known:
            response = self._error("permission_denied", f"{name} is not an allowed tool")
        elif not permitted:
            response = self._error("permission_denied", f"{name} is not permitted for this task")
        elif not args_valid:
            problems = validate(args, spec["input"]) if isinstance(args, dict) else ["arguments must be an object"]
            response = self._error("invalid_arguments", "; ".join(problems))
        else:
            response = None
            for rule in self.case.get("injectedErrors", []):
                if _matches(rule, name, args, nth):
                    response, injected = self._error(**rule["error"]), True
                    break
            if response is None:
                for rule in self.case.get("injectedToolResponses", []):
                    if _matches(rule, name, args, nth):
                        response, injected = copy.deepcopy(rule["response"]), True
                        break
            if response is None:
                try:
                    response = {"ok": True, "data": TOOL_IMPLS[name](self.state, args)}
                except ToolError as err:
                    response = self._error(err.code, err.message, err.retryable)

        ok = bool(response.get("ok"))
        record = CallRecord(
            seq=seq,
            tool=name,
            args=copy.deepcopy(args),
            ok=ok,
            error_code=None if ok else response["error"]["code"],
            injected=injected,
            args_valid=args_valid,
            permitted=known and permitted,
            known=known,
            records=collect_records(response.get("data")) if ok else {},
            data=copy.deepcopy(response.get("data")),
        )
        self.calls.append(record)
        if self.trace:
            summary = f"{name}: ok, {len(record.records)} records" if ok else f"{name}: {record.error_code}"
            data = {"tool": name, "callSeq": seq, "ok": ok, "errorCode": record.error_code, "injected": injected, "records": record.records}
            if self.trace.debug:
                data["result"] = response
            self.trace.emit("tool_result", summary, data, duration_ms=(time.perf_counter() - started) * 1000)

        while self._pending_changes and self._pending_changes[0]["afterToolCall"] <= seq:
            for change in self._pending_changes.pop(0)["changes"]:
                self.state.apply(change)
        return copy.deepcopy(response)

    # -- facts for scorers

    def current_revision(self, record_id: str) -> int | None:
        for entity in ("tasks", "plans", "milestones", "events", "taskBlocks"):
            row = self.state.find(entity, record_id)
            if row is not None:
                return row.get("revision")
        return None
