//! Capacity: planned work against the time left in working hours once DayPlan events and busy
//! calendar events are placed. DayPlan reports these numbers and never moves anything because of
//! them.

use crate::calendar::ics::local_midnight;
use crate::calendar::store::timestamp;
use crate::db::CapacityFacts;
use crate::model::{
    Capacity, DayCapacity, ExternalEvent, ScheduleEvent, Task, TaskBlock, TimeRange, WorkingHours,
};
use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

type Span = (DateTime<Utc>, DateTime<Utc>);

/// Capacity for `days` local days starting at `first_day` in `zone`. Time blocks are planned
/// work: they narrow the free time left for new blocks but are not subtracted from what is
/// available, so their tasks' estimates aren't counted twice.
pub fn capacity(
    first_day: NaiveDate,
    days: u32,
    zone: Tz,
    facts: CapacityFacts,
    external_busy: &[ExternalEvent],
    calendars_incomplete: bool,
) -> Capacity {
    let busy_spans = merge(
        facts
            .events
            .iter()
            .filter_map(event_span)
            .chain(
                external_busy
                    .iter()
                    .filter(|event| event.busy)
                    .filter_map(|event| external_span(event, zone)),
            )
            .collect(),
    );
    let block_spans = merge(facts.blocks.iter().filter_map(block_span).collect());
    let days = (0..days)
        .filter_map(|offset| first_day.checked_add_days(Days::new(u64::from(offset))))
        .map(|day| {
            let label = day.to_string();
            let (planned_minutes, unestimated_tasks) = estimates(
                facts
                    .scheduled
                    .iter()
                    .filter(|task| task.scheduled_day.as_deref() == Some(label.as_str())),
            );
            let Some(window) = working_span(day, zone, &facts.working_hours) else {
                return DayCapacity {
                    day: label,
                    working_minutes: 0,
                    busy_minutes: 0,
                    available_minutes: 0,
                    planned_minutes,
                    unestimated_tasks,
                    blocked_minutes: 0,
                    free: Vec::new(),
                };
            };
            let busy = clip(&busy_spans, window);
            let blocked = clip(&block_spans, window);
            let occupied = merge(busy.iter().chain(&blocked).copied().collect());
            let working_minutes = minutes(window);
            let busy_minutes = total(&busy);
            DayCapacity {
                day: label,
                working_minutes,
                busy_minutes,
                available_minutes: (working_minutes - busy_minutes).max(0),
                planned_minutes,
                unestimated_tasks,
                blocked_minutes: total(&blocked),
                free: gaps(window, &occupied)
                    .into_iter()
                    .map(|(start, end)| TimeRange {
                        start_at_utc: timestamp(start),
                        end_at_utc: timestamp(end),
                    })
                    .collect(),
            }
        })
        .collect();
    let (pooled_minutes, pooled_unestimated_tasks) = estimates(facts.pooled.iter());
    Capacity {
        working_hours: facts.working_hours,
        days,
        pooled_minutes,
        pooled_unestimated_tasks,
        calendars_incomplete,
    }
}

/// Estimated minutes of open tasks, and how many open tasks have no estimate.
fn estimates<'a>(tasks: impl Iterator<Item = &'a Task>) -> (i64, i64) {
    tasks.fold((0, 0), |(minutes, unestimated), task| {
        match task.estimated_minutes {
            Some(estimate) => (minutes + estimate, unestimated),
            None => (minutes, unestimated + 1),
        }
    })
}

/// A day's working hours as UTC instants, or `None` on a day off.
fn working_span(day: NaiveDate, zone: Tz, hours: &WorkingHours) -> Option<Span> {
    let weekday = day.weekday().number_from_monday() as u8;
    if !hours.days.contains(&weekday) {
        return None;
    }
    let at = |minute: i64| -> Option<DateTime<Utc>> {
        if minute >= 24 * 60 {
            return day.succ_opt().map(|next| local_midnight(next, zone));
        }
        let local = day.and_hms_opt((minute / 60) as u32, (minute % 60) as u32, 0)?;
        // A time skipped by a clock change is read with the offset before the change, as
        // RFC 5545 reads nonexistent local times.
        [local, local + Duration::hours(1)]
            .into_iter()
            .find_map(|candidate| zone.from_local_datetime(&candidate).earliest())
            .map(|value| value.with_timezone(&Utc))
    };
    let span = (at(hours.start_minute)?, at(hours.end_minute)?);
    (span.1 > span.0).then_some(span)
}

fn event_span(event: &ScheduleEvent) -> Option<Span> {
    let start = parse_instant(&event.start_at_utc)?;
    Some((start, start + Duration::minutes(event.duration_minutes)))
}

fn block_span(block: &TaskBlock) -> Option<Span> {
    let start = parse_instant(&block.start_at_utc)?;
    Some((start, start + Duration::minutes(block.duration_minutes)))
}

/// A busy calendar event's span; an all-day event covers its whole local days.
fn external_span(event: &ExternalEvent, zone: Tz) -> Option<Span> {
    if event.all_day {
        let day =
            |value: &Option<String>| NaiveDate::parse_from_str(value.as_deref()?, "%Y-%m-%d").ok();
        return Some((
            local_midnight(day(&event.start_date)?, zone),
            local_midnight(day(&event.end_date)?, zone),
        ));
    }
    Some((
        parse_instant(event.start_at_utc.as_deref()?)?,
        parse_instant(event.end_at_utc.as_deref()?)?,
    ))
}

fn parse_instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// Sorts spans and joins any that overlap or touch; empty spans are dropped.
fn merge(mut spans: Vec<Span>) -> Vec<Span> {
    spans.retain(|(start, end)| end > start);
    spans.sort();
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// The parts of merged `spans` inside `window`.
fn clip(spans: &[Span], window: Span) -> Vec<Span> {
    spans
        .iter()
        .map(|(start, end)| ((*start).max(window.0), (*end).min(window.1)))
        .filter(|(start, end)| end > start)
        .collect()
}

/// The time in `window` not covered by merged `occupied` spans.
fn gaps(window: Span, occupied: &[Span]) -> Vec<Span> {
    let mut free = Vec::new();
    let mut cursor = window.0;
    for (start, end) in occupied {
        if *start > cursor {
            free.push((cursor, *start));
        }
        cursor = cursor.max(*end);
    }
    if window.1 > cursor {
        free.push((cursor, window.1));
    }
    free
}

fn minutes((start, end): Span) -> i64 {
    (end - start).num_minutes()
}

fn total(spans: &[Span]) -> i64 {
    spans.iter().copied().map(minutes).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ReminderStatus, TaskPriority, TaskStatus};

    const NEW_YORK: Tz = chrono_tz::America::New_York;

    fn hours(days: &[u8], start: i64, end: i64) -> WorkingHours {
        WorkingHours {
            days: days.to_vec(),
            start_minute: start,
            end_minute: end,
            revision: 1,
            updated_at: "2026-09-01T00:00:00.000Z".into(),
        }
    }

    fn event(start: &str, minutes: i64) -> ScheduleEvent {
        ScheduleEvent {
            id: "event".into(),
            title: "Event".into(),
            notes: String::new(),
            start_at_utc: start.into(),
            time_zone: "America/New_York".into(),
            duration_minutes: minutes,
            reminder_minutes_before: None,
            reminder_status: ReminderStatus::None,
            plan_id: None,
            location: String::new(),
            workstream_id: None,
            owner_id: None,
            revision: 1,
            created_at: start.into(),
            updated_at: start.into(),
        }
    }

    fn block(start: &str, minutes: i64) -> TaskBlock {
        TaskBlock {
            id: "block".into(),
            task_id: "task".into(),
            start_at_utc: start.into(),
            time_zone: "America/New_York".into(),
            duration_minutes: minutes,
            revision: 1,
            created_at: start.into(),
            updated_at: start.into(),
        }
    }

    fn task(day: Option<&str>, estimate: Option<i64>) -> Task {
        Task {
            id: "task".into(),
            title: "Task".into(),
            description: String::new(),
            plan_id: None,
            milestone_id: None,
            workstream_id: None,
            owner_id: None,
            due_date: None,
            scheduled_day: day.map(Into::into),
            planned_week: None,
            estimated_minutes: estimate,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
            completed_at: None,
            sort_order: 0,
            revision: 1,
            created_at: "2026-09-01T00:00:00.000Z".into(),
            updated_at: "2026-09-01T00:00:00.000Z".into(),
        }
    }

    fn timed(start: &str, end: &str, busy: bool) -> ExternalEvent {
        ExternalEvent {
            calendar_id: "work".into(),
            key: start.into(),
            title: "Meeting".into(),
            location: String::new(),
            all_day: false,
            start_at_utc: Some(start.into()),
            end_at_utc: Some(end.into()),
            start_date: None,
            end_date: None,
            busy,
            tentative: false,
            recurring: false,
        }
    }

    fn day(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn facts(working_hours: WorkingHours) -> CapacityFacts {
        CapacityFacts {
            working_hours,
            events: Vec::new(),
            blocks: Vec::new(),
            scheduled: Vec::new(),
            pooled: Vec::new(),
        }
    }

    #[test]
    fn subtracts_merged_busy_time_and_leaves_blocks_as_planned_work() {
        let mut facts = facts(hours(&[1, 2, 3, 4, 5], 9 * 60, 17 * 60));
        // Thursday, September 17, 2026, in New York (UTC-4).
        facts.events = vec![event("2026-09-17T14:00:00.000Z", 60)];
        facts.blocks = vec![block("2026-09-17T17:00:00.000Z", 60)];
        facts.scheduled = vec![
            task(Some("2026-09-17"), Some(90)),
            task(Some("2026-09-17"), None),
            task(Some("2026-09-18"), Some(30)),
        ];
        facts.pooled = vec![task(None, Some(120)), task(None, None)];
        let external = [
            timed("2026-09-17T14:30:00.000Z", "2026-09-17T16:00:00.000Z", true),
            timed(
                "2026-09-17T19:00:00.000Z",
                "2026-09-17T20:00:00.000Z",
                false,
            ),
            timed("2026-09-17T11:00:00.000Z", "2026-09-17T12:00:00.000Z", true),
        ];
        let report = capacity(day("2026-09-17"), 2, NEW_YORK, facts, &external, false);
        let thursday = &report.days[0];
        assert_eq!(
            (
                thursday.working_minutes,
                thursday.busy_minutes,
                thursday.available_minutes,
                thursday.blocked_minutes
            ),
            (480, 120, 360, 60),
            "10:00–12:00 is busy once; free and pre-work events don't count"
        );
        assert_eq!(
            (thursday.planned_minutes, thursday.unestimated_tasks),
            (90, 1)
        );
        assert_eq!(
            thursday
                .free
                .iter()
                .map(|range| (range.start_at_utc.as_str(), range.end_at_utc.as_str()))
                .collect::<Vec<_>>(),
            [
                ("2026-09-17T13:00:00.000Z", "2026-09-17T14:00:00.000Z"),
                ("2026-09-17T16:00:00.000Z", "2026-09-17T17:00:00.000Z"),
                ("2026-09-17T18:00:00.000Z", "2026-09-17T21:00:00.000Z"),
            ]
        );
        assert_eq!(report.days[1].planned_minutes, 30);
        assert_eq!(
            (report.pooled_minutes, report.pooled_unestimated_tasks),
            (120, 1)
        );
    }

    #[test]
    fn days_off_have_no_working_time_and_busy_all_day_events_fill_the_day() {
        let facts = facts(hours(&[1, 2, 3, 4, 5], 9 * 60, 17 * 60));
        let mut vacation = timed("", "", true);
        vacation.all_day = true;
        vacation.start_at_utc = None;
        vacation.end_at_utc = None;
        vacation.start_date = Some("2026-09-18".into());
        vacation.end_date = Some("2026-09-19".into());
        let report = capacity(day("2026-09-18"), 2, NEW_YORK, facts, &[vacation], true);
        let friday = &report.days[0];
        assert_eq!(
            (
                friday.working_minutes,
                friday.available_minutes,
                friday.free.len()
            ),
            (480, 0, 0)
        );
        let saturday = &report.days[1];
        assert_eq!((saturday.working_minutes, saturday.free.len()), (0, 0));
        assert!(report.calendars_incomplete);
    }

    #[test]
    fn working_hours_follow_the_local_clock_across_daylight_saving_changes() {
        let fall = capacity(
            day("2026-11-01"),
            1,
            NEW_YORK,
            facts(hours(&[7], 0, 24 * 60)),
            &[],
            false,
        );
        assert_eq!(
            fall.days[0].working_minutes,
            25 * 60,
            "the day clocks fall back has 25 hours"
        );
        let spring = capacity(
            day("2026-03-08"),
            1,
            NEW_YORK,
            facts(hours(&[7], 2 * 60 + 30, 4 * 60)),
            &[],
            false,
        );
        assert_eq!(
            spring.days[0].working_minutes, 30,
            "02:30 doesn't exist, so it is read as 03:30"
        );
    }

    #[test]
    fn merges_touching_spans_and_finds_gaps() {
        let at = |hour: u32| Utc.with_ymd_and_hms(2026, 9, 17, hour, 0, 0).unwrap();
        let merged = merge(vec![
            (at(12), at(13)),
            (at(9), at(10)),
            (at(10), at(11)),
            (at(12), at(12)),
        ]);
        assert_eq!(merged, [(at(9), at(11)), (at(12), at(13))]);
        assert_eq!(
            gaps((at(8), at(14)), &merged),
            [(at(8), at(9)), (at(11), at(12)), (at(13), at(14))]
        );
        assert_eq!(clip(&merged, (at(10), at(12))), [(at(10), at(11))]);
    }
}
