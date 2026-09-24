//! Time blocks and working hours. A block reserves time for a task and is planned work, not an
//! event; working hours are the time capacity counts as available.

use super::planning::{stale_write_error, task_by_id, task_from_row, TASK_SELECT};
use super::{
    collect, day_bounds, now, validate_id, validate_revision, validate_time, PlannerDatabase,
    SqlConnection,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    CreateTaskBlockInput, RecordVersion, ScheduleEvent, ScheduledBlock, Task, TaskBlock,
    TaskStatus, UpdateTaskBlockInput, UpdateWorkingHoursInput, WorkingHours, MAX_BLOCK_CHANGES,
};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) const BLOCK_SELECT: &str = "SELECT id, task_id, start_at_utc, time_zone,
        duration_minutes, revision, created_at, updated_at
     FROM task_blocks";
const DEFAULT_WORK_DAYS: &str = "1,2,3,4,5";
const DEFAULT_START_MINUTE: i64 = 9 * 60;
const DEFAULT_END_MINUTE: i64 = 17 * 60;

/// The planner facts capacity is computed from, for a window of local days.
pub struct CapacityFacts {
    pub working_hours: WorkingHours,
    pub events: Vec<ScheduleEvent>,
    pub blocks: Vec<TaskBlock>,
    /// Open tasks scheduled on a day in the window.
    pub scheduled: Vec<Task>,
    /// Open tasks chosen for a week that starts in the window, with no day yet.
    pub pooled: Vec<Task>,
}

impl PlannerDatabase {
    pub fn working_hours(&self) -> AppResult<WorkingHours> {
        self.connection
            .query_row(
                "SELECT days, start_minute, end_minute, revision, updated_at
                 FROM working_hours WHERE id = 1",
                [],
                working_hours_from_row,
            )
            .optional()?
            .ok_or_else(|| AppError::Internal("Working hours are unavailable.".into()))
    }

    pub fn update_working_hours(
        &mut self,
        input: UpdateWorkingHoursInput,
    ) -> AppResult<WorkingHours> {
        validate_revision(input.revision)?;
        let days = validate_working_hours(&input)?;
        let changed = self.connection.execute(
            "UPDATE working_hours SET days = ?1, start_minute = ?2, end_minute = ?3,
                 revision = revision + 1, updated_at = ?4
             WHERE id = 1 AND revision = ?5",
            params![
                days,
                input.start_minute,
                input.end_minute,
                now(),
                input.revision
            ],
        )?;
        if changed != 1 {
            return Err(AppError::Conflict);
        }
        self.working_hours()
    }

    pub fn create_task_block(&mut self, input: CreateTaskBlockInput) -> AppResult<ScheduledBlock> {
        validate_id(&input.task_id)?;
        let (start_at_utc, time_zone) = validate_block_shape(
            &input.start_at_utc,
            &input.time_zone,
            input.duration_minutes,
        )?;
        let task = task_by_id(&self.connection, &input.task_id)?.ok_or(AppError::NotFound)?;
        if task.status == TaskStatus::Done {
            return Err(AppError::Validation(
                "A finished task can't take a new time block.".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        self.connection.execute(
            "INSERT INTO task_blocks
             (id, task_id, start_at_utc, time_zone, duration_minutes, revision, created_at,
              updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            params![
                id,
                task.id,
                start_at_utc,
                time_zone,
                input.duration_minutes,
                timestamp
            ],
        )?;
        let block = block_by_id(&self.connection, &id)?.ok_or(AppError::NotFound)?;
        Ok(ScheduledBlock { block, task })
    }

    /// Moves or resizes a block. Its task is never changed.
    pub fn update_task_block(&mut self, input: UpdateTaskBlockInput) -> AppResult<ScheduledBlock> {
        validate_id(&input.id)?;
        validate_revision(input.revision)?;
        let (start_at_utc, time_zone) = validate_block_shape(
            &input.start_at_utc,
            &input.time_zone,
            input.duration_minutes,
        )?;
        let changed = self.connection.execute(
            "UPDATE task_blocks SET start_at_utc = ?1, time_zone = ?2, duration_minutes = ?3,
                 revision = revision + 1, updated_at = ?4
             WHERE id = ?5 AND revision = ?6",
            params![
                start_at_utc,
                time_zone,
                input.duration_minutes,
                now(),
                input.id,
                input.revision
            ],
        )?;
        if changed != 1 {
            return Err(stale_write_error(
                &self.connection,
                "task_blocks",
                &input.id,
            )?);
        }
        let block = block_by_id(&self.connection, &input.id)?.ok_or(AppError::NotFound)?;
        let task = task_by_id(&self.connection, &block.task_id)?.ok_or(AppError::NotFound)?;
        Ok(ScheduledBlock { block, task })
    }

    pub fn delete_task_block(&mut self, id: &str, revision: i64) -> AppResult<()> {
        self.delete_task_blocks(vec![RecordVersion {
            id: id.into(),
            revision,
        }])
    }

    /// Deletes every listed block in one transaction, or none if any changed or is gone.
    pub fn delete_task_blocks(&mut self, versions: Vec<RecordVersion>) -> AppResult<()> {
        if versions.is_empty() || versions.len() > MAX_BLOCK_CHANGES {
            return Err(AppError::Validation(format!(
                "Change between 1 and {MAX_BLOCK_CHANGES} time blocks at a time."
            )));
        }
        let mut seen = HashSet::new();
        for version in &versions {
            validate_id(&version.id)?;
            validate_revision(version.revision)?;
            if !seen.insert(version.id.as_str()) {
                return Err(AppError::Validation(
                    "A time block can only be listed once.".into(),
                ));
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for version in &versions {
            let changed = transaction.execute(
                "DELETE FROM task_blocks WHERE id = ?1 AND revision = ?2",
                params![version.id, version.revision],
            )?;
            if changed != 1 {
                return Err(stale_write_error(&transaction, "task_blocks", &version.id)?);
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Every block of one task, earliest first.
    pub fn task_blocks(&self, task_id: &str) -> AppResult<Vec<TaskBlock>> {
        validate_id(task_id)?;
        let mut statement = self.connection.prepare(&format!(
            "{BLOCK_SELECT} WHERE task_id = ?1 ORDER BY start_at_utc ASC"
        ))?;
        let rows = statement.query_map(params![task_id], block_from_row)?;
        collect(rows)
    }

    /// Blocks that start after `now` and belong to finished tasks: time Delve Planner offers to
    /// give back. Past blocks are kept as a record of the work.
    pub fn releasable_blocks(&self) -> AppResult<Vec<ScheduledBlock>> {
        let mut statement = self.connection.prepare(&format!(
            "{BLOCK_SELECT}
             WHERE julianday(start_at_utc) > julianday(?1)
               AND task_id IN (SELECT id FROM tasks WHERE status = 'done')
             ORDER BY start_at_utc ASC"
        ))?;
        let rows = statement.query_map(params![now()], block_from_row)?;
        self.with_tasks(collect(rows)?)
    }

    /// Blocks overlapping the local days `[start_day, end_day)` in `time_zone`, with their tasks.
    pub(super) fn blocks_between(
        &self,
        start_day: &str,
        end_day: &str,
        time_zone: &str,
    ) -> AppResult<Vec<ScheduledBlock>> {
        self.with_tasks(self.block_records_between(start_day, end_day, time_zone)?)
    }

    pub(super) fn all_task_blocks(&self) -> AppResult<Vec<TaskBlock>> {
        let mut statement = self.connection.prepare(&format!(
            "{BLOCK_SELECT} ORDER BY start_at_utc ASC, created_at ASC"
        ))?;
        let rows = statement.query_map([], block_from_row)?;
        collect(rows)
    }

    /// Events, blocks, open scheduled tasks, and open week-pool tasks for the local days
    /// `[first_day, end_day)`. `excluded_block` leaves out a block being moved, so its own current
    /// time doesn't count as taken.
    pub fn capacity_facts(
        &self,
        first_day: &str,
        end_day: &str,
        time_zone: &str,
        excluded_block: Option<&str>,
    ) -> AppResult<CapacityFacts> {
        let open_tasks = |condition: &str| -> AppResult<Vec<Task>> {
            let mut statement = self.connection.prepare(&format!(
                "{TASK_SELECT} WHERE status <> 'done' AND {condition}
                 ORDER BY sort_order ASC, created_at ASC"
            ))?;
            let rows = statement.query_map(params![first_day, end_day], task_from_row)?;
            collect(rows)
        };
        Ok(CapacityFacts {
            working_hours: self.working_hours()?,
            events: self.events_between(first_day, end_day, time_zone)?,
            blocks: self
                .block_records_between(first_day, end_day, time_zone)?
                .into_iter()
                .filter(|block| Some(block.id.as_str()) != excluded_block)
                .collect(),
            scheduled: open_tasks("scheduled_day >= ?1 AND scheduled_day < ?2")?,
            pooled: open_tasks(
                "scheduled_day IS NULL AND planned_week >= ?1 AND planned_week < ?2",
            )?,
        })
    }

    fn block_records_between(
        &self,
        start_day: &str,
        end_day: &str,
        time_zone: &str,
    ) -> AppResult<Vec<TaskBlock>> {
        let (start, _) = day_bounds(start_day, time_zone)?;
        let (end, _) = day_bounds(end_day, time_zone)?;
        let mut statement = self.connection.prepare(&format!(
            "{BLOCK_SELECT}
             WHERE julianday(start_at_utc) < julianday(?2)
               AND julianday(start_at_utc, printf('+%d minutes', duration_minutes))
                   > julianday(?1)
             ORDER BY start_at_utc ASC, created_at ASC"
        ))?;
        let rows = statement.query_map(params![start, end], block_from_row)?;
        collect(rows)
    }

    fn with_tasks(&self, blocks: Vec<TaskBlock>) -> AppResult<Vec<ScheduledBlock>> {
        blocks
            .into_iter()
            .map(|block| {
                let task =
                    task_by_id(&self.connection, &block.task_id)?.ok_or(AppError::NotFound)?;
                Ok(ScheduledBlock { block, task })
            })
            .collect()
    }
}

/// Schema 6: time blocks for tasks, and one working-hours record defaulting to 09:00–17:00,
/// Monday through Friday.
pub(super) fn ensure_availability_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_blocks (
             id TEXT PRIMARY KEY NOT NULL,
             task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
             start_at_utc TEXT NOT NULL,
             time_zone TEXT NOT NULL,
             duration_minutes INTEGER NOT NULL CHECK(duration_minutes BETWEEN 5 AND 1440),
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS task_blocks_start_idx ON task_blocks(start_at_utc);
         CREATE INDEX IF NOT EXISTS task_blocks_task_idx ON task_blocks(task_id);
         CREATE TABLE IF NOT EXISTS working_hours (
             id INTEGER PRIMARY KEY NOT NULL CHECK(id = 1),
             days TEXT NOT NULL,
             start_minute INTEGER NOT NULL CHECK(start_minute BETWEEN 0 AND 1439),
             end_minute INTEGER NOT NULL CHECK(end_minute BETWEEN 1 AND 1440),
             revision INTEGER NOT NULL DEFAULT 1,
             updated_at TEXT NOT NULL,
             CHECK(end_minute > start_minute)
         );",
    )?;
    connection.execute(
        "INSERT OR IGNORE INTO working_hours (id, days, start_minute, end_minute, updated_at)
         VALUES (1, ?1, ?2, ?3, ?4)",
        params![
            DEFAULT_WORK_DAYS,
            DEFAULT_START_MINUTE,
            DEFAULT_END_MINUTE,
            now()
        ],
    )?;
    Ok(())
}

/// Checks a block's start, zone, and length, returning the canonical start and zone.
pub(super) fn validate_block_shape(
    start_at_utc: &str,
    time_zone: &str,
    duration_minutes: i64,
) -> AppResult<(String, String)> {
    if !(5..=1440).contains(&duration_minutes) {
        return Err(AppError::Validation(
            "Time blocks must be between 5 minutes and 24 hours.".into(),
        ));
    }
    validate_time(start_at_utc, time_zone)
}

/// Checks working hours and returns their days in stored form, such as "1,2,3,4,5".
fn validate_working_hours(input: &UpdateWorkingHoursInput) -> AppResult<String> {
    let mut days = input.days.clone();
    days.sort_unstable();
    days.dedup();
    if days.is_empty() || days.len() != input.days.len() {
        return Err(AppError::Validation(
            "Choose each working day once, and at least one.".into(),
        ));
    }
    if days.iter().any(|day| !(1..=7).contains(day)) {
        return Err(AppError::Validation(
            "Working days run from Monday (1) to Sunday (7).".into(),
        ));
    }
    if !(0..1440).contains(&input.start_minute)
        || !(1..=1440).contains(&input.end_minute)
        || input.end_minute <= input.start_minute
    {
        return Err(AppError::Validation(
            "Working hours must end after they start, within one day.".into(),
        ));
    }
    Ok(days.iter().map(u8::to_string).collect::<Vec<_>>().join(","))
}

fn working_hours_from_row(row: &Row<'_>) -> rusqlite::Result<WorkingHours> {
    let days = row.get::<_, String>(0)?;
    Ok(WorkingHours {
        days: days
            .split(',')
            .filter_map(|day| day.trim().parse().ok())
            .collect(),
        start_minute: row.get(1)?,
        end_minute: row.get(2)?,
        revision: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

pub(super) fn block_by_id<C: SqlConnection>(
    connection: &C,
    id: &str,
) -> AppResult<Option<TaskBlock>> {
    connection
        .connection()
        .query_row(
            &format!("{BLOCK_SELECT} WHERE id = ?1"),
            params![id],
            block_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn block_from_row(row: &Row<'_>) -> rusqlite::Result<TaskBlock> {
    Ok(TaskBlock {
        id: row.get(0)?,
        task_id: row.get(1)?,
        start_at_utc: row.get(2)?,
        time_zone: row.get(3)?,
        duration_minutes: row.get(4)?,
        revision: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CreateTaskInput, TaskPriority, UpdateTaskInput};
    use chrono::{Duration, Utc};
    use tempfile::tempdir;

    const ZONE: &str = "America/New_York";

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn task(database: &mut PlannerDatabase, title: &str) -> Task {
        database
            .create_task(CreateTaskInput {
                title: title.into(),
                description: String::new(),
                plan_id: None,
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: Some("2030-11-12".into()),
                planned_week: None,
                estimated_minutes: Some(90),
                status: TaskStatus::Todo,
                priority: TaskPriority::Normal,
                checklist: Vec::new(),
                recurrence: None,
                waiting_on: Vec::new(),
            })
            .unwrap()
    }

    fn block(task: &Task, start: &str, minutes: i64) -> CreateTaskBlockInput {
        CreateTaskBlockInput {
            task_id: task.id.clone(),
            start_at_utc: start.into(),
            time_zone: ZONE.into(),
            duration_minutes: minutes,
        }
    }

    fn future(hours: i64) -> String {
        super::super::utc_string(Utc::now() + Duration::hours(hours))
    }

    #[test]
    fn blocks_move_and_delete_without_changing_their_task() {
        let mut database = database();
        let outline = task(&mut database, "Write outline");
        let created = database
            .create_task_block(block(&outline, "2030-11-12T14:00:00Z", 60))
            .unwrap();
        assert_eq!(created.block.start_at_utc, "2030-11-12T14:00:00.000Z");
        assert_eq!(created.task.revision, outline.revision);

        let moved = database
            .update_task_block(UpdateTaskBlockInput {
                id: created.block.id.clone(),
                revision: 1,
                start_at_utc: "2030-11-13T15:30:00Z".into(),
                time_zone: ZONE.into(),
                duration_minutes: 45,
            })
            .unwrap();
        assert_eq!(
            (moved.block.revision, moved.block.duration_minutes),
            (2, 45)
        );
        assert_eq!(moved.task, outline, "moving a block never touches the task");
        assert!(matches!(
            database.delete_task_block(&created.block.id, 1),
            Err(AppError::Conflict)
        ));
        database.delete_task_block(&created.block.id, 2).unwrap();
        assert!(matches!(
            database.delete_task_block(&created.block.id, 2),
            Err(AppError::NotFound)
        ));
        assert_eq!(
            database.tasks_for_day("2030-11-12").unwrap()[0].revision,
            outline.revision
        );
        for (start, minutes) in [
            ("2030-11-12T14:00:00-05:00", 60),
            ("2030-11-12T14:00:00Z", 4),
        ] {
            assert!(matches!(
                database.create_task_block(block(&outline, start, minutes)),
                Err(AppError::Validation(_))
            ));
        }
    }

    #[test]
    fn finished_tasks_take_no_new_blocks_and_deleted_tasks_take_theirs_along() {
        let mut database = database();
        let outline = task(&mut database, "Write outline");
        database
            .create_task_block(block(&outline, "2030-11-12T14:00:00Z", 60))
            .unwrap();
        database
            .create_task_block(block(&outline, "2030-11-14T14:00:00Z", 60))
            .unwrap();
        assert_eq!(database.task_blocks(&outline.id).unwrap().len(), 2);
        let done = database
            .update_task(UpdateTaskInput {
                id: outline.id.clone(),
                revision: outline.revision,
                title: outline.title.clone(),
                description: String::new(),
                plan_id: None,
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: outline.scheduled_day.clone(),
                planned_week: None,
                estimated_minutes: outline.estimated_minutes,
                status: TaskStatus::Done,
                priority: TaskPriority::Normal,
                checklist: Vec::new(),
                recurrence: None,
                waiting_on: Vec::new(),
            })
            .unwrap();
        assert!(matches!(
            database.create_task_block(block(&done, "2030-11-15T14:00:00Z", 60)),
            Err(AppError::Validation(_))
        ));
        database.delete_task(&done.id, done.revision).unwrap();
        assert!(database.all_task_blocks().unwrap().is_empty());
    }

    #[test]
    fn releasable_blocks_are_future_blocks_of_finished_tasks_released_together() {
        let mut database = database();
        let finished = task(&mut database, "Finished");
        let open = task(&mut database, "Still open");
        let past = database
            .create_task_block(block(&finished, &future(-3), 60))
            .unwrap();
        let later = database
            .create_task_block(block(&finished, &future(20), 60))
            .unwrap();
        let later_again = database
            .create_task_block(block(&finished, &future(44), 30))
            .unwrap();
        database
            .create_task_block(block(&open, &future(20), 60))
            .unwrap();
        assert!(database.releasable_blocks().unwrap().is_empty());
        database
            .connection
            .execute(
                "UPDATE tasks SET status = 'done', completed_at = ?1 WHERE id = ?2",
                params![now(), finished.id],
            )
            .unwrap();
        let releasable = database.releasable_blocks().unwrap();
        assert_eq!(
            releasable
                .iter()
                .map(|scheduled| scheduled.block.id.as_str())
                .collect::<Vec<_>>(),
            [later.block.id.as_str(), later_again.block.id.as_str()]
        );
        assert!(matches!(
            database.delete_task_blocks(vec![
                RecordVersion {
                    id: later.block.id.clone(),
                    revision: 1
                },
                RecordVersion {
                    id: later_again.block.id.clone(),
                    revision: 9
                },
            ]),
            Err(AppError::Conflict)
        ));
        assert_eq!(
            database.releasable_blocks().unwrap().len(),
            2,
            "nothing was released"
        );
        database
            .delete_task_blocks(
                releasable
                    .iter()
                    .map(|scheduled| RecordVersion {
                        id: scheduled.block.id.clone(),
                        revision: scheduled.block.revision,
                    })
                    .collect(),
            )
            .unwrap();
        assert_eq!(
            database
                .task_blocks(&finished.id)
                .unwrap()
                .into_iter()
                .map(|kept| kept.id)
                .collect::<Vec<_>>(),
            [past.block.id],
            "the past block stays as a record"
        );
    }

    #[test]
    fn agenda_lists_blocks_that_overlap_the_day() {
        let mut database = database();
        let outline = task(&mut database, "Write outline");
        database
            .create_task_block(block(&outline, "2030-11-12T03:00:00Z", 60))
            .unwrap();
        database
            .create_task_block(block(&outline, "2030-11-13T04:30:00Z", 60))
            .unwrap();
        let agenda = database.agenda("2030-11-12", 1, ZONE).unwrap();
        assert_eq!(agenda.blocks.len(), 1);
        assert_eq!(agenda.blocks[0].task.title, "Write outline");
        assert_eq!(
            agenda.blocks[0].block.start_at_utc, "2030-11-13T04:30:00.000Z",
            "23:30 on the 12th in New York"
        );
    }

    #[test]
    fn working_hours_default_and_update_under_a_revision_check() {
        let mut database = database();
        let defaults = database.working_hours().unwrap();
        assert_eq!(
            (
                defaults.days.clone(),
                defaults.start_minute,
                defaults.end_minute,
                defaults.revision
            ),
            (vec![1, 2, 3, 4, 5], 540, 1020, 1)
        );
        let input = |revision, days: Vec<u8>, start, end| UpdateWorkingHoursInput {
            revision,
            days,
            start_minute: start,
            end_minute: end,
        };
        let updated = database
            .update_working_hours(input(1, vec![6, 1, 2], 480, 1440))
            .unwrap();
        assert_eq!(
            (updated.days, updated.end_minute, updated.revision),
            (vec![1, 2, 6], 1440, 2)
        );
        assert!(matches!(
            database.update_working_hours(input(1, vec![1], 480, 960)),
            Err(AppError::Conflict)
        ));
        for invalid in [
            input(2, vec![], 480, 960),
            input(2, vec![1, 1], 480, 960),
            input(2, vec![0], 480, 960),
            input(2, vec![1], 960, 480),
            input(2, vec![1], -1, 480),
        ] {
            assert!(matches!(
                database.update_working_hours(invalid),
                Err(AppError::Validation(_))
            ));
        }
    }

    #[test]
    fn capacity_facts_collect_open_scheduled_and_pooled_work() {
        let mut database = database();
        let scheduled = task(&mut database, "Scheduled");
        let pooled = database
            .create_task(CreateTaskInput {
                scheduled_day: None,
                planned_week: Some("2030-11-10".into()),
                ..CreateTaskInput {
                    title: "Pooled".into(),
                    description: String::new(),
                    plan_id: None,
                    milestone_id: None,
                    workstream_id: None,
                    owner_id: None,
                    due_date: None,
                    scheduled_day: None,
                    planned_week: None,
                    estimated_minutes: Some(30),
                    status: TaskStatus::Todo,
                    priority: TaskPriority::Normal,
                    checklist: Vec::new(),
                    recurrence: None,
                    waiting_on: Vec::new(),
                }
            })
            .unwrap();
        let reserved = database
            .create_task_block(block(&scheduled, "2030-11-12T15:00:00Z", 60))
            .unwrap();
        let facts = database
            .capacity_facts("2030-11-10", "2030-11-17", ZONE, None)
            .unwrap();
        assert_eq!(facts.scheduled[0].id, scheduled.id);
        assert_eq!(facts.pooled[0].id, pooled.id);
        assert_eq!(facts.blocks.len(), 1);
        let moving = database
            .capacity_facts(
                "2030-11-10",
                "2030-11-17",
                ZONE,
                Some(reserved.block.id.as_str()),
            )
            .unwrap();
        assert!(
            moving.blocks.is_empty(),
            "a block being moved doesn't occupy its own time"
        );
        let later = database
            .capacity_facts("2030-11-17", "2030-11-24", ZONE, None)
            .unwrap();
        assert!(later.scheduled.is_empty() && later.pooled.is_empty());
    }
}
