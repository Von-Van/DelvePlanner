mod availability;
mod capture;
mod horizons;
mod planning;
mod proposals;
mod team;

use crate::error::{AppError, AppResult};
use crate::model::{
    Agenda, BackupInfo, CreateEventInput, ExportBundle, ImportPreview, LegacyExportBundle,
    LocalDateTimeInput, LocalDateTimeResolution, LocalTimeOption, PlanColor, ReminderChange,
    ReminderStatus, RescheduleEventInput, ScheduleEvent, Task, TaskPriority, TaskStatus,
    UpdateEventInput, MAX_LOCATION_LENGTH, MAX_NOTES_LENGTH, MAX_REMINDER_MINUTES,
    MAX_TITLE_LENGTH,
};
use availability::{ensure_availability_schema, validate_block_shape};
use capture::validate_inbox_text;
use chrono::{
    DateTime, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    TimeZone, Utc,
};
use chrono_tz::Tz;
use planning::{
    ensure_plan_exists, milestone_plan_mismatch, validate_milestone_shape, validate_plan_shape,
    validate_task_shape, TaskShape,
};
use rusqlite::{
    params, types::Type, Connection, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use team::{
    validate_links, validate_person_shape, validate_workstream_shape, workstream_plan_mismatch,
};
use uuid::Uuid;

pub use availability::CapacityFacts;
pub use proposals::{accepted_operations, validate_model_response, CandidateRequest};
pub(crate) use proposals::{fold, planning_tokens, title_matches};

pub const CURRENT_SCHEMA_VERSION: u32 = 6;
pub const EXPORT_FORMAT_VERSION: u32 = 6;
const BACKUP_RETENTION: usize = 5;
const REMINDER_DELIVERY_GRACE_MINUTES: i64 = 15;
const MAX_IMPORT_RECORDS: usize = 100_000;
const MAX_AGENDA_DAYS: u32 = 14;
const EVENT_SELECT: &str = "SELECT id, title, notes, start_at_utc, time_zone, duration_minutes,
        reminder_minutes_before, reminder_status, plan_id, location, workstream_id, owner_id,
        revision, created_at, updated_at
     FROM schedule_events";

#[derive(Debug, Clone)]
pub struct DueReminder {
    pub event_id: String,
    pub event_revision: i64,
    pub notification_id: String,
    pub title: String,
    pub start_at_utc: String,
    pub time_zone: String,
}

pub struct PlannerDatabase {
    connection: Connection,
    path: PathBuf,
}

impl PlannerDatabase {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let existed = path.exists()
            && path
                .metadata()
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false);
        let connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let version = schema_version(&connection)?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(AppError::UnsupportedDatabaseVersion);
        }
        if version < CURRENT_SCHEMA_VERSION {
            connection.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
            if existed {
                create_backup_file(path, version, "migration")?;
            }
            migrate(&connection, version)?;
        }
        verify_integrity(&connection)?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    pub fn schema_version(&self) -> AppResult<u32> {
        schema_version(&self.connection)
    }

    pub fn list_backups(&self) -> AppResult<Vec<BackupInfo>> {
        list_backup_files(&self.path)
    }

    pub fn backup(&self, reason: &str) -> AppResult<BackupInfo> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        create_backup_file(&self.path, self.schema_version()?, reason)
    }

    pub fn export_bundle(&self) -> AppResult<ExportBundle> {
        Ok(ExportBundle {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: now(),
            people: self.all_people()?,
            plans: self.all_plans()?,
            workstreams: self.all_workstreams()?,
            milestones: self.all_milestones()?,
            events: self.all_events()?,
            tasks: self.all_tasks()?,
            inbox_items: self.all_inbox_items()?,
            task_blocks: self.all_task_blocks()?,
        })
    }

    pub fn preview_import(bundle: &ExportBundle) -> AppResult<ImportPreview> {
        validate_export_bundle(bundle)?;
        let mut days = Vec::new();
        for plan in &bundle.plans {
            days.extend(plan.start_date.iter().chain(&plan.target_date).cloned());
        }
        for milestone in &bundle.milestones {
            days.extend(milestone.target_date.iter().cloned());
        }
        for task in &bundle.tasks {
            days.extend(
                task.scheduled_day
                    .iter()
                    .chain(&task.due_date)
                    .chain(&task.planned_week)
                    .cloned(),
            );
        }
        for start_at_utc in bundle
            .events
            .iter()
            .map(|event| &event.start_at_utc)
            .chain(bundle.task_blocks.iter().map(|block| &block.start_at_utc))
        {
            let parsed = DateTime::parse_from_rfc3339(start_at_utc).map_err(|_| {
                AppError::Validation("An imported event or time block has an invalid start.".into())
            })?;
            days.push(parsed.date_naive().format("%Y-%m-%d").to_string());
        }
        days.sort();
        Ok(ImportPreview {
            person_count: bundle.people.len(),
            plan_count: bundle.plans.len(),
            workstream_count: bundle.workstreams.len(),
            milestone_count: bundle.milestones.len(),
            event_count: bundle.events.len(),
            task_count: bundle.tasks.len(),
            inbox_item_count: bundle.inbox_items.len(),
            task_block_count: bundle.task_blocks.len(),
            earliest_day: days.first().cloned(),
            latest_day: days.last().cloned(),
        })
    }

    pub fn import_bundle(&mut self, bundle: &ExportBundle) -> AppResult<ImportPreview> {
        let preview = Self::preview_import(bundle)?;
        self.backup("import")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for table in [
            "task_blocks",
            "inbox_items",
            "tasks",
            "milestones",
            "schedule_events",
            "workstreams",
            "plans",
            "people",
        ] {
            transaction.execute(&format!("DELETE FROM {table}"), [])?;
        }
        for person in &bundle.people {
            transaction.execute(
                "INSERT INTO people
                 (id, display_name, role, email, notes, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    person.id,
                    person.display_name.trim(),
                    person.role.trim(),
                    person.email.as_deref().map(str::trim),
                    person.notes.trim(),
                    person.revision,
                    person.created_at,
                    person.updated_at
                ],
            )?;
        }
        for plan in &bundle.plans {
            transaction.execute(
                "INSERT INTO plans
                 (id, title, description, status, start_date, target_date, color, archived,
                  revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    plan.id,
                    plan.title.trim(),
                    plan.description.trim(),
                    plan.status.as_str(),
                    plan.start_date,
                    plan.target_date,
                    plan.color.map(PlanColor::as_str),
                    plan.archived as i64,
                    plan.revision,
                    plan.created_at,
                    plan.updated_at
                ],
            )?;
        }
        for workstream in &bundle.workstreams {
            transaction.execute(
                "INSERT INTO workstreams
                 (id, plan_id, name, description, sort_order, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    workstream.id,
                    workstream.plan_id,
                    workstream.name.trim(),
                    workstream.description.trim(),
                    workstream.sort_order,
                    workstream.revision,
                    workstream.created_at,
                    workstream.updated_at
                ],
            )?;
        }
        for milestone in &bundle.milestones {
            transaction.execute(
                "INSERT INTO milestones
                 (id, plan_id, title, description, target_date, status, workstream_id, sort_order,
                  revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    milestone.id,
                    milestone.plan_id,
                    milestone.title.trim(),
                    milestone.description.trim(),
                    milestone.target_date,
                    milestone.status.as_str(),
                    milestone.workstream_id,
                    milestone.sort_order,
                    milestone.revision,
                    milestone.created_at,
                    milestone.updated_at
                ],
            )?;
        }
        for event in &bundle.events {
            let reminder_status = if event.reminder_minutes_before.is_some() {
                ReminderStatus::Pending
            } else {
                ReminderStatus::None
            };
            transaction.execute(
                "INSERT INTO schedule_events
                 (id, title, notes, start_at_utc, time_zone, duration_minutes,
                  reminder_minutes_before, reminder_status, notification_id, plan_id, location,
                  workstream_id, owner_id, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    event.id,
                    event.title.trim(),
                    event.notes.trim(),
                    event.start_at_utc,
                    event.time_zone,
                    event.duration_minutes,
                    event.reminder_minutes_before,
                    reminder_status_string(&reminder_status),
                    event
                        .reminder_minutes_before
                        .map(|_| Uuid::new_v4().to_string()),
                    event.plan_id,
                    event.location.trim(),
                    event.workstream_id,
                    event.owner_id,
                    event.revision,
                    event.created_at,
                    event.updated_at
                ],
            )?;
        }
        for task in &bundle.tasks {
            transaction.execute(
                "INSERT INTO tasks
                 (id, title, description, plan_id, milestone_id, workstream_id, owner_id,
                  due_date, scheduled_day, planned_week, estimated_minutes, status, priority,
                  completed_at, sort_order, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18)",
                params![
                    task.id,
                    task.title.trim(),
                    task.description.trim(),
                    task.plan_id,
                    task.milestone_id,
                    task.workstream_id,
                    task.owner_id,
                    task.due_date,
                    task.scheduled_day,
                    task.planned_week,
                    task.estimated_minutes,
                    task.status.as_str(),
                    task.priority.as_str(),
                    task.completed_at,
                    task.sort_order,
                    task.revision,
                    task.created_at,
                    task.updated_at
                ],
            )?;
        }
        for item in &bundle.inbox_items {
            transaction.execute(
                "INSERT INTO inbox_items (id, text, notes, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    item.id,
                    item.text.trim(),
                    item.notes.trim(),
                    item.revision,
                    item.created_at,
                    item.updated_at
                ],
            )?;
        }
        for block in &bundle.task_blocks {
            transaction.execute(
                "INSERT INTO task_blocks
                 (id, task_id, start_at_utc, time_zone, duration_minutes, revision, created_at,
                  updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    block.id,
                    block.task_id,
                    block.start_at_utc,
                    block.time_zone,
                    block.duration_minutes,
                    block.revision,
                    block.created_at,
                    block.updated_at
                ],
            )?;
        }
        transaction.commit()?;
        Ok(preview)
    }

    pub fn resolve_local_datetime(
        input: &LocalDateTimeInput,
    ) -> AppResult<LocalDateTimeResolution> {
        let date = parse_day(&input.day)?;
        let time = NaiveTime::parse_from_str(&input.time, "%H:%M")
            .map_err(|_| AppError::Validation("Times must use 24-hour HH:MM format.".into()))?;
        let zone = parse_time_zone(&input.time_zone)?;
        let local = NaiveDateTime::new(date, time);
        Ok(match zone.from_local_datetime(&local) {
            LocalResult::Single(value) => LocalDateTimeResolution::Resolved {
                start_at_utc: utc_string(value.with_timezone(&Utc)),
            },
            LocalResult::Ambiguous(first, second) => LocalDateTimeResolution::Ambiguous {
                options: [first, second]
                    .into_iter()
                    .map(|value| {
                        let offset = value.offset().fix().local_minus_utc() / 60;
                        LocalTimeOption {
                            start_at_utc: utc_string(value.with_timezone(&Utc)),
                            utc_offset_minutes: offset,
                            label: format!("{} (UTC{:+03}:{:02})", value.format("%H:%M"), offset / 60, offset.abs() % 60),
                        }
                    })
                    .collect(),
            },
            LocalResult::None => LocalDateTimeResolution::Nonexistent {
                message: "That local time does not exist because the clock moves forward. Choose another time.".into(),
            },
        })
    }

    fn all_events(&self) -> AppResult<Vec<ScheduleEvent>> {
        let mut statement = self.connection.prepare(&format!(
            "{EVENT_SELECT} ORDER BY start_at_utc ASC, created_at ASC"
        ))?;
        let rows = statement.query_map([], event_from_row)?;
        collect(rows)
    }

    fn create_latest_schema(connection: &Connection) -> AppResult<()> {
        create_planning_tables(connection)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS schedule_events (
                 id TEXT PRIMARY KEY NOT NULL,
                 title TEXT NOT NULL,
                 notes TEXT NOT NULL DEFAULT '',
                 start_at_utc TEXT NOT NULL,
                 time_zone TEXT NOT NULL,
                 duration_minutes INTEGER NOT NULL CHECK(duration_minutes BETWEEN 5 AND 1440),
                 reminder_minutes_before INTEGER CHECK(reminder_minutes_before BETWEEN 0 AND 10080),
                 reminder_status TEXT NOT NULL DEFAULT 'none',
                 notification_id TEXT UNIQUE,
                 reminder_last_error TEXT,
                 plan_id TEXT REFERENCES plans(id) ON DELETE SET NULL,
                 location TEXT NOT NULL DEFAULT '',
                 workstream_id TEXT REFERENCES workstreams(id) ON DELETE SET NULL,
                 owner_id TEXT REFERENCES people(id) ON DELETE SET NULL,
                 revision INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS schedule_events_start_idx ON schedule_events(start_at_utc);",
        )?;
        Ok(())
    }

    pub fn create_event(&mut self, input: CreateEventInput) -> AppResult<ScheduleEvent> {
        let prepared = PreparedEvent::from_input(input)?;
        insert_event(&self.connection, prepared)
    }

    pub fn update_event(&mut self, input: UpdateEventInput) -> AppResult<ScheduleEvent> {
        let existing = self.event_by_id(&input.id)?.ok_or(AppError::NotFound)?;
        if existing.revision != input.revision {
            return Err(AppError::Conflict);
        }
        let title = input.title.unwrap_or(existing.title);
        let notes = input.notes.unwrap_or(existing.notes);
        let start_at_utc = input.start_at_utc.unwrap_or(existing.start_at_utc);
        let time_zone = input.time_zone.unwrap_or(existing.time_zone);
        let duration_minutes = input.duration_minutes.unwrap_or(existing.duration_minutes);
        let reminder_minutes_before =
            apply_reminder_change(&input.reminder_change, existing.reminder_minutes_before)?;
        let location = input.location.unwrap_or(existing.location);
        let plan_id = input.plan_change.apply(existing.plan_id);
        let workstream_id = input.workstream_change.apply(existing.workstream_id);
        let owner_id = input.owner_change.apply(existing.owner_id);
        if let Some(plan_id) = &plan_id {
            ensure_plan_exists(&self.connection, plan_id)?;
        }
        validate_links(
            &self.connection,
            plan_id.as_deref(),
            workstream_id.as_deref(),
            owner_id.as_deref(),
        )?;
        validate_title(&title)?;
        validate_notes(&notes)?;
        validate_location(&location)?;
        let (start_at_utc, time_zone) = validate_time(&start_at_utc, &time_zone)?;
        validate_duration(duration_minutes)?;
        if matches!(input.reminder_change, ReminderChange::Set { .. }) {
            validate_reminder_delivery_time(&start_at_utc, reminder_minutes_before)?;
        }
        let (reminder_status, notification_id) =
            reminder_outbox_values(&start_at_utc, reminder_minutes_before)?;
        let now = now();
        let changed = self.connection.execute(
            "UPDATE schedule_events
             SET title = ?1, notes = ?2, start_at_utc = ?3, time_zone = ?4,
                 duration_minutes = ?5, reminder_minutes_before = ?6,
                 reminder_status = ?7, notification_id = ?8, reminder_last_error = NULL,
                 plan_id = ?9, location = ?10, workstream_id = ?11, owner_id = ?12,
                 revision = revision + 1, updated_at = ?13
             WHERE id = ?14 AND revision = ?15",
            params![
                title.trim(),
                notes.trim(),
                start_at_utc,
                time_zone,
                duration_minutes,
                reminder_minutes_before,
                reminder_status_string(&reminder_status),
                notification_id,
                plan_id,
                location.trim(),
                workstream_id,
                owner_id,
                now,
                input.id,
                input.revision
            ],
        )?;
        if changed != 1 {
            return Err(AppError::Conflict);
        }
        self.event_by_id(&existing.id)?.ok_or(AppError::NotFound)
    }

    pub fn delete_event(&mut self, id: &str, revision: i64) -> AppResult<()> {
        let changed = self.connection.execute(
            "DELETE FROM schedule_events WHERE id = ?1 AND revision = ?2",
            params![id, revision],
        )?;
        match changed {
            1 => Ok(()),
            0 if self.event_by_id(id)?.is_some() => Err(AppError::Conflict),
            _ => Err(AppError::NotFound),
        }
    }

    pub fn reschedule_event(&mut self, input: RescheduleEventInput) -> AppResult<ScheduleEvent> {
        let existing = self.event_by_id(&input.id)?.ok_or(AppError::NotFound)?;
        if existing.revision != input.revision {
            return Err(AppError::Conflict);
        }
        let (start_at_utc, time_zone) = validate_time(&input.start_at_utc, &input.time_zone)?;
        validate_duration(input.duration_minutes)?;
        let reminder_minutes_before =
            apply_reminder_change(&input.reminder_change, existing.reminder_minutes_before)?;
        if matches!(input.reminder_change, ReminderChange::Set { .. }) {
            validate_reminder_delivery_time(&start_at_utc, reminder_minutes_before)?;
        }
        let (reminder_status, notification_id) =
            reminder_outbox_values(&start_at_utc, reminder_minutes_before)?;
        let changed = self.connection.execute(
            "UPDATE schedule_events
             SET start_at_utc=?1, time_zone=?2, duration_minutes=?3,
                 reminder_minutes_before=?4, reminder_status=?5, notification_id=?6,
                 reminder_last_error=NULL, revision=revision+1, updated_at=?7
             WHERE id=?8 AND revision=?9",
            params![
                start_at_utc,
                time_zone,
                input.duration_minutes,
                reminder_minutes_before,
                reminder_status_string(&reminder_status),
                notification_id,
                now(),
                input.id,
                input.revision
            ],
        )?;
        match changed {
            1 => self.event_by_id(&input.id)?.ok_or(AppError::NotFound),
            0 if self.event_by_id(&input.id)?.is_some() => Err(AppError::Conflict),
            _ => Err(AppError::NotFound),
        }
    }

    pub fn events_for_day(&self, day: &str, time_zone: &str) -> AppResult<Vec<ScheduleEvent>> {
        self.events_between(day, &next_day(day)?, time_zone)
    }

    /// Everything dated in `days` local days starting at `start_day`: overlapping events, tasks
    /// scheduled or due in the window, and milestone checkpoints. Today uses one day; Week uses 7.
    pub fn agenda(&self, start_day: &str, days: u32, time_zone: &str) -> AppResult<Agenda> {
        if !(1..=MAX_AGENDA_DAYS).contains(&days) {
            return Err(AppError::Validation(
                "An agenda can cover between 1 and 14 days.".into(),
            ));
        }
        let start = normalize_day(start_day)?;
        let end = offset_day(&start, i64::from(days))?;
        Ok(Agenda {
            events: self.events_between(&start, &end, time_zone)?,
            tasks: self.tasks_scheduled_between(&start, &end)?,
            due_tasks: self.tasks_due_between(&start, &end)?,
            milestones: self.milestones_between(&start, &end)?,
            blocks: self.blocks_between(&start, &end, time_zone)?,
        })
    }

    /// Events overlapping the local days `[start_day, end_day)` in `time_zone`.
    fn events_between(
        &self,
        start_day: &str,
        end_day: &str,
        time_zone: &str,
    ) -> AppResult<Vec<ScheduleEvent>> {
        let (start, _) = day_bounds(start_day, time_zone)?;
        let (end, _) = day_bounds(end_day, time_zone)?;
        let mut statement = self.connection.prepare(&format!(
            "{EVENT_SELECT}
             WHERE julianday(start_at_utc) < julianday(?2)
               AND julianday(start_at_utc, printf('+%d minutes', duration_minutes)) > julianday(?1)
             ORDER BY start_at_utc ASC, created_at ASC"
        ))?;
        let rows = statement.query_map(params![start, end], event_from_row)?;
        collect(rows)
    }

    /// Reconciles the persisted reminder outbox after startup or an interrupted delivery pass.
    /// Future reminders become schedulable; reminders missed by more than the grace window expire.
    pub fn reconcile_reminders(&mut self) -> AppResult<()> {
        let timestamp = now();
        self.connection.execute(
            "UPDATE schedule_events
             SET reminder_status = 'none', notification_id = NULL, reminder_last_error = NULL
             WHERE reminder_minutes_before IS NULL",
            [],
        )?;
        self.connection.execute(
            "UPDATE schedule_events
             SET reminder_status = 'expired', reminder_last_error = NULL
             WHERE reminder_minutes_before IS NOT NULL
               AND julianday(start_at_utc, printf('-%d minutes', reminder_minutes_before))
                   <= julianday(?1, printf('-%d minutes', ?2))",
            params![timestamp, REMINDER_DELIVERY_GRACE_MINUTES],
        )?;
        self.connection.execute(
            "UPDATE schedule_events
             SET reminder_status = 'scheduled',
                 notification_id = COALESCE(notification_id, lower(hex(randomblob(16)))),
                 reminder_last_error = NULL
             WHERE reminder_minutes_before IS NOT NULL
               AND reminder_status IN ('pending', 'error', 'needs_permission')
               AND julianday(start_at_utc, printf('-%d minutes', reminder_minutes_before))
                   > julianday(?1, printf('-%d minutes', ?2))",
            params![timestamp, REMINDER_DELIVERY_GRACE_MINUTES],
        )?;
        Ok(())
    }

    pub fn due_reminders(&self, limit: usize) -> AppResult<Vec<DueReminder>> {
        let timestamp = now();
        let mut statement = self.connection.prepare(
            "SELECT id, revision, notification_id, title, start_at_utc, time_zone
             FROM schedule_events
             WHERE reminder_minutes_before IS NOT NULL
               AND reminder_status IN ('scheduled', 'error')
               AND notification_id IS NOT NULL
               AND julianday(start_at_utc, printf('-%d minutes', reminder_minutes_before)) <= julianday(?1)
               AND julianday(start_at_utc, printf('-%d minutes', reminder_minutes_before))
                   > julianday(?1, printf('-%d minutes', ?2))
             ORDER BY julianday(start_at_utc, printf('-%d minutes', reminder_minutes_before)) ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                timestamp,
                REMINDER_DELIVERY_GRACE_MINUTES,
                limit.min(50) as i64
            ],
            |row| {
                Ok(DueReminder {
                    event_id: row.get(0)?,
                    event_revision: row.get(1)?,
                    notification_id: row.get(2)?,
                    title: row.get(3)?,
                    start_at_utc: row.get(4)?,
                    time_zone: row.get(5)?,
                })
            },
        )?;
        collect(rows)
    }

    pub fn mark_reminder_delivered(&mut self, reminder: &DueReminder) -> AppResult<()> {
        self.connection.execute(
            "UPDATE schedule_events
             SET reminder_status='expired', reminder_last_error=NULL
             WHERE id=?1 AND revision=?2 AND notification_id=?3",
            params![
                reminder.event_id,
                reminder.event_revision,
                reminder.notification_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_reminder_error(
        &mut self,
        reminder: &DueReminder,
        error_code: &str,
    ) -> AppResult<()> {
        let redacted = error_code
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .take(64)
            .collect::<String>();
        self.connection.execute(
            "UPDATE schedule_events
             SET reminder_status='error', reminder_last_error=?1
             WHERE id=?2 AND revision=?3 AND notification_id=?4",
            params![
                redacted,
                reminder.event_id,
                reminder.event_revision,
                reminder.notification_id
            ],
        )?;
        Ok(())
    }

    fn event_by_id(&self, id: &str) -> AppResult<Option<ScheduleEvent>> {
        event_by_id(&self.connection, id)
    }
}

/// A new event whose fields have passed validation that needs no database access.
struct PreparedEvent {
    input: CreateEventInput,
}

impl PreparedEvent {
    fn from_input(mut input: CreateEventInput) -> AppResult<Self> {
        validate_title(&input.title)?;
        validate_notes(&input.notes)?;
        validate_location(&input.location)?;
        (input.start_at_utc, input.time_zone) =
            validate_time(&input.start_at_utc, &input.time_zone)?;
        validate_duration(input.duration_minutes)?;
        validate_reminder(input.reminder_minutes_before)?;
        for id in [&input.plan_id, &input.workstream_id, &input.owner_id]
            .into_iter()
            .flatten()
        {
            validate_id(id)?;
        }
        if input.workstream_id.is_some() && input.plan_id.is_none() {
            return Err(workstream_plan_mismatch());
        }
        input.title = input.title.trim().to_string();
        input.notes = input.notes.trim().to_string();
        input.location = input.location.trim().to_string();
        Ok(Self { input })
    }
}

trait SqlConnection {
    fn connection(&self) -> &Connection;
}

impl SqlConnection for Connection {
    fn connection(&self) -> &Connection {
        self
    }
}

impl SqlConnection for Transaction<'_> {
    fn connection(&self) -> &Connection {
        self
    }
}

fn insert_event<C: SqlConnection>(
    connection: &C,
    prepared: PreparedEvent,
) -> AppResult<ScheduleEvent> {
    let input = prepared.input;
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    validate_reminder_delivery_time(&input.start_at_utc, input.reminder_minutes_before)?;
    if let Some(plan_id) = &input.plan_id {
        ensure_plan_exists(connection, plan_id)?;
    }
    validate_links(
        connection,
        input.plan_id.as_deref(),
        input.workstream_id.as_deref(),
        input.owner_id.as_deref(),
    )?;
    let (reminder_status, notification_id) =
        reminder_outbox_values(&input.start_at_utc, input.reminder_minutes_before)?;
    connection.connection().execute(
        "INSERT INTO schedule_events
         (id, title, notes, start_at_utc, time_zone, duration_minutes,
          reminder_minutes_before, reminder_status, notification_id, plan_id, location,
          workstream_id, owner_id, revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, ?14)",
        params![
            id,
            input.title,
            input.notes,
            input.start_at_utc,
            input.time_zone,
            input.duration_minutes,
            input.reminder_minutes_before,
            reminder_status_string(&reminder_status),
            notification_id,
            input.plan_id,
            input.location,
            input.workstream_id,
            input.owner_id,
            timestamp
        ],
    )?;
    event_by_id(connection, &id)?.ok_or(AppError::NotFound)
}

fn event_by_id<C: SqlConnection>(connection: &C, id: &str) -> AppResult<Option<ScheduleEvent>> {
    connection
        .connection()
        .query_row(
            &format!("{EVENT_SELECT} WHERE id = ?1"),
            params![id],
            event_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<ScheduleEvent> {
    Ok(ScheduleEvent {
        id: row.get(0)?,
        title: row.get(1)?,
        notes: row.get(2)?,
        start_at_utc: row.get(3)?,
        time_zone: row.get(4)?,
        duration_minutes: row.get(5)?,
        reminder_minutes_before: row.get(6)?,
        reminder_status: reminder_status_from_row(row.get::<_, String>(7)?)?,
        plan_id: row.get(8)?,
        location: row.get(9)?,
        workstream_id: row.get(10)?,
        owner_id: row.get(11)?,
        revision: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn reminder_status_from_row(value: String) -> rusqlite::Result<ReminderStatus> {
    match value.as_str() {
        "none" => Ok(ReminderStatus::None),
        "pending" => Ok(ReminderStatus::Pending),
        "scheduled" => Ok(ReminderStatus::Scheduled),
        "needs_permission" => Ok(ReminderStatus::NeedsPermission),
        "error" => Ok(ReminderStatus::Error),
        "expired" => Ok(ReminderStatus::Expired),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            7,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid reminder status",
            )),
        )),
    }
}

fn reminder_status_string(status: &ReminderStatus) -> &'static str {
    match status {
        ReminderStatus::None => "none",
        ReminderStatus::Pending => "pending",
        ReminderStatus::Scheduled => "scheduled",
        ReminderStatus::NeedsPermission => "needs_permission",
        ReminderStatus::Error => "error",
        ReminderStatus::Expired => "expired",
    }
}

fn collect<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>,
) -> AppResult<Vec<T>> {
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn day_bounds(day: &str, time_zone: &str) -> AppResult<(String, String)> {
    let date = parse_day(day)?;
    let next_day = date
        .succ_opt()
        .ok_or_else(|| AppError::Validation("That date is out of range.".into()))?;
    let zone = parse_time_zone(time_zone)?;
    let start = zone
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .ok_or_else(|| {
            AppError::Validation("That day has no local midnight in this time zone.".into())
        })?;
    let end = zone
        .from_local_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .ok_or_else(|| {
            AppError::Validation("That day has no local midnight in this time zone.".into())
        })?;
    Ok((
        utc_string(start.with_timezone(&Utc)),
        utc_string(end.with_timezone(&Utc)),
    ))
}

pub fn validate_title(value: &str) -> AppResult<()> {
    let length = value.trim().chars().count();
    if !(1..=MAX_TITLE_LENGTH).contains(&length) {
        return Err(AppError::Validation(
            "Titles must be 1–140 characters.".into(),
        ));
    }
    Ok(())
}

pub fn validate_notes(value: &str) -> AppResult<()> {
    if value.trim().chars().count() > MAX_NOTES_LENGTH {
        return Err(AppError::Validation(
            "Event notes must be 800 characters or fewer.".into(),
        ));
    }
    Ok(())
}

fn validate_duration(value: i64) -> AppResult<()> {
    if !(5..=1440).contains(&value) {
        return Err(AppError::Validation(
            "Event duration must be between 5 and 1,440 minutes.".into(),
        ));
    }
    Ok(())
}

fn validate_reminder(value: Option<i64>) -> AppResult<()> {
    if value.is_some_and(|minutes| !(0..=MAX_REMINDER_MINUTES).contains(&minutes)) {
        return Err(AppError::Validation(
            "A reminder must be between the event start and seven days beforehand.".into(),
        ));
    }
    Ok(())
}

fn validate_reminder_change(change: &ReminderChange) -> AppResult<()> {
    match change {
        ReminderChange::Set { minutes_before } => validate_reminder(Some(*minutes_before)),
        ReminderChange::Unchanged | ReminderChange::Clear => Ok(()),
    }
}

fn apply_reminder_change(change: &ReminderChange, current: Option<i64>) -> AppResult<Option<i64>> {
    validate_reminder_change(change)?;
    Ok(match change {
        ReminderChange::Unchanged => current,
        ReminderChange::Clear => None,
        ReminderChange::Set { minutes_before } => Some(*minutes_before),
    })
}

fn reminder_fire_time(start_at_utc: &str, minutes_before: i64) -> AppResult<DateTime<Utc>> {
    let start = DateTime::parse_from_rfc3339(start_at_utc)
        .map_err(|_| AppError::Validation("The reminder has an invalid event start time.".into()))?
        .with_timezone(&Utc);
    Ok(start - ChronoDuration::minutes(minutes_before))
}

fn validate_reminder_delivery_time(
    start_at_utc: &str,
    minutes_before: Option<i64>,
) -> AppResult<()> {
    validate_reminder(minutes_before)?;
    if let Some(minutes) = minutes_before {
        if reminder_fire_time(start_at_utc, minutes)? <= Utc::now() {
            return Err(AppError::Validation(
                "That reminder time has already passed. Choose a later event or a shorter reminder."
                    .into(),
            ));
        }
    }
    Ok(())
}

fn reminder_outbox_values(
    start_at_utc: &str,
    minutes_before: Option<i64>,
) -> AppResult<(ReminderStatus, Option<String>)> {
    validate_reminder(minutes_before)?;
    let Some(minutes) = minutes_before else {
        return Ok((ReminderStatus::None, None));
    };
    if reminder_fire_time(start_at_utc, minutes)? <= Utc::now() {
        Ok((ReminderStatus::Expired, None))
    } else {
        Ok((ReminderStatus::Pending, Some(Uuid::new_v4().to_string())))
    }
}

fn validate_id(id: &str) -> AppResult<()> {
    Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| AppError::Validation("A record used an invalid identifier.".into()))
}

fn validate_revision(value: i64) -> AppResult<()> {
    if value > 0 {
        Ok(())
    } else {
        Err(AppError::Validation(
            "A record used an invalid revision.".into(),
        ))
    }
}

fn parse_day(day: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| AppError::Validation("Dates must use YYYY-MM-DD.".into()))
}

/// Parses a local calendar day and returns it zero-padded so SQL equality comparisons hold.
fn normalize_day(day: &str) -> AppResult<String> {
    parse_day(day).map(|date| date.format("%Y-%m-%d").to_string())
}

fn offset_day(day: &str, days: i64) -> AppResult<String> {
    parse_day(day)?
        .checked_add_signed(ChronoDuration::days(days))
        .map(|date| date.format("%Y-%m-%d").to_string())
        .ok_or_else(|| AppError::Validation("That date is out of range.".into()))
}

fn next_day(day: &str) -> AppResult<String> {
    offset_day(day, 1)
}

fn validate_location(value: &str) -> AppResult<()> {
    if value.trim().chars().count() > MAX_LOCATION_LENGTH {
        return Err(AppError::Validation(
            "Locations must be 140 characters or fewer.".into(),
        ));
    }
    Ok(())
}

fn parse_time_zone(time_zone: &str) -> AppResult<Tz> {
    time_zone
        .parse::<Tz>()
        .map_err(|_| AppError::Validation("A valid IANA time zone is required.".into()))
}

fn validate_time(start_at_utc: &str, time_zone: &str) -> AppResult<(String, String)> {
    let parsed = DateTime::parse_from_rfc3339(start_at_utc).map_err(|_| {
        AppError::Validation("Event start times must be ISO-8601 UTC timestamps.".into())
    })?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(AppError::Validation(
            "Event start times must be expressed in UTC (Z).".into(),
        ));
    }
    let zone = parse_time_zone(time_zone)?;
    Ok((
        utc_string(parsed.with_timezone(&Utc)),
        zone.name().to_string(),
    ))
}

fn utc_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn now() -> String {
    utc_string(Utc::now())
}

fn schema_version(connection: &Connection) -> AppResult<u32> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .map_err(AppError::from)
}

fn migrate(connection: &Connection, from_version: u32) -> AppResult<()> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if from_version == 0 {
            PlannerDatabase::create_latest_schema(connection)?;
        }
        ensure_reminder_columns(connection)?;
        ensure_planning_schema(connection)?;
        ensure_team_schema(connection)?;
        ensure_horizon_schema(connection)?;
        ensure_availability_schema(connection)?;
        connection.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        Ok::<(), AppError>(())
    })();
    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn ensure_reminder_columns(connection: &Connection) -> AppResult<()> {
    if !column_exists(connection, "schedule_events", "reminder_minutes_before")? {
        connection.execute(
            "ALTER TABLE schedule_events ADD COLUMN reminder_minutes_before INTEGER
             CHECK(reminder_minutes_before BETWEEN 0 AND 10080)",
            [],
        )?;
    }
    if !column_exists(connection, "schedule_events", "reminder_status")? {
        connection.execute(
            "ALTER TABLE schedule_events ADD COLUMN reminder_status TEXT NOT NULL DEFAULT 'none'",
            [],
        )?;
    }
    if !column_exists(connection, "schedule_events", "notification_id")? {
        connection.execute(
            "ALTER TABLE schedule_events ADD COLUMN notification_id TEXT",
            [],
        )?;
    }
    if !column_exists(connection, "schedule_events", "reminder_last_error")? {
        connection.execute(
            "ALTER TABLE schedule_events ADD COLUMN reminder_last_error TEXT",
            [],
        )?;
    }
    connection.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS schedule_events_notification_idx
         ON schedule_events(notification_id) WHERE notification_id IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// Creates the planning tables. Every statement is idempotent so migrations can repeat it.
fn create_planning_tables(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS plans (
             id TEXT PRIMARY KEY NOT NULL,
             title TEXT NOT NULL,
             description TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL DEFAULT 'planning',
             start_date TEXT,
             target_date TEXT,
             color TEXT,
             archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)),
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS milestones (
             id TEXT PRIMARY KEY NOT NULL,
             plan_id TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
             title TEXT NOT NULL,
             description TEXT NOT NULL DEFAULT '',
             target_date TEXT,
             status TEXT NOT NULL DEFAULT 'pending',
             sort_order INTEGER NOT NULL DEFAULT 0,
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS milestones_plan_idx ON milestones(plan_id, target_date);
         CREATE TABLE IF NOT EXISTS tasks (
             id TEXT PRIMARY KEY NOT NULL,
             title TEXT NOT NULL,
             description TEXT NOT NULL DEFAULT '',
             plan_id TEXT REFERENCES plans(id) ON DELETE SET NULL,
             milestone_id TEXT REFERENCES milestones(id) ON DELETE SET NULL,
             due_date TEXT,
             scheduled_day TEXT,
             status TEXT NOT NULL DEFAULT 'todo',
             priority TEXT NOT NULL DEFAULT 'normal',
             completed_at TEXT,
             sort_order INTEGER NOT NULL DEFAULT 0,
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             CHECK(milestone_id IS NULL OR plan_id IS NOT NULL),
             CHECK((status = 'done') = (completed_at IS NOT NULL))
         );
         CREATE INDEX IF NOT EXISTS tasks_scheduled_day_idx ON tasks(scheduled_day, sort_order);
         CREATE INDEX IF NOT EXISTS tasks_due_date_idx ON tasks(due_date);
         CREATE INDEX IF NOT EXISTS tasks_plan_idx ON tasks(plan_id);
         CREATE INDEX IF NOT EXISTS tasks_milestone_idx ON tasks(milestone_id);",
    )?;
    Ok(())
}

/// Schema 3: adds plans and milestones, gives events an optional plan, and moves day-bound
/// `daily_tasks` rows into `tasks` with `scheduled_day` = the old day and a done/todo status.
fn ensure_planning_schema(connection: &Connection) -> AppResult<()> {
    create_planning_tables(connection)?;
    if !column_exists(connection, "schedule_events", "plan_id")? {
        connection.execute(
            "ALTER TABLE schedule_events
             ADD COLUMN plan_id TEXT REFERENCES plans(id) ON DELETE SET NULL",
            [],
        )?;
    }
    connection.execute(
        "CREATE INDEX IF NOT EXISTS schedule_events_plan_idx ON schedule_events(plan_id)",
        [],
    )?;
    if table_exists(connection, "daily_tasks")? {
        connection.execute(
            "INSERT INTO tasks
             (id, title, description, plan_id, milestone_id, due_date, scheduled_day, status,
              priority, completed_at, sort_order, revision, created_at, updated_at)
             SELECT id, title, '', NULL, NULL, NULL, day,
                    CASE WHEN completed != 0 THEN 'done' ELSE 'todo' END,
                    'normal',
                    CASE WHEN completed != 0 THEN COALESCE(completed_at, updated_at) END,
                    sort_order, 1, created_at, updated_at
             FROM daily_tasks",
            [],
        )?;
        connection.execute("DROP TABLE daily_tasks", [])?;
    }
    Ok(())
}

/// Schema 4: adds people and plan workstreams, lets tasks and events have an owner and a
/// workstream, lets milestones have a workstream, and gives events a free-text location.
fn ensure_team_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS people (
             id TEXT PRIMARY KEY NOT NULL,
             display_name TEXT NOT NULL,
             role TEXT NOT NULL DEFAULT '',
             email TEXT,
             notes TEXT NOT NULL DEFAULT '',
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS workstreams (
             id TEXT PRIMARY KEY NOT NULL,
             plan_id TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             description TEXT NOT NULL DEFAULT '',
             sort_order INTEGER NOT NULL DEFAULT 0,
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS workstreams_plan_name_idx
             ON workstreams(plan_id, name COLLATE NOCASE);",
    )?;
    const WORKSTREAM_LINK: &str = "TEXT REFERENCES workstreams(id) ON DELETE SET NULL";
    const OWNER_LINK: &str = "TEXT REFERENCES people(id) ON DELETE SET NULL";
    for (table, column, definition) in [
        ("tasks", "workstream_id", WORKSTREAM_LINK),
        ("tasks", "owner_id", OWNER_LINK),
        ("milestones", "workstream_id", WORKSTREAM_LINK),
        ("schedule_events", "location", "TEXT NOT NULL DEFAULT ''"),
        ("schedule_events", "workstream_id", WORKSTREAM_LINK),
        ("schedule_events", "owner_id", OWNER_LINK),
    ] {
        if !column_exists(connection, table, column)? {
            connection.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS tasks_workstream_idx ON tasks(workstream_id);
         CREATE INDEX IF NOT EXISTS tasks_owner_idx ON tasks(owner_id);
         CREATE INDEX IF NOT EXISTS milestones_workstream_idx ON milestones(workstream_id);
         CREATE INDEX IF NOT EXISTS schedule_events_workstream_idx ON schedule_events(workstream_id);
         CREATE INDEX IF NOT EXISTS schedule_events_owner_idx ON schedule_events(owner_id);",
    )?;
    Ok(())
}

/// Schema 5: adds the capture inbox, lets a task be chosen for a week without a day, and gives
/// tasks an optional effort estimate. Existing tasks keep every value they had.
fn ensure_horizon_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS inbox_items (
             id TEXT PRIMARY KEY NOT NULL,
             text TEXT NOT NULL,
             notes TEXT NOT NULL DEFAULT '',
             revision INTEGER NOT NULL DEFAULT 1,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS inbox_items_created_idx ON inbox_items(created_at);",
    )?;
    for (column, definition) in [
        ("planned_week", "TEXT"),
        (
            "estimated_minutes",
            "INTEGER CHECK(estimated_minutes BETWEEN 1 AND 1440)",
        ),
    ] {
        if !column_exists(connection, "tasks", column)? {
            connection.execute(
                &format!("ALTER TABLE tasks ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    connection.execute(
        "CREATE INDEX IF NOT EXISTS tasks_planned_week_idx ON tasks(planned_week)",
        [],
    )?;
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> AppResult<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get(0),
        )
        .map_err(AppError::from)
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> AppResult<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn verify_integrity(connection: &Connection) -> AppResult<()> {
    let result: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(AppError::CorruptDatabase)
    }
}

fn backup_directory(path: &Path) -> AppResult<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Validation("The database path has no parent directory.".into()))?;
    let directory = parent.join("backups");
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn create_backup_file(path: &Path, schema: u32, reason: &str) -> AppResult<BackupInfo> {
    if !path.exists() {
        return Err(AppError::NotFound);
    }
    let safe_reason = reason
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(24)
        .collect::<String>();
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = format!("dayplan-v{schema}-{timestamp}-{safe_reason}.sqlite3");
    let destination = backup_directory(path)?.join(&name);
    fs::copy(path, &destination)?;
    prune_backups(path)?;
    let metadata = destination.metadata()?;
    Ok(BackupInfo {
        name,
        created_at: utc_string(DateTime::<Utc>::from(metadata.modified()?)),
        size_bytes: metadata.len(),
    })
}

fn list_backup_files(path: &Path) -> AppResult<Vec<BackupInfo>> {
    let directory = backup_directory(path)?;
    let mut backups = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("dayplan-v") || !name.ends_with(".sqlite3") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some(BackupInfo {
                name,
                created_at: utc_string(DateTime::<Utc>::from(metadata.modified().ok()?)),
                size_bytes: metadata.len(),
            })
        })
        .collect::<Vec<_>>();
    backups.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(backups)
}

pub fn backups_for_path(path: &Path) -> AppResult<Vec<BackupInfo>> {
    list_backup_files(path)
}

fn prune_backups(path: &Path) -> AppResult<()> {
    let directory = backup_directory(path)?;
    for backup in list_backup_files(path)?.into_iter().skip(BACKUP_RETENTION) {
        let backup_path = directory.join(backup.name);
        if backup_path.is_file() {
            fs::remove_file(backup_path)?;
        }
    }
    Ok(())
}

pub fn restore_backup(path: &Path, backup_name: &str) -> AppResult<()> {
    let backup = list_backup_files(path)?
        .into_iter()
        .find(|backup| backup.name == backup_name)
        .ok_or(AppError::BackupNotFound)?;
    let source = backup_directory(path)?.join(backup.name);
    let temporary = path.with_extension("restore.sqlite3");
    fs::copy(source, &temporary)?;
    let candidate = Connection::open(&temporary)?;
    verify_integrity(&candidate)?;
    drop(candidate);

    let displaced = path.with_extension("before-restore.sqlite3");
    if displaced.exists() {
        fs::remove_file(&displaced)?;
    }
    fs::rename(path, &displaced)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::rename(&displaced, path);
        return Err(AppError::Io(error));
    }
    if displaced.exists() {
        fs::remove_file(displaced)?;
    }
    for suffix in ["sqlite3-wal", "sqlite3-shm"] {
        let sidecar = path.with_extension(suffix);
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
    }
    Ok(())
}

/// Reads any supported export format. Formats 1–2 are upgraded to the current bundle shape;
/// format 3 predates people and workstreams, formats 3–4 predate the inbox, task weeks, and
/// estimates, and formats 3–5 predate time blocks. Missing fields default to empty.
pub fn parse_export_bundle(contents: &str) -> AppResult<ExportBundle> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FormatHeader {
        format_version: u32,
    }
    match serde_json::from_str::<FormatHeader>(contents)?.format_version {
        1 | 2 => Ok(upgrade_legacy_bundle(serde_json::from_str(contents)?)),
        3..=5 | EXPORT_FORMAT_VERSION => {
            let bundle: ExportBundle = serde_json::from_str(contents)?;
            Ok(ExportBundle {
                format_version: EXPORT_FORMAT_VERSION,
                ..bundle
            })
        }
        other => Err(AppError::Validation(format!(
            "Unsupported DayPlan export format {other}."
        ))),
    }
}

/// Mirrors the schema 3 migration: each legacy task keeps its day as `scheduled_day`.
/// Completion timestamps are carried over unchanged so validation still rejects mismatches.
fn upgrade_legacy_bundle(legacy: LegacyExportBundle) -> ExportBundle {
    ExportBundle {
        format_version: EXPORT_FORMAT_VERSION,
        exported_at: legacy.exported_at,
        people: Vec::new(),
        plans: Vec::new(),
        workstreams: Vec::new(),
        milestones: Vec::new(),
        inbox_items: Vec::new(),
        task_blocks: Vec::new(),
        events: legacy
            .events
            .into_iter()
            .map(|event| ScheduleEvent {
                plan_id: None,
                location: String::new(),
                workstream_id: None,
                owner_id: None,
                ..event
            })
            .collect(),
        tasks: legacy
            .tasks
            .into_iter()
            .map(|task| Task {
                id: task.id,
                title: task.title,
                description: String::new(),
                plan_id: None,
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: Some(normalize_day(&task.day).unwrap_or(task.day)),
                planned_week: None,
                estimated_minutes: None,
                status: if task.completed {
                    TaskStatus::Done
                } else {
                    TaskStatus::Todo
                },
                priority: TaskPriority::Normal,
                completed_at: task.completed_at,
                sort_order: task.sort_order,
                revision: 1,
                created_at: task.created_at,
                updated_at: task.updated_at,
            })
            .collect(),
    }
}

/// Validates every record and every cross-record reference before an import may replace data.
fn validate_export_bundle(bundle: &ExportBundle) -> AppResult<()> {
    if bundle.format_version != EXPORT_FORMAT_VERSION {
        return Err(AppError::Validation(format!(
            "Unsupported DayPlan export format {}.",
            bundle.format_version
        )));
    }
    if [
        bundle.people.len(),
        bundle.plans.len(),
        bundle.workstreams.len(),
        bundle.milestones.len(),
        bundle.events.len(),
        bundle.tasks.len(),
        bundle.inbox_items.len(),
        bundle.task_blocks.len(),
    ]
    .into_iter()
    .any(|count| count > MAX_IMPORT_RECORDS)
    {
        return Err(AppError::Validation(
            "That export contains too many records.".into(),
        ));
    }
    validate_timestamp(&bundle.exported_at)?;
    let mut person_ids = HashSet::new();
    for person in &bundle.people {
        validate_id(&person.id)?;
        if !person_ids.insert(person.id.as_str()) {
            return Err(duplicate_identifiers("person"));
        }
        let email = validate_person_shape(
            &person.display_name,
            &person.role,
            person.email.as_deref(),
            &person.notes,
        )?;
        if email != person.email {
            return Err(AppError::Validation(
                "An imported email address must be trimmed or omitted.".into(),
            ));
        }
        validate_record_metadata(person.revision, &person.created_at, &person.updated_at)?;
    }
    let mut plan_ids = HashSet::new();
    for plan in &bundle.plans {
        validate_id(&plan.id)?;
        if !plan_ids.insert(plan.id.as_str()) {
            return Err(duplicate_identifiers("plan"));
        }
        let dates = validate_plan_shape(
            &plan.title,
            &plan.description,
            plan.start_date.as_deref(),
            plan.target_date.as_deref(),
        )?;
        if dates != (plan.start_date.clone(), plan.target_date.clone()) {
            return Err(non_canonical_dates());
        }
        validate_record_metadata(plan.revision, &plan.created_at, &plan.updated_at)?;
    }
    let mut workstream_plans = HashMap::new();
    let mut workstream_names = HashSet::new();
    for workstream in &bundle.workstreams {
        validate_id(&workstream.id)?;
        if workstream_plans
            .insert(workstream.id.as_str(), workstream.plan_id.as_str())
            .is_some()
        {
            return Err(duplicate_identifiers("workstream"));
        }
        if !plan_ids.contains(workstream.plan_id.as_str()) {
            return Err(missing_reference("A workstream"));
        }
        validate_workstream_shape(&workstream.name, &workstream.description)?;
        if !workstream_names.insert((
            workstream.plan_id.as_str(),
            workstream.name.trim().to_ascii_lowercase(),
        )) {
            return Err(AppError::Validation(
                "The export repeats a workstream name within one plan.".into(),
            ));
        }
        validate_sort_order(workstream.sort_order)?;
        validate_record_metadata(
            workstream.revision,
            &workstream.created_at,
            &workstream.updated_at,
        )?;
    }
    let links = ExportLinks {
        people: &person_ids,
        workstream_plans: &workstream_plans,
    };
    let mut milestone_plans = HashMap::new();
    for milestone in &bundle.milestones {
        validate_id(&milestone.id)?;
        if milestone_plans
            .insert(milestone.id.as_str(), milestone.plan_id.as_str())
            .is_some()
        {
            return Err(duplicate_identifiers("milestone"));
        }
        if !plan_ids.contains(milestone.plan_id.as_str()) {
            return Err(missing_reference("A milestone"));
        }
        let target_date = validate_milestone_shape(
            &milestone.title,
            &milestone.description,
            milestone.target_date.as_deref(),
        )?;
        if target_date != milestone.target_date {
            return Err(non_canonical_dates());
        }
        links.validate(
            "A milestone",
            Some(&milestone.plan_id),
            milestone.workstream_id.as_deref(),
            None,
        )?;
        validate_sort_order(milestone.sort_order)?;
        validate_record_metadata(
            milestone.revision,
            &milestone.created_at,
            &milestone.updated_at,
        )?;
    }
    let mut event_ids = HashSet::new();
    for event in &bundle.events {
        validate_id(&event.id)?;
        if !event_ids.insert(&event.id) {
            return Err(duplicate_identifiers("event"));
        }
        validate_title(&event.title)?;
        validate_notes(&event.notes)?;
        validate_location(&event.location)?;
        validate_time(&event.start_at_utc, &event.time_zone)?;
        validate_duration(event.duration_minutes)?;
        validate_reminder(event.reminder_minutes_before)?;
        if event.reminder_minutes_before.is_none() && event.reminder_status != ReminderStatus::None
        {
            return Err(AppError::Validation(
                "An imported reminder status does not match its event.".into(),
            ));
        }
        if event
            .plan_id
            .as_deref()
            .is_some_and(|plan_id| !plan_ids.contains(plan_id))
        {
            return Err(missing_reference("An event"));
        }
        links.validate(
            "An event",
            event.plan_id.as_deref(),
            event.workstream_id.as_deref(),
            event.owner_id.as_deref(),
        )?;
        validate_record_metadata(event.revision, &event.created_at, &event.updated_at)?;
    }
    let mut task_ids = HashSet::new();
    for task in &bundle.tasks {
        validate_id(&task.id)?;
        if !task_ids.insert(&task.id) {
            return Err(duplicate_identifiers("task"));
        }
        let days = validate_task_shape(&TaskShape {
            title: &task.title,
            description: &task.description,
            plan_id: task.plan_id.as_deref(),
            milestone_id: task.milestone_id.as_deref(),
            workstream_id: task.workstream_id.as_deref(),
            owner_id: task.owner_id.as_deref(),
            due_date: task.due_date.as_deref(),
            scheduled_day: task.scheduled_day.as_deref(),
            planned_week: task.planned_week.as_deref(),
            estimated_minutes: task.estimated_minutes,
        })?;
        if days.due_date != task.due_date
            || days.scheduled_day != task.scheduled_day
            || days.planned_week != task.planned_week
        {
            return Err(non_canonical_dates());
        }
        if task
            .plan_id
            .as_deref()
            .is_some_and(|plan_id| !plan_ids.contains(plan_id))
        {
            return Err(missing_reference("A task"));
        }
        if let Some(milestone_id) = task.milestone_id.as_deref() {
            match milestone_plans.get(milestone_id) {
                None => return Err(missing_reference("A task")),
                Some(plan_id) if Some(*plan_id) != task.plan_id.as_deref() => {
                    return Err(milestone_plan_mismatch())
                }
                Some(_) => {}
            }
        }
        links.validate(
            "A task",
            task.plan_id.as_deref(),
            task.workstream_id.as_deref(),
            task.owner_id.as_deref(),
        )?;
        validate_sort_order(task.sort_order)?;
        if (task.status == TaskStatus::Done) != task.completed_at.is_some() {
            return Err(AppError::Validation(
                "A task completion timestamp does not match its status.".into(),
            ));
        }
        if let Some(completed_at) = &task.completed_at {
            validate_timestamp(completed_at)?;
        }
        validate_record_metadata(task.revision, &task.created_at, &task.updated_at)?;
    }
    let mut inbox_ids = HashSet::new();
    for item in &bundle.inbox_items {
        validate_id(&item.id)?;
        if !inbox_ids.insert(&item.id) {
            return Err(duplicate_identifiers("inbox item"));
        }
        validate_inbox_text(&item.text, &item.notes)?;
        validate_record_metadata(item.revision, &item.created_at, &item.updated_at)?;
    }
    let mut block_ids = HashSet::new();
    for block in &bundle.task_blocks {
        validate_id(&block.id)?;
        if !block_ids.insert(&block.id) {
            return Err(duplicate_identifiers("time block"));
        }
        if !task_ids.contains(&block.task_id) {
            return Err(missing_reference("A time block"));
        }
        validate_block_shape(
            &block.start_at_utc,
            &block.time_zone,
            block.duration_minutes,
        )?;
        validate_record_metadata(block.revision, &block.created_at, &block.updated_at)?;
    }
    Ok(())
}

/// The people and workstreams present in an export, for checking owner and workstream links.
struct ExportLinks<'a> {
    people: &'a HashSet<&'a str>,
    workstream_plans: &'a HashMap<&'a str, &'a str>,
}

impl ExportLinks<'_> {
    fn validate(
        &self,
        subject: &str,
        plan_id: Option<&str>,
        workstream_id: Option<&str>,
        owner_id: Option<&str>,
    ) -> AppResult<()> {
        if let Some(workstream_id) = workstream_id {
            match self.workstream_plans.get(workstream_id) {
                None => return Err(missing_reference(subject)),
                Some(workstream_plan) if Some(*workstream_plan) != plan_id => {
                    return Err(workstream_plan_mismatch())
                }
                Some(_) => {}
            }
        }
        if owner_id.is_some_and(|owner_id| !self.people.contains(owner_id)) {
            return Err(missing_reference(subject));
        }
        Ok(())
    }
}

fn validate_record_metadata(revision: i64, created_at: &str, updated_at: &str) -> AppResult<()> {
    validate_revision(revision)?;
    validate_timestamp(created_at)?;
    validate_timestamp(updated_at)
}

fn duplicate_identifiers(kind: &str) -> AppError {
    AppError::Validation(format!("The export contains duplicate {kind} identifiers."))
}

fn missing_reference(subject: &str) -> AppError {
    AppError::Validation(format!(
        "{subject} in the export references a record that is not included."
    ))
}

fn non_canonical_dates() -> AppError {
    AppError::Validation("Imported dates must use zero-padded YYYY-MM-DD.".into())
}

fn validate_sort_order(value: i64) -> AppResult<()> {
    if value < 0 {
        return Err(AppError::Validation(
            "Ordering values cannot be negative.".into(),
        ));
    }
    Ok(())
}

fn validate_timestamp(value: &str) -> AppResult<()> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| AppError::Validation("Imported timestamps must use ISO-8601.".into()))?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(AppError::Validation(
            "Imported timestamps must be expressed in UTC (Z).".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LinkChange, ModelResponse, MutationOperation};
    use tempfile::tempdir;

    fn database() -> PlannerDatabase {
        let directory = tempdir().unwrap();
        let path = directory.keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn event_input(title: &str, start: &str) -> CreateEventInput {
        CreateEventInput {
            title: title.into(),
            notes: String::new(),
            start_at_utc: start.into(),
            time_zone: "America/New_York".into(),
            duration_minutes: 60,
            reminder_minutes_before: None,
            plan_id: None,
            location: String::new(),
            workstream_id: None,
            owner_id: None,
        }
    }

    fn plan_input(title: &str) -> crate::model::CreatePlanInput {
        crate::model::CreatePlanInput {
            title: title.into(),
            description: String::new(),
            status: crate::model::PlanStatus::Active,
            start_date: None,
            target_date: Some("2026-10-16".into()),
            color: Some(PlanColor::Clay),
        }
    }

    fn task_input(title: &str) -> crate::model::CreateTaskInput {
        crate::model::CreateTaskInput {
            title: title.into(),
            description: String::new(),
            plan_id: None,
            milestone_id: None,
            workstream_id: None,
            owner_id: None,
            due_date: None,
            scheduled_day: None,
            planned_week: None,
            estimated_minutes: None,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
        }
    }

    fn future_start(hours: i64) -> String {
        utc_string(Utc::now() + ChronoDuration::hours(hours))
    }

    #[test]
    fn rejects_non_utc_event_times() {
        let mut db = database();
        let result = db.create_event(event_input("Gym", "2026-08-12T10:00:00-04:00"));
        assert!(matches!(result, Err(AppError::Validation(_))));
    }

    #[test]
    fn daylight_saving_day_queries_the_right_window() {
        let mut db = database();
        db.create_event(event_input("Breakfast", "2026-03-08T05:30:00Z"))
            .unwrap();
        let events = db.events_for_day("2026-03-08", "America/New_York").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn stale_proposal_rolls_back_everything() {
        let mut db = database();
        let existing = db
            .create_event(event_input("Gym", "2026-08-12T22:00:00Z"))
            .unwrap();
        let proposal = ModelResponse::proposal(
            "Two changes",
            vec![
                MutationOperation::CreateEvent {
                    title: "Dentist".into(),
                    notes: String::new(),
                    start_at_utc: "2026-08-13T18:00:00Z".into(),
                    time_zone: "America/New_York".into(),
                    duration_minutes: 60,
                    reminder_minutes_before: None,
                    plan: None,
                },
                MutationOperation::RescheduleEvent {
                    event_id: existing.id,
                    expected_revision: 99,
                    title: None,
                    notes: None,
                    start_at_utc: "2026-08-13T22:00:00Z".into(),
                    time_zone: "America/New_York".into(),
                    duration_minutes: None,
                    reminder_change: ReminderChange::Unchanged,
                },
            ],
        );
        assert!(matches!(
            db.apply_proposal(&proposal),
            Err(AppError::Conflict)
        ));
        assert!(db
            .events_for_day("2026-08-13", "America/New_York")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn migrates_a_legacy_database_and_keeps_a_backup() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let database = PlannerDatabase::open(&path).unwrap();
        database
            .connection
            .pragma_update(None, "user_version", 0)
            .unwrap();
        drop(database);

        let migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert_eq!(migrated.list_backups().unwrap().len(), 1);
    }

    #[test]
    fn migrates_schema_one_events_to_reminder_outbox_columns() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schedule_events (
                    id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL,
                    notes TEXT NOT NULL DEFAULT '', start_at_utc TEXT NOT NULL,
                    time_zone TEXT NOT NULL, duration_minutes INTEGER NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE TABLE daily_tasks (
                    id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, day TEXT NOT NULL,
                    completed INTEGER NOT NULL DEFAULT 0, completed_at TEXT,
                    sort_order INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO schedule_events
                 (id, title, notes, start_at_utc, time_zone, duration_minutes, revision, created_at, updated_at)
                 VALUES (?1, 'Gym', '', '2030-05-10T22:00:00.000Z', 'America/New_York', 60, 1, ?2, ?2)",
                params![Uuid::new_v4().to_string(), "2026-08-13T12:00:00.000Z"],
            )
            .unwrap();
        drop(connection);

        let migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        let event = migrated.all_events().unwrap().remove(0);
        assert_eq!(event.reminder_minutes_before, None);
        assert_eq!(event.reminder_status, ReminderStatus::None);
        assert_eq!(migrated.list_backups().unwrap().len(), 1);
    }

    #[test]
    fn import_replaces_data_only_after_full_validation() {
        let mut database = database();
        database
            .create_event(event_input("Original", "2026-08-12T14:00:00Z"))
            .unwrap();
        let mut invalid = database.export_bundle().unwrap();
        invalid.events[0].id = "not-a-uuid".into();
        assert!(matches!(
            database.import_bundle(&invalid),
            Err(AppError::Validation(_))
        ));
        assert_eq!(
            database
                .events_for_day("2026-08-12", "America/New_York")
                .unwrap()[0]
                .title,
            "Original"
        );
        assert!(database.list_backups().unwrap().is_empty());
    }

    #[test]
    fn an_event_edit_updates_every_field_with_one_revision() {
        let mut database = database();
        let original = database
            .create_event(event_input("Gym", "2026-08-12T22:00:00Z"))
            .unwrap();
        let updated = database
            .update_event(UpdateEventInput {
                id: original.id,
                revision: original.revision,
                title: Some("Evening gym".into()),
                notes: Some("Bring water".into()),
                start_at_utc: Some("2026-08-13T23:00:00Z".into()),
                time_zone: Some("America/Chicago".into()),
                duration_minutes: Some(75),
                reminder_change: ReminderChange::Unchanged,
                location: None,
                plan_change: LinkChange::Unchanged,
                workstream_change: LinkChange::Unchanged,
                owner_change: LinkChange::Unchanged,
            })
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.title, "Evening gym");
        assert_eq!(updated.start_at_utc, "2026-08-13T23:00:00.000Z");
        assert_eq!(updated.time_zone, "America/Chicago");
        assert_eq!(updated.duration_minutes, 75);
    }

    #[test]
    fn a_reminder_is_revision_checked_in_the_same_event_transaction() {
        let mut database = database();
        let start = future_start(48);
        let original = database
            .create_event(event_input("Release", &start))
            .unwrap();
        let updated = database
            .update_event(UpdateEventInput {
                id: original.id.clone(),
                revision: original.revision,
                title: Some("Release review".into()),
                notes: None,
                start_at_utc: None,
                time_zone: None,
                duration_minutes: None,
                reminder_change: ReminderChange::Set { minutes_before: 30 },
                location: None,
                plan_change: LinkChange::Unchanged,
                workstream_change: LinkChange::Unchanged,
                owner_change: LinkChange::Unchanged,
            })
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.reminder_minutes_before, Some(30));
        assert_eq!(updated.reminder_status, ReminderStatus::Pending);

        let stale = database.update_event(UpdateEventInput {
            id: original.id,
            revision: 1,
            title: None,
            notes: None,
            start_at_utc: None,
            time_zone: None,
            duration_minutes: None,
            reminder_change: ReminderChange::Clear,
            location: None,
            plan_change: LinkChange::Unchanged,
            workstream_change: LinkChange::Unchanged,
            owner_change: LinkChange::Unchanged,
        });
        assert!(matches!(stale, Err(AppError::Conflict)));
        assert_eq!(
            database.all_events().unwrap()[0].reminder_minutes_before,
            Some(30)
        );
    }

    #[test]
    fn reminder_outbox_reconciles_and_marks_due_delivery_once() {
        let mut database = database();
        let mut input = event_input("Standup", &future_start(1));
        input.reminder_minutes_before = Some(0);
        let event = database.create_event(input).unwrap();
        database
            .connection
            .execute(
                "UPDATE schedule_events
                 SET start_at_utc=?1, reminder_status='pending'
                 WHERE id=?2",
                params![
                    utc_string(Utc::now() - ChronoDuration::minutes(1)),
                    event.id
                ],
            )
            .unwrap();
        database.reconcile_reminders().unwrap();
        let due = database.due_reminders(10).unwrap();
        assert_eq!(due.len(), 1);
        database.mark_reminder_delivered(&due[0]).unwrap();
        assert!(database.due_reminders(10).unwrap().is_empty());
        assert_eq!(
            database.all_events().unwrap()[0].reminder_status,
            ReminderStatus::Expired
        );
    }

    #[test]
    fn import_regenerates_internal_notification_identifiers() {
        let mut database = database();
        let mut input = event_input("Flight", &future_start(72));
        input.reminder_minutes_before = Some(60);
        let event = database.create_event(input).unwrap();
        let before: String = database
            .connection
            .query_row(
                "SELECT notification_id FROM schedule_events WHERE id=?1",
                params![event.id],
                |row| row.get(0),
            )
            .unwrap();
        let bundle = database.export_bundle().unwrap();
        database.import_bundle(&bundle).unwrap();
        let after: String = database
            .connection
            .query_row(
                "SELECT notification_id FROM schedule_events WHERE id=?1",
                params![event.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(before, after);
        assert_eq!(
            database.all_events().unwrap()[0].reminder_status,
            ReminderStatus::Pending
        );
    }

    #[test]
    fn reminder_offsets_are_limited_to_seven_days() {
        let mut input = event_input("Trip", &future_start(240));
        input.reminder_minutes_before = Some(MAX_REMINDER_MINUTES + 1);
        assert!(matches!(
            database().create_event(input),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn agenda_includes_an_event_that_overlaps_midnight() {
        let mut database = database();
        database
            .create_event(CreateEventInput {
                title: "Overnight flight".into(),
                notes: String::new(),
                start_at_utc: "2026-08-13T03:30:00Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 180,
                reminder_minutes_before: None,
                plan_id: None,
                location: String::new(),
                workstream_id: None,
                owner_id: None,
            })
            .unwrap();
        assert_eq!(
            database
                .events_for_day("2026-08-13", "America/New_York")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn daylight_saving_overlap_returns_both_clock_occurrences() {
        let result = PlannerDatabase::resolve_local_datetime(&LocalDateTimeInput {
            day: "2026-11-01".into(),
            time: "01:30".into(),
            time_zone: "America/New_York".into(),
        })
        .unwrap();
        assert!(matches!(
            result,
            LocalDateTimeResolution::Ambiguous { options } if options.len() == 2
        ));
    }

    #[test]
    fn daylight_saving_gap_is_never_silently_normalized() {
        let result = PlannerDatabase::resolve_local_datetime(&LocalDateTimeInput {
            day: "2026-03-08".into(),
            time: "02:30".into(),
            time_zone: "America/New_York".into(),
        })
        .unwrap();
        assert!(matches!(
            result,
            LocalDateTimeResolution::Nonexistent { .. }
        ));
    }

    #[test]
    fn migrates_schema_two_daily_tasks_into_general_tasks() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schedule_events (
                    id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL,
                    notes TEXT NOT NULL DEFAULT '', start_at_utc TEXT NOT NULL,
                    time_zone TEXT NOT NULL, duration_minutes INTEGER NOT NULL,
                    reminder_minutes_before INTEGER, reminder_status TEXT NOT NULL DEFAULT 'none',
                    notification_id TEXT UNIQUE, reminder_last_error TEXT,
                    revision INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE TABLE daily_tasks (
                    id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, day TEXT NOT NULL,
                    completed INTEGER NOT NULL DEFAULT 0, completed_at TEXT,
                    sort_order INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        let open_id = Uuid::new_v4().to_string();
        let done_id = Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO daily_tasks
                 (id, title, day, completed, completed_at, sort_order, created_at, updated_at)
                 VALUES (?1, 'Buy milk', '2026-08-12', 0, NULL, 0, ?3, ?3),
                        (?2, 'Pay rent', '2026-08-12', 1, ?4, 1, ?3, ?4)",
                params![
                    open_id,
                    done_id,
                    "2026-08-11T12:00:00.000Z",
                    "2026-08-12T09:00:00.000Z"
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO schedule_events
                 (id, title, notes, start_at_utc, time_zone, duration_minutes, revision, created_at, updated_at)
                 VALUES (?1, 'Gym', '', '2026-08-12T22:00:00.000Z', 'America/New_York', 60, 3, ?2, ?2)",
                params![Uuid::new_v4().to_string(), "2026-08-11T12:00:00.000Z"],
            )
            .unwrap();
        drop(connection);

        let migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert_eq!(migrated.list_backups().unwrap().len(), 1);
        assert!(!table_exists(&migrated.connection, "daily_tasks").unwrap());
        let tasks = migrated.tasks_for_day("2026-08-12").unwrap();
        let open = tasks.iter().find(|task| task.id == open_id).unwrap();
        assert_eq!(open.status, TaskStatus::Todo);
        assert_eq!(open.completed_at, None);
        assert_eq!(open.scheduled_day.as_deref(), Some("2026-08-12"));
        assert_eq!(
            (open.plan_id.as_deref(), open.due_date.as_deref()),
            (None, None)
        );
        let done = tasks.iter().find(|task| task.id == done_id).unwrap();
        assert_eq!(done.status, TaskStatus::Done);
        assert_eq!(
            done.completed_at.as_deref(),
            Some("2026-08-12T09:00:00.000Z")
        );
        assert_eq!((done.sort_order, done.revision), (1, 1));
        let event = migrated.all_events().unwrap().remove(0);
        assert_eq!((event.plan_id, event.revision), (None, 3));
    }

    #[test]
    fn events_join_and_leave_plans_and_ai_edits_keep_membership() {
        let mut database = database();
        let plan = database
            .create_plan(plan_input("Creator Showcase"))
            .unwrap();
        let mut input = event_input("Technical rehearsal", "2026-10-14T18:00:00Z");
        input.plan_id = Some(Uuid::new_v4().to_string());
        assert!(matches!(
            database.create_event(input.clone()),
            Err(AppError::Validation(_))
        ));
        input.plan_id = Some(plan.id.clone());
        let event = database.create_event(input).unwrap();
        assert_eq!(event.plan_id.as_deref(), Some(plan.id.as_str()));

        let proposal = ModelResponse::proposal(
            "Move the rehearsal",
            vec![MutationOperation::RescheduleEvent {
                event_id: event.id.clone(),
                expected_revision: event.revision,
                title: Some("Tech rehearsal".into()),
                notes: None,
                start_at_utc: "2026-10-14T19:00:00Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: None,
                reminder_change: ReminderChange::Unchanged,
            }],
        );
        let applied_id = database
            .apply_proposal(&proposal)
            .unwrap()
            .event_ids
            .remove(0);
        let applied = database.event_by_id(&applied_id).unwrap().unwrap();
        assert_eq!(applied.plan_id.as_deref(), Some(plan.id.as_str()));
        assert_eq!(database.plan_workspace(&plan.id).unwrap().events.len(), 1);

        let cleared = database
            .update_event(UpdateEventInput {
                id: applied.id,
                revision: applied.revision,
                title: None,
                notes: None,
                start_at_utc: None,
                time_zone: None,
                duration_minutes: None,
                reminder_change: ReminderChange::Unchanged,
                location: None,
                plan_change: LinkChange::Clear,
                workstream_change: LinkChange::Unchanged,
                owner_change: LinkChange::Unchanged,
            })
            .unwrap();
        assert_eq!((cleared.plan_id, cleared.revision), (None, 3));
        assert!(database.plan_workspace(&plan.id).unwrap().events.is_empty());
    }

    #[test]
    fn export_round_trips_the_planning_hierarchy() {
        let mut database = database();
        let plan = database.create_plan(plan_input("Launch")).unwrap();
        let milestone = database
            .create_milestone(crate::model::CreateMilestoneInput {
                plan_id: plan.id.clone(),
                title: "Beta".into(),
                description: "Invite testers".into(),
                target_date: Some("2026-10-01".into()),
                status: crate::model::MilestoneStatus::Pending,
                workstream_id: None,
            })
            .unwrap();
        database
            .create_task(crate::model::CreateTaskInput {
                plan_id: Some(plan.id.clone()),
                milestone_id: Some(milestone.id.clone()),
                status: TaskStatus::Done,
                ..task_input("Write release notes")
            })
            .unwrap();
        database
            .create_task(crate::model::CreateTaskInput {
                scheduled_day: Some("2026-09-15".into()),
                ..task_input("Laundry")
            })
            .unwrap();
        let mut event = event_input("Launch review", "2026-09-30T15:00:00Z");
        event.plan_id = Some(plan.id.clone());
        database.create_event(event).unwrap();

        let bundle = database.export_bundle().unwrap();
        let parsed = parse_export_bundle(&serde_json::to_string(&bundle).unwrap()).unwrap();
        assert_eq!(parsed, bundle);
        let preview = database.import_bundle(&parsed).unwrap();
        assert_eq!(
            (
                preview.plan_count,
                preview.milestone_count,
                preview.event_count,
                preview.task_count
            ),
            (1, 1, 1, 2)
        );
        let restored = database.export_bundle().unwrap();
        assert_eq!(
            (restored.plans, restored.milestones, restored.tasks),
            (bundle.plans, bundle.milestones, bundle.tasks)
        );
        assert_eq!(
            restored.events[0].plan_id.as_deref(),
            Some(plan.id.as_str())
        );
    }

    #[test]
    fn legacy_exports_upgrade_day_tasks_before_validation() {
        let legacy = serde_json::json!({
            "formatVersion": 2,
            "exportedAt": "2026-08-12T12:00:00.000Z",
            "events": [{
                "id": Uuid::new_v4(), "title": "Gym", "notes": "",
                "startAtUtc": "2026-08-12T22:00:00.000Z", "timeZone": "America/New_York",
                "durationMinutes": 60, "reminderMinutesBefore": null, "reminderStatus": "none",
                "revision": 2, "createdAt": "2026-08-11T12:00:00.000Z",
                "updatedAt": "2026-08-11T12:00:00.000Z"
            }],
            "tasks": [
                {
                    "id": Uuid::new_v4(), "title": "Buy milk", "day": "2026-08-12",
                    "completed": false, "completedAt": null, "sortOrder": 0,
                    "createdAt": "2026-08-11T12:00:00.000Z", "updatedAt": "2026-08-11T12:00:00.000Z"
                },
                {
                    "id": Uuid::new_v4(), "title": "Pay rent", "day": "2026-08-13",
                    "completed": true, "completedAt": "2026-08-12T09:00:00.000Z", "sortOrder": 1,
                    "createdAt": "2026-08-11T12:00:00.000Z", "updatedAt": "2026-08-12T09:00:00.000Z"
                }
            ]
        });
        let bundle = parse_export_bundle(&legacy.to_string()).unwrap();
        assert_eq!(bundle.format_version, EXPORT_FORMAT_VERSION);
        assert_eq!(bundle.tasks[0].status, TaskStatus::Todo);
        assert_eq!(bundle.tasks[1].status, TaskStatus::Done);
        assert_eq!(bundle.tasks[1].scheduled_day.as_deref(), Some("2026-08-13"));
        let preview = PlannerDatabase::preview_import(&bundle).unwrap();
        assert_eq!((preview.event_count, preview.task_count), (1, 2));
        assert_eq!(preview.latest_day.as_deref(), Some("2026-08-13"));

        let mut mismatched = legacy.clone();
        mismatched["tasks"][0]["completedAt"] = serde_json::json!("2026-08-12T09:00:00.000Z");
        let bundle = parse_export_bundle(&mismatched.to_string()).unwrap();
        assert!(matches!(
            PlannerDatabase::preview_import(&bundle),
            Err(AppError::Validation(_))
        ));
        let mut hybrid = legacy;
        hybrid["plans"] = serde_json::json!([]);
        assert!(parse_export_bundle(&hybrid.to_string()).is_err());
        assert!(matches!(
            parse_export_bundle(r#"{"formatVersion": 9}"#),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn import_rejects_dangling_plan_references_without_replacing_data() {
        let mut database = database();
        let plan = database.create_plan(plan_input("Trip")).unwrap();
        let mut event = event_input("Flight", "2026-10-02T12:00:00Z");
        event.plan_id = Some(plan.id.clone());
        database.create_event(event).unwrap();
        let mut bundle = database.export_bundle().unwrap();
        bundle.plans.clear();
        assert!(matches!(
            database.import_bundle(&bundle),
            Err(AppError::Validation(_))
        ));
        assert_eq!(database.list_plans("2026-09-14").unwrap().len(), 1);
        assert!(database.list_backups().unwrap().is_empty());
    }

    const SCHEMA_TWO_TABLES: &str = "CREATE TABLE schedule_events (
            id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL,
            notes TEXT NOT NULL DEFAULT '', start_at_utc TEXT NOT NULL,
            time_zone TEXT NOT NULL, duration_minutes INTEGER NOT NULL,
            reminder_minutes_before INTEGER, reminder_status TEXT NOT NULL DEFAULT 'none',
            notification_id TEXT UNIQUE, reminder_last_error TEXT,
            revision INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE TABLE daily_tasks (
            id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, day TEXT NOT NULL,
            completed INTEGER NOT NULL DEFAULT 0, completed_at TEXT,
            sort_order INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         PRAGMA user_version = 2;";

    #[test]
    fn migrates_schema_three_plans_to_the_team_schema() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_TWO_TABLES).unwrap();
        ensure_reminder_columns(&connection).unwrap();
        ensure_planning_schema(&connection).unwrap();
        connection.pragma_update(None, "user_version", 3).unwrap();
        let stamp = "2026-09-14T12:00:00.000Z";
        let plan_id = Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO plans (id, title, status, created_at, updated_at)
                 VALUES (?1, 'Creator Showcase', 'active', ?2, ?2)",
                params![plan_id, stamp],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO tasks (id, title, plan_id, status, created_at, updated_at)
                 VALUES (?1, 'Collect bios', ?2, 'todo', ?3, ?3)",
                params![Uuid::new_v4().to_string(), plan_id, stamp],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO schedule_events
                 (id, title, start_at_utc, time_zone, duration_minutes, plan_id, revision,
                  created_at, updated_at)
                 VALUES (?1, 'Production review', '2030-10-02T15:00:00.000Z',
                         'America/New_York', 60, ?2, 4, ?3, ?3)",
                params![Uuid::new_v4().to_string(), plan_id, stamp],
            )
            .unwrap();
        drop(connection);

        let migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(migrated.list_backups().unwrap()[0]
            .name
            .starts_with("dayplan-v3-"));
        for (table, column) in [
            ("people", "display_name"),
            ("workstreams", "plan_id"),
            ("tasks", "owner_id"),
            ("milestones", "workstream_id"),
            ("schedule_events", "location"),
        ] {
            assert!(column_exists(&migrated.connection, table, column).unwrap());
        }
        let workspace = migrated.plan_workspace(&plan_id).unwrap();
        assert_eq!(workspace.tasks[0].title, "Collect bios");
        assert_eq!(workspace.tasks[0].owner_id, None);
        let event = &workspace.events[0];
        assert_eq!(
            (
                event.location.as_str(),
                event.revision,
                event.workstream_id.as_deref()
            ),
            ("", 4, None)
        );
    }

    #[test]
    fn migrates_schema_four_tasks_to_weeks_estimates_and_the_inbox() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_TWO_TABLES).unwrap();
        ensure_reminder_columns(&connection).unwrap();
        ensure_planning_schema(&connection).unwrap();
        ensure_team_schema(&connection).unwrap();
        connection.pragma_update(None, "user_version", 4).unwrap();
        let stamp = "2026-09-14T12:00:00.000Z";
        connection
            .execute(
                "INSERT INTO tasks (id, title, scheduled_day, status, revision, created_at,
                                    updated_at)
                 VALUES (?1, 'Call parents', '2026-09-14', 'todo', 3, ?2, ?2)",
                params![Uuid::new_v4().to_string(), stamp],
            )
            .unwrap();
        drop(connection);

        let mut migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(migrated.list_backups().unwrap()[0]
            .name
            .starts_with("dayplan-v4-"));
        for column in ["planned_week", "estimated_minutes"] {
            assert!(column_exists(&migrated.connection, "tasks", column).unwrap());
        }
        let kept = migrated.tasks_for_day("2026-09-14").unwrap().remove(0);
        assert_eq!(
            (
                kept.title.as_str(),
                kept.revision,
                kept.planned_week,
                kept.estimated_minutes
            ),
            ("Call parents", 3, None, None)
        );
        assert!(migrated.list_inbox_items().unwrap().is_empty());
        migrated
            .create_inbox_item(crate::model::CreateInboxItemInput {
                text: "Ask Sarah about Saturday".into(),
                notes: String::new(),
            })
            .unwrap();
        let overlong = migrated.connection.execute(
            "UPDATE tasks SET estimated_minutes = 1441 WHERE id = ?1",
            params![kept.id],
        );
        assert!(overlong.is_err(), "the estimate check constraint applies");
    }

    #[test]
    fn export_round_trips_the_inbox_weeks_and_estimates_and_reads_format_four() {
        let mut database = database();
        database
            .create_task(crate::model::CreateTaskInput {
                planned_week: Some("2026-09-13".into()),
                estimated_minutes: Some(90),
                ..task_input("Write literature review outline")
            })
            .unwrap();
        database
            .create_inbox_item(crate::model::CreateInboxItemInput {
                text: "Look into authentication bug".into(),
                notes: "Only on Windows".into(),
            })
            .unwrap();

        let bundle = database.export_bundle().unwrap();
        assert_eq!(bundle.inbox_items.len(), 1);
        let json = serde_json::to_value(&bundle).unwrap();
        let parsed = parse_export_bundle(&json.to_string()).unwrap();
        assert_eq!(parsed, bundle);
        let preview = database.import_bundle(&parsed).unwrap();
        assert_eq!((preview.inbox_item_count, preview.task_count), (1, 1));
        let restored = database.export_bundle().unwrap();
        assert_eq!(restored.tasks, bundle.tasks);
        assert_eq!(restored.inbox_items, bundle.inbox_items);

        let mut format_four = json.clone();
        format_four["formatVersion"] = serde_json::json!(4);
        format_four.as_object_mut().unwrap().remove("inboxItems");
        format_four["tasks"][0]
            .as_object_mut()
            .unwrap()
            .remove("estimatedMinutes");
        format_four["tasks"][0]["scheduledDay"] = serde_json::json!("2026-09-15");
        format_four["tasks"][0]
            .as_object_mut()
            .unwrap()
            .remove("plannedWeek");
        let upgraded = parse_export_bundle(&format_four.to_string()).unwrap();
        assert_eq!(upgraded.format_version, EXPORT_FORMAT_VERSION);
        assert!(upgraded.inbox_items.is_empty());
        assert_eq!(
            (
                upgraded.tasks[0].planned_week.as_deref(),
                upgraded.tasks[0].estimated_minutes
            ),
            (None, None)
        );
        PlannerDatabase::preview_import(&upgraded).unwrap();

        let mut non_canonical = bundle.clone();
        non_canonical.tasks[0].planned_week = Some("2026-9-13".into());
        assert!(matches!(
            PlannerDatabase::preview_import(&non_canonical),
            Err(AppError::Validation(_))
        ));
        let mut duplicate = bundle;
        duplicate.inbox_items.push(duplicate.inbox_items[0].clone());
        assert!(matches!(
            PlannerDatabase::preview_import(&duplicate),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn migrates_schema_five_to_time_blocks_and_working_hours() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_TWO_TABLES).unwrap();
        ensure_reminder_columns(&connection).unwrap();
        ensure_planning_schema(&connection).unwrap();
        ensure_team_schema(&connection).unwrap();
        ensure_horizon_schema(&connection).unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        let task_id = Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO tasks (id, title, scheduled_day, estimated_minutes, status, revision,
                                    created_at, updated_at)
                 VALUES (?1, 'Draft the grant', '2030-09-16', 90, 'todo', 4, ?2, ?2)",
                params![task_id, "2026-09-14T12:00:00.000Z"],
            )
            .unwrap();
        drop(connection);

        let mut migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(migrated.list_backups().unwrap()[0]
            .name
            .starts_with("dayplan-v5-"));
        let kept = migrated.tasks_for_day("2030-09-16").unwrap().remove(0);
        assert_eq!(
            (kept.revision, kept.estimated_minutes),
            (4, Some(90)),
            "tasks migrate unchanged"
        );
        let hours = migrated.working_hours().unwrap();
        assert_eq!(
            (hours.days, hours.start_minute, hours.end_minute),
            (vec![1, 2, 3, 4, 5], 540, 1020)
        );
        migrated
            .create_task_block(crate::model::CreateTaskBlockInput {
                task_id: task_id.clone(),
                start_at_utc: "2030-09-16T14:00:00Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 90,
            })
            .unwrap();
        let overlong = migrated.connection.execute(
            "UPDATE task_blocks SET duration_minutes = 1441 WHERE task_id = ?1",
            params![task_id],
        );
        assert!(overlong.is_err(), "the duration check constraint applies");
    }

    #[test]
    fn export_round_trips_time_blocks_and_reads_format_five() {
        let mut database = database();
        let task = database
            .create_task(crate::model::CreateTaskInput {
                scheduled_day: Some("2030-09-16".into()),
                ..task_input("Draft the grant")
            })
            .unwrap();
        database
            .create_task_block(crate::model::CreateTaskBlockInput {
                task_id: task.id.clone(),
                start_at_utc: "2030-09-16T14:00:00.000Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 90,
            })
            .unwrap();

        let bundle = database.export_bundle().unwrap();
        assert_eq!(bundle.task_blocks.len(), 1);
        let json = serde_json::to_value(&bundle).unwrap();
        let parsed = parse_export_bundle(&json.to_string()).unwrap();
        assert_eq!(parsed, bundle);
        let preview = database.import_bundle(&parsed).unwrap();
        assert_eq!((preview.task_block_count, preview.task_count), (1, 1));
        assert_eq!(
            database.export_bundle().unwrap().task_blocks,
            bundle.task_blocks
        );

        let mut format_five = json.clone();
        format_five["formatVersion"] = serde_json::json!(5);
        format_five.as_object_mut().unwrap().remove("taskBlocks");
        let upgraded = parse_export_bundle(&format_five.to_string()).unwrap();
        assert_eq!(upgraded.format_version, EXPORT_FORMAT_VERSION);
        assert!(upgraded.task_blocks.is_empty());
        PlannerDatabase::preview_import(&upgraded).unwrap();

        let mut orphan = bundle.clone();
        orphan.tasks.clear();
        assert!(matches!(
            PlannerDatabase::preview_import(&orphan),
            Err(AppError::Validation(_))
        ));
        let mut shifted = bundle;
        shifted.task_blocks[0].start_at_utc = "2030-09-16T10:00:00-04:00".into();
        assert!(matches!(
            PlannerDatabase::preview_import(&shifted),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn restoring_an_older_backup_migrates_it_on_open() {
        let directory = tempdir().unwrap().keep();
        let path = directory.join("dayplan.sqlite3");
        let mut current = PlannerDatabase::open(&path).unwrap();
        current.create_plan(plan_input("Discarded plan")).unwrap();
        drop(current);

        let backups = directory.join("backups");
        fs::create_dir_all(&backups).unwrap();
        let backup_path = backups.join("dayplan-v2-20260801T120000Z-manual.sqlite3");
        let legacy = Connection::open(&backup_path).unwrap();
        legacy.execute_batch(SCHEMA_TWO_TABLES).unwrap();
        legacy
            .execute(
                "INSERT INTO daily_tasks
                 (id, title, day, completed, sort_order, created_at, updated_at)
                 VALUES (?1, 'Call parents', '2026-08-01', 0, 0, ?2, ?2)",
                params![Uuid::new_v4().to_string(), "2026-07-31T12:00:00.000Z"],
            )
            .unwrap();
        drop(legacy);

        restore_backup(&path, "dayplan-v2-20260801T120000Z-manual.sqlite3").unwrap();
        let restored = PlannerDatabase::open(&path).unwrap();
        assert_eq!(restored.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(restored.list_plans("2026-09-14").unwrap().is_empty());
        assert_eq!(
            restored.tasks_for_day("2026-08-01").unwrap()[0].title,
            "Call parents"
        );
    }

    #[test]
    fn foreign_keys_cascade_plan_structure_and_null_optional_links() {
        let mut database = database();
        let plan = database.create_plan(plan_input("Wedding")).unwrap();
        let person = database
            .create_person(crate::model::CreatePersonInput {
                display_name: "Venue contact".into(),
                role: String::new(),
                email: None,
                notes: String::new(),
            })
            .unwrap();
        let workstream = database
            .create_workstream(crate::model::CreateWorkstreamInput {
                plan_id: plan.id.clone(),
                name: "Venue".into(),
                description: String::new(),
            })
            .unwrap();
        database
            .create_milestone(crate::model::CreateMilestoneInput {
                plan_id: plan.id.clone(),
                title: "Venue confirmed".into(),
                description: String::new(),
                target_date: None,
                status: crate::model::MilestoneStatus::Pending,
                workstream_id: Some(workstream.id.clone()),
            })
            .unwrap();
        let task = database
            .create_task(crate::model::CreateTaskInput {
                scheduled_day: Some("2027-05-01".into()),
                owner_id: Some(person.id.clone()),
                ..task_input("Sign contract")
            })
            .unwrap();
        let orphan = database.connection.execute(
            "INSERT INTO milestones (id, plan_id, title, created_at, updated_at)
             VALUES (?1, ?2, 'Orphan', ?3, ?3)",
            params![
                Uuid::new_v4().to_string(),
                Uuid::new_v4().to_string(),
                now()
            ],
        );
        assert!(
            orphan.is_err(),
            "a milestone cannot reference a missing plan"
        );

        database
            .connection
            .execute("DELETE FROM people WHERE id = ?1", params![person.id])
            .unwrap();
        assert_eq!(
            database.tasks_for_day("2027-05-01").unwrap()[0].owner_id,
            None,
            "deleting a person row nulls task owners (row id {})",
            task.id
        );
        database
            .connection
            .execute("DELETE FROM plans WHERE id = ?1", params![plan.id])
            .unwrap();
        for table in ["milestones", "workstreams"] {
            let remaining: i64 = database
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(remaining, 0, "{table} cascade with their plan");
        }
    }

    #[test]
    fn week_agenda_collects_everything_dated_in_the_window() {
        let mut database = database();
        let plan = database
            .create_plan(plan_input("Streamer Charity Week"))
            .unwrap();
        database
            .create_milestone(crate::model::CreateMilestoneInput {
                plan_id: plan.id.clone(),
                title: "Event start".into(),
                description: String::new(),
                target_date: Some("2030-11-09".into()),
                status: crate::model::MilestoneStatus::Pending,
                workstream_id: None,
            })
            .unwrap();
        database
            .create_milestone(crate::model::CreateMilestoneInput {
                plan_id: plan.id.clone(),
                title: "Post-event report".into(),
                description: String::new(),
                target_date: Some("2030-11-16".into()),
                status: crate::model::MilestoneStatus::Pending,
                workstream_id: None,
            })
            .unwrap();
        for (title, scheduled, due) in [
            ("Finish overlays", Some("2030-11-10"), None),
            ("Approve sponsor graphics", None, Some("2030-11-15")),
            (
                "Prepare social posts",
                Some("2030-11-11"),
                Some("2030-11-11"),
            ),
            ("Outside the week", Some("2030-11-16"), None),
        ] {
            database
                .create_task(crate::model::CreateTaskInput {
                    plan_id: Some(plan.id.clone()),
                    scheduled_day: scheduled.map(Into::into),
                    due_date: due.map(Into::into),
                    ..task_input(title)
                })
                .unwrap();
        }
        database
            .create_event(event_input("Opening broadcast", "2030-11-09T23:00:00Z"))
            .unwrap();
        database
            .create_event(event_input(
                "Late closing broadcast",
                "2030-11-16T04:30:00Z",
            ))
            .unwrap();
        database
            .create_event(event_input("Next week", "2030-11-16T18:00:00Z"))
            .unwrap();

        let week = database
            .agenda("2030-11-09", 7, "America/New_York")
            .unwrap();
        let titles = |items: &[Task]| {
            items
                .iter()
                .map(|task| task.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            titles(&week.tasks),
            ["Finish overlays", "Prepare social posts"]
        );
        assert_eq!(titles(&week.due_tasks), ["Approve sponsor graphics"]);
        assert_eq!(
            week.milestones
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["Event start"]
        );
        assert_eq!(
            week.events
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["Opening broadcast", "Late closing broadcast"]
        );
        let today = database
            .agenda("2030-11-11", 1, "America/New_York")
            .unwrap();
        assert_eq!(titles(&today.tasks), ["Prepare social posts"]);
        assert!(today.due_tasks.is_empty());
        assert!(matches!(
            database.agenda("2030-11-09", 15, "America/New_York"),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn export_round_trips_people_and_workstreams_and_reads_format_three() {
        let mut database = database();
        let plan = database
            .create_plan(plan_input("Streamer Charity Week"))
            .unwrap();
        let maya = database
            .create_person(crate::model::CreatePersonInput {
                display_name: "Maya".into(),
                role: "Creator Relations".into(),
                email: Some("maya@example.com".into()),
                notes: String::new(),
            })
            .unwrap();
        let relations = database
            .create_workstream(crate::model::CreateWorkstreamInput {
                plan_id: plan.id.clone(),
                name: "Creator Relations".into(),
                description: String::new(),
            })
            .unwrap();
        database
            .create_task(crate::model::CreateTaskInput {
                plan_id: Some(plan.id.clone()),
                workstream_id: Some(relations.id.clone()),
                owner_id: Some(maya.id.clone()),
                ..task_input("Confirm creators")
            })
            .unwrap();
        let mut briefing = event_input("Creator briefing", "2030-11-09T15:00:00Z");
        briefing.plan_id = Some(plan.id.clone());
        briefing.owner_id = Some(maya.id.clone());
        briefing.location = "Stage A".into();
        database.create_event(briefing).unwrap();

        let bundle = database.export_bundle().unwrap();
        let json = serde_json::to_value(&bundle).unwrap();
        let parsed = parse_export_bundle(&json.to_string()).unwrap();
        assert_eq!(parsed, bundle);
        let preview = database.import_bundle(&parsed).unwrap();
        assert_eq!((preview.person_count, preview.workstream_count), (1, 1));
        let restored = database.export_bundle().unwrap();
        assert_eq!(restored.people, bundle.people);
        assert_eq!(restored.workstreams, bundle.workstreams);
        assert_eq!(restored.events[0].location, "Stage A");

        let mut format_three = json.clone();
        format_three["formatVersion"] = serde_json::json!(3);
        let object = format_three.as_object_mut().unwrap();
        object.remove("people");
        object.remove("workstreams");
        for task in format_three["tasks"].as_array_mut().unwrap() {
            let task = task.as_object_mut().unwrap();
            task.remove("workstreamId");
            task.remove("ownerId");
        }
        for event in format_three["events"].as_array_mut().unwrap() {
            let event = event.as_object_mut().unwrap();
            event.remove("location");
            event.remove("workstreamId");
            event.remove("ownerId");
        }
        let upgraded = parse_export_bundle(&format_three.to_string()).unwrap();
        assert_eq!(upgraded.format_version, EXPORT_FORMAT_VERSION);
        let preview = PlannerDatabase::preview_import(&upgraded).unwrap();
        assert_eq!((preview.person_count, preview.task_count), (0, 1));

        let mut dangling = bundle.clone();
        dangling.people.clear();
        assert!(matches!(
            PlannerDatabase::preview_import(&dangling),
            Err(AppError::Validation(_))
        ));
        let mut cross_plan = bundle;
        let other = crate::model::Plan {
            id: Uuid::new_v4().to_string(),
            title: "Other".into(),
            ..cross_plan.plans[0].clone()
        };
        cross_plan.tasks[0].plan_id = Some(other.id.clone());
        cross_plan.plans.push(other);
        assert!(matches!(
            PlannerDatabase::preview_import(&cross_plan),
            Err(AppError::Validation(_))
        ));
    }
}
