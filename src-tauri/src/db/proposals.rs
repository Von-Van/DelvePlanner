//! The planner's side of the database: choosing the records a request may refer to, and applying
//! an approved proposal in one transaction.

use super::planning::{
    insert_milestone, insert_plan, insert_task, milestone_by_id, plan_by_id, remove_milestone,
    remove_task, replace_milestone, replace_plan, replace_task, task_by_id, validate_description,
    validate_milestone_shape, validate_plan_shape,
};
use super::team::validate_links;
use super::{
    apply_reminder_change, event_by_id, insert_event, normalize_day, now, parse_day,
    parse_time_zone, reminder_outbox_values, reminder_status_string, validate_duration,
    validate_id, validate_notes, validate_reminder, validate_reminder_change,
    validate_reminder_delivery_time, validate_revision, validate_time, validate_title,
    PlannerDatabase, PreparedEvent,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    AppliedProposal, CreateEventInput, CreateMilestoneInput, CreatePlanInput, CreateTaskInput,
    DayChange, EstimateChange, Milestone, MilestoneStatus, ModelResponse, MutationOperation, Plan,
    PlannerCandidates, ProposedOperation, RecordRef, ReminderChange, ScheduleEvent, Task,
    UpdateMilestoneInput, UpdatePlanInput, UpdateTaskInput, MAX_OPERATIONS,
};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Transaction, TransactionBehavior};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

const EVENT_CANDIDATE_LIMIT: usize = 40;
const PLAN_CANDIDATE_LIMIT: usize = 12;
const MILESTONE_CANDIDATE_LIMIT: usize = 20;
const TASK_CANDIDATE_LIMIT: usize = 30;

const REFERENCED_SCORE: usize = 100_000;
const ACTIVE_PLAN_SCORE: usize = 50_000;
const TITLE_MATCH_SCORE: usize = 10_000;
const FOCUS_PLAN_SCORE: usize = 5_000;
const NEARBY_DAY_SCORE: usize = 3_000;

/// What the planner knows about one request when it gathers candidate records.
pub struct CandidateRequest<'a> {
    pub command: &'a str,
    pub selected_day: &'a str,
    pub time_zone: &'a str,
    /// Records referenced earlier in the planner session, so follow-ups can reach them.
    pub referenced_ids: &'a [String],
    /// The plan the user is looking at, if any.
    pub active_plan_id: Option<&'a str>,
}

impl PlannerDatabase {
    /// Events ranked by title match with the command, session references, and closeness to the
    /// selected day.
    pub fn candidate_events(
        &self,
        command: &str,
        selected_day: &str,
        viewer_time_zone: &str,
        referenced_ids: &[String],
        limit: usize,
    ) -> AppResult<Vec<ScheduleEvent>> {
        let events = self.all_events()?;
        let zone = parse_time_zone(viewer_time_zone)?;
        let selected = parse_day(selected_day)?;
        let selected_noon = zone
            .from_local_datetime(
                &selected.and_hms_opt(12, 0, 0).ok_or_else(|| {
                    AppError::Validation("The selected day is out of range.".into())
                })?,
            )
            .earliest()
            .ok_or_else(|| {
                AppError::Validation("The selected day is invalid in this time zone.".into())
            })?
            .with_timezone(&Utc);
        let tokens = search_tokens(command);
        let referenced: HashSet<&str> = referenced_ids.iter().map(String::as_str).collect();
        let mut scored = events
            .into_iter()
            .map(|event| {
                let start = DateTime::parse_from_rfc3339(&event.start_at_utc)
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or(selected_noon);
                let distance_hours = (start - selected_noon).num_hours().unsigned_abs() as usize;
                let title = event.title.to_ascii_lowercase();
                let token_matches = tokens
                    .iter()
                    .filter(|token| title.contains(token.as_str()))
                    .count();
                let score = if referenced.contains(event.id.as_str()) {
                    REFERENCED_SCORE
                } else {
                    0
                } + token_matches * TITLE_MATCH_SCORE
                    + 5_000usize.saturating_sub(distance_hours.min(5_000));
                (score, distance_hours, event)
            })
            .collect::<Vec<_>>();
        scored.sort_by_key(|(score, distance, _)| (Reverse(*score), *distance));
        Ok(scored
            .into_iter()
            .take(limit.min(60))
            .map(|(_, _, event)| event)
            .collect())
    }

    /// The events, plans, milestones, and tasks a request may refer to. Plans are limited to
    /// open ones; milestones and tasks are included only when they relate to the request, the
    /// active plan, the session, or the selected day.
    pub fn planner_candidates(
        &self,
        request: &CandidateRequest<'_>,
    ) -> AppResult<PlannerCandidates> {
        let selected_day = normalize_day(request.selected_day)?;
        if let Some(id) = request.active_plan_id {
            validate_id(id)?;
        }
        let events = self.candidate_events(
            request.command,
            &selected_day,
            request.time_zone,
            request.referenced_ids,
            EVENT_CANDIDATE_LIMIT,
        )?;
        let tokens = planning_tokens(request.command);
        let referenced: HashSet<&str> = request.referenced_ids.iter().map(String::as_str).collect();
        let is_referenced = |id: &str| referenced.contains(id);

        let open_plans = self
            .all_plans()?
            .into_iter()
            .filter(|plan| !plan.archived)
            .collect::<Vec<_>>();
        let open_plan_ids = open_plans
            .iter()
            .map(|plan| plan.id.clone())
            .collect::<HashSet<_>>();
        let mut plans = ranked(open_plans, |plan: &Plan| {
            let mut score = title_matches(&plan.title, &tokens) * TITLE_MATCH_SCORE;
            if is_referenced(&plan.id) {
                score += REFERENCED_SCORE;
            }
            if request.active_plan_id == Some(plan.id.as_str()) {
                score += ACTIVE_PLAN_SCORE;
            }
            score
        });
        let focus_plans = plans
            .iter()
            .filter(|(score, _)| *score >= TITLE_MATCH_SCORE)
            .map(|(_, plan)| plan.id.clone())
            .collect::<HashSet<_>>();
        plans.truncate(PLAN_CANDIDATE_LIMIT);

        let related_milestones = self
            .all_milestones()?
            .into_iter()
            .filter(|milestone| open_plan_ids.contains(&milestone.plan_id))
            .collect::<Vec<_>>();
        let mut milestones = ranked(related_milestones, |milestone: &Milestone| {
            let mut score = title_matches(&milestone.title, &tokens) * TITLE_MATCH_SCORE;
            if is_referenced(&milestone.id) {
                score += REFERENCED_SCORE;
            }
            if focus_plans.contains(&milestone.plan_id) {
                score += FOCUS_PLAN_SCORE;
            }
            score
        });
        milestones.retain(|(score, _)| *score >= FOCUS_PLAN_SCORE);
        milestones.truncate(MILESTONE_CANDIDATE_LIMIT);

        let window_end = super::offset_day(&selected_day, 2)?;
        let near_selected_day = |day: &Option<String>| {
            day.as_deref()
                .is_some_and(|day| day >= selected_day.as_str() && day < window_end.as_str())
        };
        let related_tasks = self
            .all_tasks()?
            .into_iter()
            .filter(|task| {
                task.plan_id
                    .as_ref()
                    .is_none_or(|plan_id| open_plan_ids.contains(plan_id))
            })
            .collect::<Vec<_>>();
        let mut tasks = ranked(related_tasks, |task: &Task| {
            let mut score = title_matches(&task.title, &tokens) * TITLE_MATCH_SCORE;
            if is_referenced(&task.id) {
                score += REFERENCED_SCORE;
            }
            if task
                .plan_id
                .as_ref()
                .is_some_and(|plan_id| focus_plans.contains(plan_id))
            {
                score += FOCUS_PLAN_SCORE;
            }
            if near_selected_day(&task.scheduled_day) || near_selected_day(&task.due_date) {
                score += NEARBY_DAY_SCORE;
            }
            score
        });
        tasks.retain(|(score, _)| *score >= NEARBY_DAY_SCORE);
        tasks.truncate(TASK_CANDIDATE_LIMIT);

        Ok(PlannerCandidates {
            events,
            plans: plans.into_iter().map(|(_, plan)| plan).collect(),
            milestones: milestones
                .into_iter()
                .map(|(_, milestone)| milestone)
                .collect(),
            tasks: tasks.into_iter().map(|(_, task)| task).collect(),
        })
    }

    /// Applies every operation of an approved proposal or none of them. Plans are created first
    /// and milestones second so later operations can refer to them by title; milestone deletions
    /// run last so detaching their tasks cannot invalidate an earlier edit in the same proposal.
    pub fn apply_proposal(&mut self, proposal: &ModelResponse) -> AppResult<AppliedProposal> {
        let ModelResponse::Proposal { operations, .. } = proposal else {
            return Err(AppError::Validation(
                "Clarifications cannot be applied as changes.".into(),
            ));
        };
        validate_operations(operations)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut writer = ProposalWriter {
            transaction: &transaction,
            applied: AppliedProposal::default(),
            new_plans: HashMap::new(),
            new_milestones: HashMap::new(),
            revisions: HashMap::new(),
        };
        for operation in operations {
            if let MutationOperation::CreatePlan { .. } = operation {
                writer.apply(operation)?;
            }
        }
        for operation in operations {
            if let MutationOperation::CreateMilestone { .. } = operation {
                writer.apply(operation)?;
            }
        }
        for operation in operations {
            if !matches!(
                operation,
                MutationOperation::CreatePlan { .. }
                    | MutationOperation::CreateMilestone { .. }
                    | MutationOperation::DeleteMilestone { .. }
            ) {
                writer.apply(operation)?;
            }
        }
        for operation in operations {
            if let MutationOperation::DeleteMilestone { .. } = operation {
                writer.apply(operation)?;
            }
        }
        let applied = writer.applied;
        transaction.commit()?;
        Ok(applied)
    }
}

/// The bounds the tasks table itself enforces, checked before anything is written.
fn validate_estimate(minutes: i64) -> AppResult<()> {
    if !(1..=1440).contains(&minutes) {
        return Err(AppError::Validation(
            "An estimate must be between 1 minute and 24 hours.".into(),
        ));
    }
    Ok(())
}

/// Narrows a proposal to the suggestions the user accepted, keeping their original order. The
/// handles are the ones the review showed. An accepted suggestion always brings what it depends
/// on: a task can't join a plan this proposal creates unless that plan is created too.
pub fn accepted_operations(
    proposal: &ModelResponse,
    accepted: Option<&[String]>,
) -> AppResult<ModelResponse> {
    let ModelResponse::Proposal {
        summary,
        operations,
    } = proposal
    else {
        return Err(AppError::Validation(
            "Clarifications cannot be applied as changes.".into(),
        ));
    };
    let Some(accepted) = accepted else {
        return Ok(proposal.clone());
    };
    // The handles come from the same positions the review showed, so no notes are needed here.
    let review = ProposedOperation::review(operations, &[]);
    let chosen: HashSet<&str> = accepted.iter().map(String::as_str).collect();
    if let Some(unknown) = chosen
        .iter()
        .find(|handle| !review.iter().any(|operation| operation.id == **handle))
    {
        return Err(AppError::Validation(format!(
            "“{unknown}” is not one of this proposal's changes."
        )));
    }
    if chosen.is_empty() {
        return Err(AppError::Validation(
            "Choose at least one change to apply.".into(),
        ));
    }
    let kept: Vec<MutationOperation> = review
        .iter()
        .filter(|operation| chosen.contains(operation.id.as_str()))
        .map(|operation| {
            if operation
                .depends_on
                .iter()
                .all(|needed| chosen.contains(needed.as_str()))
            {
                Ok(operation.change.clone())
            } else {
                Err(AppError::Validation(
                    "That change needs the plan or milestone it belongs to, which wasn't accepted."
                        .into(),
                ))
            }
        })
        .collect::<AppResult<_>>()?;
    Ok(ModelResponse::proposal(summary.clone(), kept))
}

/// Sorts records by descending score, keeping the database order for ties.
fn ranked<T>(records: Vec<T>, score: impl Fn(&T) -> usize) -> Vec<(usize, T)> {
    let mut scored = records
        .into_iter()
        .map(|record| (score(&record), record))
        .collect::<Vec<_>>();
    scored.sort_by_key(|(score, _)| Reverse(*score));
    scored
}

struct ProposalWriter<'a, 'connection> {
    transaction: &'a Transaction<'connection>,
    applied: AppliedProposal,
    /// Plans created by this proposal, keyed by folded title.
    new_plans: HashMap<String, String>,
    /// Milestones created by this proposal, keyed by plan ID and folded title.
    new_milestones: HashMap<(String, String), String>,
    /// For records this proposal already changed: the revision it was proposed against and the
    /// revision it has now, so a second operation on the same record is not seen as stale.
    revisions: HashMap<String, (i64, i64)>,
}

impl ProposalWriter<'_, '_> {
    fn apply(&mut self, operation: &MutationOperation) -> AppResult<()> {
        let connection = self.transaction;
        match operation {
            MutationOperation::CreateEvent {
                title,
                notes,
                start_at_utc,
                time_zone,
                duration_minutes,
                reminder_minutes_before,
                plan,
            } => {
                let plan_id = plan
                    .as_ref()
                    .map(|reference| self.plan_id(reference))
                    .transpose()?;
                let input = CreateEventInput {
                    title: title.clone(),
                    notes: notes.clone(),
                    start_at_utc: start_at_utc.clone(),
                    time_zone: time_zone.clone(),
                    duration_minutes: *duration_minutes,
                    reminder_minutes_before: *reminder_minutes_before,
                    plan_id,
                    location: String::new(),
                    workstream_id: None,
                    owner_id: None,
                };
                let event = insert_event(connection, PreparedEvent::from_input(input)?)?;
                self.applied.event_ids.push(event.id);
            }
            MutationOperation::UpdateEvent {
                event_id,
                expected_revision,
                title,
                notes,
                duration_minutes,
                reminder_change,
            } => {
                let current = self.event(event_id, *expected_revision)?;
                self.write_event(
                    &current,
                    *expected_revision,
                    EventEdit {
                        title: title.as_deref(),
                        notes: notes.as_deref(),
                        start: None,
                        duration_minutes: *duration_minutes,
                        reminder_change,
                    },
                )?;
            }
            MutationOperation::RescheduleEvent {
                event_id,
                expected_revision,
                title,
                notes,
                start_at_utc,
                time_zone,
                duration_minutes,
                reminder_change,
            } => {
                let current = self.event(event_id, *expected_revision)?;
                self.write_event(
                    &current,
                    *expected_revision,
                    EventEdit {
                        title: title.as_deref(),
                        notes: notes.as_deref(),
                        start: Some((start_at_utc, time_zone)),
                        duration_minutes: *duration_minutes,
                        reminder_change,
                    },
                )?;
            }
            MutationOperation::DeleteEvent {
                event_id,
                expected_revision,
            } => {
                let current = self.event(event_id, *expected_revision)?;
                connection.execute(
                    "DELETE FROM schedule_events WHERE id = ?1 AND revision = ?2",
                    params![current.id, current.revision],
                )?;
                self.applied.event_ids.push(current.id);
            }
            MutationOperation::SetEventPlan {
                event_id,
                expected_revision,
                plan,
            } => {
                let current = self.event(event_id, *expected_revision)?;
                let plan_id = plan
                    .as_ref()
                    .map(|reference| self.plan_id(reference))
                    .transpose()?;
                let workstream_id = if plan_id == current.plan_id {
                    current.workstream_id.clone()
                } else {
                    None
                };
                validate_links(
                    connection,
                    plan_id.as_deref(),
                    workstream_id.as_deref(),
                    current.owner_id.as_deref(),
                )?;
                let changed = connection.execute(
                    "UPDATE schedule_events
                     SET plan_id = ?1, workstream_id = ?2, revision = revision + 1, updated_at = ?3
                     WHERE id = ?4 AND revision = ?5",
                    params![plan_id, workstream_id, now(), current.id, current.revision],
                )?;
                if changed != 1 {
                    return Err(AppError::Conflict);
                }
                self.record(event_id, *expected_revision, current.revision + 1);
                self.applied.event_ids.push(current.id);
            }
            MutationOperation::CreatePlan {
                title,
                description,
                status,
                start_date,
                target_date,
            } => {
                let plan = insert_plan(
                    connection,
                    &CreatePlanInput {
                        title: title.clone(),
                        description: description.clone(),
                        status: *status,
                        start_date: start_date.clone(),
                        target_date: target_date.clone(),
                        color: None,
                    },
                )?;
                if self
                    .new_plans
                    .insert(fold(title), plan.id.clone())
                    .is_some()
                {
                    return Err(AppError::Validation(
                        "A proposal cannot create two plans with the same title.".into(),
                    ));
                }
                self.applied.plan_ids.push(plan.id);
            }
            MutationOperation::UpdatePlan {
                plan_id,
                expected_revision,
                title,
                description,
                status,
                start_date,
                target_date,
            } => {
                let revision = self.revision(plan_id, *expected_revision)?;
                let plan = plan_by_id(connection, plan_id)?.ok_or(AppError::NotFound)?;
                if plan.revision != revision {
                    return Err(AppError::Conflict);
                }
                let updated = replace_plan(
                    connection,
                    &UpdatePlanInput {
                        id: plan.id,
                        revision,
                        title: title.clone().unwrap_or(plan.title),
                        description: description.clone().unwrap_or(plan.description),
                        status: status.unwrap_or(plan.status),
                        start_date: start_date.clone().or(plan.start_date),
                        target_date: target_date.clone().or(plan.target_date),
                        color: plan.color,
                        archived: plan.archived,
                    },
                )?;
                self.record(plan_id, *expected_revision, updated.revision);
                self.applied.plan_ids.push(updated.id);
            }
            MutationOperation::CreateMilestone {
                plan,
                title,
                description,
                target_date,
            } => {
                let plan_id = self.plan_id(plan)?;
                let milestone = insert_milestone(
                    connection,
                    &CreateMilestoneInput {
                        plan_id: plan_id.clone(),
                        title: title.clone(),
                        description: description.clone(),
                        target_date: target_date.clone(),
                        status: MilestoneStatus::Pending,
                        workstream_id: None,
                    },
                )?;
                if self
                    .new_milestones
                    .insert((plan_id, fold(title)), milestone.id.clone())
                    .is_some()
                {
                    return Err(AppError::Validation(
                        "A proposal cannot create two milestones with the same title in one plan."
                            .into(),
                    ));
                }
                self.applied.milestone_ids.push(milestone.id);
            }
            MutationOperation::UpdateMilestone {
                milestone_id,
                expected_revision,
                title,
                description,
                target_date,
                status,
            } => {
                let revision = self.revision(milestone_id, *expected_revision)?;
                let milestone =
                    milestone_by_id(connection, milestone_id)?.ok_or(AppError::NotFound)?;
                let updated = replace_milestone(
                    connection,
                    &UpdateMilestoneInput {
                        id: milestone.id,
                        revision,
                        title: title.clone().unwrap_or(milestone.title),
                        description: description.clone().unwrap_or(milestone.description),
                        target_date: target_date.clone().or(milestone.target_date),
                        status: status.unwrap_or(milestone.status),
                        workstream_id: milestone.workstream_id,
                    },
                )?;
                self.record(milestone_id, *expected_revision, updated.revision);
                self.applied.milestone_ids.push(updated.id);
            }
            MutationOperation::DeleteMilestone {
                milestone_id,
                expected_revision,
            } => {
                let revision = self.revision(milestone_id, *expected_revision)?;
                remove_milestone(connection, milestone_id, revision)?;
                self.applied.milestone_ids.push(milestone_id.clone());
            }
            MutationOperation::CreateTask {
                title,
                description,
                plan,
                milestone,
                scheduled_day,
                due_date,
                status,
                priority,
            } => {
                let plan_id = plan
                    .as_ref()
                    .map(|reference| self.plan_id(reference))
                    .transpose()?;
                let milestone_id = milestone
                    .as_ref()
                    .map(|reference| self.milestone_id(reference, plan_id.as_deref()))
                    .transpose()?;
                let task = insert_task(
                    connection,
                    &CreateTaskInput {
                        title: title.clone(),
                        description: description.clone(),
                        plan_id,
                        milestone_id,
                        workstream_id: None,
                        owner_id: None,
                        due_date: due_date.clone(),
                        scheduled_day: scheduled_day.clone(),
                        planned_week: None,
                        estimated_minutes: None,
                        status: *status,
                        priority: *priority,
                    },
                )?;
                self.applied.task_ids.push(task.id);
            }
            MutationOperation::UpdateTask {
                task_id,
                expected_revision,
                title,
                description,
                status,
                priority,
                estimate,
            } => {
                self.edit_task(task_id, *expected_revision, |_, input| {
                    if let Some(title) = title {
                        input.title = title.clone();
                    }
                    if let Some(description) = description {
                        input.description = description.clone();
                    }
                    if let Some(status) = status {
                        input.status = *status;
                    }
                    if let Some(priority) = priority {
                        input.priority = *priority;
                    }
                    input.estimated_minutes = estimate.apply(input.estimated_minutes.take());
                    Ok(())
                })?;
            }
            MutationOperation::ScheduleTask {
                task_id,
                expected_revision,
                scheduled_day,
                due_date,
                planned_week,
            } => {
                self.edit_task(task_id, *expected_revision, |_, input| {
                    input.scheduled_day = scheduled_day.apply(input.scheduled_day.take());
                    input.due_date = due_date.apply(input.due_date.take());
                    input.planned_week = planned_week.apply(input.planned_week.take());
                    Ok(())
                })?;
            }
            MutationOperation::SetTaskPlan {
                task_id,
                expected_revision,
                plan,
                milestone,
            } => {
                let plan_id = plan
                    .as_ref()
                    .map(|reference| self.plan_id(reference))
                    .transpose()?;
                let milestone_id = milestone
                    .as_ref()
                    .map(|reference| self.milestone_id(reference, plan_id.as_deref()))
                    .transpose()?;
                self.edit_task(task_id, *expected_revision, |current, input| {
                    if plan_id != current.plan_id {
                        input.workstream_id = None;
                    }
                    input.plan_id = plan_id;
                    input.milestone_id = milestone_id;
                    Ok(())
                })?;
            }
            MutationOperation::DeleteTask {
                task_id,
                expected_revision,
            } => {
                let revision = self.revision(task_id, *expected_revision)?;
                remove_task(connection, task_id, revision)?;
                self.applied.task_ids.push(task_id.clone());
            }
        }
        Ok(())
    }

    /// The revision a record must still have for this operation to apply.
    fn revision(&self, id: &str, expected_revision: i64) -> AppResult<i64> {
        match self.revisions.get(id) {
            Some((proposed, current)) if *proposed == expected_revision => Ok(*current),
            Some(_) => Err(AppError::Conflict),
            None => Ok(expected_revision),
        }
    }

    fn record(&mut self, id: &str, expected_revision: i64, revision: i64) {
        self.revisions
            .entry(id.to_string())
            .or_insert((expected_revision, revision))
            .1 = revision;
    }

    fn event(&self, id: &str, expected_revision: i64) -> AppResult<ScheduleEvent> {
        let revision = self.revision(id, expected_revision)?;
        let current = event_by_id(self.transaction, id)?.ok_or(AppError::NotFound)?;
        if current.revision != revision {
            return Err(AppError::Conflict);
        }
        Ok(current)
    }

    fn write_event(
        &mut self,
        current: &ScheduleEvent,
        expected_revision: i64,
        edit: EventEdit<'_>,
    ) -> AppResult<()> {
        let title = edit.title.unwrap_or(&current.title);
        let notes = edit.notes.unwrap_or(&current.notes);
        let duration = edit.duration_minutes.unwrap_or(current.duration_minutes);
        let (start, zone) = match edit.start {
            Some((start, zone)) => validate_time(start, zone)?,
            None => (current.start_at_utc.clone(), current.time_zone.clone()),
        };
        validate_title(title)?;
        validate_notes(notes)?;
        validate_duration(duration)?;
        let reminder =
            apply_reminder_change(edit.reminder_change, current.reminder_minutes_before)?;
        if matches!(edit.reminder_change, ReminderChange::Set { .. }) {
            validate_reminder_delivery_time(&start, reminder)?;
        }
        let (reminder_status, notification_id) = reminder_outbox_values(&start, reminder)?;
        let changed = self.transaction.execute(
            "UPDATE schedule_events
             SET title = ?1, notes = ?2, start_at_utc = ?3, time_zone = ?4, duration_minutes = ?5,
                 reminder_minutes_before = ?6, reminder_status = ?7, notification_id = ?8,
                 reminder_last_error = NULL, revision = revision + 1, updated_at = ?9
             WHERE id = ?10 AND revision = ?11",
            params![
                title.trim(),
                notes.trim(),
                start,
                zone,
                duration,
                reminder,
                reminder_status_string(&reminder_status),
                notification_id,
                now(),
                current.id,
                current.revision
            ],
        )?;
        if changed != 1 {
            return Err(AppError::Conflict);
        }
        self.record(&current.id, expected_revision, current.revision + 1);
        self.applied.event_ids.push(current.id.clone());
        Ok(())
    }

    /// Loads a task at the expected revision, lets `change` edit a full replacement, and saves it.
    fn edit_task(
        &mut self,
        id: &str,
        expected_revision: i64,
        change: impl FnOnce(&Task, &mut UpdateTaskInput) -> AppResult<()>,
    ) -> AppResult<()> {
        let revision = self.revision(id, expected_revision)?;
        let current = task_by_id(self.transaction, id)?.ok_or(AppError::NotFound)?;
        if current.revision != revision {
            return Err(AppError::Conflict);
        }
        let mut input = UpdateTaskInput {
            id: current.id.clone(),
            revision,
            title: current.title.clone(),
            description: current.description.clone(),
            plan_id: current.plan_id.clone(),
            milestone_id: current.milestone_id.clone(),
            workstream_id: current.workstream_id.clone(),
            owner_id: current.owner_id.clone(),
            due_date: current.due_date.clone(),
            scheduled_day: current.scheduled_day.clone(),
            planned_week: current.planned_week.clone(),
            estimated_minutes: current.estimated_minutes,
            status: current.status,
            priority: current.priority,
        };
        change(&current, &mut input)?;
        let updated = replace_task(self.transaction, &input)?;
        self.record(id, expected_revision, updated.revision);
        self.applied.task_ids.push(updated.id);
        Ok(())
    }

    fn plan_id(&self, reference: &RecordRef) -> AppResult<String> {
        match reference {
            RecordRef::Existing(existing) => {
                let plan = plan_by_id(self.transaction, &existing.id)?.ok_or_else(|| {
                    AppError::Validation("A plan in this proposal no longer exists.".into())
                })?;
                if plan.archived {
                    return Err(AppError::Validation(
                        "A plan in this proposal was archived after it was proposed.".into(),
                    ));
                }
                Ok(plan.id)
            }
            RecordRef::New(new) => self
                .new_plans
                .get(&fold(&new.new_title))
                .cloned()
                .ok_or_else(|| {
                    AppError::Validation("The proposal refers to a plan it does not create.".into())
                }),
        }
    }

    fn milestone_id(&self, reference: &RecordRef, plan_id: Option<&str>) -> AppResult<String> {
        match reference {
            RecordRef::Existing(existing) => Ok(existing.id.clone()),
            RecordRef::New(new) => plan_id
                .and_then(|plan_id| {
                    self.new_milestones
                        .get(&(plan_id.to_string(), fold(&new.new_title)))
                        .cloned()
                })
                .ok_or_else(|| {
                    AppError::Validation(
                        "The proposal refers to a milestone it does not create in that plan."
                            .into(),
                    )
                }),
        }
    }
}

struct EventEdit<'a> {
    title: Option<&'a str>,
    notes: Option<&'a str>,
    start: Option<(&'a String, &'a String)>,
    duration_minutes: Option<i64>,
    reminder_change: &'a ReminderChange,
}

/// Case- and whitespace-insensitive form of a title, used to match references by title.
pub(crate) fn fold(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Checks each operation's shape without reading the database.
fn validate_operations(operations: &[MutationOperation]) -> AppResult<()> {
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err(AppError::Validation(
            "A proposal must contain between 1 and 12 operations.".into(),
        ));
    }
    for operation in operations {
        match operation {
            MutationOperation::CreateEvent {
                title,
                notes,
                start_at_utc,
                time_zone,
                duration_minutes,
                reminder_minutes_before,
                plan,
            } => {
                validate_title(title)?;
                validate_notes(notes)?;
                validate_time(start_at_utc, time_zone)?;
                validate_duration(*duration_minutes)?;
                validate_reminder(*reminder_minutes_before)?;
                validate_optional_reference(plan.as_ref())?;
            }
            MutationOperation::UpdateEvent {
                event_id,
                expected_revision,
                title,
                notes,
                duration_minutes,
                reminder_change,
            } => {
                validate_target(event_id, *expected_revision)?;
                validate_event_fields(title, notes, duration_minutes, reminder_change)?;
                if title.is_none()
                    && notes.is_none()
                    && duration_minutes.is_none()
                    && reminder_change == &ReminderChange::Unchanged
                {
                    return Err(nothing_to_change("An event update"));
                }
            }
            MutationOperation::DeleteEvent {
                event_id,
                expected_revision,
            }
            | MutationOperation::SetEventPlan {
                event_id,
                expected_revision,
                plan: None,
            } => validate_target(event_id, *expected_revision)?,
            MutationOperation::SetEventPlan {
                event_id,
                expected_revision,
                plan: Some(plan),
            } => {
                validate_target(event_id, *expected_revision)?;
                validate_reference(plan)?;
            }
            MutationOperation::RescheduleEvent {
                event_id,
                expected_revision,
                title,
                notes,
                start_at_utc,
                time_zone,
                duration_minutes,
                reminder_change,
            } => {
                validate_target(event_id, *expected_revision)?;
                validate_event_fields(title, notes, duration_minutes, reminder_change)?;
                validate_time(start_at_utc, time_zone)?;
            }
            MutationOperation::CreatePlan {
                title,
                description,
                start_date,
                target_date,
                ..
            } => {
                validate_plan_shape(
                    title,
                    description,
                    start_date.as_deref(),
                    target_date.as_deref(),
                )?;
            }
            MutationOperation::UpdatePlan {
                plan_id,
                expected_revision,
                title,
                description,
                status,
                start_date,
                target_date,
            } => {
                validate_target(plan_id, *expected_revision)?;
                validate_optional_text(title.as_deref(), description.as_deref())?;
                validate_optional_days([start_date, target_date])?;
                if title.is_none()
                    && description.is_none()
                    && status.is_none()
                    && start_date.is_none()
                    && target_date.is_none()
                {
                    return Err(nothing_to_change("A plan update"));
                }
            }
            MutationOperation::CreateMilestone {
                plan,
                title,
                description,
                target_date,
            } => {
                validate_reference(plan)?;
                validate_milestone_shape(title, description, target_date.as_deref())?;
            }
            MutationOperation::UpdateMilestone {
                milestone_id,
                expected_revision,
                title,
                description,
                target_date,
                status,
            } => {
                validate_target(milestone_id, *expected_revision)?;
                validate_optional_text(title.as_deref(), description.as_deref())?;
                validate_optional_days([target_date])?;
                if title.is_none()
                    && description.is_none()
                    && target_date.is_none()
                    && status.is_none()
                {
                    return Err(nothing_to_change("A milestone update"));
                }
            }
            MutationOperation::DeleteMilestone {
                milestone_id: id,
                expected_revision,
            }
            | MutationOperation::DeleteTask {
                task_id: id,
                expected_revision,
            } => validate_target(id, *expected_revision)?,
            MutationOperation::CreateTask {
                title,
                description,
                plan,
                milestone,
                scheduled_day,
                due_date,
                ..
            } => {
                validate_title(title)?;
                validate_description(description)?;
                validate_optional_reference(plan.as_ref())?;
                validate_optional_reference(milestone.as_ref())?;
                validate_optional_days([scheduled_day, due_date])?;
                if milestone.is_some() && plan.is_none() {
                    return Err(super::planning::milestone_plan_mismatch());
                }
                if plan.is_none() && scheduled_day.is_none() && due_date.is_none() {
                    return Err(AppError::Validation(
                        "A task needs a plan, a scheduled day, or a due date.".into(),
                    ));
                }
            }
            MutationOperation::UpdateTask {
                task_id,
                expected_revision,
                title,
                description,
                status,
                priority,
                estimate,
            } => {
                validate_target(task_id, *expected_revision)?;
                validate_optional_text(title.as_deref(), description.as_deref())?;
                if let EstimateChange::Set { minutes } = estimate {
                    validate_estimate(*minutes)?;
                }
                if title.is_none()
                    && description.is_none()
                    && status.is_none()
                    && priority.is_none()
                    && estimate == &EstimateChange::Unchanged
                {
                    return Err(nothing_to_change("A task update"));
                }
            }
            MutationOperation::ScheduleTask {
                task_id,
                expected_revision,
                scheduled_day,
                due_date,
                planned_week,
            } => {
                validate_target(task_id, *expected_revision)?;
                for change in [scheduled_day, due_date, planned_week] {
                    if let DayChange::Set { day } = change {
                        parse_day(day)?;
                    }
                }
                if scheduled_day == &DayChange::Unchanged
                    && due_date == &DayChange::Unchanged
                    && planned_week == &DayChange::Unchanged
                {
                    return Err(nothing_to_change("A task scheduling change"));
                }
            }
            MutationOperation::SetTaskPlan {
                task_id,
                expected_revision,
                plan,
                milestone,
            } => {
                validate_target(task_id, *expected_revision)?;
                validate_optional_reference(plan.as_ref())?;
                validate_optional_reference(milestone.as_ref())?;
                if milestone.is_some() && plan.is_none() {
                    return Err(super::planning::milestone_plan_mismatch());
                }
            }
        }
    }
    Ok(())
}

pub fn validate_model_response(response: &ModelResponse) -> AppResult<()> {
    match response {
        ModelResponse::Proposal {
            summary,
            operations,
        } => {
            if summary.trim().is_empty() || summary.chars().count() > 280 {
                return Err(AppError::Validation(
                    "A proposal needs a short summary.".into(),
                ));
            }
            validate_operations(operations)
        }
        ModelResponse::Clarification { question }
            if question.trim().is_empty() || question.chars().count() > 280 =>
        {
            Err(AppError::Validation(
                "A clarification needs one short question.".into(),
            ))
        }
        ModelResponse::Clarification { .. } => Ok(()),
    }
}

fn validate_target(id: &str, revision: i64) -> AppResult<()> {
    validate_id(id)?;
    validate_revision(revision)
}

fn validate_event_fields(
    title: &Option<String>,
    notes: &Option<String>,
    duration_minutes: &Option<i64>,
    reminder_change: &ReminderChange,
) -> AppResult<()> {
    if let Some(title) = title {
        validate_title(title)?;
    }
    if let Some(notes) = notes {
        validate_notes(notes)?;
    }
    if let Some(duration) = duration_minutes {
        validate_duration(*duration)?;
    }
    validate_reminder_change(reminder_change)
}

fn validate_optional_text(title: Option<&str>, description: Option<&str>) -> AppResult<()> {
    if let Some(title) = title {
        validate_title(title)?;
    }
    if let Some(description) = description {
        validate_description(description)?;
    }
    Ok(())
}

fn validate_optional_days<const N: usize>(days: [&Option<String>; N]) -> AppResult<()> {
    for day in days.into_iter().flatten() {
        parse_day(day)?;
    }
    Ok(())
}

fn validate_reference(reference: &RecordRef) -> AppResult<()> {
    match reference {
        RecordRef::Existing(existing) => validate_id(&existing.id),
        RecordRef::New(new) => validate_title(&new.new_title),
    }
}

fn validate_optional_reference(reference: Option<&RecordRef>) -> AppResult<()> {
    reference.map_or(Ok(()), validate_reference)
}

fn nothing_to_change(what: &str) -> AppError {
    AppError::Validation(format!("{what} must change at least one permitted field."))
}

fn search_tokens(command: &str) -> Vec<String> {
    const IGNORED: &[&str] = &[
        "add",
        "after",
        "at",
        "back",
        "cancel",
        "change",
        "delete",
        "event",
        "for",
        "later",
        "make",
        "move",
        "next",
        "on",
        "reschedule",
        "shift",
        "the",
        "to",
        "today",
        "tomorrow",
        "update",
    ];
    command
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2 && !IGNORED.contains(token))
        .map(str::to_string)
        .collect()
}

/// Words in a command that could name a plan, milestone, or task.
pub(crate) fn planning_tokens(command: &str) -> Vec<String> {
    const IGNORED: &[&str] = &[
        "about",
        "add",
        "after",
        "all",
        "also",
        "and",
        "back",
        "before",
        "by",
        "can",
        "cancel",
        "change",
        "complete",
        "completed",
        "day",
        "delete",
        "done",
        "due",
        "each",
        "every",
        "finish",
        "finished",
        "for",
        "from",
        "have",
        "into",
        "its",
        "list",
        "make",
        "mark",
        "milestone",
        "milestones",
        "move",
        "need",
        "needs",
        "new",
        "next",
        "not",
        "now",
        "off",
        "one",
        "our",
        "out",
        "plan",
        "plans",
        "please",
        "project",
        "put",
        "remove",
        "rename",
        "schedule",
        "set",
        "should",
        "task",
        "tasks",
        "that",
        "the",
        "their",
        "them",
        "then",
        "this",
        "today",
        "todo",
        "tomorrow",
        "update",
        "want",
        "week",
        "what",
        "when",
        "with",
        "work",
        "your",
    ];
    command
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() >= 3 && !IGNORED.contains(token))
        .map(str::to_string)
        .collect()
}

/// How many command tokens name words of `title`, allowing simple plural differences.
pub(crate) fn title_matches(title: &str, tokens: &[String]) -> usize {
    let words = title
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tokens
        .iter()
        .filter(|token| {
            words.iter().any(|word| {
                word == *token
                    || (token.len() >= 4
                        && word.len() >= 4
                        && (word.strip_suffix('s') == Some(token.as_str())
                            || token.strip_suffix('s') == Some(word.as_str())))
            })
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PlanStatus, TaskPriority, TaskStatus};
    use tempfile::tempdir;
    use uuid::Uuid;

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
            })
            .unwrap()
    }

    fn task(database: &mut PlannerDatabase, title: &str, plan_id: Option<&str>) -> Task {
        database
            .create_task(CreateTaskInput {
                title: title.into(),
                description: String::new(),
                plan_id: plan_id.map(Into::into),
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: plan_id.is_none().then(|| "2026-09-15".into()),
                planned_week: None,
                estimated_minutes: None,
                status: TaskStatus::Todo,
                priority: TaskPriority::Normal,
            })
            .unwrap()
    }

    fn request<'a>(command: &'a str, referenced_ids: &'a [String]) -> CandidateRequest<'a> {
        CandidateRequest {
            command,
            selected_day: "2026-09-15",
            time_zone: "America/New_York",
            referenced_ids,
            active_plan_id: None,
        }
    }

    #[test]
    fn a_compound_proposal_creates_a_plan_milestone_task_and_event_together() {
        let mut database = database();
        let proposal = ModelResponse::proposal(
            "Start Charity Week",
            vec![
                MutationOperation::CreateTask {
                    title: "Book venue".into(),
                    description: String::new(),
                    plan: Some(RecordRef::new_title("Charity week")),
                    milestone: Some(RecordRef::new_title("Venue locked")),
                    scheduled_day: None,
                    due_date: Some("2026-10-03".into()),
                    status: TaskStatus::Todo,
                    priority: TaskPriority::High,
                },
                MutationOperation::CreateEvent {
                    title: "Planning call".into(),
                    notes: String::new(),
                    start_at_utc: "2026-09-18T19:00:00Z".into(),
                    time_zone: "America/New_York".into(),
                    duration_minutes: 30,
                    reminder_minutes_before: None,
                    plan: Some(RecordRef::new_title("charity WEEK")),
                },
                MutationOperation::CreateMilestone {
                    plan: RecordRef::new_title("Charity week"),
                    title: "Venue locked".into(),
                    description: String::new(),
                    target_date: Some("2026-10-05".into()),
                },
                MutationOperation::CreatePlan {
                    title: "Charity week".into(),
                    description: String::new(),
                    status: PlanStatus::Planning,
                    start_date: None,
                    target_date: Some("2026-10-16".into()),
                },
            ],
        );
        let applied = database.apply_proposal(&proposal).unwrap();
        assert_eq!(
            (
                applied.plan_ids.len(),
                applied.milestone_ids.len(),
                applied.task_ids.len(),
                applied.event_ids.len()
            ),
            (1, 1, 1, 1)
        );
        let workspace = database.plan_workspace(&applied.plan_ids[0]).unwrap();
        assert_eq!(workspace.milestones[0].title, "Venue locked");
        assert_eq!(
            workspace.tasks[0].milestone_id.as_deref(),
            Some(workspace.milestones[0].id.as_str())
        );
        assert_eq!(workspace.events[0].title, "Planning call");
    }

    /// A plan and the task that would live in it: accepting only the task is impossible, and
    /// accepting only the plan leaves the task behind.
    fn plan_and_its_task() -> ModelResponse {
        ModelResponse::proposal(
            "Start the bake sale",
            vec![
                MutationOperation::CreatePlan {
                    title: "Bake sale".into(),
                    description: String::new(),
                    status: PlanStatus::Planning,
                    start_date: None,
                    target_date: None,
                },
                MutationOperation::CreateTask {
                    title: "Buy flour".into(),
                    description: String::new(),
                    plan: Some(RecordRef::new_title("Bake sale")),
                    milestone: None,
                    scheduled_day: None,
                    due_date: Some("2026-10-01".into()),
                    status: TaskStatus::Todo,
                    priority: TaskPriority::Normal,
                },
            ],
        )
    }

    #[test]
    fn a_review_marks_what_each_suggestion_depends_on() {
        let ModelResponse::Proposal { operations, .. } = plan_and_its_task() else {
            panic!("expected a proposal");
        };
        let review = ProposedOperation::review(&operations, &[]);
        assert_eq!(review[0].id, "op-0");
        assert!(review[0].depends_on.is_empty(), "a new plan needs nothing");
        // The task names the plan this proposal creates, so it can't be applied without it.
        assert_eq!(review[1].depends_on, vec!["op-0".to_string()]);
    }

    #[test]
    fn applying_a_chosen_few_leaves_the_rest_undone() {
        let mut database = database();
        let proposal = plan_and_its_task();
        let chosen = accepted_operations(&proposal, Some(&["op-0".to_string()])).unwrap();

        let applied = database.apply_proposal(&chosen).unwrap();

        assert_eq!(applied.plan_ids.len(), 1);
        assert!(applied.task_ids.is_empty(), "the task was not accepted");
        assert_eq!(database.list_plans("2026-09-15").unwrap().len(), 1);
    }

    #[test]
    fn a_suggestion_is_never_applied_without_what_it_needs() {
        let proposal = plan_and_its_task();
        // Keeping the task while rejecting the plan it joins is refused before anything is written.
        let error = accepted_operations(&proposal, Some(&["op-1".to_string()])).unwrap_err();
        assert!(matches!(error, AppError::Validation(_)), "{error:?}");
        assert!(accepted_operations(&proposal, Some(&[])).is_err());
        assert!(accepted_operations(&proposal, Some(&["op-9".to_string()])).is_err());
        // Leaving the choice out applies everything, as it did before reviews existed.
        let all = accepted_operations(&proposal, None).unwrap();
        assert_eq!(all, proposal);
    }

    /// Accepting only part of a proposal can undo what made the rest legal, so the per-write rule
    /// that a task keeps a plan, a week, a day, or a due date has to hold for the chosen few too.
    #[test]
    fn a_chosen_few_that_would_strand_a_task_is_refused() {
        let mut database = database();
        let launch = plan(&mut database, "Launch");
        let notes = task(&mut database, "Draft notes", Some(&launch.id));
        // Together these keep the task somewhere: onto a day first, then out of the plan.
        let proposal = ModelResponse::proposal(
            "Move it out of the plan",
            vec![
                MutationOperation::ScheduleTask {
                    task_id: notes.id.clone(),
                    expected_revision: notes.revision,
                    scheduled_day: DayChange::Set {
                        day: "2026-10-02".into(),
                    },
                    due_date: DayChange::Unchanged,
                    planned_week: DayChange::Unchanged,
                },
                MutationOperation::SetTaskPlan {
                    task_id: notes.id.clone(),
                    expected_revision: notes.revision,
                    plan: None,
                    milestone: None,
                },
            ],
        );
        // Taking the task out of the plan without the day it was going to get strands it.
        let stranding = accepted_operations(&proposal, Some(&["op-1".to_string()])).unwrap();
        let error = database.apply_proposal(&stranding).unwrap_err();
        assert!(matches!(error, AppError::Validation(_)), "{error:?}");
        let kept = database.plan_workspace(&launch.id).unwrap().tasks.remove(0);
        assert_eq!(kept.id, notes.id, "nothing was written");

        // Accepting both is fine: the task leaves the plan but lands on a day.
        database.apply_proposal(&proposal).unwrap();
        assert!(database
            .plan_workspace(&launch.id)
            .unwrap()
            .tasks
            .is_empty());
        let moved = database.tasks_for_day("2026-10-02").unwrap().remove(0);
        assert_eq!(moved.id, notes.id);
        assert_eq!(moved.plan_id, None);
    }

    #[test]
    fn a_stale_planning_operation_rolls_back_the_whole_proposal() {
        let mut database = database();
        let launch = plan(&mut database, "Launch");
        let original = task(&mut database, "Draft notes", Some(&launch.id));
        database
            .update_task(UpdateTaskInput {
                id: original.id.clone(),
                revision: original.revision,
                title: "Draft launch notes".into(),
                description: String::new(),
                plan_id: original.plan_id.clone(),
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: None,
                planned_week: None,
                estimated_minutes: None,
                status: TaskStatus::Todo,
                priority: TaskPriority::Normal,
            })
            .unwrap();
        let proposal = ModelResponse::proposal(
            "Rename the plan and finish the notes",
            vec![
                MutationOperation::UpdatePlan {
                    plan_id: launch.id.clone(),
                    expected_revision: launch.revision,
                    title: Some("Launch 2.0".into()),
                    description: None,
                    status: None,
                    start_date: None,
                    target_date: None,
                },
                MutationOperation::UpdateTask {
                    task_id: original.id.clone(),
                    expected_revision: original.revision,
                    title: None,
                    description: None,
                    status: Some(TaskStatus::Done),
                    priority: None,
                    estimate: EstimateChange::Unchanged,
                },
            ],
        );
        assert!(matches!(
            database.apply_proposal(&proposal),
            Err(AppError::Conflict)
        ));
        assert_eq!(
            database.list_plans("2026-09-14").unwrap()[0].plan.title,
            "Launch"
        );
    }

    #[test]
    fn several_operations_may_edit_one_task_against_the_proposed_revision() {
        let mut database = database();
        let launch = plan(&mut database, "Launch");
        let venue = plan(&mut database, "Venue");
        let original = task(&mut database, "Book caterer", Some(&launch.id));
        let proposal = ModelResponse::proposal(
            "Finish and move",
            vec![
                MutationOperation::UpdateTask {
                    task_id: original.id.clone(),
                    expected_revision: original.revision,
                    title: None,
                    description: None,
                    status: Some(TaskStatus::Done),
                    priority: None,
                    estimate: EstimateChange::Unchanged,
                },
                MutationOperation::ScheduleTask {
                    task_id: original.id.clone(),
                    expected_revision: original.revision,
                    scheduled_day: DayChange::Set {
                        day: "2026-09-16".into(),
                    },
                    due_date: DayChange::Unchanged,
                    planned_week: DayChange::Unchanged,
                },
                MutationOperation::SetTaskPlan {
                    task_id: original.id.clone(),
                    expected_revision: original.revision,
                    plan: Some(RecordRef::existing(venue.id.clone())),
                    milestone: None,
                },
            ],
        );
        database.apply_proposal(&proposal).unwrap();
        let moved = database.plan_workspace(&venue.id).unwrap().tasks.remove(0);
        assert_eq!(moved.status, TaskStatus::Done);
        assert_eq!(moved.scheduled_day.as_deref(), Some("2026-09-16"));
        assert_eq!(moved.revision, original.revision + 3);
    }

    #[test]
    fn deleted_targets_and_unknown_titles_fail_without_partial_writes() {
        let mut database = database();
        let launch = plan(&mut database, "Launch");
        let gone = task(&mut database, "Old task", Some(&launch.id));
        database.delete_task(&gone.id, gone.revision).unwrap();
        let deleted_target = ModelResponse::proposal(
            "Create and finish",
            vec![
                MutationOperation::CreatePlan {
                    title: "Side project".into(),
                    description: String::new(),
                    status: PlanStatus::Planning,
                    start_date: None,
                    target_date: None,
                },
                MutationOperation::UpdateTask {
                    task_id: gone.id.clone(),
                    expected_revision: gone.revision,
                    title: None,
                    description: None,
                    status: Some(TaskStatus::Done),
                    priority: None,
                    estimate: EstimateChange::Unchanged,
                },
            ],
        );
        assert!(matches!(
            database.apply_proposal(&deleted_target),
            Err(AppError::NotFound)
        ));
        let unknown_plan = ModelResponse::proposal(
            "Add to a plan that is not created",
            vec![MutationOperation::CreateTask {
                title: "Order banners".into(),
                description: String::new(),
                plan: Some(RecordRef::new_title("Never created")),
                milestone: None,
                scheduled_day: None,
                due_date: None,
                status: TaskStatus::Todo,
                priority: TaskPriority::Normal,
            }],
        );
        assert!(matches!(
            database.apply_proposal(&unknown_plan),
            Err(AppError::Validation(_))
        ));
        assert_eq!(database.list_plans("2026-09-14").unwrap().len(), 1);
    }

    #[test]
    fn malformed_operations_are_rejected_before_any_write() {
        let homeless = MutationOperation::CreateTask {
            title: "Loose end".into(),
            description: String::new(),
            plan: None,
            milestone: None,
            scheduled_day: None,
            due_date: None,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
        };
        let empty_update = MutationOperation::UpdateTask {
            task_id: Uuid::new_v4().to_string(),
            expected_revision: 1,
            title: None,
            description: None,
            status: None,
            priority: None,
            estimate: EstimateChange::Unchanged,
        };
        let bad_day = MutationOperation::ScheduleTask {
            task_id: Uuid::new_v4().to_string(),
            expected_revision: 1,
            scheduled_day: DayChange::Set {
                day: "2026-02-30".into(),
            },
            due_date: DayChange::Unchanged,
            planned_week: DayChange::Unchanged,
        };
        let milestone_without_plan = MutationOperation::SetTaskPlan {
            task_id: Uuid::new_v4().to_string(),
            expected_revision: 1,
            plan: None,
            milestone: Some(RecordRef::existing(Uuid::new_v4().to_string())),
        };
        let bad_reference = MutationOperation::SetEventPlan {
            event_id: Uuid::new_v4().to_string(),
            expected_revision: 1,
            plan: Some(RecordRef::existing("not-a-uuid")),
        };
        for operation in [
            homeless,
            empty_update,
            bad_day,
            milestone_without_plan,
            bad_reference,
        ] {
            assert!(matches!(
                validate_operations(&[operation]),
                Err(AppError::Validation(_))
            ));
        }
        assert!(validate_operations(&[]).is_err());
    }

    #[test]
    fn moving_an_event_or_task_to_another_plan_clears_its_workstream() {
        let mut database = database();
        let show = plan(&mut database, "Show");
        let other = plan(&mut database, "Other");
        let stream = database
            .create_workstream(crate::model::CreateWorkstreamInput {
                plan_id: show.id.clone(),
                name: "Production".into(),
                description: String::new(),
            })
            .unwrap();
        let event = database
            .create_event(CreateEventInput {
                title: "Tech check".into(),
                notes: String::new(),
                start_at_utc: "2026-10-14T18:00:00Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 30,
                reminder_minutes_before: None,
                plan_id: Some(show.id.clone()),
                location: String::new(),
                workstream_id: Some(stream.id.clone()),
                owner_id: None,
            })
            .unwrap();
        let proposal = ModelResponse::proposal(
            "Move the tech check",
            vec![MutationOperation::SetEventPlan {
                event_id: event.id.clone(),
                expected_revision: event.revision,
                plan: Some(RecordRef::existing(other.id.clone())),
            }],
        );
        database.apply_proposal(&proposal).unwrap();
        let moved = database.plan_workspace(&other.id).unwrap().events.remove(0);
        assert_eq!((moved.workstream_id, moved.revision), (None, 2));
    }

    #[test]
    fn candidates_follow_titles_the_active_plan_and_the_selected_day() {
        let mut database = database();
        let charity = plan(&mut database, "Streamer Charity Week");
        let home = plan(&mut database, "Home renovation");
        let venue = task(&mut database, "Book venue", Some(&charity.id));
        let paint = task(&mut database, "Pick paint colors", Some(&home.id));
        let milk = task(&mut database, "Buy milk", None);
        let archived = plan(&mut database, "Old charity drive");
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
            })
            .unwrap();
        task(&mut database, "Charity archive task", Some(&archived.id));

        let ids = |tasks: &[Task]| tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        let matched = database
            .planner_candidates(&request("mark the venue booking done", &[]))
            .unwrap();
        assert_eq!(ids(&matched.tasks), [venue.id.clone(), milk.id.clone()]);
        assert_eq!(matched.plans.len(), 2);

        let focused = database
            .planner_candidates(&CandidateRequest {
                active_plan_id: Some(&home.id),
                ..request("what next", &[])
            })
            .unwrap();
        assert_eq!(focused.plans[0].id, home.id);
        assert_eq!(ids(&focused.tasks), [paint.id.clone(), milk.id.clone()]);

        let referenced = [paint.id.clone()];
        let follow_up = database
            .planner_candidates(&request("move it to the charity plan", &referenced))
            .unwrap();
        assert_eq!(follow_up.tasks[0].id, paint.id);
        assert_eq!(follow_up.plans[0].id, charity.id);
        assert!(follow_up.plans.iter().all(|plan| !plan.archived));
    }

    #[test]
    fn title_matching_ignores_filler_words_and_plurals() {
        let tokens = planning_tokens("Add a task to order the banner for the charity plan");
        assert_eq!(tokens, ["order", "banner", "charity"]);
        assert_eq!(title_matches("Order banners", &tokens), 2);
        assert_eq!(title_matches("Streamer Charity Week", &tokens), 1);
        assert_eq!(title_matches("Plan the week", &tokens), 0);
    }
}
