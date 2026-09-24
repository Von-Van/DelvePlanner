//! Weekly and daily planning: the work a planning session chooses from, and moves that put one
//! task onto a day, into a week's pool, or back into its plan without copying it.

use super::planning::{
    milestone_from_row, replace_task, task_by_id, task_from_row, MILESTONE_ORDER, MILESTONE_SELECT,
    TASK_SELECT,
};
use super::{
    collect, normalize_day, now, offset_day, validate_id, validate_revision, PlannerDatabase,
};
use crate::error::{AppError, AppResult};
use crate::model::{DayChange, PlanningBoard, Task, TaskMove, UpdateTaskInput, MAX_TASK_MOVES};
use rusqlite::{params, Transaction, TransactionBehavior};
use std::collections::HashSet;

impl PlannerDatabase {
    /// Every open task, finished tasks chosen for or scheduled in the week starting at
    /// `start_day`, and the pending dated milestones of plans that are not archived.
    pub fn planning_board(&self, start_day: &str) -> AppResult<PlanningBoard> {
        let start = normalize_day(start_day)?;
        let end = offset_day(&start, 7)?;
        let mut statement = self.connection.prepare(&format!(
            "{TASK_SELECT}
             WHERE status <> 'done'
                OR (planned_week >= ?1 AND planned_week < ?2)
                OR (scheduled_day >= ?1 AND scheduled_day < ?2)
             ORDER BY sort_order ASC, created_at ASC"
        ))?;
        let tasks = collect(statement.query_map(params![start, end], task_from_row)?)?;
        let mut statement = self.connection.prepare(&format!(
            "{MILESTONE_SELECT}
             WHERE status = 'pending' AND target_date IS NOT NULL
               AND plan_id IN (SELECT id FROM plans WHERE archived = 0)
             {MILESTONE_ORDER}"
        ))?;
        let milestones = collect(statement.query_map([], milestone_from_row)?)?;
        Ok(PlanningBoard { tasks, milestones })
    }

    /// Applies every move in one transaction. A stale revision, a missing task, or a move that
    /// would leave a task with no plan, week, day, or due date rejects the whole batch.
    pub fn move_tasks(&mut self, moves: Vec<TaskMove>) -> AppResult<Vec<Task>> {
        if moves.is_empty() || moves.len() > MAX_TASK_MOVES {
            return Err(AppError::Validation(
                "Move between 1 and 500 tasks at a time.".into(),
            ));
        }
        let mut seen = HashSet::new();
        for task_move in &moves {
            validate_id(&task_move.id)?;
            validate_revision(task_move.revision)?;
            if !seen.insert(task_move.id.as_str()) {
                return Err(AppError::Validation(
                    "A task can only move once in the same change.".into(),
                ));
            }
            for change in [&task_move.scheduled_day, &task_move.planned_week] {
                if let DayChange::Set { day } = change {
                    normalize_day(day)?;
                }
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut moved = Vec::with_capacity(moves.len());
        for task_move in moves {
            let current = task_by_id(&transaction, &task_move.id)?.ok_or(AppError::NotFound)?;
            if current.revision != task_move.revision {
                return Err(AppError::Conflict);
            }
            let input = UpdateTaskInput {
                scheduled_day: task_move.scheduled_day.apply(current.scheduled_day.clone()),
                planned_week: task_move.planned_week.apply(current.planned_week.clone()),
                status: task_move.status.unwrap_or(current.status),
                ..UpdateTaskInput::keeping(&current)
            };
            // A task that leaves a day it was already on is what "carried forward" means, and it
            // is the only thing Delve Planner keeps a history of. It starts empty in v0.3.5, and
            // What Delve Planner Knows can clear it or switch the observation off.
            let from_day = current.scheduled_day.clone();
            let to_day = input.scheduled_day.clone();
            if from_day.is_some() && from_day != to_day {
                record_move(
                    &transaction,
                    &input.id,
                    from_day.as_deref(),
                    to_day.as_deref(),
                )?;
            }
            moved.push(replace_task(&transaction, &input)?);
        }
        transaction.commit()?;
        Ok(moved)
    }
}

/// Notes that a task moved off a day it was on. Only the days and the moment are kept: no title,
/// no notes, nothing that says what the work was.
fn record_move(
    transaction: &Transaction<'_>,
    task_id: &str,
    from_day: Option<&str>,
    to_day: Option<&str>,
) -> AppResult<()> {
    transaction.execute(
        "INSERT INTO task_moves (task_id, from_day, to_day, kind, moved_at)
         VALUES (?1, ?2, ?3, 'carried_forward', ?4)",
        params![task_id, from_day, to_day, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CreateMilestoneInput, CreatePlanInput, CreateTaskInput, MilestoneStatus, Plan, PlanStatus,
        TaskPriority, TaskStatus, UpdatePlanInput,
    };
    use tempfile::tempdir;

    const WEEK: &str = "2026-09-13";

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn plan(database: &mut PlannerDatabase, title: &str) -> Plan {
        database
            .create_plan(CreatePlanInput {
                title: title.into(),
                description: String::new(),
                status: PlanStatus::Active,
                start_date: None,
                target_date: None,
                color: None,
                links: Vec::new(),
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
            planned_week: None,
            estimated_minutes: None,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
            checklist: Vec::new(),
            recurrence: None,
            waiting_on: Vec::new(),
        }
    }

    fn day(value: &str) -> DayChange {
        DayChange::Set { day: value.into() }
    }

    fn titles(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|task| task.title.as_str()).collect()
    }

    #[test]
    fn a_task_moves_between_plan_week_and_day_as_one_record() {
        let mut database = database();
        let move_plan = plan(&mut database, "Move apartments");
        let created = database
            .create_task(CreateTaskInput {
                plan_id: Some(move_plan.id.clone()),
                estimated_minutes: Some(45),
                ..task("Book movers")
            })
            .unwrap();
        let [chosen] = database
            .move_tasks(vec![TaskMove {
                id: created.id.clone(),
                revision: created.revision,
                scheduled_day: DayChange::Unchanged,
                planned_week: day(WEEK),
                status: None,
            }])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            (
                chosen.planned_week.as_deref(),
                chosen.scheduled_day.as_deref()
            ),
            (Some(WEEK), None)
        );
        let [scheduled] = database
            .move_tasks(vec![TaskMove {
                id: chosen.id.clone(),
                revision: chosen.revision,
                scheduled_day: day("2026-09-16"),
                planned_week: DayChange::Unchanged,
                status: None,
            }])
            .unwrap()
            .try_into()
            .unwrap();
        let [unscheduled] = database
            .move_tasks(vec![TaskMove {
                id: scheduled.id.clone(),
                revision: scheduled.revision,
                scheduled_day: DayChange::Clear,
                planned_week: DayChange::Clear,
                status: None,
            }])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            (
                unscheduled.plan_id.as_deref(),
                unscheduled.planned_week.as_deref(),
                unscheduled.scheduled_day.as_deref(),
                unscheduled.estimated_minutes,
                unscheduled.revision
            ),
            (Some(move_plan.id.as_str()), None, None, Some(45), 4)
        );
        assert_eq!(database.all_tasks().unwrap().len(), 1);
    }

    #[test]
    fn a_batch_of_moves_is_all_or_nothing() {
        let mut database = database();
        let loose = database
            .create_task(CreateTaskInput {
                scheduled_day: Some("2026-09-14".into()),
                ..task("Call dentist")
            })
            .unwrap();
        let other = database
            .create_task(CreateTaskInput {
                scheduled_day: Some("2026-09-14".into()),
                ..task("Order invitations")
            })
            .unwrap();
        let to_today = |task: &Task, revision| TaskMove {
            id: task.id.clone(),
            revision,
            scheduled_day: day("2026-09-15"),
            planned_week: DayChange::Unchanged,
            status: None,
        };
        let stale = database.move_tasks(vec![
            to_today(&loose, loose.revision),
            to_today(&other, other.revision + 1),
        ]);
        assert!(matches!(stale, Err(AppError::Conflict)));
        let homeless = database.move_tasks(vec![TaskMove {
            scheduled_day: DayChange::Clear,
            ..to_today(&loose, loose.revision)
        }]);
        assert!(matches!(homeless, Err(AppError::Validation(_))));
        let repeated = database.move_tasks(vec![
            to_today(&loose, loose.revision),
            to_today(&loose, loose.revision),
        ]);
        assert!(matches!(repeated, Err(AppError::Validation(_))));
        assert!(database.tasks_for_day("2026-09-15").unwrap().is_empty());

        let done = database
            .move_tasks(vec![TaskMove {
                status: Some(TaskStatus::Done),
                ..to_today(&loose, loose.revision)
            }])
            .unwrap();
        assert_eq!(done[0].status, TaskStatus::Done);
        assert!(done[0].completed_at.is_some());
    }

    #[test]
    fn the_board_holds_open_work_the_weeks_finished_work_and_live_milestones() {
        let mut database = database();
        let wedding = plan(&mut database, "Wedding");
        let archived = plan(&mut database, "Old trip");
        database
            .update_plan(UpdatePlanInput {
                id: archived.id.clone(),
                revision: archived.revision,
                title: archived.title.clone(),
                description: String::new(),
                status: archived.status,
                start_date: None,
                target_date: None,
                color: None,
                archived: true,
                links: Vec::new(),
            })
            .unwrap();
        for (plan_id, title, target_date) in [
            (&wedding.id, "Venue booked", Some("2026-09-20")),
            (&wedding.id, "Guest list", None),
            (&archived.id, "Flights", Some("2026-09-18")),
        ] {
            database
                .create_milestone(CreateMilestoneInput {
                    plan_id: plan_id.clone(),
                    title: title.into(),
                    description: String::new(),
                    target_date: target_date.map(Into::into),
                    status: MilestoneStatus::Pending,
                    workstream_id: None,
                })
                .unwrap();
        }
        for input in [
            CreateTaskInput {
                plan_id: Some(wedding.id.clone()),
                ..task("Taste cakes")
            },
            CreateTaskInput {
                planned_week: Some(WEEK.into()),
                status: TaskStatus::Done,
                ..task("Send save-the-dates")
            },
            CreateTaskInput {
                scheduled_day: Some("2026-09-01".into()),
                status: TaskStatus::Done,
                ..task("Finished last month")
            },
            CreateTaskInput {
                scheduled_day: Some("2026-09-02".into()),
                ..task("Unfinished from before")
            },
        ] {
            database.create_task(input).unwrap();
        }
        let board = database.planning_board(WEEK).unwrap();
        assert_eq!(
            titles(&board.tasks),
            [
                "Taste cakes",
                "Send save-the-dates",
                "Unfinished from before"
            ]
        );
        assert_eq!(
            board
                .milestones
                .iter()
                .map(|milestone| milestone.title.as_str())
                .collect::<Vec<_>>(),
            ["Venue booked"]
        );
    }
}
