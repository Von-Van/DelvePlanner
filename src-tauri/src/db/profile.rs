//! What Delve Planner knows about how the user plans: the profile they set themselves, and the
//! local history observations are computed from. Both stay on the device, neither is exported, and
//! everything here can be turned off or cleared.

use super::{now, validate_revision, PlannerDatabase};
use crate::error::{AppError, AppResult};
use crate::model::{
    DayPreference, Observation, ObservationKind, PlanningProfile, UpdatePlanningProfileInput,
};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashMap;

/// The longest a single stretch of planned work can be, matching a time block's limit.
const MAX_FOCUS_MINUTES: i64 = 1440;

impl PlannerDatabase {
    pub fn planning_profile(&self) -> AppResult<PlanningProfile> {
        Ok(self.connection.query_row(
            "SELECT preferred_start_minute, preferred_end_minute, max_planned_minutes,
                    focus_minutes, break_minutes, no_work_days, energy, muted_observations,
                    revision, updated_at
             FROM planning_profile WHERE id = 1",
            [],
            profile_from_row,
        )?)
    }

    pub fn update_planning_profile(
        &mut self,
        input: UpdatePlanningProfileInput,
    ) -> AppResult<PlanningProfile> {
        validate_revision(input.revision)?;
        let days = validate_profile(&input)?;
        let muted = stored_kinds(&input.muted_observations);
        let changed = self.connection.execute(
            "UPDATE planning_profile
             SET preferred_start_minute = ?1, preferred_end_minute = ?2, max_planned_minutes = ?3,
                 focus_minutes = ?4, break_minutes = ?5, no_work_days = ?6, energy = ?7,
                 muted_observations = ?8, revision = revision + 1, updated_at = ?9
             WHERE id = 1 AND revision = ?10",
            params![
                input.preferred_start_minute,
                input.preferred_end_minute,
                input.max_planned_minutes,
                input.focus_minutes,
                input.break_minutes,
                days,
                input.energy.map(|energy| energy.as_str()),
                muted,
                now(),
                input.revision
            ],
        )?;
        if changed == 0 {
            return Err(AppError::Conflict);
        }
        self.planning_profile()
    }

    /// Forgets the profile, leaving it as empty as it was before anything was set.
    pub fn clear_planning_profile(&mut self) -> AppResult<PlanningProfile> {
        self.connection.execute(
            "UPDATE planning_profile
             SET preferred_start_minute = NULL, preferred_end_minute = NULL,
                 max_planned_minutes = NULL, focus_minutes = NULL, break_minutes = NULL,
                 no_work_days = '', energy = NULL, muted_observations = '',
                 revision = revision + 1, updated_at = ?1
             WHERE id = 1",
            params![now()],
        )?;
        self.planning_profile()
    }
}

/// Checks every value the user can set, and returns the no-work days in stored form.
fn validate_profile(input: &UpdatePlanningProfileInput) -> AppResult<String> {
    for minute in [input.preferred_start_minute, input.preferred_end_minute]
        .into_iter()
        .flatten()
    {
        if !(0..=1440).contains(&minute) {
            return Err(AppError::Validation(
                "Preferred hours must be times of day.".into(),
            ));
        }
    }
    if let (Some(start), Some(end)) = (input.preferred_start_minute, input.preferred_end_minute) {
        if end <= start {
            return Err(AppError::Validation(
                "Preferred hours must end after they start.".into(),
            ));
        }
    }
    for (value, what) in [
        (input.max_planned_minutes, "A day's most planned work"),
        (input.focus_minutes, "A focus block"),
        (input.break_minutes, "A break"),
    ]
    .into_iter()
    .filter_map(|(value, what)| value.map(|value| (value, what)))
    {
        if !(1..=MAX_FOCUS_MINUTES).contains(&value) {
            return Err(AppError::Validation(format!(
                "{what} must be between 1 minute and 24 hours."
            )));
        }
    }
    let mut days = input.no_work_days.clone();
    days.sort_unstable();
    days.dedup();
    if days.iter().any(|day| !(1..=7).contains(day)) {
        return Err(AppError::Validation("Days off must be weekdays.".into()));
    }
    if days.len() == 7 {
        return Err(AppError::Validation(
            "Leave at least one day to work on.".into(),
        ));
    }
    Ok(days
        .iter()
        .map(|day| day.to_string())
        .collect::<Vec<_>>()
        .join(","))
}

/// The observation kinds the user switched off, in stored form.
fn stored_kinds(kinds: &[ObservationKind]) -> String {
    let mut stored: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
    stored.sort_unstable();
    stored.dedup();
    stored.join(",")
}

fn profile_from_row(row: &Row<'_>) -> rusqlite::Result<PlanningProfile> {
    let days: String = row.get(5)?;
    let energy: Option<String> = row.get(6)?;
    let muted: String = row.get(7)?;
    Ok(PlanningProfile {
        preferred_start_minute: row.get(0)?,
        preferred_end_minute: row.get(1)?,
        max_planned_minutes: row.get(2)?,
        focus_minutes: row.get(3)?,
        break_minutes: row.get(4)?,
        no_work_days: days
            .split(',')
            .filter_map(|day| day.trim().parse().ok())
            .collect(),
        energy: energy.as_deref().and_then(DayPreference::parse),
        muted_observations: muted
            .split(',')
            .filter_map(|kind| ObservationKind::parse(kind.trim()))
            .collect(),
        revision: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// Schema 7: one planning-profile record, and the history observations are drawn from. The
/// history starts empty and only fills once this version is installed, so nothing was recorded
/// before the user could see it.
pub(super) fn ensure_profile_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS planning_profile (
             id INTEGER PRIMARY KEY NOT NULL CHECK(id = 1),
             preferred_start_minute INTEGER CHECK(preferred_start_minute BETWEEN 0 AND 1440),
             preferred_end_minute INTEGER CHECK(preferred_end_minute BETWEEN 0 AND 1440),
             max_planned_minutes INTEGER CHECK(max_planned_minutes BETWEEN 1 AND 1440),
             focus_minutes INTEGER CHECK(focus_minutes BETWEEN 1 AND 1440),
             break_minutes INTEGER CHECK(break_minutes BETWEEN 1 AND 1440),
             no_work_days TEXT NOT NULL DEFAULT '',
             energy TEXT,
             muted_observations TEXT NOT NULL DEFAULT '',
             revision INTEGER NOT NULL DEFAULT 1,
             updated_at TEXT NOT NULL,
             CHECK(preferred_end_minute IS NULL OR preferred_start_minute IS NULL
                   OR preferred_end_minute > preferred_start_minute)
         );
         CREATE TABLE IF NOT EXISTS task_moves (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
             from_day TEXT,
             to_day TEXT,
             kind TEXT NOT NULL CHECK(kind IN ('carried_forward', 'moved', 'released')),
             moved_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS task_moves_at_idx ON task_moves(moved_at);",
    )?;
    connection.execute(
        "INSERT OR IGNORE INTO planning_profile (id, updated_at) VALUES (1, ?1)",
        params![now()],
    )?;
    Ok(())
}

impl PlannerDatabase {
    /// What Delve Planner has noticed, computed fresh from local records each time it is asked.
    /// Nothing is stored, so deleting the data behind an observation makes it go away on its own.
    pub fn observations(&self) -> AppResult<Vec<Observation>> {
        let muted = self.planning_profile()?.muted_observations;
        let wanted = |kind: ObservationKind| !muted.contains(&kind);
        let mut found = Vec::new();
        if wanted(ObservationKind::EstimateAccuracy) {
            found.extend(self.estimate_accuracy()?);
        }
        if wanted(ObservationKind::UsualStart) {
            found.extend(self.usual_start()?);
        }
        if wanted(ObservationKind::TypicalDailyLoad) {
            found.extend(self.typical_daily_load()?);
        }
        if wanted(ObservationKind::DeferredDays) {
            found.extend(self.deferred_days()?);
        }
        if wanted(ObservationKind::RepeatedlyMoved) {
            found.extend(self.repeatedly_moved()?);
        }
        if wanted(ObservationKind::SlippingPlan) {
            found.extend(self.slipping_plan()?);
        }
        Ok(found)
    }

    /// How an estimate compares with the time actually blocked for the same task, over tasks that
    /// are finished and had both.
    fn estimate_accuracy(&self) -> AppResult<Option<Observation>> {
        let (count, estimated, blocked) = self.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(tasks.estimated_minutes), 0), COALESCE(SUM(blocked), 0)
             FROM tasks
             JOIN (SELECT task_id, SUM(duration_minutes) AS blocked
                   FROM task_blocks GROUP BY task_id) AS blocks ON blocks.task_id = tasks.id
             WHERE tasks.status = 'done' AND tasks.estimated_minutes IS NOT NULL",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        if count < MIN_SAMPLE || estimated == 0 {
            return Ok(None);
        }
        let percent = (blocked - estimated) * 100 / estimated;
        let summary = match percent {
            difference if difference >= 15 => {
                format!("Work takes about {difference}% longer than you estimate.")
            }
            difference if difference <= -15 => format!(
                "Work takes about {}% less than you estimate.",
                difference.abs()
            ),
            _ => "Your estimates are close to the time you block for the work.".into(),
        };
        Ok(Some(Observation {
            kind: ObservationKind::EstimateAccuracy,
            summary,
            evidence: format!(
                "{count} finished {} with both an estimate and blocked time.",
                plural(count, "task")
            ),
            sample: count,
        }))
    }

    /// The hour work is usually blocked for, which is not always the hour the user says they work.
    fn usual_start(&self) -> AppResult<Option<Observation>> {
        let mut statement = self.connection.prepare(
            "SELECT start_at_utc, time_zone FROM task_blocks ORDER BY start_at_utc DESC LIMIT 200",
        )?;
        let starts = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let minutes: Vec<i64> = starts
            .iter()
            .filter_map(|(start, zone)| local_minute_of_day(start, zone))
            .collect();
        if (minutes.len() as i64) < MIN_SAMPLE {
            return Ok(None);
        }
        let average = minutes.iter().sum::<i64>() / minutes.len() as i64;
        Ok(Some(Observation {
            kind: ObservationKind::UsualStart,
            summary: format!("You usually start blocked work around {}.", clock(average)),
            evidence: format!(
                "{} recent time {}.",
                minutes.len(),
                plural(minutes.len() as i64, "block")
            ),
            sample: minutes.len() as i64,
        }))
    }

    /// How much work a day usually carries, from the days that had any blocked at all. A block
    /// counts on the local day it starts, in the zone it was made in: evening work belongs to
    /// that evening, not to the next day in UTC.
    fn typical_daily_load(&self) -> AppResult<Option<Observation>> {
        let mut statement = self
            .connection
            .prepare("SELECT start_at_utc, time_zone, duration_minutes FROM task_blocks")?;
        let blocks = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut minutes_by_day: HashMap<NaiveDate, i64> = HashMap::new();
        for (start, zone, minutes) in &blocks {
            if let Some(local) = local_time(start, zone) {
                *minutes_by_day.entry(local.date()).or_default() += minutes;
            }
        }
        let days = minutes_by_day.len() as i64;
        if days < MIN_SAMPLE {
            return Ok(None);
        }
        let average = minutes_by_day.values().sum::<i64>() / days;
        Ok(Some(Observation {
            kind: ObservationKind::TypicalDailyLoad,
            summary: format!(
                "A day you block work on usually holds about {}.",
                hours(average)
            ),
            evidence: format!("{days} {} with blocked time.", plural(days, "day")),
            sample: days,
        }))
    }

    /// The weekday work is most often carried away from, once there is enough history to say.
    fn deferred_days(&self) -> AppResult<Option<Observation>> {
        let mut statement = self.connection.prepare(
            "SELECT from_day FROM task_moves
             WHERE kind = 'carried_forward' AND from_day IS NOT NULL",
        )?;
        let days = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if (days.len() as i64) < MIN_SAMPLE {
            return Ok(None);
        }
        let mut counts = [0_i64; 7];
        for day in &days {
            if let Some(weekday) = weekday_index(day) {
                counts[weekday] += 1;
            }
        }
        let (index, count) = counts
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .map(|(index, count)| (index, *count))
            .unwrap_or((0, 0));
        if count == 0 {
            return Ok(None);
        }
        Ok(Some(Observation {
            kind: ObservationKind::DeferredDays,
            summary: format!("{} is the day work most often moves off.", WEEKDAYS[index]),
            evidence: format!(
                "{count} of {} carried-forward {}.",
                days.len(),
                plural(days.len() as i64, "task")
            ),
            sample: days.len() as i64,
        }))
    }

    /// How many times work has been carried off a day it was on, the floor every deferral
    /// observation starts from.
    fn carried_forward_count(&self) -> AppResult<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM task_moves WHERE kind = 'carried_forward'",
            [],
            |row| row.get(0),
        )?)
    }

    /// Open work that keeps being carried off its day, which usually means it is too big,
    /// waiting on something, or no longer wanted. Names the task moved most.
    fn repeatedly_moved(&self) -> AppResult<Option<Observation>> {
        let total = self.carried_forward_count()?;
        if total < MIN_SAMPLE {
            return Ok(None);
        }
        let mut statement = self.connection.prepare(
            "SELECT tasks.title, COUNT(*) AS moves
             FROM task_moves JOIN tasks ON tasks.id = task_moves.task_id
             WHERE task_moves.kind = 'carried_forward' AND tasks.status <> 'done'
             GROUP BY tasks.id
             HAVING COUNT(*) >= ?1
             ORDER BY moves DESC, MIN(task_moves.moved_at) ASC",
        )?;
        let repeated = statement
            .query_map(params![REPEATED_MOVES], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let Some((title, most)) = repeated.first() else {
            return Ok(None);
        };
        let their_moves: i64 = repeated.iter().map(|(_, moves)| moves).sum();
        let summary = if repeated.len() == 1 {
            format!("“{title}” has moved off its day {most} times.")
        } else {
            format!(
                "{} open tasks have each moved off their day at least {REPEATED_MOVES} times; “{title}” most, {most} times.",
                repeated.len()
            )
        };
        Ok(Some(Observation {
            kind: ObservationKind::RepeatedlyMoved,
            summary,
            evidence: format!(
                "{their_moves} of {total} carried-forward {}.",
                plural(total, "move")
            ),
            sample: total,
        }))
    }

    /// The plan whose work is carried off its day most, said only when one plan accounts for at
    /// least half of it and there is scheduled work elsewhere it could be compared with.
    fn slipping_plan(&self) -> AppResult<Option<Observation>> {
        let total = self.carried_forward_count()?;
        if total < MIN_SAMPLE {
            return Ok(None);
        }
        let top = self
            .connection
            .query_row(
                "SELECT plans.id, plans.title, COUNT(*) AS moves
                 FROM task_moves
                 JOIN tasks ON tasks.id = task_moves.task_id
                 JOIN plans ON plans.id = tasks.plan_id
                 WHERE task_moves.kind = 'carried_forward' AND plans.archived = 0
                 GROUP BY plans.id
                 ORDER BY moves DESC, plans.title ASC
                 LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((plan_id, title, moves)) = top else {
            return Ok(None);
        };
        // A plan that holds every scheduled task would top this by default, which says nothing.
        let other_work: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE scheduled_day IS NOT NULL AND (plan_id IS NULL OR plan_id <> ?1)",
            params![plan_id],
            |row| row.get(0),
        )?;
        if moves < REPEATED_MOVES || moves * 2 < total || other_work == 0 {
            return Ok(None);
        }
        Ok(Some(Observation {
            kind: ObservationKind::SlippingPlan,
            summary: format!(
                "Work in {title} moves off its day more often than work anywhere else."
            ),
            evidence: format!(
                "{moves} of {total} carried-forward {}.",
                plural(total, "move")
            ),
            sample: total,
        }))
    }

    /// Forgets the history observations are drawn from. The observations go with it.
    pub fn clear_planning_history(&mut self) -> AppResult<()> {
        self.connection.execute("DELETE FROM task_moves", [])?;
        Ok(())
    }
}

/// An instant as local time in the zone the block was made in.
fn local_time(start_at_utc: &str, time_zone: &str) -> Option<NaiveDateTime> {
    let instant: DateTime<Utc> = start_at_utc.parse().ok()?;
    let zone: Tz = time_zone.parse().ok()?;
    Some(instant.with_timezone(&zone).naive_local())
}

/// Minutes after local midnight for an instant, in the zone the block was made in.
fn local_minute_of_day(start_at_utc: &str, time_zone: &str) -> Option<i64> {
    let local = local_time(start_at_utc, time_zone)?.time();
    Some(i64::from(local.hour()) * 60 + i64::from(local.minute()))
}

/// Monday = 0 through Sunday = 6, for a `YYYY-MM-DD` day.
fn weekday_index(day: &str) -> Option<usize> {
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .ok()
        .map(|day| day.weekday().num_days_from_monday() as usize)
}

/// Below this, a pattern is a coincidence and Delve Planner says nothing.
const MIN_SAMPLE: i64 = 5;

/// How many times a task has to be carried off its day before it counts as repeatedly moved.
const REPEATED_MOVES: i64 = 3;

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn plural(count: i64, word: &str) -> String {
    if count == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

/// "09:30", from minutes after local midnight.
fn clock(minute: i64) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// "3 h 30 m", or "45 m" under an hour.
fn hours(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, rest) => format!("{rest} m"),
        (whole, 0) => format!("{whole} h"),
        (whole, rest) => format!("{whole} h {rest} m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CreatePlanInput, CreateTaskBlockInput, CreateTaskInput, DayChange, PlanStatus, Task,
        TaskMove, TaskPriority, TaskStatus, UpdateTaskInput,
    };
    use tempfile::tempdir;

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    #[test]
    fn a_new_profile_is_empty_and_updates_under_a_revision_check() {
        let mut database = database();
        let empty = database.planning_profile().unwrap();
        assert_eq!(empty.preferred_start_minute, None);
        assert_eq!(empty.no_work_days, Vec::<u8>::new());
        assert!(empty.muted_observations.is_empty());

        let filled = database
            .update_planning_profile(UpdatePlanningProfileInput {
                revision: empty.revision,
                preferred_start_minute: Some(540),
                preferred_end_minute: Some(780),
                max_planned_minutes: Some(300),
                focus_minutes: Some(50),
                break_minutes: Some(10),
                no_work_days: vec![7, 6, 6],
                energy: Some(DayPreference::Morning),
                muted_observations: vec![ObservationKind::DeferredDays],
            })
            .unwrap();
        assert_eq!(filled.no_work_days, vec![6, 7], "sorted, without repeats");
        assert_eq!(filled.energy, Some(DayPreference::Morning));
        assert_eq!(
            filled.muted_observations,
            vec![ObservationKind::DeferredDays]
        );
        assert_eq!(filled.revision, empty.revision + 1);

        // The same revision twice is a conflict, as everywhere else.
        assert!(matches!(
            database.update_planning_profile(input(empty.revision)),
            Err(AppError::Conflict)
        ));
        // Nothing may be out of range, and a week with no working day is refused.
        assert!(database
            .update_planning_profile(UpdatePlanningProfileInput {
                preferred_start_minute: Some(600),
                preferred_end_minute: Some(540),
                ..input(filled.revision)
            })
            .is_err());
        assert!(database
            .update_planning_profile(UpdatePlanningProfileInput {
                no_work_days: vec![1, 2, 3, 4, 5, 6, 7],
                ..input(filled.revision)
            })
            .is_err());

        let cleared = database.clear_planning_profile().unwrap();
        assert_eq!(cleared.preferred_start_minute, None);
        assert_eq!(cleared.energy, None);
        assert!(cleared.muted_observations.is_empty());
    }

    fn input(revision: i64) -> UpdatePlanningProfileInput {
        UpdatePlanningProfileInput {
            revision,
            preferred_start_minute: None,
            preferred_end_minute: None,
            max_planned_minutes: None,
            focus_minutes: None,
            break_minutes: None,
            no_work_days: Vec::new(),
            energy: None,
            muted_observations: Vec::new(),
        }
    }

    #[test]
    fn an_observation_needs_enough_behind_it_and_says_what_that_was() {
        let mut database = database();
        assert!(
            database.observations().unwrap().is_empty(),
            "a fresh device has noticed nothing"
        );

        // Four finished tasks is under the threshold; the fifth crosses it.
        for index in 0..5 {
            let task = database
                .create_task(CreateTaskInput {
                    title: format!("Task {index}"),
                    description: String::new(),
                    plan_id: None,
                    milestone_id: None,
                    workstream_id: None,
                    owner_id: None,
                    due_date: None,
                    scheduled_day: Some("2026-09-15".into()),
                    planned_week: None,
                    estimated_minutes: Some(60),
                    status: TaskStatus::Todo,
                    priority: TaskPriority::Normal,
                    checklist: Vec::new(),
                    recurrence: None,
                    waiting_on: Vec::new(),
                })
                .unwrap();
            database
                .create_task_block(CreateTaskBlockInput {
                    task_id: task.id.clone(),
                    start_at_utc: format!("2026-09-1{}T13:00:00.000Z", index + 1),
                    time_zone: "America/New_York".into(),
                    duration_minutes: 90,
                })
                .unwrap();
            database
                .update_task(UpdateTaskInput {
                    id: task.id.clone(),
                    revision: task.revision,
                    title: task.title.clone(),
                    description: String::new(),
                    plan_id: None,
                    milestone_id: None,
                    workstream_id: None,
                    owner_id: None,
                    due_date: None,
                    scheduled_day: task.scheduled_day.clone(),
                    planned_week: None,
                    estimated_minutes: task.estimated_minutes,
                    status: TaskStatus::Done,
                    priority: TaskPriority::Normal,
                    checklist: Vec::new(),
                    recurrence: None,
                    waiting_on: Vec::new(),
                })
                .unwrap();
            if index < 3 {
                assert!(
                    !database
                        .observations()
                        .unwrap()
                        .iter()
                        .any(|observation| observation.kind == ObservationKind::EstimateAccuracy),
                    "too little to say anything yet"
                );
            }
        }

        let observations = database.observations().unwrap();
        let estimates = observations
            .iter()
            .find(|observation| observation.kind == ObservationKind::EstimateAccuracy)
            .expect("an estimate observation");
        // 90 blocked against 60 estimated is half again as long.
        assert!(estimates.summary.contains("50%"), "{}", estimates.summary);
        assert_eq!(estimates.sample, 5);
        assert!(estimates.evidence.contains("5 finished tasks"));

        // Switching it off takes it away without touching anything it was drawn from.
        let profile = database.planning_profile().unwrap();
        database
            .update_planning_profile(UpdatePlanningProfileInput {
                muted_observations: vec![ObservationKind::EstimateAccuracy],
                ..input(profile.revision)
            })
            .unwrap();
        assert!(!database
            .observations()
            .unwrap()
            .iter()
            .any(|observation| observation.kind == ObservationKind::EstimateAccuracy));
    }

    fn task_on(database: &mut PlannerDatabase, title: &str, plan_id: Option<&str>) -> Task {
        database
            .create_task(CreateTaskInput {
                title: title.into(),
                description: String::new(),
                plan_id: plan_id.map(Into::into),
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: Some("2026-09-14".into()),
                planned_week: None,
                estimated_minutes: None,
                status: TaskStatus::Todo,
                priority: TaskPriority::Normal,
                checklist: Vec::new(),
                recurrence: None,
                waiting_on: Vec::new(),
            })
            .unwrap()
    }

    /// Carries a task forward one day at a time, as Today's carry-forward does.
    fn carry(database: &mut PlannerDatabase, task: &Task, times: usize) -> Task {
        let mut current = task.clone();
        for _ in 0..times {
            let day = crate::db::offset_day(current.scheduled_day.as_deref().unwrap(), 1).unwrap();
            current = database
                .move_tasks(vec![TaskMove {
                    id: current.id.clone(),
                    revision: current.revision,
                    scheduled_day: DayChange::Set { day },
                    planned_week: DayChange::Unchanged,
                    status: None,
                }])
                .unwrap()
                .remove(0);
        }
        current
    }

    fn observation(database: &PlannerDatabase, kind: ObservationKind) -> Option<Observation> {
        database
            .observations()
            .unwrap()
            .into_iter()
            .find(|observation| observation.kind == kind)
    }

    #[test]
    fn a_day_of_blocked_work_is_the_local_day_it_started_on() {
        let mut database = database();
        let task = task_on(&mut database, "Write", None);
        // Five New York days, each with an hour at 10:00 and an hour at 21:00, which is already
        // the next day in UTC. Counted by UTC date this would be six days averaging 1 h 40 m.
        for day in 14..19 {
            for start in [
                format!("2026-09-{day}T14:00:00.000Z"),
                format!("2026-09-{}T01:00:00.000Z", day + 1),
            ] {
                database
                    .create_task_block(CreateTaskBlockInput {
                        task_id: task.id.clone(),
                        start_at_utc: start,
                        time_zone: "America/New_York".into(),
                        duration_minutes: 60,
                    })
                    .unwrap();
            }
        }
        let load = observation(&database, ObservationKind::TypicalDailyLoad).expect("a load");
        assert!(load.summary.contains("about 2 h."), "{}", load.summary);
        assert_eq!(load.evidence, "5 days with blocked time.");
    }

    #[test]
    fn work_that_keeps_moving_is_named_once_there_is_enough_history() {
        let mut database = database();
        let plan = database
            .create_plan(CreatePlanInput {
                title: "Wedding".into(),
                description: String::new(),
                status: PlanStatus::Active,
                start_date: None,
                target_date: None,
                color: None,
                links: Vec::new(),
            })
            .unwrap();
        let venue = task_on(&mut database, "Book the venue", Some(&plan.id));
        let cake = task_on(&mut database, "Taste cakes", Some(&plan.id));
        let errand = task_on(&mut database, "Return library books", None);

        carry(&mut database, &venue, 2);
        carry(&mut database, &errand, 1);
        assert!(
            observation(&database, ObservationKind::RepeatedlyMoved).is_none(),
            "three moves is under the floor"
        );

        let venue = task_by_title(&database, "Book the venue");
        carry(&mut database, &venue, 2);
        let moved = observation(&database, ObservationKind::RepeatedlyMoved).expect("moved work");
        assert_eq!(
            moved.summary,
            "“Book the venue” has moved off its day 4 times."
        );
        assert_eq!(moved.evidence, "4 of 5 carried-forward moves.");

        // Wedding holds four of the five moves, and there is other scheduled work to compare.
        let slipping = observation(&database, ObservationKind::SlippingPlan).expect("a plan");
        assert_eq!(
            slipping.summary,
            "Work in Wedding moves off its day more often than work anywhere else."
        );
        assert_eq!(slipping.evidence, "4 of 5 carried-forward moves.");

        // A second task crossing the line changes the wording; finished work drops out.
        carry(&mut database, &cake, 3);
        let moved = observation(&database, ObservationKind::RepeatedlyMoved).unwrap();
        assert!(
            moved
                .summary
                .starts_with("2 open tasks have each moved off their day at least 3 times"),
            "{}",
            moved.summary
        );
        let venue = task_by_title(&database, "Book the venue");
        database
            .update_task(UpdateTaskInput {
                status: TaskStatus::Done,
                ..UpdateTaskInput::keeping(&venue)
            })
            .unwrap();
        let moved = observation(&database, ObservationKind::RepeatedlyMoved).unwrap();
        assert_eq!(
            moved.summary,
            "“Taste cakes” has moved off its day 3 times."
        );
    }

    #[test]
    fn a_plan_holding_all_the_scheduled_work_is_not_called_out() {
        let mut database = database();
        let plan = database
            .create_plan(CreatePlanInput {
                title: "Move".into(),
                description: String::new(),
                status: PlanStatus::Active,
                start_date: None,
                target_date: None,
                color: None,
                links: Vec::new(),
            })
            .unwrap();
        let task = task_on(&mut database, "Pack the kitchen", Some(&plan.id));
        carry(&mut database, &task, 5);
        assert!(observation(&database, ObservationKind::RepeatedlyMoved).is_some());
        assert!(
            observation(&database, ObservationKind::SlippingPlan).is_none(),
            "with nothing scheduled elsewhere there is nothing to compare"
        );
    }

    fn task_by_title(database: &PlannerDatabase, title: &str) -> Task {
        database
            .planning_board("2026-09-14")
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.title == title)
            .unwrap()
    }
}
