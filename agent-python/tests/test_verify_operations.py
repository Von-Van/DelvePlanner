from __future__ import annotations

from delveops_agent.drafts import OP_SPECS
from delveops_agent.verify.operations import normalize_payload, payload_problems

TZ = "America/New_York"


def test_every_operation_is_understood():
    for op in OP_SPECS:
        assert payload_problems(op, {"bogus": 1}), op


def test_a1_style_event_payload_is_refused_with_hints():
    problems = payload_problems("create_event", {"title": "Mockups", "start": "x", "end": "y"})
    text = "\n".join(problems)
    assert "use startAtUtc" in text and "use durationMinutes" in text
    assert "missing 'notes'" in text and "missing 'timeZone'" in text


def test_value_shapes_are_checked():
    assert payload_problems("schedule_task", {"scheduledDay": "2026-11-20", "dueDate": {}, "plannedWeek": {}})
    good = {
        "scheduledDay": {"action": "unchanged"},
        "dueDate": {"action": "set", "day": "2026-11-20"},
        "plannedWeek": {"action": "clear"},
    }
    assert payload_problems("schedule_task", good) == []
    assert payload_problems("update_task", {"estimate": {"action": "unchanged"}, "priority": "medium"})
    assert payload_problems(
        "reschedule_event",
        {"startAtUtc": "2026-11-04T15:00:00-05:00", "timeZone": TZ, "reminderChange": {"action": "unchanged"}},
    )
    assert payload_problems(
        "create_event",
        {
            "title": "x",
            "notes": "",
            "startAtUtc": "2026-11-04T20:00:00Z",
            "timeZone": "Mars/Base",
            "durationMinutes": 30,
        },
    )
    assert payload_problems("set_task_plan", {"plan": None}) == []
    assert payload_problems("delete_task", {}) == []
    assert payload_problems("delete_task", {"taskId": "t-1"})


def test_normalize_converts_local_start_end_to_utc_and_duration():
    payload, notes = normalize_payload(
        "create_event",
        {"title": "Mockups", "notes": "", "start": "2026-11-04T15:00-05:00", "end": "2026-11-04T16:00-05:00"},
        TZ,
    )
    assert payload == {
        "title": "Mockups",
        "notes": "",
        "startAtUtc": "2026-11-04T20:00:00Z",
        "durationMinutes": 60,
        "timeZone": TZ,
    }
    assert notes and payload_problems("create_event", payload) == []


def test_normalize_fills_unchanged_and_wraps_bare_values():
    payload, _ = normalize_payload("schedule_task", {"dueDate": "2026-11-20"}, TZ)
    assert payload == {
        "dueDate": {"action": "set", "day": "2026-11-20"},
        "scheduledDay": {"action": "unchanged"},
        "plannedWeek": {"action": "unchanged"},
    }
    payload, _ = normalize_payload("update_task", {"status": "done"}, TZ)
    assert payload == {"status": "done", "estimate": {"action": "unchanged"}}
    payload, _ = normalize_payload("update_event", {"title": "x", "reminderChange": 10}, TZ)
    assert payload["reminderChange"] == {"action": "set", "minutesBefore": 10}
    payload, _ = normalize_payload("create_task", {"plan": "plan-launch"}, TZ)
    assert payload["plan"] == {"id": "plan-launch"}
    payload, _ = normalize_payload("reschedule_event", {"startAtUtc": "2026-11-04T20:00Z", "timeZone": TZ}, TZ)
    assert payload["startAtUtc"] == "2026-11-04T20:00:00Z"


def test_normalize_leaves_guesses_to_the_model():
    # No offset on the start: converting would need a guess about the zone, so it stays as it is.
    payload, notes = normalize_payload("create_event", {"start": "2026-11-04T15:00"}, TZ)
    assert payload == {"start": "2026-11-04T15:00"} and notes == []
    payload, _ = normalize_payload("update_task", {"status": "finished"}, TZ)
    assert payload_problems("update_task", payload)
