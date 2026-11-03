//! Heads-ups: the assistant brings things up before it is asked.
//!
//! Every heads-up comes from a fixed rule over the planner's own records, never from guesswork,
//! and no model runs to find one. A quiet week produces nothing. The rules:
//!
//! - **Deadline**: an open task is due within two days (or was due in the last week) and no time
//!   is planned for it. Says where free time is before the due date, if there is any.
//! - **Clash**: two commitments overlap in the next two days, at least one of them made in Delve
//!   Planner (an event or a time block).
//! - **Free focus time**: a real stretch of free time is left today while important work has no
//!   time planned.
//! - **Overloaded day**: a coming day holds more planned work than time.
//! - **Pushed back**: open work has been carried off its day several times.
//! - **Quiet project**: an active project with open work hasn't changed for two weeks.
//!
//! What is found is stored by [`PlannerDatabase::record_heads_ups`], which handles dedupe,
//! dismissals, snoozes and the daily limit. A background worker checks every few minutes;
//! urgent heads-ups may also send a system notification, outside quiet hours and within a daily
//! limit. Acting on one hands its request to the assistant, which reads the current planner
//! data itself and drafts changes for approval.

use crate::calendar::{system_zone, DayRange};
use crate::capacity;
use crate::db::{
    HeadsUp, HeadsUpDismissal, HeadsUpKind, HeadsUpRefresh, HeadsUpSettings, HeadsUpTarget,
    HeadsUpTargetKind, NewHeadsUp, PlannerDatabase, PushedTask, QuietPlan, UpcomingBlock,
    MAX_SHOWN_HEADS_UPS,
};
use crate::error::{AppResult, CommandError};
use crate::feedback::{receipts, FeedbackReceipt};
use crate::model::{Capacity, ExternalEvent, PlanningProfile, ScheduleEvent, Task, TaskPriority};
use crate::tools::BusySource;
use crate::{with_database, AppState};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use std::collections::HashSet;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

/// Tells the renderer the heads-ups may have changed.
pub const HEADS_UPS_CHANGED: &str = "heads-ups-changed";
/// How often the background worker checks.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Deadlines this many days ahead (0 = today) count as near.
const DEADLINE_DAYS_AHEAD: i64 = 2;
/// Overdue work counts for this many days after its due date; older is a different problem.
const OVERDUE_DAYS: i64 = 7;
/// Overlaps shorter than this are rounding, not clashes.
const MIN_CLASH_MINUTES: i64 = 5;
/// At most this many clashes are passed on per check; storage shows two at a time.
const MAX_CLASHES_CONSIDERED: usize = 10;
/// Clashes are looked for from now through the end of tomorrow.
const CLASH_DAYS: u64 = 2;
/// A day counts as overloaded once planned work exceeds the time for it by this much.
const MIN_OVERLOAD_MINUTES: i64 = 30;
/// Capacity is read for today and the six days after it.
const CAPACITY_DAYS: u32 = 7;
/// A free stretch counts as focus time from this long, or the person's focus length if longer.
const MIN_FOCUS_MINUTES: i64 = 60;
const DEFAULT_FOCUS_MINUTES: i64 = 90;
/// A project counts as quiet after this many days without a change.
const QUIET_PROJECT_DAYS: i64 = 14;
/// Free time is reported from the next quarter hour, not from this exact minute.
const ROUND_TO_MINUTES: i64 = 15;

/// Everything the rules look at, for one check at one moment.
#[derive(Debug, Clone)]
pub struct HeadsUpFacts {
    pub now: DateTime<Utc>,
    pub zone: Tz,
    pub today: NaiveDate,
    pub profile: PlanningProfile,
    /// Today and the six days after it.
    pub capacity: Capacity,
    pub due_tasks: Vec<Task>,
    pub important_open: Vec<Task>,
    pub blocks: Vec<UpcomingBlock>,
    /// Delve Planner events over the capacity window.
    pub events: Vec<ScheduleEvent>,
    /// Busy events from read-only calendars over the capacity window.
    pub external: Vec<ExternalEvent>,
    pub pushed_back: Vec<PushedTask>,
    pub quiet_plans: Vec<QuietPlan>,
}

/// Reads what the rules need: planner records, busy calendar time, and capacity.
pub fn collect(
    database: &PlannerDatabase,
    calendars: &dyn BusySource,
    now: DateTime<Utc>,
    zone: Tz,
) -> AppResult<HeadsUpFacts> {
    let today = now.with_timezone(&zone).date_naive();
    let zone_name = zone.name();
    let planner =
        database.heads_up_facts(now, today, today + Duration::days(DEADLINE_DAYS_AHEAD))?;
    let range = DayRange::new(&today.to_string(), CAPACITY_DAYS, zone_name)?;
    let facts = database.capacity_facts(&range.first_day, &range.end_day, zone_name, None)?;
    let profile = database.planning_profile()?;
    let (external, incomplete) = calendars
        .busy(&range)
        .unwrap_or_else(|_| (Vec::new(), true));
    let events = facts.events.clone();
    let capacity = capacity::capacity(
        today,
        CAPACITY_DAYS,
        zone,
        facts,
        &external,
        incomplete,
        &profile,
    );
    Ok(HeadsUpFacts {
        now,
        zone,
        today,
        profile,
        capacity,
        due_tasks: planner.due_tasks,
        important_open: planner.important_open,
        blocks: planner.blocks,
        events,
        external,
        pushed_back: planner.pushed_back,
        quiet_plans: planner.quiet_plans,
    })
}

/// Runs every rule over one snapshot. Pure: the same facts always give the same heads-ups.
pub fn detect(facts: &HeadsUpFacts) -> Vec<NewHeadsUp> {
    let mut found = Vec::new();
    let deadlines = deadlines(facts);
    // A deadline that already points at free time today makes a separate focus-time heads-up
    // say the same thing twice.
    let window_named = deadlines.iter().any(|(_, window)| {
        window.is_some_and(|(start, _)| local_day(start, facts.zone) == facts.today)
    });
    found.extend(deadlines.into_iter().map(|(heads_up, _)| heads_up));
    found.extend(clashes(facts));
    if !window_named {
        found.extend(focus_window(facts));
    }
    found.extend(overloaded(facts));
    found.extend(pushed_back(facts));
    found.extend(quiet_project(facts));
    found.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.key.cmp(&b.key)));
    found
}

/// One check: read, apply the rules, store. Returns what changed.
pub fn run(
    database: &mut PlannerDatabase,
    calendars: &dyn BusySource,
    now: DateTime<Utc>,
    zone: Tz,
) -> AppResult<HeadsUpRefresh> {
    if !database.heads_up_settings()?.enabled {
        return Ok(HeadsUpRefresh::default());
    }
    let facts = collect(database, calendars, now, zone)?;
    let found = detect(&facts);
    database.record_heads_ups(&found, now, day_start(now, zone))
}

/// Whether `now` is outside working hours or on a day off, when nothing should interrupt.
pub fn quiet_hours(facts: &HeadsUpFacts) -> bool {
    let local = facts.now.with_timezone(&facts.zone);
    let weekday = local.weekday().number_from_monday() as u8;
    let hours = &facts.capacity.working_hours;
    if !hours.days.contains(&weekday) || facts.profile.no_work_days.contains(&weekday) {
        return true;
    }
    let minute = i64::from(local.hour()) * 60 + i64::from(local.minute());
    minute < hours.start_minute || minute >= hours.end_minute
}

/// The start of the local day `now` falls on, in UTC.
pub fn day_start(now: DateTime<Utc>, zone: Tz) -> DateTime<Utc> {
    crate::calendar::ics::local_midnight(now.with_timezone(&zone).date_naive(), zone)
}

// ---------------------------------------------------------------------------------------------
// The rules

/// A free stretch of time, from start to end.
type Window = (DateTime<Utc>, DateTime<Utc>);

/// Near deadlines with nothing planned, each with the free window it names (if any).
fn deadlines(facts: &HeadsUpFacts) -> Vec<(NewHeadsUp, Option<Window>)> {
    let today = facts.today;
    let mut found = Vec::new();
    for task in &facts.due_tasks {
        let Some(due) = task.due_date.as_deref().and_then(parse_day) else {
            continue;
        };
        let days_left = (due - today).num_days();
        if !(-OVERDUE_DAYS..=DEADLINE_DAYS_AHEAD).contains(&days_left) {
            continue;
        }
        let due_end =
            crate::calendar::ics::local_midnight(due.succ_opt().unwrap_or(due), facts.zone);
        // Time is planned if a block for it hasn't ended yet (and, for a coming deadline, starts
        // before the end of the due day), or it is on a day from today through the due date.
        let has_block = facts
            .blocks
            .iter()
            .any(|block| block.task_id == task.id && (days_left < 0 || block.start < due_end));
        let on_a_day = task
            .scheduled_day
            .as_deref()
            .and_then(parse_day)
            .is_some_and(|day| day >= today && (days_left < 0 || day <= due));
        if has_block || on_a_day {
            continue;
        }
        let need = task.estimated_minutes.unwrap_or(30).clamp(15, 120);
        let search_until = if days_left < 0 {
            crate::calendar::ics::local_midnight(today + Duration::days(1), facts.zone)
        } else {
            due_end
        };
        let window = free_window(facts, need, search_until);
        let when = if days_left < 0 {
            format!("was due {}", day_phrase(due, today))
        } else {
            format!("is due {}", day_phrase(due, today))
        };
        let title = format!("“{}” {when}", task.title);
        let mut evidence = if days_left < 0 {
            "It isn't done and no time is planned for it.".to_string()
        } else {
            "No time is planned for it yet.".to_string()
        };
        match window {
            Some((start, end)) => evidence.push_str(&format!(
                " You have {} free {} ({}).",
                hours(minutes_between(start, end)),
                part_of_day_phrase(start, facts),
                time_range(start, end, facts.zone),
            )),
            None if days_left >= 0 => {
                evidence.push_str(" There's no free stretch long enough before then.")
            }
            None => {}
        }
        if let Some(estimate) = task.estimated_minutes {
            evidence.push_str(&format!(" Estimated at {}.", hours(estimate)));
        }
        let mut score = match days_left {
            days if days < 0 => 95,
            0 => 100,
            1 => 90,
            _ => 80,
        };
        score += priority_boost(task.priority);
        let request = match window {
            Some((start, end)) => format!(
                "“{}” {when} ({due}) and no time is planned for it. I have {} free {} ({}). \
                 Plan time for it and draft a time block.",
                task.title,
                hours(minutes_between(start, end)),
                part_of_day_phrase(start, facts),
                time_range(start, end, facts.zone),
            ),
            None => format!(
                "“{}” {when} ({due}) and no time is planned for it. Find time for it, or tell me \
                 what has to give, and draft the change.",
                task.title
            ),
        };
        let expires_at = if days_left < 0 {
            facts.now + Duration::days(1)
        } else {
            due_end
        };
        found.push((
            NewHeadsUp {
                kind: HeadsUpKind::Deadline,
                key: format!("deadline:{}:{due}", task.id),
                title,
                evidence,
                target: Some(HeadsUpTarget {
                    kind: HeadsUpTargetKind::Task,
                    id: task.id.clone(),
                }),
                action_label: "Plan it".into(),
                action_request: request,
                score,
                urgent: days_left <= 1,
                expires_at,
            },
            window,
        ));
    }
    found
}

/// Something with a time: an event, a time block, or a busy calendar event.
struct Timed {
    key: String,
    title: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    /// Made in Delve Planner (an event or a block), so it can be moved here.
    ours: bool,
    task_id: Option<String>,
    target: Option<HeadsUpTarget>,
}

/// Overlapping commitments from now through the end of tomorrow, earliest first. Storage shows
/// at most two at a time.
fn clashes(facts: &HeadsUpFacts) -> Vec<NewHeadsUp> {
    let until = crate::calendar::ics::local_midnight(
        facts.today + chrono::Days::new(CLASH_DAYS),
        facts.zone,
    );
    let in_window = |start: DateTime<Utc>, end: DateTime<Utc>| end > facts.now && start < until;
    let mut timed: Vec<Timed> = Vec::new();
    for event in &facts.events {
        let Ok(start) = event.start_at_utc.parse::<DateTime<Utc>>() else {
            continue;
        };
        let end = start + Duration::minutes(event.duration_minutes);
        if in_window(start, end) {
            timed.push(Timed {
                key: format!("event:{}", event.id),
                title: event.title.clone(),
                start,
                end,
                ours: true,
                task_id: None,
                target: Some(HeadsUpTarget {
                    kind: HeadsUpTargetKind::Event,
                    id: event.id.clone(),
                }),
            });
        }
    }
    for block in facts.blocks.iter().filter(|block| !block.task_done) {
        if in_window(block.start, block.end) {
            timed.push(Timed {
                key: format!("block:{}", block.id),
                title: block.task_title.clone(),
                start: block.start,
                end: block.end,
                ours: true,
                task_id: Some(block.task_id.clone()),
                target: Some(HeadsUpTarget {
                    kind: HeadsUpTargetKind::Block,
                    id: block.id.clone(),
                }),
            });
        }
    }
    for event in facts
        .external
        .iter()
        .filter(|event| event.busy && !event.tentative && !event.all_day)
    {
        let (Some(start), Some(end)) = (
            event
                .start_at_utc
                .as_deref()
                .and_then(|value| value.parse().ok()),
            event
                .end_at_utc
                .as_deref()
                .and_then(|value| value.parse().ok()),
        ) else {
            continue;
        };
        if in_window(start, end) {
            timed.push(Timed {
                key: format!("calendar:{}:{}", event.calendar_id, event.key),
                title: event.title.clone(),
                start,
                end,
                ours: false,
                task_id: None,
                target: None,
            });
        }
    }
    timed.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.key.cmp(&b.key)));

    let mut pairs = Vec::new();
    for (index, first) in timed.iter().enumerate() {
        for second in &timed[index + 1..] {
            if second.start >= first.end {
                continue;
            }
            if !first.ours && !second.ours {
                continue;
            }
            if first.task_id.is_some() && first.task_id == second.task_id {
                continue;
            }
            let overlap_start = first.start.max(second.start);
            let overlap_end = first.end.min(second.end);
            if minutes_between(overlap_start, overlap_end) < MIN_CLASH_MINUTES
                || overlap_end <= facts.now
            {
                continue;
            }
            pairs.push((first, second, overlap_start, overlap_end));
        }
    }
    pairs.sort_by_key(|pair| pair.2);
    pairs
        .into_iter()
        .take(MAX_CLASHES_CONSIDERED)
        .enumerate()
        .map(|(rank, (first, second, overlap_start, overlap_end))| {
            let day = local_day(overlap_start, facts.zone);
            let soon = overlap_start - facts.now <= Duration::hours(3);
            let mut keys = [first.key.as_str(), second.key.as_str()];
            keys.sort_unstable();
            NewHeadsUp {
                kind: HeadsUpKind::Clash,
                key: format!(
                    "clash:{}|{}:{}|{}",
                    keys[0],
                    keys[1],
                    first.start.timestamp(),
                    second.start.timestamp()
                ),
                title: format!("“{}” and “{}” overlap", first.title, second.title),
                evidence: format!(
                    "{}: {} and {} overlap by {}.",
                    capitalized(&day_phrase(day, facts.today)),
                    time_range(first.start, first.end, facts.zone),
                    time_range(second.start, second.end, facts.zone),
                    hours(minutes_between(overlap_start, overlap_end)),
                ),
                target: first.target.clone().or_else(|| second.target.clone()),
                action_label: "Sort it out".into(),
                action_request: format!(
                    "“{}” ({} {}) overlaps “{}” ({}). Help me resolve the clash: suggest which \
                     one to move and draft the change.",
                    first.title,
                    day_phrase(day, facts.today),
                    time_range(first.start, first.end, facts.zone),
                    second.title,
                    time_range(second.start, second.end, facts.zone),
                ),
                score: if soon { 97 } else { 85 } - rank as i64,
                urgent: day == facts.today,
                expires_at: overlap_end,
            }
        })
        .collect()
}

/// A real stretch of free time left today, while important work has no time planned.
fn focus_window(facts: &HeadsUpFacts) -> Option<NewHeadsUp> {
    let need = facts
        .profile
        .focus_minutes
        .unwrap_or(DEFAULT_FOCUS_MINUTES)
        .max(MIN_FOCUS_MINUTES);
    let end_of_today =
        crate::calendar::ics::local_midnight(facts.today + Duration::days(1), facts.zone);
    let (start, end) = free_window(facts, need, end_of_today)?;
    let planned: HashSet<&str> = facts
        .blocks
        .iter()
        .map(|block| block.task_id.as_str())
        .collect();
    let candidate = facts.important_open.iter().find(|task| {
        !planned.contains(task.id.as_str())
            && task
                .scheduled_day
                .as_deref()
                .and_then(parse_day)
                .is_none_or(|day| day <= facts.today)
    })?;
    let why = match (
        candidate.priority,
        candidate.due_date.as_deref().and_then(parse_day),
    ) {
        (TaskPriority::Critical, _) => "critical priority".to_string(),
        (TaskPriority::High, _) => "high priority".to_string(),
        (_, Some(due)) => format!("due {}", day_phrase(due, facts.today)),
        _ => "important".to_string(),
    };
    let length = minutes_between(start, end);
    Some(NewHeadsUp {
        kind: HeadsUpKind::FocusWindow,
        key: format!("focus:{}", facts.today),
        title: format!(
            "You have {} free {}",
            hours(length),
            part_of_day_phrase(start, facts)
        ),
        evidence: format!(
            "Nothing is booked {}, and “{}” ({why}) has no time planned.",
            time_range(start, end, facts.zone),
            candidate.title
        ),
        target: Some(HeadsUpTarget {
            kind: HeadsUpTargetKind::Day,
            id: facts.today.to_string(),
        }),
        action_label: "Use it".into(),
        action_request: format!(
            "I have free time today, {}. Help me decide what to work on in it and draft time \
             blocks.",
            time_range(start, end, facts.zone)
        ),
        score: if length >= 120 { 60 } else { 50 },
        urgent: false,
        expires_at: end,
    })
}

/// Coming days (tomorrow through six days out) with more planned work than time, worst first.
/// Storage shows one at a time, so dismissing one day lets the next come through.
fn overloaded(facts: &HeadsUpFacts) -> Vec<NewHeadsUp> {
    let limit = facts.capacity.planned_limit_minutes;
    let over_days: Vec<(NaiveDate, i64, i64, i64)> = facts
        .capacity
        .days
        .iter()
        .filter_map(|day| {
            let date = parse_day(&day.day)?;
            if date <= facts.today {
                return None;
            }
            let room = match limit {
                Some(limit) => day.available_minutes.min(limit),
                None => day.available_minutes,
            };
            let over = day.planned_minutes - room;
            (over >= MIN_OVERLOAD_MINUTES).then_some((date, over, day.planned_minutes, room))
        })
        .collect();
    let mut worst_first = over_days.clone();
    worst_first.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let others = over_days.len().saturating_sub(1);
    worst_first
        .into_iter()
        .enumerate()
        .map(|(rank, (date, over, planned, room))| {
            overloaded_day(facts, date, over, planned, room, others, rank)
        })
        .collect()
}

fn overloaded_day(
    facts: &HeadsUpFacts,
    date: NaiveDate,
    over: i64,
    planned: i64,
    room: i64,
    others: usize,
    rank: usize,
) -> NewHeadsUp {
    let day_name = day_phrase(date, facts.today);
    let mut evidence = if room == 0 {
        format!(
            "It has {} of planned work and no working time.",
            hours(planned)
        )
    } else {
        format!(
            "It has {} of planned work and {} to do it in.",
            hours(planned),
            hours(room)
        )
    };
    if others > 0 {
        evidence.push_str(&format!(
            " {} other coming {} {} over too.",
            others,
            if others == 1 { "day" } else { "days" },
            if others == 1 { "is" } else { "are" }
        ));
    }
    NewHeadsUp {
        kind: HeadsUpKind::Overloaded,
        key: format!("overloaded:{date}"),
        title: format!(
            "{} is overbooked by {}",
            capitalized(&day_name),
            hours(over)
        ),
        evidence,
        target: Some(HeadsUpTarget {
            kind: HeadsUpTargetKind::Day,
            id: date.to_string(),
        }),
        action_label: "Rebalance".into(),
        action_request: format!(
            "{} ({date}) has {} of planned work but only {} to do it in. Help me rebalance the \
             week and draft the moves.",
            capitalized(&day_name),
            hours(planned),
            hours(room)
        ),
        score: if date == facts.today + Duration::days(1) {
            80
        } else {
            70
        } - rank as i64,
        urgent: false,
        expires_at: crate::calendar::ics::local_midnight(date, facts.zone),
    }
}

/// Open tasks carried off their day repeatedly, most moved first. Storage shows one at a time.
fn pushed_back(facts: &HeadsUpFacts) -> Vec<NewHeadsUp> {
    facts
        .pushed_back
        .iter()
        .enumerate()
        .map(|(rank, task)| pushed_back_task(facts, task, rank))
        .collect()
}

fn pushed_back_task(facts: &HeadsUpFacts, task: &PushedTask, rank: usize) -> NewHeadsUp {
    NewHeadsUp {
        kind: HeadsUpKind::PushedBack,
        key: format!("pushed_back:{}", task.task_id),
        title: format!("“{}” keeps getting pushed back", task.title),
        evidence: format!("It has moved off its day {} times.", task.moves),
        target: Some(HeadsUpTarget {
            kind: HeadsUpTargetKind::Task,
            id: task.task_id.clone(),
        }),
        action_label: "Decide".into(),
        action_request: format!(
            "“{}” has been carried forward {} times. Help me decide whether to break it up, \
             give it real time, or drop it, and draft the change.",
            task.title, task.moves
        ),
        score: 40 + task.moves.min(10) - rank as i64,
        urgent: false,
        expires_at: facts.now + Duration::days(1),
    }
}

/// Active projects quiet for two weeks, quietest first. Storage shows one at a time.
fn quiet_project(facts: &HeadsUpFacts) -> Vec<NewHeadsUp> {
    let cutoff = facts.now - Duration::days(QUIET_PROJECT_DAYS);
    let mut quiet: Vec<&QuietPlan> = facts
        .quiet_plans
        .iter()
        .filter(|plan| plan.last_activity < cutoff)
        .collect();
    quiet.sort_by(|a, b| {
        a.last_activity
            .cmp(&b.last_activity)
            .then_with(|| a.plan_id.cmp(&b.plan_id))
    });
    quiet
        .into_iter()
        .enumerate()
        .map(|(rank, plan)| quiet_plan(facts, plan, rank))
        .collect()
}

fn quiet_plan(facts: &HeadsUpFacts, plan: &QuietPlan, rank: usize) -> NewHeadsUp {
    let days = (facts.now - plan.last_activity).num_days();
    NewHeadsUp {
        kind: HeadsUpKind::QuietProject,
        key: format!("quiet_project:{}", plan.plan_id),
        title: format!("“{}” has gone quiet", plan.title),
        evidence: format!(
            "Nothing in it has changed for {days} days, and {} still open.",
            if plan.open_tasks == 1 {
                "1 task is".to_string()
            } else {
                format!("{} tasks are", plan.open_tasks)
            }
        ),
        target: Some(HeadsUpTarget {
            kind: HeadsUpTargetKind::Plan,
            id: plan.plan_id.clone(),
        }),
        action_label: "Check in".into(),
        action_request: format!(
            "Nothing in the project “{}” has changed for {days} days and it has open tasks. \
             What's the state of it, and what should happen next?",
            plan.title
        ),
        score: 30 - (rank as i64).min(10),
        urgent: false,
        expires_at: facts.now + Duration::days(1),
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers

/// The first free stretch of at least `need` minutes from now until `until`, starting at the next
/// quarter hour.
fn free_window(facts: &HeadsUpFacts, need: i64, until: DateTime<Utc>) -> Option<Window> {
    let from = round_up(facts.now);
    facts
        .capacity
        .days
        .iter()
        .flat_map(|day| day.free.iter())
        .filter_map(|range| {
            let start: DateTime<Utc> = range.start_at_utc.parse().ok()?;
            let end: DateTime<Utc> = range.end_at_utc.parse().ok()?;
            let start = start.max(from);
            let end = end.min(until);
            (minutes_between(start, end) >= need).then_some((start, end))
        })
        .next()
}

fn round_up(now: DateTime<Utc>) -> DateTime<Utc> {
    let step = ROUND_TO_MINUTES * 60;
    let seconds = now.timestamp();
    let rounded = (seconds + step - 1).div_euclid(step) * step;
    DateTime::from_timestamp(rounded, 0).unwrap_or(now)
}

fn minutes_between(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    (end - start).num_minutes()
}

fn local_day(instant: DateTime<Utc>, zone: Tz) -> NaiveDate {
    instant.with_timezone(&zone).date_naive()
}

fn parse_day(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn priority_boost(priority: TaskPriority) -> i64 {
    match priority {
        TaskPriority::Critical => 10,
        TaskPriority::High => 5,
        TaskPriority::Normal | TaskPriority::Low => 0,
    }
}

/// "today", "tomorrow", "yesterday", "Friday" within the week either side, else "Friday, Nov 14".
fn day_phrase(day: NaiveDate, today: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "today".into(),
        1 => "tomorrow".into(),
        -1 => "yesterday".into(),
        offset if (2..=6).contains(&offset) => day.format("%A").to_string(),
        offset if (-6..=-2).contains(&offset) => format!("last {}", day.format("%A")),
        _ => day.format("%A, %b %-d").to_string(),
    }
}

/// "this afternoon", "tomorrow morning", "Friday evening".
fn part_of_day_phrase(start: DateTime<Utc>, facts: &HeadsUpFacts) -> String {
    let local = start.with_timezone(&facts.zone);
    let part = match local.hour() {
        0..=11 => "morning",
        12..=16 => "afternoon",
        _ => "evening",
    };
    let day = local.date_naive();
    match (day - facts.today).num_days() {
        0 => format!("this {part}"),
        1 => format!("tomorrow {part}"),
        _ => format!("{} {part}", day.format("%A")),
    }
}

/// "2 PM", "2:30 PM".
fn clock(instant: DateTime<Utc>, zone: Tz) -> String {
    let local = instant.with_timezone(&zone);
    if local.minute() == 0 {
        local.format("%-I %p").to_string()
    } else {
        local.format("%-I:%M %p").to_string()
    }
}

/// "2 PM–4 PM".
fn time_range(start: DateTime<Utc>, end: DateTime<Utc>, zone: Tz) -> String {
    format!("{}–{}", clock(start, zone), clock(end, zone))
}

/// "3 h 30 m", "2 h", or "45 m".
fn hours(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, rest) => format!("{rest} m"),
        (whole, 0) => format!("{whole} h"),
        (whole, rest) => format!("{whole} h {rest} m"),
    }
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// Commands and the background worker

/// The heads-ups to show now. With `refresh`, checks first, so a change the person just made
/// (planning the time, moving the clash) is reflected straight away.
#[tauri::command]
pub(crate) fn list_heads_ups(
    state: State<'_, AppState>,
    refresh: bool,
) -> Result<Vec<HeadsUp>, CommandError> {
    let now = Utc::now();
    let zone = system_zone();
    with_database(&state, |database| {
        if !database.heads_up_settings()?.enabled {
            return Ok(Vec::new());
        }
        if refresh {
            if let Err(error) = run(database, &state.calendars, now, zone) {
                let code = CommandError::from(error).code;
                tauri_plugin_log::log::warn!("heads_up_check_failed code={code}");
            }
        }
        database.active_heads_ups(now, MAX_SHOWN_HEADS_UPS)
    })
}

/// A dismissed heads-up, with the kind it stopped and the feedback (and lesson on offer) when
/// the person said why.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadsUpDismissReceipt {
    pub heads_up: HeadsUp,
    pub muted_kind: Option<HeadsUpKind>,
    pub feedback: Option<FeedbackReceipt>,
}

/// "Don't show this again", optionally "and stop this kind" and "because …". A reason becomes
/// feedback that waits for "just this once" or "remember for next time".
#[tauri::command]
pub(crate) fn dismiss_heads_up(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    dismissal: Option<HeadsUpDismissal>,
) -> Result<HeadsUpDismissReceipt, CommandError> {
    let dismissed = with_database(&state, |database| {
        database.dismiss_heads_up_because(&id, dismissal.unwrap_or_default(), Utc::now())
    })?;
    if dismissed.muted_kind.is_some() {
        let _ = app.emit(HEADS_UPS_CHANGED, ());
    }
    Ok(HeadsUpDismissReceipt {
        heads_up: dismissed.heads_up,
        muted_kind: dismissed.muted_kind,
        feedback: dismissed
            .feedback
            .map(|feedback| receipts(vec![feedback]).remove(0)),
    })
}

/// Hides a heads-up until the start of tomorrow's working hours.
#[tauri::command]
pub(crate) fn snooze_heads_up(
    state: State<'_, AppState>,
    id: String,
) -> Result<HeadsUp, CommandError> {
    let now = Utc::now();
    let zone = system_zone();
    with_database(&state, |database| {
        let hours = database.working_hours()?;
        let tomorrow = now.with_timezone(&zone).date_naive() + Duration::days(1);
        let until = crate::calendar::ics::local_midnight(tomorrow, zone)
            + Duration::minutes(hours.start_minute);
        database.snooze_heads_up(&id, until, now)
    })
}

/// Marks a heads-up as handed to the assistant and returns it, with the request to send.
#[tauri::command]
pub(crate) fn act_on_heads_up(
    state: State<'_, AppState>,
    id: String,
) -> Result<HeadsUp, CommandError> {
    with_database(&state, |database| database.act_on_heads_up(&id, Utc::now()))
}

#[tauri::command]
pub(crate) fn get_heads_up_settings(
    state: State<'_, AppState>,
) -> Result<HeadsUpSettings, CommandError> {
    with_database(&state, |database| database.heads_up_settings())
}

#[tauri::command]
pub(crate) fn update_heads_up_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: HeadsUpSettings,
) -> Result<HeadsUpSettings, CommandError> {
    let saved = with_database(&state, |database| {
        database.update_heads_up_settings(settings, Utc::now())
    })?;
    let _ = app.emit(HEADS_UPS_CHANGED, ());
    Ok(saved)
}

/// Checks for heads-ups every few minutes, tells the screen when they change, and sends a system
/// notification for an urgent one when that is allowed.
pub(crate) async fn heads_up_worker(app: AppHandle) {
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let mut interval = tokio::time::interval(CHECK_EVERY);
    loop {
        interval.tick().await;
        let now = Utc::now();
        let zone = system_zone();
        let (changed, notify) = {
            let state = app.state::<AppState>();
            let Ok(mut runtime) = state.database.lock() else {
                continue;
            };
            let Some(database) = runtime.database.as_mut() else {
                continue;
            };
            let Ok(settings) = database.heads_up_settings() else {
                continue;
            };
            if !settings.enabled {
                continue;
            }
            let Ok(facts) = collect(database, &state.calendars, now, zone) else {
                continue;
            };
            let found = detect(&facts);
            let Ok(outcome) = database.record_heads_ups(&found, now, day_start(now, zone)) else {
                continue;
            };
            let notify = if settings.notify && !quiet_hours(&facts) {
                database
                    .heads_ups_to_notify(now, day_start(now, zone))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            (outcome.changed, notify)
        };
        if changed {
            let _ = app.emit(HEADS_UPS_CHANGED, ());
        }
        for heads_up in notify {
            let delivered = app
                .notification()
                .builder()
                .title(&heads_up.title)
                .body(&heads_up.evidence)
                .show()
                .is_ok();
            if delivered {
                let state = app.state::<AppState>();
                let Ok(mut runtime) = state.database.lock() else {
                    continue;
                };
                if let Some(database) = runtime.database.as_mut() {
                    let _ = database.mark_heads_up_notified(&heads_up.id, now);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
