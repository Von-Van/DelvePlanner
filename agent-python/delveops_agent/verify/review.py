"""The draft verifier (architecture A3): deterministic checks run on drafts before they leave the agent.

The A0-vs-A1 experiment found that every draft A1 wrote had a shape the app would refuse (`start`/`end`
instead of `startAtUtc`, a bare date where a day change was meant, a missing "no change" field). Rust
would have caught all of them, but only after the person asked for help, and with no way back to the
model. The verifier moves those checks inside the agent, where a failed check can be fixed:

* **normalise**: rewrites that keep the meaning exactly (see `operations.normalize_payload`);
* **payload check**: every reason Rust would refuse the payload, as an *error* the model must fix;
* **time check**: an event that overlaps something the agent itself saw on that day, falls outside
  working hours, or starts in the past, as a *warning* the model sees once and may keep.

Freshness (re-reading targets before finishing) needs tool calls, so it lives in `freshness.py` and the
agent loop. Nothing here calls a tool or the model.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Any, Literal
from zoneinfo import ZoneInfo

from ..types import Draft
from .operations import normalize_payload, parse_utc, payload_problems

Severity = Literal["error", "warning"]


@dataclass(frozen=True)
class VerifierConfig:
    """Which checks run. Each switch is one ablation in the A2/A3 experiment."""

    normalize: bool = True
    check_payloads: bool = True
    check_times: bool = True
    check_freshness: bool = True

    def to_dict(self) -> dict[str, bool]:
        return {
            "normalize": self.normalize,
            "checkPayloads": self.check_payloads,
            "checkTimes": self.check_times,
            "checkFreshness": self.check_freshness,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> VerifierConfig:
        return cls(
            normalize=bool(data.get("normalize", True)),
            check_payloads=bool(data.get("checkPayloads", True)),
            check_times=bool(data.get("checkTimes", True)),
            check_freshness=bool(data.get("checkFreshness", True)),
        )


@dataclass(frozen=True)
class Finding:
    severity: Severity
    draft: int  # index into the drafts list
    code: str  # payload_shape | time_conflict | outside_hours | in_past
    message: str

    def to_dict(self) -> dict[str, Any]:
        return {"severity": self.severity, "draft": self.draft, "code": self.code, "message": self.message}


@dataclass
class Review:
    drafts: list[dict[str, Any]]  # raw drafts after normalisation (still unparsed)
    findings: list[Finding] = field(default_factory=list)
    normalized: list[str] = field(default_factory=list)

    @property
    def errors(self) -> list[Finding]:
        return [f for f in self.findings if f.severity == "error"]

    @property
    def warnings(self) -> list[Finding]:
        return [f for f in self.findings if f.severity == "warning"]


@dataclass(frozen=True)
class Observation:
    """One successful tool call the agent made in this run."""

    call_id: str
    tool: str
    arguments: dict[str, Any]
    data: Any


def normalize_drafts(raw: list[Any], time_zone: str) -> tuple[list[Any], list[str]]:
    out, notes = [], []
    for index, item in enumerate(raw):
        if isinstance(item, dict) and isinstance(item.get("payload"), dict) and isinstance(item.get("op"), str):
            payload, changed = normalize_payload(item["op"], item["payload"], time_zone)
            if changed:
                item = {**item, "payload": payload}
                notes.extend(f"draft {index}: {c}" for c in changed)
        out.append(item)
    return out, notes


def shape_findings(raw: list[Any]) -> list[Finding]:
    findings = []
    for index, item in enumerate(raw):
        if not isinstance(item, dict):
            continue  # the grounding parser reports these
        for problem in payload_problems(str(item.get("op")), item.get("payload")):
            findings.append(Finding("error", index, "payload_shape", problem))
    return findings


def _moment(value: Any) -> datetime | None:
    if not isinstance(value, str) or "T" not in value:
        return None
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return moment if moment.tzinfo is not None else None


def _event_duration(target_id: str | None, observations: Iterable[Observation]) -> int | None:
    """The duration of an existing event, from the latest day context that showed it."""
    found = None
    for obs in observations:
        if obs.tool != "get_day_context" or not isinstance(obs.data, dict):
            continue
        for event in obs.data.get("events") or []:
            if isinstance(event, dict) and event.get("id") == target_id:
                start, end = _moment(event.get("start")), _moment(event.get("end"))
                if start and end:
                    found = int((end - start).total_seconds() // 60)
    return found


def _label(item: dict[str, Any], kind: str) -> str:
    return str(item.get("title") or item.get("calendar") or item.get("taskId") or kind)


def time_findings(drafts: list[Draft], observations: list[Observation], task_now: str, time_zone: str) -> list[Finding]:
    """Warnings for events placed on top of something the agent saw, outside working hours, or in the past.

    Only days the agent actually read are checked: the verifier never assumes a day is free.
    """
    findings: list[Finding] = []
    now = _moment(task_now)
    for index, draft in enumerate(drafts):
        if draft.op not in ("create_event", "reschedule_event"):
            continue
        start = parse_utc(draft.payload.get("startAtUtc"))
        if start is None:
            continue
        target_id = (draft.target or {}).get("id")
        minutes = draft.payload.get("durationMinutes") or _event_duration(target_id, observations)
        if not isinstance(minutes, int) or minutes <= 0:
            continue
        end = start + timedelta(minutes=minutes)
        try:
            zone = ZoneInfo(str(draft.payload.get("timeZone") or time_zone))
        except Exception:  # noqa: BLE001 - a bad zone is a payload error, reported elsewhere
            continue
        local_day = start.astimezone(zone).date().isoformat()
        if now is not None and start < now:
            when = f"{start.astimezone(zone):%H:%M on %b %d}"
            message = f"draft {index} starts at {when}, which is in the past"
            findings.append(Finding("warning", index, "in_past", message))
        for obs in reversed(observations):  # the latest read of a day wins
            if obs.tool != "get_day_context" or not isinstance(obs.data, dict) or obs.data.get("date") != local_day:
                continue
            busy: list[tuple[str, datetime, datetime]] = []
            for kind in ("events", "taskBlocks", "externalBusy"):
                for item in obs.data.get(kind) or []:
                    if not isinstance(item, dict) or (target_id and item.get("id") == target_id):
                        continue
                    s, e = _moment(item.get("start")), _moment(item.get("end"))
                    if s and e:
                        busy.append((_label(item, kind), s, e))
            clashes = [f"{name} ({s:%H:%M}-{e:%H:%M})" for name, s, e in busy if s < end and e > start]
            if clashes:
                findings.append(
                    Finding(
                        "warning",
                        index,
                        "time_conflict",
                        f"draft {index} ({start.astimezone(zone):%H:%M}-{end.astimezone(zone):%H:%M}) overlaps "
                        + ", ".join(sorted(set(clashes)))
                        + f" from {obs.call_id}; free ranges that day: {_free_text(obs.data)}",
                    )
                )
            hours = obs.data.get("workingHours")
            if isinstance(hours, dict):
                ws, we = _moment(hours.get("start")), _moment(hours.get("end"))
                if ws and we and (start < ws or end > we):
                    findings.append(
                        Finding(
                            "warning",
                            index,
                            "outside_hours",
                            f"draft {index} falls outside working hours ({ws:%H:%M}-{we:%H:%M})",
                        )
                    )
            break  # earlier reads of the same day can be stale
    return findings


def _free_text(day: dict[str, Any]) -> str:
    ranges = []
    for item in day.get("free") or []:
        if not isinstance(item, dict):
            continue
        s, e = _moment(item.get("start")), _moment(item.get("end"))
        if s and e:
            ranges.append(f"{s:%H:%M}-{e:%H:%M}")
    return ", ".join(ranges) or "none"


def format_findings(findings: list[Finding]) -> str:
    return "\n".join(f"- [{f.severity}] {f.message}" for f in findings)
