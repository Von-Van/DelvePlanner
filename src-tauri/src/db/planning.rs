//! Plans, milestones, and general tasks: the planning hierarchy above the day agenda.

use super::team::validate_links;
use super::{
    collect, event_from_row, normalize_day, now, validate_id, validate_revision, validate_title,
    PlannerDatabase, SqlConnection, EVENT_SELECT,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    CreateMilestoneInput, CreatePlanInput, CreateTaskInput, Milestone, MilestoneStatus, Plan,
    PlanColor, PlanDeletion, PlanStatus, PlanSummary, PlanWorkspace, Task, TaskPriority,
    TaskStatus, UpdateMilestoneInput, UpdatePlanInput, UpdateTaskInput, MAX_DESCRIPTION_LENGTH,
};
use rusqlite::{params, types::Type, OptionalExtension, Row, TransactionBehavior};
use std::collections::HashMap;
use uuid::Uuid;

const PLAN_SELECT: &str = "SELECT id, title, description, status, start_date, target_date, color,
        archived, revision, created_at, updated_at
     FROM plans";
const MILESTONE_SELECT: &str = "SELECT id, plan_id, title, description, target_date, status,
        workstream_id, sort_order, revision, created_at, updated_at
     FROM milestones";
pub(super) const TASK_SELECT: &str = "SELECT id, title, description, plan_id, milestone_id,
        workstream_id, owner_id, due_date, scheduled_day, status, priority, completed_at,
        sort_order, revision, created_at, updated_at
     FROM tasks";
const PLAN_ORDER: &str =
    "ORDER BY archived ASC, target_date IS NULL, target_date ASC, created_at ASC";
const MILESTONE_ORDER: &str =
    "ORDER BY target_date IS NULL, target_date ASC, sort_order ASC, created_at ASC";
const TASK_ORDER: &str = "ORDER BY status = 'done', sort_order ASC, created_at ASC";

impl PlannerDatabase {
    /// Every plan with its progress, milestone count, open tasks overdue as of `today` (a local
    /// day), and next pending milestone.
    pub fn list_plans(&self, today: &str) -> AppResult<Vec<PlanSummary>> {
        let today = normalize_day(today)?;
        let plans = self.all_plans()?;
        let mut task_counts = HashMap::new();
        let mut statement = self.connection.prepare(
            "SELECT plan_id, COUNT(*), SUM(status = 'done'),
                    SUM(status <> 'done' AND due_date IS NOT NULL AND due_date < ?1)
             FROM tasks
             WHERE plan_id IS NOT NULL GROUP BY plan_id",
        )?;
        let rows = statement.query_map(params![today], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TaskTotals {
                    total: row.get(1)?,
                    done: row.get(2)?,
                    overdue: row.get(3)?,
                },
            ))
        })?;
        for row in rows {
            let (plan_id, totals) = row?;
            task_counts.insert(plan_id, totals);
        }
        let mut milestone_counts = HashMap::new();
        let mut statement = self
            .connection
            .prepare("SELECT plan_id, COUNT(*) FROM milestones GROUP BY plan_id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (plan_id, count) = row?;
            milestone_counts.insert(plan_id, count);
        }
        let mut next_milestones = HashMap::new();
        let mut statement = self.connection.prepare(&format!(
            "{MILESTONE_SELECT} WHERE status = 'pending' {MILESTONE_ORDER}"
        ))?;
        for milestone in collect(statement.query_map([], milestone_from_row)?)? {
            next_milestones
                .entry(milestone.plan_id.clone())
                .or_insert(milestone);
        }
        Ok(plans
            .into_iter()
            .map(|plan| {
                let totals = task_counts.get(&plan.id).copied().unwrap_or_default();
                PlanSummary {
                    next_milestone: next_milestones.remove(&plan.id),
                    milestone_count: milestone_counts.get(&plan.id).copied().unwrap_or(0),
                    task_count: totals.total,
                    completed_task_count: totals.done,
                    overdue_task_count: totals.overdue,
                    plan,
                }
            })
            .collect())
    }

    pub fn plan_workspace(&self, id: &str) -> AppResult<PlanWorkspace> {
        validate_id(id)?;
        let plan = plan_by_id(&self.connection, id)?.ok_or(AppError::NotFound)?;
        let mut statement = self.connection.prepare(&format!(
            "{MILESTONE_SELECT} WHERE plan_id = ?1 {MILESTONE_ORDER}"
        ))?;
        let milestones = collect(statement.query_map(params![id], milestone_from_row)?)?;
        let mut statement = self
            .connection
            .prepare(&format!("{TASK_SELECT} WHERE plan_id = ?1 {TASK_ORDER}"))?;
        let tasks = collect(statement.query_map(params![id], task_from_row)?)?;
        let mut statement = self.connection.prepare(&format!(
            "{EVENT_SELECT} WHERE plan_id = ?1 ORDER BY start_at_utc ASC, created_at ASC"
        ))?;
        let events = collect(statement.query_map(params![id], event_from_row)?)?;
        Ok(PlanWorkspace {
            workstreams: self.workstreams_for_plan(id)?,
            plan,
            milestones,
            tasks,
            events,
        })
    }

    pub fn create_plan(&mut self, input: CreatePlanInput) -> AppResult<Plan> {
        insert_plan(&self.connection, &input)
    }

    pub fn update_plan(&mut self, input: UpdatePlanInput) -> AppResult<Plan> {
        replace_plan(&self.connection, &input)
    }

    /// Permanently deletes an archived plan in one transaction. Its workstreams, milestones, and
    /// the tasks that exist only inside it are removed; tasks with a scheduled or due day and all
    /// events are kept without a plan or workstream, with their revisions advanced so stale edits
    /// and proposals are rejected. Owners are people outside the plan and stay assigned.
    pub fn delete_plan(&mut self, id: &str, revision: i64) -> AppResult<PlanDeletion> {
        validate_id(id)?;
        validate_revision(revision)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let plan = plan_by_id(&transaction, id)?.ok_or(AppError::NotFound)?;
        if plan.revision != revision {
            return Err(AppError::Conflict);
        }
        if !plan.archived {
            return Err(AppError::Validation(
                "Archive a plan before deleting it permanently.".into(),
            ));
        }
        let timestamp = now();
        let deleted_tasks = transaction.execute(
            "DELETE FROM tasks
             WHERE plan_id = ?1 AND scheduled_day IS NULL AND due_date IS NULL",
            params![id],
        )?;
        let detached_tasks = transaction.execute(
            "UPDATE tasks
             SET plan_id = NULL, milestone_id = NULL, workstream_id = NULL,
                 revision = revision + 1, updated_at = ?1
             WHERE plan_id = ?2",
            params![timestamp, id],
        )?;
        let detached_events = transaction.execute(
            "UPDATE schedule_events
             SET plan_id = NULL, workstream_id = NULL, revision = revision + 1, updated_at = ?1
             WHERE plan_id = ?2",
            params![timestamp, id],
        )?;
        let deleted_milestones =
            transaction.execute("DELETE FROM milestones WHERE plan_id = ?1", params![id])?;
        let deleted_workstreams =
            transaction.execute("DELETE FROM workstreams WHERE plan_id = ?1", params![id])?;
        transaction.execute("DELETE FROM plans WHERE id = ?1", params![id])?;
        transaction.commit()?;
        Ok(PlanDeletion {
            deleted_workstreams,
            deleted_milestones,
            deleted_tasks,
            detached_tasks,
            detached_events,
        })
    }

    pub fn create_milestone(&mut self, input: CreateMilestoneInput) -> AppResult<Milestone> {
        insert_milestone(&self.connection, &input)
    }

    pub fn update_milestone(&mut self, input: UpdateMilestoneInput) -> AppResult<Milestone> {
        replace_milestone(&self.connection, &input)
    }

    /// Deletes a milestone and keeps its tasks in the plan without a milestone.
    pub fn delete_milestone(&mut self, id: &str, revision: i64) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        remove_milestone(&transaction, id, revision)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn create_task(&mut self, input: CreateTaskInput) -> AppResult<Task> {
        insert_task(&self.connection, &input)
    }

    pub fn update_task(&mut self, input: UpdateTaskInput) -> AppResult<Task> {
        replace_task(&self.connection, &input)
    }

    pub fn delete_task(&mut self, id: &str, revision: i64) -> AppResult<()> {
        remove_task(&self.connection, id, revision)
    }

    pub fn tasks_for_day(&self, day: &str) -> AppResult<Vec<Task>> {
        let day = normalize_day(day)?;
        self.tasks_scheduled_between(&day, &super::next_day(&day)?)
    }

    /// Tasks due on `day` that are not also scheduled onto it, so the agenda lists each task once.
    pub fn tasks_due_on(&self, day: &str) -> AppResult<Vec<Task>> {
        let day = normalize_day(day)?;
        self.tasks_due_between(&day, &super::next_day(&day)?)
    }

    /// Tasks scheduled on a day in `[start, end)`; both bounds are canonical local days.
    pub(super) fn tasks_scheduled_between(&self, start: &str, end: &str) -> AppResult<Vec<Task>> {
        let mut statement = self.connection.prepare(&format!(
            "{TASK_SELECT} WHERE scheduled_day >= ?1 AND scheduled_day < ?2
             ORDER BY scheduled_day ASC, status = 'done', sort_order ASC, created_at ASC"
        ))?;
        let rows = statement.query_map(params![start, end], task_from_row)?;
        collect(rows)
    }

    /// Tasks due in `[start, end)`, excluding a task already scheduled on its own due day.
    pub(super) fn tasks_due_between(&self, start: &str, end: &str) -> AppResult<Vec<Task>> {
        let mut statement = self.connection.prepare(&format!(
            "{TASK_SELECT}
             WHERE due_date >= ?1 AND due_date < ?2
               AND (scheduled_day IS NULL OR scheduled_day <> due_date)
             ORDER BY due_date ASC, status = 'done', sort_order ASC, created_at ASC"
        ))?;
        let rows = statement.query_map(params![start, end], task_from_row)?;
        collect(rows)
    }

    pub(super) fn milestones_between(&self, start: &str, end: &str) -> AppResult<Vec<Milestone>> {
        let mut statement = self.connection.prepare(&format!(
            "{MILESTONE_SELECT} WHERE target_date >= ?1 AND target_date < ?2 {MILESTONE_ORDER}"
        ))?;
        let rows = statement.query_map(params![start, end], milestone_from_row)?;
        collect(rows)
    }

    pub(super) fn all_plans(&self) -> AppResult<Vec<Plan>> {
        let mut statement = self
            .connection
            .prepare(&format!("{PLAN_SELECT} {PLAN_ORDER}"))?;
        let rows = statement.query_map([], plan_from_row)?;
        collect(rows)
    }

    pub(super) fn all_milestones(&self) -> AppResult<Vec<Milestone>> {
        let mut statement = self.connection.prepare(&format!(
            "{MILESTONE_SELECT} ORDER BY plan_id ASC, sort_order ASC"
        ))?;
        let rows = statement.query_map([], milestone_from_row)?;
        collect(rows)
    }

    pub(super) fn all_tasks(&self) -> AppResult<Vec<Task>> {
        let mut statement = self.connection.prepare(&format!(
            "{TASK_SELECT} ORDER BY sort_order ASC, created_at ASC"
        ))?;
        let rows = statement.query_map([], task_from_row)?;
        collect(rows)
    }
}

#[derive(Clone, Copy, Default)]
struct TaskTotals {
    total: i64,
    done: i64,
    overdue: i64,
}

pub(super) fn insert_plan<C: SqlConnection>(
    connection: &C,
    input: &CreatePlanInput,
) -> AppResult<Plan> {
    let (start_date, target_date) = validate_plan_shape(
        &input.title,
        &input.description,
        input.start_date.as_deref(),
        input.target_date.as_deref(),
    )?;
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    connection.connection().execute(
        "INSERT INTO plans
         (id, title, description, status, start_date, target_date, color, archived,
          revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 1, ?8, ?8)",
        params![
            id,
            input.title.trim(),
            input.description.trim(),
            input.status.as_str(),
            start_date,
            target_date,
            input.color.map(PlanColor::as_str),
            timestamp
        ],
    )?;
    plan_by_id(connection, &id)?.ok_or(AppError::NotFound)
}

/// Replaces a plan's editable fields when its revision still matches.
pub(super) fn replace_plan<C: SqlConnection>(
    connection: &C,
    input: &UpdatePlanInput,
) -> AppResult<Plan> {
    validate_id(&input.id)?;
    validate_revision(input.revision)?;
    let (start_date, target_date) = validate_plan_shape(
        &input.title,
        &input.description,
        input.start_date.as_deref(),
        input.target_date.as_deref(),
    )?;
    let changed = connection.connection().execute(
        "UPDATE plans
         SET title = ?1, description = ?2, status = ?3, start_date = ?4, target_date = ?5,
             color = ?6, archived = ?7, revision = revision + 1, updated_at = ?8
         WHERE id = ?9 AND revision = ?10",
        params![
            input.title.trim(),
            input.description.trim(),
            input.status.as_str(),
            start_date,
            target_date,
            input.color.map(PlanColor::as_str),
            input.archived as i64,
            now(),
            input.id,
            input.revision
        ],
    )?;
    if changed != 1 {
        return Err(stale_write_error(connection, "plans", &input.id)?);
    }
    plan_by_id(connection, &input.id)?.ok_or(AppError::NotFound)
}

pub(super) fn insert_milestone<C: SqlConnection>(
    connection: &C,
    input: &CreateMilestoneInput,
) -> AppResult<Milestone> {
    let target_date = validate_milestone_shape(
        &input.title,
        &input.description,
        input.target_date.as_deref(),
    )?;
    ensure_plan_exists(connection, &input.plan_id)?;
    validate_links(
        connection,
        Some(&input.plan_id),
        input.workstream_id.as_deref(),
        None,
    )?;
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    let next_order: i64 = connection.connection().query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM milestones WHERE plan_id = ?1",
        params![input.plan_id],
        |row| row.get(0),
    )?;
    connection.connection().execute(
        "INSERT INTO milestones
         (id, plan_id, title, description, target_date, status, workstream_id, sort_order,
          revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
        params![
            id,
            input.plan_id,
            input.title.trim(),
            input.description.trim(),
            target_date,
            input.status.as_str(),
            input.workstream_id,
            next_order,
            timestamp
        ],
    )?;
    milestone_by_id(connection, &id)?.ok_or(AppError::NotFound)
}

/// Replaces a milestone's editable fields when its revision still matches.
pub(super) fn replace_milestone<C: SqlConnection>(
    connection: &C,
    input: &UpdateMilestoneInput,
) -> AppResult<Milestone> {
    validate_id(&input.id)?;
    validate_revision(input.revision)?;
    let target_date = validate_milestone_shape(
        &input.title,
        &input.description,
        input.target_date.as_deref(),
    )?;
    let existing = milestone_by_id(connection, &input.id)?.ok_or(AppError::NotFound)?;
    if existing.revision != input.revision {
        return Err(AppError::Conflict);
    }
    validate_links(
        connection,
        Some(&existing.plan_id),
        input.workstream_id.as_deref(),
        None,
    )?;
    let changed = connection.connection().execute(
        "UPDATE milestones
         SET title = ?1, description = ?2, target_date = ?3, status = ?4, workstream_id = ?5,
             revision = revision + 1, updated_at = ?6
         WHERE id = ?7 AND revision = ?8",
        params![
            input.title.trim(),
            input.description.trim(),
            target_date,
            input.status.as_str(),
            input.workstream_id,
            now(),
            input.id,
            input.revision
        ],
    )?;
    if changed != 1 {
        return Err(stale_write_error(connection, "milestones", &input.id)?);
    }
    milestone_by_id(connection, &input.id)?.ok_or(AppError::NotFound)
}

/// Deletes a milestone inside the caller's transaction, detaching its tasks with a revision bump.
pub(super) fn remove_milestone<C: SqlConnection>(
    connection: &C,
    id: &str,
    revision: i64,
) -> AppResult<()> {
    validate_id(id)?;
    validate_revision(revision)?;
    let milestone = milestone_by_id(connection, id)?.ok_or(AppError::NotFound)?;
    if milestone.revision != revision {
        return Err(AppError::Conflict);
    }
    let connection = connection.connection();
    connection.execute(
        "UPDATE tasks
         SET milestone_id = NULL, revision = revision + 1, updated_at = ?1
         WHERE milestone_id = ?2",
        params![now(), id],
    )?;
    connection.execute("DELETE FROM milestones WHERE id = ?1", params![id])?;
    Ok(())
}

pub(super) fn remove_task<C: SqlConnection>(
    connection: &C,
    id: &str,
    revision: i64,
) -> AppResult<()> {
    validate_id(id)?;
    validate_revision(revision)?;
    let changed = connection.connection().execute(
        "DELETE FROM tasks WHERE id = ?1 AND revision = ?2",
        params![id, revision],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(stale_write_error(connection, "tasks", id)?)
    }
}

pub(super) fn insert_task<C: SqlConnection>(
    connection: &C,
    input: &CreateTaskInput,
) -> AppResult<Task> {
    let shape = TaskShape {
        title: &input.title,
        description: &input.description,
        plan_id: input.plan_id.as_deref(),
        milestone_id: input.milestone_id.as_deref(),
        workstream_id: input.workstream_id.as_deref(),
        owner_id: input.owner_id.as_deref(),
        due_date: input.due_date.as_deref(),
        scheduled_day: input.scheduled_day.as_deref(),
    };
    let days = validate_task_shape(&shape)?;
    validate_task_references(connection, &shape)?;
    let connection = connection.connection();
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    let completed_at = (input.status == TaskStatus::Done).then(|| timestamp.clone());
    let next_order: i64 = connection.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM tasks",
        [],
        |row| row.get(0),
    )?;
    connection.execute(
        "INSERT INTO tasks
         (id, title, description, plan_id, milestone_id, workstream_id, owner_id, due_date,
          scheduled_day, status, priority, completed_at, sort_order, revision, created_at,
          updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, ?14)",
        params![
            id,
            input.title.trim(),
            input.description.trim(),
            input.plan_id,
            input.milestone_id,
            input.workstream_id,
            input.owner_id,
            days.due_date,
            days.scheduled_day,
            input.status.as_str(),
            input.priority.as_str(),
            completed_at,
            next_order,
            timestamp
        ],
    )?;
    task_by_id(connection, &id)?.ok_or(AppError::NotFound)
}

/// Replaces a task's editable fields when its revision still matches.
pub(super) fn replace_task<C: SqlConnection>(
    connection: &C,
    input: &UpdateTaskInput,
) -> AppResult<Task> {
    validate_id(&input.id)?;
    validate_revision(input.revision)?;
    let existing = task_by_id(connection, &input.id)?.ok_or(AppError::NotFound)?;
    if existing.revision != input.revision {
        return Err(AppError::Conflict);
    }
    let shape = TaskShape {
        title: &input.title,
        description: &input.description,
        plan_id: input.plan_id.as_deref(),
        milestone_id: input.milestone_id.as_deref(),
        workstream_id: input.workstream_id.as_deref(),
        owner_id: input.owner_id.as_deref(),
        due_date: input.due_date.as_deref(),
        scheduled_day: input.scheduled_day.as_deref(),
    };
    let days = validate_task_shape(&shape)?;
    validate_task_references(connection, &shape)?;
    let completed_at = match input.status {
        TaskStatus::Done if existing.status == TaskStatus::Done => {
            existing.completed_at.or_else(|| Some(now()))
        }
        TaskStatus::Done => Some(now()),
        _ => None,
    };
    let changed = connection.connection().execute(
        "UPDATE tasks
         SET title = ?1, description = ?2, plan_id = ?3, milestone_id = ?4, workstream_id = ?5,
             owner_id = ?6, due_date = ?7, scheduled_day = ?8, status = ?9, priority = ?10,
             completed_at = ?11, revision = revision + 1, updated_at = ?12
         WHERE id = ?13 AND revision = ?14",
        params![
            input.title.trim(),
            input.description.trim(),
            input.plan_id,
            input.milestone_id,
            input.workstream_id,
            input.owner_id,
            days.due_date,
            days.scheduled_day,
            input.status.as_str(),
            input.priority.as_str(),
            completed_at,
            now(),
            input.id,
            input.revision
        ],
    )?;
    if changed != 1 {
        return Err(stale_write_error(connection, "tasks", &input.id)?);
    }
    task_by_id(connection, &input.id)?.ok_or(AppError::NotFound)
}

/// The user-editable parts of a task, validated without touching the database.
pub(super) struct TaskShape<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub plan_id: Option<&'a str>,
    pub milestone_id: Option<&'a str>,
    pub workstream_id: Option<&'a str>,
    pub owner_id: Option<&'a str>,
    pub due_date: Option<&'a str>,
    pub scheduled_day: Option<&'a str>,
}

pub(super) struct TaskDays {
    pub due_date: Option<String>,
    pub scheduled_day: Option<String>,
}

pub(super) fn validate_task_shape(shape: &TaskShape<'_>) -> AppResult<TaskDays> {
    validate_title(shape.title)?;
    validate_description(shape.description)?;
    for id in [shape.plan_id, shape.owner_id].into_iter().flatten() {
        validate_id(id)?;
    }
    if let Some(milestone_id) = shape.milestone_id {
        validate_id(milestone_id)?;
        if shape.plan_id.is_none() {
            return Err(milestone_plan_mismatch());
        }
    }
    if let Some(workstream_id) = shape.workstream_id {
        validate_id(workstream_id)?;
        if shape.plan_id.is_none() {
            return Err(super::team::workstream_plan_mismatch());
        }
    }
    let days = TaskDays {
        due_date: shape.due_date.map(normalize_day).transpose()?,
        scheduled_day: shape.scheduled_day.map(normalize_day).transpose()?,
    };
    if shape.plan_id.is_none() && days.due_date.is_none() && days.scheduled_day.is_none() {
        return Err(AppError::Validation(
            "A task needs a plan, a scheduled day, or a due date.".into(),
        ));
    }
    Ok(days)
}

fn validate_task_references<C: SqlConnection>(
    connection: &C,
    shape: &TaskShape<'_>,
) -> AppResult<()> {
    if let Some(plan_id) = shape.plan_id {
        ensure_plan_exists(connection, plan_id)?;
    }
    if let Some(milestone_id) = shape.milestone_id {
        let milestone = milestone_by_id(connection, milestone_id)?.ok_or_else(|| {
            AppError::Validation("The selected milestone no longer exists.".into())
        })?;
        if shape.plan_id != Some(milestone.plan_id.as_str()) {
            return Err(milestone_plan_mismatch());
        }
    }
    validate_links(
        connection,
        shape.plan_id,
        shape.workstream_id,
        shape.owner_id,
    )
}

pub(super) fn milestone_plan_mismatch() -> AppError {
    AppError::Validation("A task's milestone must belong to the task's plan.".into())
}

/// Validates plan text and dates, returning the dates in canonical `YYYY-MM-DD` form.
pub(super) fn validate_plan_shape(
    title: &str,
    description: &str,
    start_date: Option<&str>,
    target_date: Option<&str>,
) -> AppResult<(Option<String>, Option<String>)> {
    validate_title(title)?;
    validate_description(description)?;
    let start_date = start_date.map(normalize_day).transpose()?;
    let target_date = target_date.map(normalize_day).transpose()?;
    if let (Some(start), Some(target)) = (&start_date, &target_date) {
        if start > target {
            return Err(AppError::Validation(
                "A plan's start date must be on or before its target date.".into(),
            ));
        }
    }
    Ok((start_date, target_date))
}

pub(super) fn validate_milestone_shape(
    title: &str,
    description: &str,
    target_date: Option<&str>,
) -> AppResult<Option<String>> {
    validate_title(title)?;
    validate_description(description)?;
    target_date.map(normalize_day).transpose()
}

pub(super) fn validate_description(value: &str) -> AppResult<()> {
    if value.trim().chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err(AppError::Validation(
            "Descriptions must be 2,000 characters or fewer.".into(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_plan_exists<C: SqlConnection>(connection: &C, plan_id: &str) -> AppResult<()> {
    validate_id(plan_id)?;
    let exists: bool = connection.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM plans WHERE id = ?1)",
        params![plan_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(AppError::Validation(
            "The selected plan no longer exists.".into(),
        ))
    }
}

/// Distinguishes a revision conflict from a missing record after a guarded write changed nothing.
fn stale_write_error<C: SqlConnection>(
    connection: &C,
    table: &'static str,
    id: &str,
) -> AppResult<AppError> {
    let exists: bool = connection.connection().query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
        params![id],
        |row| row.get(0),
    )?;
    Ok(if exists {
        AppError::Conflict
    } else {
        AppError::NotFound
    })
}

pub(super) fn plan_by_id<C: SqlConnection>(connection: &C, id: &str) -> AppResult<Option<Plan>> {
    connection
        .connection()
        .query_row(
            &format!("{PLAN_SELECT} WHERE id = ?1"),
            params![id],
            plan_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

pub(super) fn milestone_by_id<C: SqlConnection>(
    connection: &C,
    id: &str,
) -> AppResult<Option<Milestone>> {
    connection
        .connection()
        .query_row(
            &format!("{MILESTONE_SELECT} WHERE id = ?1"),
            params![id],
            milestone_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

pub(super) fn task_by_id<C: SqlConnection>(connection: &C, id: &str) -> AppResult<Option<Task>> {
    connection
        .connection()
        .query_row(
            &format!("{TASK_SELECT} WHERE id = ?1"),
            params![id],
            task_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn plan_from_row(row: &Row<'_>) -> rusqlite::Result<Plan> {
    Ok(Plan {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: stored_value(row, 3, PlanStatus::parse)?,
        start_date: row.get(4)?,
        target_date: row.get(5)?,
        color: row
            .get::<_, Option<String>>(6)?
            .map(|value| PlanColor::parse(&value).ok_or_else(|| invalid_stored_value(6)))
            .transpose()?,
        archived: row.get::<_, i64>(7)? != 0,
        revision: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn milestone_from_row(row: &Row<'_>) -> rusqlite::Result<Milestone> {
    Ok(Milestone {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        target_date: row.get(4)?,
        status: stored_value(row, 5, MilestoneStatus::parse)?,
        workstream_id: row.get(6)?,
        sort_order: row.get(7)?,
        revision: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

pub(super) fn task_from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        plan_id: row.get(3)?,
        milestone_id: row.get(4)?,
        workstream_id: row.get(5)?,
        owner_id: row.get(6)?,
        due_date: row.get(7)?,
        scheduled_day: row.get(8)?,
        status: stored_value(row, 9, TaskStatus::parse)?,
        priority: stored_value(row, 10, TaskPriority::parse)?,
        completed_at: row.get(11)?,
        sort_order: row.get(12)?,
        revision: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn stored_value<T>(
    row: &Row<'_>,
    index: usize,
    parse: fn(&str) -> Option<T>,
) -> rusqlite::Result<T> {
    let value = row.get::<_, String>(index)?;
    parse(&value).ok_or_else(|| invalid_stored_value(index))
}

fn invalid_stored_value(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid stored planning value",
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CreateEventInput;
    use tempfile::tempdir;

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn plan_input(title: &str) -> CreatePlanInput {
        CreatePlanInput {
            title: title.into(),
            description: String::new(),
            status: PlanStatus::Active,
            start_date: None,
            target_date: None,
            color: None,
        }
    }

    fn plan(database: &mut PlannerDatabase, title: &str) -> Plan {
        database.create_plan(plan_input(title)).unwrap()
    }

    fn archive(database: &mut PlannerDatabase, plan: &Plan) -> Plan {
        database
            .update_plan(UpdatePlanInput {
                id: plan.id.clone(),
                revision: plan.revision,
                title: plan.title.clone(),
                description: plan.description.clone(),
                status: plan.status,
                start_date: plan.start_date.clone(),
                target_date: plan.target_date.clone(),
                color: plan.color,
                archived: true,
            })
            .unwrap()
    }

    fn milestone(
        database: &mut PlannerDatabase,
        plan_id: &str,
        title: &str,
        target_date: Option<&str>,
    ) -> Milestone {
        database
            .create_milestone(CreateMilestoneInput {
                plan_id: plan_id.into(),
                title: title.into(),
                description: String::new(),
                target_date: target_date.map(Into::into),
                status: MilestoneStatus::Pending,
                workstream_id: None,
            })
            .unwrap()
    }

    fn task(title: &str) -> CreateTaskInput {
        CreateTaskInput {
            title: title.into(),
            description: String::new(),
            plan_id: None,
            milestone_id: None,
            workstream_id: None,
            owner_id: None,
            due_date: None,
            scheduled_day: None,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
        }
    }

    fn edit(task: &Task) -> UpdateTaskInput {
        UpdateTaskInput {
            id: task.id.clone(),
            revision: task.revision,
            title: task.title.clone(),
            description: task.description.clone(),
            plan_id: task.plan_id.clone(),
            milestone_id: task.milestone_id.clone(),
            workstream_id: task.workstream_id.clone(),
            owner_id: task.owner_id.clone(),
            due_date: task.due_date.clone(),
            scheduled_day: task.scheduled_day.clone(),
            status: task.status,
            priority: task.priority,
        }
    }

    fn titles(tasks: Vec<Task>) -> Vec<String> {
        tasks.into_iter().map(|task| task.title).collect()
    }

    #[test]
    fn plans_allow_open_dates_but_not_inverted_ranges() {
        let mut database = database();
        let undated = plan(&mut database, "Normal work");
        assert_eq!((undated.start_date, undated.target_date), (None, None));
        let inverted = database.create_plan(CreatePlanInput {
            start_date: Some("2026-10-16".into()),
            target_date: Some("2026-10-01".into()),
            ..plan_input("TwitchCon 2027")
        });
        assert!(matches!(inverted, Err(AppError::Validation(_))));
        assert!(matches!(
            database.create_plan(plan_input("   ")),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn plan_edits_are_revision_checked() {
        let mut database = database();
        let original = plan(&mut database, "Wedding");
        let edit = |revision, title: &str| UpdatePlanInput {
            id: original.id.clone(),
            revision,
            title: title.into(),
            description: "Summer".into(),
            status: PlanStatus::Planning,
            start_date: Some("2027-06-01".into()),
            target_date: Some("2027-06-12".into()),
            color: Some(PlanColor::Plum),
            archived: false,
        };
        let updated = database.update_plan(edit(1, "Our wedding")).unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.color, Some(PlanColor::Plum));
        assert_eq!(updated.target_date.as_deref(), Some("2027-06-12"));
        assert!(matches!(
            database.update_plan(edit(1, "Stale")),
            Err(AppError::Conflict)
        ));
        let missing = UpdatePlanInput {
            id: Uuid::new_v4().to_string(),
            ..edit(1, "Ghost")
        };
        assert!(matches!(
            database.update_plan(missing),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn plan_list_reports_progress_and_the_next_pending_milestone() {
        let mut database = database();
        let launch = plan(&mut database, "Launch");
        let someday = plan(&mut database, "Someday");
        let launch_day = milestone(&mut database, &launch.id, "Launch day", Some("2026-10-16"));
        let beta = milestone(&mut database, &launch.id, "Beta", Some("2026-10-01"));
        milestone(&mut database, &launch.id, "Retro", None);
        database
            .update_milestone(UpdateMilestoneInput {
                id: beta.id.clone(),
                revision: beta.revision,
                title: beta.title.clone(),
                description: String::new(),
                target_date: beta.target_date.clone(),
                status: MilestoneStatus::Complete,
                workstream_id: None,
            })
            .unwrap();
        for (title, status, due_date) in [
            ("Draft notes", TaskStatus::Done, Some("2026-09-01")),
            ("Record demo", TaskStatus::InProgress, Some("2026-09-13")),
            ("Book venue", TaskStatus::Todo, Some("2026-09-14")),
        ] {
            database
                .create_task(CreateTaskInput {
                    plan_id: Some(launch.id.clone()),
                    status,
                    due_date: due_date.map(Into::into),
                    ..task(title)
                })
                .unwrap();
        }

        let summaries = database.list_plans("2026-09-14").unwrap();
        let summary = |id: &str| summaries.iter().find(|item| item.plan.id == id).unwrap();
        assert_eq!(
            (
                summary(&launch.id).milestone_count,
                summary(&launch.id).task_count,
                summary(&launch.id).completed_task_count,
                summary(&launch.id).overdue_task_count
            ),
            (3, 3, 1, 1)
        );
        assert_eq!(
            (
                summary(&someday.id).milestone_count,
                summary(&someday.id).overdue_task_count
            ),
            (0, 0)
        );
        assert_eq!(
            summary(&launch.id)
                .next_milestone
                .as_ref()
                .map(|next| next.id.as_str()),
            Some(launch_day.id.as_str())
        );
        assert_eq!(summary(&someday.id).task_count, 0);
        assert!(summary(&someday.id).next_milestone.is_none());

        let workspace = database.plan_workspace(&launch.id).unwrap();
        let milestone_titles = workspace
            .milestones
            .into_iter()
            .map(|milestone| milestone.title)
            .collect::<Vec<_>>();
        assert_eq!(milestone_titles, ["Beta", "Launch day", "Retro"]);
        assert_eq!(
            titles(workspace.tasks),
            ["Record demo", "Book venue", "Draft notes"]
        );
    }

    #[test]
    fn tasks_need_a_home_and_a_milestone_from_their_own_plan() {
        let mut database = database();
        let venue = plan(&mut database, "Venue");
        let marketing = plan(&mut database, "Marketing");
        let walkthrough = milestone(&mut database, &venue.id, "Walkthrough", None);
        assert!(matches!(
            database.create_task(task("Loose end")),
            Err(AppError::Validation(_))
        ));
        assert!(database
            .create_task(CreateTaskInput {
                due_date: Some("2026-10-13".into()),
                ..task("Confirm AV vendor")
            })
            .is_ok());
        let rejected = [
            CreateTaskInput {
                plan_id: Some(marketing.id.clone()),
                milestone_id: Some(walkthrough.id.clone()),
                ..task("Wrong plan")
            },
            CreateTaskInput {
                milestone_id: Some(walkthrough.id.clone()),
                scheduled_day: Some("2026-10-10".into()),
                ..task("Milestone without plan")
            },
            CreateTaskInput {
                plan_id: Some(Uuid::new_v4().to_string()),
                ..task("Unknown plan")
            },
            CreateTaskInput {
                scheduled_day: Some("10/10/2026".into()),
                ..task("Bad day")
            },
        ];
        for input in rejected {
            assert!(matches!(
                database.create_task(input),
                Err(AppError::Validation(_))
            ));
        }
        let created = database
            .create_task(CreateTaskInput {
                plan_id: Some(venue.id.clone()),
                milestone_id: Some(walkthrough.id.clone()),
                ..task("Measure stage")
            })
            .unwrap();
        assert_eq!(
            created.milestone_id.as_deref(),
            Some(walkthrough.id.as_str())
        );
        assert_eq!((created.scheduled_day, created.due_date), (None, None));
    }

    #[test]
    fn completion_time_follows_done_status() {
        let mut database = database();
        let created = database
            .create_task(CreateTaskInput {
                scheduled_day: Some("2026-10-01".into()),
                ..task("Pack bags")
            })
            .unwrap();
        assert_eq!(created.completed_at, None);
        let done = database
            .update_task(UpdateTaskInput {
                status: TaskStatus::Done,
                ..edit(&created)
            })
            .unwrap();
        let completed_at = done.completed_at.clone().expect("completion time");
        let renamed = database
            .update_task(UpdateTaskInput {
                title: "Pack carry-on".into(),
                ..edit(&done)
            })
            .unwrap();
        assert_eq!(renamed.completed_at, Some(completed_at));
        let reopened = database
            .update_task(UpdateTaskInput {
                status: TaskStatus::Blocked,
                ..edit(&renamed)
            })
            .unwrap();
        assert_eq!((reopened.completed_at, reopened.revision), (None, 4));
    }

    #[test]
    fn task_edits_and_deletes_are_revision_checked() {
        let mut database = database();
        let created = database
            .create_task(CreateTaskInput {
                due_date: Some("2026-09-30".into()),
                ..task("Book flights")
            })
            .unwrap();
        let updated = database
            .update_task(UpdateTaskInput {
                priority: TaskPriority::High,
                ..edit(&created)
            })
            .unwrap();
        assert_eq!(
            (updated.priority, updated.revision),
            (TaskPriority::High, 2)
        );
        assert!(matches!(
            database.update_task(edit(&created)),
            Err(AppError::Conflict)
        ));
        assert!(matches!(
            database.delete_task(&created.id, created.revision),
            Err(AppError::Conflict)
        ));
        database.delete_task(&updated.id, updated.revision).unwrap();
        assert!(matches!(
            database.delete_task(&updated.id, updated.revision),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn a_day_lists_scheduled_tasks_and_separately_what_is_due() {
        let mut database = database();
        let day = "2026-10-13";
        for input in [
            CreateTaskInput {
                scheduled_day: Some(day.into()),
                due_date: Some(day.into()),
                ..task("Stage layout")
            },
            CreateTaskInput {
                due_date: Some(day.into()),
                ..task("Confirm AV vendor")
            },
            CreateTaskInput {
                scheduled_day: Some("2026-10-12".into()),
                due_date: Some(day.into()),
                ..task("Draft run of show")
            },
            CreateTaskInput {
                scheduled_day: Some("2026-10-14".into()),
                ..task("Pick up badges")
            },
        ] {
            database.create_task(input).unwrap();
        }
        assert_eq!(
            titles(database.tasks_for_day(day).unwrap()),
            ["Stage layout"]
        );
        assert_eq!(
            titles(database.tasks_due_on(day).unwrap()),
            ["Confirm AV vendor", "Draft run of show"]
        );
    }

    #[test]
    fn deleting_a_milestone_keeps_its_tasks_in_the_plan() {
        let mut database = database();
        let launch = plan(&mut database, "Launch");
        let beta = milestone(&mut database, &launch.id, "Beta", Some("2026-10-01"));
        let created = database
            .create_task(CreateTaskInput {
                plan_id: Some(launch.id.clone()),
                milestone_id: Some(beta.id.clone()),
                ..task("Invite testers")
            })
            .unwrap();
        assert!(matches!(
            database.delete_milestone(&beta.id, beta.revision + 1),
            Err(AppError::Conflict)
        ));
        database.delete_milestone(&beta.id, beta.revision).unwrap();
        let workspace = database.plan_workspace(&launch.id).unwrap();
        assert!(workspace.milestones.is_empty());
        assert_eq!(workspace.tasks.len(), 1);
        assert_eq!(workspace.tasks[0].milestone_id, None);
        assert_eq!(workspace.tasks[0].revision, created.revision + 1);
    }

    #[test]
    fn plans_are_archived_before_they_can_be_deleted() {
        let mut database = database();
        let trip = plan(&mut database, "Vacation");
        assert!(matches!(
            database.delete_plan(&trip.id, trip.revision),
            Err(AppError::Validation(_))
        ));
        let archived = archive(&mut database, &trip);
        assert!(archived.archived);
        assert!(matches!(
            database.delete_plan(&trip.id, trip.revision),
            Err(AppError::Conflict)
        ));
        database
            .delete_plan(&archived.id, archived.revision)
            .unwrap();
        assert!(database.list_plans("2026-09-14").unwrap().is_empty());
    }

    #[test]
    fn deleting_a_plan_keeps_dated_work_and_events_without_the_plan() {
        let mut database = database();
        let showcase = plan(&mut database, "Creator Showcase");
        let rehearsal = milestone(&mut database, &showcase.id, "Rehearsal", Some("2026-10-14"));
        database
            .create_task(CreateTaskInput {
                plan_id: Some(showcase.id.clone()),
                milestone_id: Some(rehearsal.id.clone()),
                ..task("Brainstorm segments")
            })
            .unwrap();
        let dated = database
            .create_task(CreateTaskInput {
                plan_id: Some(showcase.id.clone()),
                milestone_id: Some(rehearsal.id.clone()),
                due_date: Some("2026-10-13".into()),
                ..task("Complete AV setup")
            })
            .unwrap();
        let unrelated = database
            .create_task(CreateTaskInput {
                scheduled_day: Some("2026-10-13".into()),
                ..task("Laundry")
            })
            .unwrap();
        let event = database
            .create_event(CreateEventInput {
                title: "Creator briefing".into(),
                notes: String::new(),
                start_at_utc: "2026-10-16T14:00:00Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 30,
                reminder_minutes_before: None,
                plan_id: Some(showcase.id.clone()),
                location: String::new(),
                workstream_id: None,
                owner_id: None,
            })
            .unwrap();
        let archived = archive(&mut database, &showcase);

        let deletion = database
            .delete_plan(&archived.id, archived.revision)
            .unwrap();
        assert_eq!(
            deletion,
            PlanDeletion {
                deleted_workstreams: 0,
                deleted_milestones: 1,
                deleted_tasks: 1,
                detached_tasks: 1,
                detached_events: 1,
            }
        );
        assert!(matches!(
            database.plan_workspace(&showcase.id),
            Err(AppError::NotFound)
        ));
        let kept = database.tasks_due_on("2026-10-13").unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].plan_id, None);
        assert_eq!(kept[0].milestone_id, None);
        assert_eq!(kept[0].revision, dated.revision + 1);
        assert_eq!(
            database.tasks_for_day("2026-10-13").unwrap()[0].revision,
            unrelated.revision
        );
        let kept_event = database
            .events_for_day("2026-10-16", "America/New_York")
            .unwrap()
            .remove(0);
        assert_eq!(kept_event.plan_id, None);
        assert_eq!(kept_event.revision, event.revision + 1);
    }
}
