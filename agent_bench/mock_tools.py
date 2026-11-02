"""Fixture-driven mock of the read tools in ``contracts/tools.v1.json``.

``MockToolClient`` answers the v1 read tools from an in-memory copy of a planner fixture, with the
same input schemas, output shapes, result envelope, error codes and budget rules as the Rust tool
broker (``src-tauri/src/tools.rs``). Nothing here touches a real database. A case can inject errors,
replace responses, and change state mid-run (to make a record go stale between two reads). Every call
is logged so scorers judge what the agent actually did, not what it says it did.

``tests/test_parity.py`` runs the same calls through the real Rust ``tool_server`` example when it has
been built, and fails if the two disagree.
"""

from __future__ import annotations

import copy
import json
import re
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
CONTRACT_PATH = HERE.parent / "contracts" / "tools.v1.json"
FIXTURES_DIR = HERE / "fixtures"

# A call to an unknown tool whose name starts like this is counted as an attempt to write.
WRITE_PREFIXES = ("create", "update", "delete", "remove", "schedule", "apply", "approve", "complete", "mark", "set", "move", "write", "send", "cancel", "reschedule")

# Caps the Rust broker applies (src-tauri/src/tools/read.rs).
MAX_DAY_ITEMS = 100
MAX_PROJECTS = 50
MAX_MILESTONES_PER_PROJECT = 20
MAX_PROJECT_TASKS = 200
MAX_QUERY_LENGTH = 200


def load_contract() -> dict:
    return json.loads(CONTRACT_PATH.read_text())


def load_tool_catalog() -> dict:
    """Tool specs by name: ``{name: {description, inputSchema, outputSchema, implemented, ...}}``."""
    return {t["name"]: t for t in load_contract()["tools"]}


def tool_schema(contract: dict, schema: dict) -> dict:
    """A tool's input or output schema, with the contract's ``$defs`` attached so ``$ref`` resolves."""
    return {**schema, "$defs": contract["$defs"]}


def load_fixture(version: str) -> dict:
    return json.loads((FIXTURES_DIR / f"{version}.json").read_text())


def parse_utc(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def utc_iso(moment: datetime) -> str:
    return moment.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def is_uuid(value: Any) -> bool:
    import uuid

    try:
        uuid.UUID(str(value))
        return True
    except ValueError:
        return False


DEFAULT_WORKING_HOURS = {"days": [1, 2, 3, 4, 5], "startMinute": 540, "endMinute": 1020, "revision": 1, "updatedAt": "2026-10-20T15:00:00.000Z"}


# ---------------------------------------------------------------------------------------------------------
# Planner state


class PlannerState:
    """The fixture's planner export plus calendar busy times. Times use zoneinfo, so DST is real."""

    ENTITIES = ("plans", "milestones", "workstreams", "tasks", "events", "taskBlocks")

    def __init__(self, fixture: dict):
        self.bundle = copy.deepcopy(fixture["bundle"])
        self.external_busy = copy.deepcopy(fixture.get("externalBusy", []))
        self.working_hours = copy.deepcopy(fixture.get("workingHours", DEFAULT_WORKING_HOURS))

    def rows(self, entity: str) -> list[dict]:
        if entity == "externalBusy":
            return self.external_busy
        return self.bundle.setdefault(entity, [])

    def apply(self, change: dict) -> None:
        rows = self.rows(change["entity"])
        if "add" in change:
            rows.append(copy.deepcopy(change["add"]))
            return
        for row in rows:
            if row.get("id") == change["id"]:
                row.update(copy.deepcopy(change["fields"]))
                return
        raise KeyError(f"{change['entity']} {change['id']} not in fixture")

    def find(self, entity: str, record_id: str) -> dict | None:
        return next((r for r in self.rows(entity) if r.get("id") == record_id), None)

    # -- time helpers

    @staticmethod
    def day_bounds(day: date, tz: ZoneInfo) -> tuple[datetime, datetime]:
        return datetime.combine(day, datetime.min.time(), tz), datetime.combine(day + timedelta(days=1), datetime.min.time(), tz)

    def timed(self, entity: str, day: date, tz: ZoneInfo) -> list[tuple[datetime, datetime, dict]]:
        start, end = self.day_bounds(day, tz)
        out = []
        for row in self.rows(entity):
            s = parse_utc(row["startAtUtc"])
            e = parse_utc(row["endAtUtc"]) if "endAtUtc" in row else s + timedelta(minutes=row["durationMinutes"])
            if s < end and e > start:
                out.append((s, e, row))
        return sorted(out, key=lambda item: (item[0], item[2].get("id", "")))

    def working_window(self, day: date, tz: ZoneInfo) -> tuple[datetime, datetime] | None:
        hours = self.working_hours
        if day.isoweekday() not in hours["days"]:
            return None
        midnight = datetime.combine(day, datetime.min.time(), tz)
        return (
            (midnight.replace(tzinfo=None) + timedelta(minutes=hours["startMinute"])).replace(tzinfo=tz),
            (midnight.replace(tzinfo=None) + timedelta(minutes=hours["endMinute"])).replace(tzinfo=tz),
        )

    @staticmethod
    def _merge(ranges: list[tuple[datetime, datetime]]) -> list[tuple[datetime, datetime]]:
        merged: list[tuple[datetime, datetime]] = []
        for s, e in sorted(ranges):
            if merged and s <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], e))
            else:
                merged.append((s, e))
        return merged

    @staticmethod
    def _clip(ranges, window):
        ws, we = window
        return [(max(s, ws), min(e, we)) for s, e in ranges if min(e, we) > max(s, ws)]

    @staticmethod
    def _minutes(ranges) -> int:
        return int(sum((e - s).total_seconds() for s, e in ranges) // 60)

    def day_capacity(self, day: date, tz: ZoneInfo) -> dict:
        """Mirrors src-tauri/src/capacity.rs: events and calendar busy time are busy; time blocks are
        blocked; available is working minus busy; free is working time with neither."""
        label = day.isoformat()
        open_tasks = [t for t in self.rows("tasks") if t.get("scheduledDay") == label and t["status"] != "done"]
        planned = sum(t.get("estimatedMinutes") or 0 for t in open_tasks)
        unestimated = sum(1 for t in open_tasks if t.get("estimatedMinutes") is None)
        window = self.working_window(day, tz)
        if window is None:
            return {"day": label, "workingMinutes": 0, "busyMinutes": 0, "availableMinutes": 0, "plannedMinutes": planned,
                    "unestimatedTasks": unestimated, "blockedMinutes": 0, "free": []}
        busy = self._clip(self._merge([(s, e) for ent in ("events", "externalBusy") for s, e, _ in self.timed(ent, day, tz)]), window)
        blocked = self._clip(self._merge([(s, e) for s, e, _ in self.timed("taskBlocks", day, tz)]), window)
        occupied = self._merge(busy + blocked)
        free, cursor = [], window[0]
        for s, e in occupied:
            if s > cursor:
                free.append((cursor, s))
            cursor = max(cursor, e)
        if cursor < window[1]:
            free.append((cursor, window[1]))
        working = self._minutes([window])
        busy_minutes = self._minutes(busy)
        return {
            "day": label,
            "workingMinutes": working,
            "busyMinutes": busy_minutes,
            "availableMinutes": max(0, working - busy_minutes),
            "plannedMinutes": planned,
            "unestimatedTasks": unestimated,
            "blockedMinutes": self._minutes(blocked),
            "free": [{"startAtUtc": utc_iso(s), "endAtUtc": utc_iso(e)} for s, e in free],
        }

    def free_ranges(self, day: date, tz: ZoneInfo) -> list[tuple[datetime, datetime]]:
        return [(parse_utc(r["startAtUtc"]), parse_utc(r["endAtUtc"])) for r in self.day_capacity(day, tz)["free"]]


# ---------------------------------------------------------------------------------------------------------
# Views: the record shapes in the contract's $defs. Descriptions and notes only when the run allows them.


class ToolError(Exception):
    def __init__(self, code: str, message: str, retryable: bool | None = None):
        super().__init__(message)
        self.code, self.message, self.retryable = code, message, retryable


def _text(value: str | None, privacy: dict) -> dict:
    return {"description": value} if privacy.get("includeDescriptions") and value else {}


def task_view(t: dict, privacy: dict) -> dict:
    keys = ("id", "revision", "title", "status", "priority", "planId", "milestoneId", "workstreamId", "dueDate", "scheduledDay", "plannedWeek", "estimatedMinutes")
    view = {k: copy.deepcopy(t.get(k)) for k in keys}
    view["waitingOn"] = list(t.get("waitingOn", []))
    view["recurring"] = t.get("recurrence") is not None
    view["completedAt"] = t.get("completedAt")
    return view | _text(t.get("description"), privacy)


def event_view(e: dict, privacy: dict) -> dict:
    start = parse_utc(e["startAtUtc"])
    view = {
        "id": e["id"],
        "revision": e["revision"],
        "title": e["title"],
        "startAtUtc": e["startAtUtc"],
        "endAtUtc": utc_iso(start + timedelta(minutes=e["durationMinutes"])),
        "timeZone": e["timeZone"],
        "durationMinutes": e["durationMinutes"],
        "planId": e.get("planId"),
        "workstreamId": e.get("workstreamId"),
        "location": e.get("location", ""),
    }
    if privacy.get("includeDescriptions") and e.get("notes"):
        view["notes"] = e["notes"]
    return view


def milestone_view(m: dict, privacy: dict) -> dict:
    keys = ("id", "revision", "planId", "title", "status", "targetDate", "workstreamId")
    return {k: m.get(k) for k in keys} | _text(m.get("description"), privacy)


def plan_view(p: dict, privacy: dict) -> dict:
    keys = ("id", "revision", "title", "status", "startDate", "targetDate", "archived")
    return {k: p.get(k) for k in keys} | _text(p.get("description"), privacy)


def workstream_view(w: dict, privacy: dict) -> dict:
    return {"id": w["id"], "revision": w["revision"], "name": w["name"]} | _text(w.get("description"), privacy)


def block_view(b: dict, state: PlannerState) -> dict:
    task = state.find("tasks", b["taskId"]) or {}
    start = parse_utc(b["startAtUtc"])
    return {
        "id": b["id"],
        "revision": b["revision"],
        "taskId": b["taskId"],
        "taskTitle": task.get("title", ""),
        "startAtUtc": b["startAtUtc"],
        "endAtUtc": utc_iso(start + timedelta(minutes=b["durationMinutes"])),
        "timeZone": b["timeZone"],
        "durationMinutes": b["durationMinutes"],
    }


def busy_view(b: dict) -> dict:
    return {"allDay": False, "startAtUtc": b["startAtUtc"], "endAtUtc": b["endAtUtc"], "startDate": None, "endDate": None, "tentative": False}


def _capped(items: list, limit: int) -> tuple[list, bool]:
    return items[:limit], len(items) > limit


# ---------------------------------------------------------------------------------------------------------
# Tool implementations: (state, args, session) -> result


@dataclass
class Session:
    now: datetime
    time_zone: str
    privacy: dict

    @property
    def today(self) -> str:
        return self.now.astimezone(ZoneInfo(self.time_zone)).date().isoformat()


def _zone(name: str) -> ZoneInfo:
    try:
        return ZoneInfo(name)
    except Exception as exc:  # noqa: BLE001 - any lookup failure is a bad argument
        raise ToolError("invalid_arguments", f"Unknown time zone '{name}'.") from exc


def _day(value: str) -> date:
    try:
        return date.fromisoformat(value)
    except ValueError as exc:
        raise ToolError("invalid_arguments", f"'{value}' is not a YYYY-MM-DD day.") from exc


def _sort_tasks(tasks: list[dict]) -> list[dict]:
    return sorted(tasks, key=lambda t: (t.get("sortOrder", 0), t["id"]))


def _done_last(tasks: list[dict]) -> list[dict]:
    return [t for t in tasks if t["status"] != "done"] + [t for t in tasks if t["status"] == "done"]


def tool_get_day_context(state: PlannerState, args: dict, s: Session) -> dict:
    tz, day = _zone(args["timeZone"]), _day(args["date"])
    p = s.privacy
    truncated = False

    def cap(items):
        nonlocal truncated
        kept, cut = _capped(items, MAX_DAY_ITEMS)
        truncated |= cut
        return kept

    capacity = state.day_capacity(day, tz)
    tasks = _sort_tasks(state.rows("tasks"))
    return {
        "date": args["date"],
        "timeZone": args["timeZone"],
        "events": [event_view(r, p) for r in cap([r for _, _, r in state.timed("events", day, tz)])],
        "scheduledTasks": [task_view(t, p) for t in cap(_done_last([t for t in tasks if t.get("scheduledDay") == args["date"]]))],
        # A task scheduled on its own due day is listed once, under scheduledTasks.
        "dueTasks": [task_view(t, p) for t in cap(_done_last([t for t in tasks if t.get("dueDate") == args["date"] and t.get("scheduledDay") != t.get("dueDate")]))],
        "milestones": [milestone_view(m, p) for m in cap([m for m in state.rows("milestones") if m.get("targetDate") == args["date"]])],
        "taskBlocks": [block_view(b, state) for b in cap([r for _, _, r in state.timed("taskBlocks", day, tz)])],
        "externalBusy": [busy_view(b) for b in cap([r for _, _, r in state.timed("externalBusy", day, tz)])],
        "calendarsIncomplete": False,
        "workingMinutes": capacity["workingMinutes"],
        "availableMinutes": capacity["availableMinutes"],
        "plannedMinutes": capacity["plannedMinutes"],
        "free": capacity["free"],
        "truncated": truncated,
    }


def tool_list_active_projects(state: PlannerState, args: dict, s: Session) -> dict:
    wanted = [
        p
        for p in state.rows("plans")
        if not p.get("archived") and (p["status"] == "active" or (args.get("includePlanning") and p["status"] == "planning"))
    ]
    kept, truncated = _capped(wanted, MAX_PROJECTS)
    plans = []
    for p in kept:
        tasks = [t for t in state.rows("tasks") if t.get("planId") == p["id"]]
        milestones, ms_cut = _capped(sorted((m for m in state.rows("milestones") if m["planId"] == p["id"]), key=lambda m: m.get("sortOrder", 0)), MAX_MILESTONES_PER_PROJECT)
        plans.append(
            {k: p.get(k) for k in ("id", "revision", "title", "status", "startDate", "targetDate")}
            | {
                "taskCount": len(tasks),
                "openTaskCount": sum(1 for t in tasks if t["status"] != "done"),
                "overdueTaskCount": sum(1 for t in tasks if t["status"] != "done" and t.get("dueDate") and t["dueDate"] < s.today),
                "milestones": [milestone_view(m, s.privacy) for m in milestones],
                "milestonesTruncated": ms_cut,
            }
        )
    return {"asOf": s.today, "plans": plans, "truncated": truncated}


def tool_get_project_context(state: PlannerState, args: dict, s: Session) -> dict:
    plan_id = args["planId"].strip()
    if not is_uuid(plan_id):
        raise ToolError("invalid_arguments", "planId must be a plan id.")
    plan = state.find("plans", plan_id)
    if plan is None:
        raise ToolError("not_found", "No plan has that id.")
    tasks = _sort_tasks([t for t in state.rows("tasks") if t.get("planId") == plan_id])
    tasks = [t for t in tasks if t["status"] != "done"] + [t for t in tasks if t["status"] == "done"]
    kept, truncated = _capped(tasks, MAX_PROJECT_TASKS)
    return {
        "plan": plan_view(plan, s.privacy),
        "milestones": [milestone_view(m, s.privacy) for m in sorted((m for m in state.rows("milestones") if m["planId"] == plan_id), key=lambda m: m.get("sortOrder", 0))],
        "workstreams": [workstream_view(w, s.privacy) for w in state.rows("workstreams") if w.get("planId") == plan_id],
        "tasks": [task_view(t, s.privacy) for t in kept],
        "truncated": truncated,
    }


def tool_search_tasks(state: PlannerState, args: dict, s: Session) -> dict:
    limit = args.get("limit", 20)
    query = args.get("query")
    if query is not None and len(query) > MAX_QUERY_LENGTH:
        raise ToolError("invalid_arguments", f"query can be at most {MAX_QUERY_LENGTH} characters.")
    if args.get("planId") is not None and not is_uuid(args["planId"]):
        raise ToolError("invalid_arguments", "planId must be a plan id.")
    due_before = _day(args["dueBefore"]).isoformat() if args.get("dueBefore") else None
    terms = [w.lower() for w in (query or "").split()]
    hits = [
        t
        for t in _sort_tasks(state.rows("tasks"))
        if all(term in t["title"].lower() for term in terms)
        and (not args.get("planId") or t.get("planId") == args["planId"])
        and (not args.get("status") or t["status"] == args["status"])
        and (due_before is None or (t.get("dueDate") is not None and t["dueDate"] < due_before))
    ]
    # Open work first, then by due date with undated tasks last, then by title.
    hits.sort(key=lambda t: (t["status"] == "done", t.get("dueDate") is None, t.get("dueDate") or "", t["title"], t["id"]))
    return {"tasks": [task_view(t, s.privacy) for t in hits[:limit]], "totalMatches": len(hits), "truncated": len(hits) > limit}


def tool_get_capacity(state: PlannerState, args: dict, s: Session) -> dict:
    tz, start = _zone(args["timeZone"]), _day(args["weekStart"])
    first, end = start.isoformat(), (start + timedelta(days=7)).isoformat()
    # Open tasks chosen for a week that starts in the window, with no day yet (db/availability.rs).
    pooled = [t for t in state.rows("tasks") if t["status"] != "done" and t.get("scheduledDay") is None and t.get("plannedWeek") and first <= t["plannedWeek"] < end]
    return {
        "workingHours": copy.deepcopy(state.working_hours),
        "plannedLimitMinutes": None,
        "days": [state.day_capacity(start + timedelta(days=i), tz) for i in range(7)],
        "pooledMinutes": sum(t.get("estimatedMinutes") or 0 for t in pooled),
        "pooledUnestimatedTasks": sum(1 for t in pooled if t.get("estimatedMinutes") is None),
        "calendarsIncomplete": False,
    }


def tool_get_recent_actions(state: PlannerState, args: dict, s: Session) -> dict:
    return {"actions": [], "historyAvailable": False}


# get_memory mirrors src-tauri/src/tools/memory.rs: same filters, same ranking, same word rules.
MEMORY_STOPWORDS = frozenset(
    "the and for with that this was were are you your our has have had from into about what when which who how did does "
    "can could should would will just also then than there their them they its move make please again last time".split()
)


def _memory_words(text: str) -> list[str]:
    return [w.lower() for w in re.split(r"[^0-9A-Za-z\u00C0-\uFFFF]+", text) if w]


def _memory_stem(word: str) -> str:
    if word.endswith("s") and len(word) - 1 >= 3 and not word[:-1].endswith("s"):
        return word[:-1]
    return word


def memory_query_terms(text: str) -> list[str]:
    return sorted({_memory_stem(w) for w in _memory_words(text) if len(w) >= 3 and w not in MEMORY_STOPWORDS})


def _memory_relevance(terms: list[str], content: str) -> float:
    if not terms:
        return 1.0
    found = {_memory_stem(w) for w in _memory_words(content)}
    return round(sum(1 for t in terms if t in found) / len(terms), 3)


def memory_trust(record: dict) -> str:
    """How far a memory can be relied on, as the Rust tool labels it."""
    if record.get("confirmation") != "confirmed":
        return "inferred"
    return "verified" if record.get("source") == "approved_action" else "user_confirmed"


def tool_get_memory(state: PlannerState, args: dict, s: Session) -> dict:
    memory = getattr(state, "memory", None) or {"records": []}
    if len(args.get("query", "")) > 200:
        raise ToolError("invalid_arguments", "query can be at most 200 characters")
    if args.get("planId") is not None and not is_uuid(args["planId"]):
        raise ToolError("invalid_arguments", "planId must be a plan id.")
    if not memory.get("enabled", True):
        return {"memoryEnabled": False, "records": [], "totalMatches": 0, "truncated": False}
    now = s.now
    disabled = set(memory.get("disabledPlanIds", []))
    kinds = set(args.get("kinds") or [])
    terms = memory_query_terms(args.get("query", ""))
    views = []
    for record in memory.get("records", []):
        if record.get("supersededBy"):
            continue
        if record.get("expiresAt") and parse_utc(record["expiresAt"]) <= now:
            continue
        plan = record.get("planId")
        if plan and plan in disabled:
            continue
        if args.get("planId") and plan and plan != args["planId"]:
            continue
        if kinds and record["kind"] not in kinds:
            continue
        relevance = _memory_relevance(terms, record["content"])
        if terms and relevance <= 0:
            continue
        view = {k: record.get(k) for k in ("id", "kind", "content", "source", "confirmation", "planId")}
        view["trust"] = memory_trust(record)
        view["recordedAt"] = record["recordedAt"]
        if record.get("expiresAt"):
            view["expiresAt"] = record["expiresAt"]
        view["relevance"] = relevance
        views.append(view)
    views.sort(key=lambda v: v["id"])
    views.sort(key=lambda v: parse_utc(v["recordedAt"]), reverse=True)
    views.sort(key=lambda v: (-v["relevance"], v["confirmation"] != "confirmed"))
    limit = args.get("limit", 8)
    return {"memoryEnabled": True, "records": views[:limit], "totalMatches": len(views), "truncated": len(views) > limit}


TOOL_IMPLS = {
    "get_day_context": tool_get_day_context,
    "list_active_projects": tool_list_active_projects,
    "get_project_context": tool_get_project_context,
    "search_tasks": tool_search_tasks,
    "get_capacity": tool_get_capacity,
    "get_recent_actions": tool_get_recent_actions,
    "get_memory": tool_get_memory,
}


def _github_tool(fn):
    """Adapts a GitHub mock tool (piece M) to this module's signature and errors."""

    def run(state: PlannerState, args: dict, s: Session) -> dict:
        try:
            return fn(state.github, args)
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
    """Read-only tools for one case. ``call`` returns the contract's ``toolResult`` envelope."""

    def __init__(self, case: dict, trace: TraceRecorder | None = None, max_tool_calls: int | None = None):
        self.case = case
        self.contract = load_contract()
        self.catalog = {t["name"]: t for t in self.contract["tools"]}
        self.retryable = {e["code"]: e["retryable"] for e in self.contract["errorCodes"]}
        self.state = PlannerState(load_fixture(case["fixture"]["version"]))
        self.state.github = github_tools.state_entries(github_tools.load_github_fixture())
        for change in case["fixture"].get("overrides", []):
            self.state.apply(change)
        self.state.memory = copy.deepcopy(case.get("memory"))
        self.session = Session(now=datetime.fromisoformat(case["now"]), time_zone=case["timeZone"], privacy=dict(case.get("privacy", {})))
        self.permitted = set(case["permittedTools"])
        budget = dict(self.contract["budgets"]["default"])
        if max_tool_calls is not None:
            budget["maxToolCalls"] = max_tool_calls
        ceiling = self.contract["budgets"]["ceiling"]
        self.budget = {k: max(1, min(v, ceiling[k])) for k, v in budget.items()}
        self.trace = trace
        self.calls: list[CallRecord] = []
        self.calls_made = 0
        self._failures: dict[str, tuple[str, int]] = {}
        self._per_tool: dict[str, int] = {}
        self._pending_changes = sorted(case.get("stateChanges", []), key=lambda c: c["afterToolCall"])

    def list_tools(self) -> list[dict]:
        return [
            {"name": name, "description": spec["description"], "inputSchema": tool_schema(self.contract, spec["inputSchema"])}
            for name, spec in self.catalog.items()
            if name in self.permitted and spec["implemented"]
        ]

    def _err(self, code: str, message: str, retryable: bool | None = None) -> ToolError:
        return ToolError(code, message, self.retryable[code] if retryable is None else retryable)

    def _admit(self, name: str, fingerprint: str) -> dict:
        """The broker's checks, in its order. Refused calls don't use up the call budget."""
        spec = self.catalog.get(name)
        if spec is None:
            raise self._err("invalid_arguments", f"There is no tool named '{name}'.")
        if name not in self.permitted:
            raise self._err("permission_denied", f"This run may not use {name}.")
        if not spec["implemented"]:
            raise self._err("unavailable", f"{name} is reserved and not available yet.")
        if name in github_tools.TOOL_IMPLS and not github_tools.connected(self.state.github):
            raise self._err("permission_denied", "GitHub context is off: the user has not selected any repositories.")
        if self.calls_made >= self.budget["maxToolCalls"]:
            raise self._err("budget_exhausted", f"This run has used all {self.budget['maxToolCalls']} of its tool calls.")
        failed = self._failures.get(fingerprint)
        if failed and failed[1] >= self.budget["maxIdenticalFailures"]:
            raise self._err("budget_exhausted", f"This exact call already failed {failed[1]} times with {failed[0]}. Change the arguments or stop.")
        return spec

    def call(self, name: str, arguments: dict | None = None) -> dict:
        started = time.perf_counter()
        args = {} if arguments is None else arguments
        seq = len(self.calls) + 1
        nth = self._per_tool[name] = self._per_tool.get(name, 0) + 1
        spec = self.catalog.get(name)
        input_schema = tool_schema(self.contract, spec["inputSchema"]) if spec else None
        args_valid = spec is not None and isinstance(args, dict) and not validate(args, input_schema)
        fingerprint = f"{name}\0{json.dumps(args, sort_keys=True, separators=(',', ':'), default=str)}"
        injected = False
        if self.trace:
            self.trace.emit("tool_call", f"{name} (call {nth})", {"tool": name, "args": args, "callSeq": seq})

        result: Any = None
        error: ToolError | None = None
        try:
            self._admit(name, fingerprint)
            self.calls_made += 1
            if not args_valid:
                problems = validate(args, input_schema) if isinstance(args, dict) else ["arguments must be an object"]
                raise self._err("invalid_arguments", "; ".join(problems))
            for rule in self.case.get("injectedErrors", []):
                if _matches(rule, name, args, nth):
                    injected = True
                    raise self._err(**rule["error"])
            for rule in self.case.get("injectedToolResponses", []):
                if _matches(rule, name, args, nth):
                    injected = True
                    resp = copy.deepcopy(rule["response"])
                    if not resp.get("ok"):
                        raise self._err(**resp["error"])
                    result = resp["result"]
                    break
            if result is None:
                result = TOOL_IMPLS[name](self.state, args, self.session)
            size = len(json.dumps(result, separators=(",", ":")).encode())
            if size > self.budget["maxResultBytes"]:
                raise self._err("budget_exhausted", f"The result is {size} bytes, over the {self.budget['maxResultBytes']} byte limit. Narrow the request.")
        except ToolError as err:
            error, result = err, None

        duration_ms = int((time.perf_counter() - started) * 1000)
        remaining = max(0, self.budget["maxToolCalls"] - self.calls_made)
        if error is None:
            self._failures.pop(fingerprint, None)
            envelope = {"callId": f"call-{seq}", "tool": name, "ok": True, "result": result, "durationMs": duration_ms,
                        "resultBytes": len(json.dumps(result, separators=(",", ":")).encode()), "callsRemaining": remaining}
        else:
            if error.code not in ("budget_exhausted", "permission_denied"):
                code, count = self._failures.get(fingerprint, (error.code, 0))
                self._failures[fingerprint] = (error.code, count + 1 if code == error.code else 1)
            envelope = {"callId": f"call-{seq}", "tool": name, "ok": False,
                        "error": {"code": error.code, "message": error.message, "retryable": bool(error.retryable)},
                        "durationMs": duration_ms, "callsRemaining": remaining}

        ok = error is None
        record = CallRecord(
            seq=seq,
            tool=name,
            args=copy.deepcopy(args),
            ok=ok,
            error_code=None if ok else error.code,
            injected=injected,
            args_valid=args_valid,
            permitted=spec is not None and name in self.permitted,
            known=spec is not None,
            records=collect_records(result) if ok else {},
            data=copy.deepcopy(result),
        )
        self.calls.append(record)
        if self.trace:
            summary = f"{name}: ok, {len(record.records)} records" if ok else f"{name}: {record.error_code}"
            data = {"tool": name, "callSeq": seq, "ok": ok, "errorCode": record.error_code, "injected": injected, "records": record.records}
            if self.trace.debug:
                data["result"] = envelope
            self.trace.emit("tool_result", summary, data, duration_ms=(time.perf_counter() - started) * 1000)

        while self._pending_changes and self._pending_changes[0]["afterToolCall"] <= seq:
            for change in self._pending_changes.pop(0)["changes"]:
                self.state.apply(change)
        return copy.deepcopy(envelope)

    # -- facts for scorers

    def current_revision(self, record_id: str) -> int | None:
        for entity in PlannerState.ENTITIES:
            row = self.state.find(entity, record_id)
            if row is not None:
                return row.get("revision")
        return None
