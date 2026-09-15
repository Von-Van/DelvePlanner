//! The model's reply format and its conversion into a checked, reviewable proposal.
//!
//! The model writes local wall-clock times, omits fields it does not change, and refers to
//! records by the IDs it was shown. Everything it writes is re-checked here: times become UTC,
//! values equal to the current ones are dropped, references are resolved, and requests that
//! cannot be applied safely become a clarifying question instead of an error.

use super::grounding::{
    is_bulk, is_follow_up, mentions_clock_time, mentions_day, mentions_duration, mentions_notes,
    mentions_plan_word, names, user_spelling, wants_reminder_at_start, words,
};
use crate::db::{fold, validate_model_response, PlannerDatabase};
use crate::error::{AppError, AppResult};
use crate::model::{
    DayChange, LocalDateTimeInput, LocalDateTimeResolution, Milestone, MilestoneStatus,
    ModelResponse, MutationOperation, Plan, PlanStatus, PlannerCandidates, ProposalReference,
    RecordKind, RecordRef, ReminderChange, ScheduleEvent, Task, TaskPriority, TaskStatus,
    MAX_DESCRIPTION_LENGTH, MAX_NOTES_LENGTH, MAX_OPERATIONS, MAX_REMINDER_MINUTES,
    MAX_TITLE_LENGTH,
};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(super) const DEFAULT_EVENT_MINUTES: i64 = 60;
const SUMMARY_LIMIT: usize = 280;

/// The single JSON object the model must produce.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Draft {
    Proposal {
        summary: String,
        operations: Vec<DraftOperation>,
    },
    Clarification {
        question: String,
    },
}

/// One operation as the model writes it. Optional fields left out keep their current or default
/// values; `plan` on `set_event_plan` and `set_task_plan` is required and `null` means no plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum DraftOperation {
    CreateEvent {
        title: String,
        start: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_minutes: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reminder_minutes_before: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<RecordRef>,
    },
    UpdateEvent {
        event_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_minutes: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reminder_change: Option<ReminderChange>,
    },
    RescheduleEvent {
        event_id: String,
        start: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_minutes: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reminder_change: Option<ReminderChange>,
    },
    DeleteEvent {
        event_id: String,
    },
    SetEventPlan {
        event_id: String,
        plan: Option<RecordRef>,
    },
    CreatePlan {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<PlanStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_date: Option<String>,
    },
    UpdatePlan {
        plan_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<PlanStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_date: Option<String>,
    },
    CreateMilestone {
        plan: RecordRef,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_date: Option<String>,
    },
    UpdateMilestone {
        milestone_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<MilestoneStatus>,
    },
    DeleteMilestone {
        milestone_id: String,
    },
    CreateTask {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<RecordRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        milestone: Option<RecordRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        do_on: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<TaskStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<TaskPriority>,
    },
    UpdateTask {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<TaskStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<TaskPriority>,
    },
    ScheduleTask {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        do_on: Option<DayChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_by: Option<DayChange>,
    },
    SetTaskPlan {
        task_id: String,
        plan: Option<RecordRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        milestone: Option<RecordRef>,
    },
    DeleteTask {
        task_id: String,
    },
}

pub(super) enum Resolution {
    Proposal(ResolvedProposal),
    Clarification(String),
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedProposal {
    pub summary: String,
    pub operations: Vec<MutationOperation>,
    /// The checked operations in the model's own format, for follow-up turns.
    pub drafts: Vec<DraftOperation>,
    pub references: Vec<ProposalReference>,
}

/// Why resolution stopped: a question for the user, or output that breaks the contract.
enum Stop {
    Ask(String),
    Invalid(AppError),
}

impl From<AppError> for Stop {
    fn from(error: AppError) -> Self {
        Self::Invalid(error)
    }
}

type Step<T> = Result<T, Stop>;

fn ask<T>(question: impl Into<String>) -> Step<T> {
    Err(Stop::Ask(question.into()))
}

fn invalid(message: &str) -> Stop {
    Stop::Invalid(AppError::InvalidModelResponse(message.into()))
}

pub(super) struct Resolver<'a> {
    pub command: &'a str,
    /// Everything the user said this session, oldest first, ending with `command`.
    pub said: &'a str,
    pub time_zone: &'a str,
    pub candidates: &'a PlannerCandidates,
    pub active_plan_id: Option<&'a str>,
    /// Records earlier turns of this session changed or created.
    pub session_ids: &'a [String],
    /// The operations of the proposal still awaiting review.
    pub pending: &'a [DraftOperation],
    /// Records the latest proposal of this session touched, or created once applied.
    pub recent_ids: &'a [String],
    pub now: DateTime<Utc>,
}

impl Resolver<'_> {
    pub fn resolve(&self, draft: Draft) -> AppResult<Resolution> {
        match self.try_resolve(draft) {
            Ok(resolution) => Ok(resolution),
            Err(Stop::Ask(question)) => Ok(Resolution::Clarification(question)),
            Err(Stop::Invalid(error)) => Err(error),
        }
    }

    fn try_resolve(&self, draft: Draft) -> Step<Resolution> {
        let (summary, operations) = match draft {
            Draft::Clarification { question } => {
                let question = question.trim();
                if question.is_empty() || question.chars().count() > SUMMARY_LIMIT {
                    return Err(invalid("a clarification needs one short question"));
                }
                return Ok(Resolution::Clarification(question.to_string()));
            }
            Draft::Proposal {
                summary,
                operations,
            } => (summary, operations),
        };
        if operations.is_empty() || operations.len() > MAX_OPERATIONS {
            return Err(invalid(
                "a proposal must contain between 1 and 12 operations",
            ));
        }
        let operations = self.retarget_follow_up(operations);
        // Days and lengths must come from this request, or from the earlier request whose pending
        // proposal it continues.
        let (operations, stated) = match self.carry_over(operations) {
            Carried::Merged(operations) => (operations, self.said),
            Carried::Unchanged(operations) => (operations, self.command),
        };
        if operations.len() > MAX_OPERATIONS {
            return ask(
                "That would be more than 12 changes at once. Which part should I do first?",
            );
        }
        let mut drafts = Vec::new();
        for operation in operations {
            if let Some(operation) = self.normalize(operation, stated)? {
                if !drafts.contains(&operation) {
                    drafts.push(operation);
                }
            }
        }
        if drafts.is_empty() {
            return ask("That would not change anything. What would you like to change?");
        }
        let mut drafts = merge_event_edits(drafts);
        self.ground(&mut drafts, stated)?;
        self.check_conflicts(&drafts)?;
        self.resolve_plan_titles(&mut drafts)?;
        self.resolve_milestone_titles(&mut drafts)?;
        self.check_task_homes(&drafts)?;
        let operations = drafts
            .iter()
            .map(|draft| self.mutation(draft))
            .collect::<Step<Vec<_>>>()?;
        let summary = summary_text(&summary);
        let response = ModelResponse::proposal(summary.clone(), operations);
        validate_model_response(&response)
            .map_err(|error| AppError::InvalidModelResponse(error.to_string()))?;
        let ModelResponse::Proposal { operations, .. } = response else {
            unreachable!("a proposal was just built");
        };
        Ok(Resolution::Proposal(ResolvedProposal {
            summary,
            references: self.references(&operations),
            operations,
            drafts,
        }))
    }

    /// Folds a follow-up into the proposal still awaiting review. The model often returns only the
    /// new change ("rename it to Strength training"): operations on the same subject merge with the
    /// new values winning, and the rest of the pending proposal carries over. When "it" can only
    /// mean the one record the pending proposal would create, an edit the model aimed at an
    /// unnamed existing record applies to that new record instead. A follow-up that touches
    /// nothing pending replaces the proposal unless it says it adds to it ("also").
    fn carry_over(&self, new: Vec<DraftOperation>) -> Carried {
        let pending = self.pending;
        if pending.is_empty() || !is_follow_up(self.command) {
            return Carried::Unchanged(new);
        }
        let overlaps = new.iter().any(|operation| {
            pending
                .iter()
                .any(|existing| same_subject(existing, operation))
        });
        let adds = words(self.command)
            .iter()
            .any(|word| matches!(word.as_str(), "also" | "too" | "plus" | "well"));
        if overlaps || adds {
            return Carried::Merged(merge_into(pending, new));
        }
        let unnamed = |operation: &DraftOperation| match operation {
            DraftOperation::UpdateTask { task_id, .. }
            | DraftOperation::ScheduleTask { task_id, .. } => self
                .task(task_id)
                .is_ok_and(|task| !names(self.command, &task.title)),
            DraftOperation::UpdateEvent { event_id, .. }
            | DraftOperation::RescheduleEvent { event_id, .. } => self
                .event(event_id)
                .is_ok_and(|event| !names(self.command, &event.title)),
            _ => false,
        };
        if !new.is_empty() && new.iter().all(unnamed) {
            let mut merged = pending.to_vec();
            if new
                .iter()
                .all(|operation| edit_pending_create(&mut merged, operation))
            {
                return Carried::Merged(merged);
            }
        }
        Carried::Unchanged(new)
    }

    /// "Mark it in progress" right after a proposal that touched exactly one task means that task,
    /// even when the model aimed the edit at a different task the user never named.
    fn retarget_follow_up(&self, mut operations: Vec<DraftOperation>) -> Vec<DraftOperation> {
        if !is_follow_up(self.command) {
            return operations;
        }
        let recent_tasks = self
            .recent_ids
            .iter()
            .filter(|id| self.task(id).is_ok())
            .collect::<Vec<_>>();
        let [recent] = recent_tasks.as_slice() else {
            return operations;
        };
        for operation in &mut operations {
            if let DraftOperation::UpdateTask { task_id, .. }
            | DraftOperation::ScheduleTask { task_id, .. }
            | DraftOperation::SetTaskPlan { task_id, .. } = operation
            {
                let named = self
                    .task(task_id)
                    .is_ok_and(|task| names(self.said, &task.title));
                if !named {
                    *task_id = (*recent).clone();
                }
            }
        }
        operations
    }

    /// Checks one operation's values and drops fields that would not change anything. Returns
    /// `None` when the whole operation is a no-op.
    fn normalize(&self, operation: DraftOperation, stated: &str) -> Step<Option<DraftOperation>> {
        Ok(match operation {
            DraftOperation::CreateEvent {
                title,
                start,
                notes,
                duration_minutes,
                reminder_minutes_before,
                plan,
            } => {
                let title = clean_title(&self.spelled(title))?;
                let start = self.stated_clock(start);
                let start_at_utc = self.utc_start(&start)?;
                let notes = notes
                    .filter(|_| mentions_notes(stated))
                    .map(|notes| clean_notes(&notes))
                    .transpose()?
                    .filter(|notes| !notes.is_empty());
                let duration_minutes = Some(check_duration(
                    duration_minutes
                        .filter(|_| mentions_duration(stated))
                        .unwrap_or(DEFAULT_EVENT_MINUTES),
                )?);
                let reminder_minutes_before = reminder_minutes_before
                    .map(|minutes| {
                        self.check_new_reminder(self.stated_reminder(minutes), &start_at_utc)
                    })
                    .transpose()?;
                self.check_plan_reference(plan.as_ref())?;
                Some(DraftOperation::CreateEvent {
                    title,
                    start,
                    notes,
                    duration_minutes,
                    reminder_minutes_before,
                    plan,
                })
            }
            DraftOperation::UpdateEvent {
                event_id,
                title,
                notes,
                duration_minutes,
                reminder_change,
            } => {
                let event = self.event(&event_id)?;
                let edit = self.event_edit(
                    event,
                    &event.start_at_utc,
                    title,
                    notes.filter(|_| mentions_notes(stated)),
                    duration_minutes.filter(|_| mentions_duration(stated)),
                    reminder_change,
                )?;
                edit.into_update(event_id)
            }
            DraftOperation::RescheduleEvent {
                event_id,
                start,
                title,
                notes,
                duration_minutes,
                reminder_change,
            } => {
                let event = self.event(&event_id)?;
                let start = self.stated_clock(start);
                let start_at_utc = self.utc_start(&start)?;
                let edit = self.event_edit(
                    event,
                    &start_at_utc,
                    title,
                    notes.filter(|_| mentions_notes(stated)),
                    duration_minutes.filter(|_| mentions_duration(stated)),
                    reminder_change,
                )?;
                if start_at_utc == event.start_at_utc {
                    edit.into_update(event_id)
                } else {
                    Some(DraftOperation::RescheduleEvent {
                        event_id,
                        start,
                        title: edit.title,
                        notes: edit.notes,
                        duration_minutes: edit.duration_minutes,
                        reminder_change: edit.reminder_change,
                    })
                }
            }
            DraftOperation::DeleteEvent { event_id } => {
                self.event(&event_id)?;
                Some(DraftOperation::DeleteEvent { event_id })
            }
            DraftOperation::SetEventPlan { event_id, plan } => {
                let event = self.event(&event_id)?;
                self.check_plan_reference(plan.as_ref())?;
                (!same_link(plan.as_ref(), event.plan_id.as_deref()))
                    .then_some(DraftOperation::SetEventPlan { event_id, plan })
            }
            DraftOperation::CreatePlan {
                title,
                description,
                status,
                start_date,
                target_date,
            } => {
                let title = clean_title(&self.spelled(title))?;
                let description = clean_optional_description(description)?;
                let start_date = start_date.map(|day| check_day(&day)).transpose()?;
                let target_date = target_date.map(|day| check_day(&day)).transpose()?;
                check_date_order(&title, start_date.as_deref(), target_date.as_deref())?;
                Some(DraftOperation::CreatePlan {
                    title,
                    description,
                    status,
                    start_date,
                    target_date,
                })
            }
            DraftOperation::UpdatePlan {
                plan_id,
                title,
                description,
                status,
                start_date,
                target_date,
            } => {
                let plan = self.plan(&plan_id)?;
                let title = changed_title(title.map(|title| self.spelled(title)), &plan.title)?;
                let description = changed_description(description, &plan.description)?;
                let status = status.filter(|status| *status != plan.status);
                let start_date = changed_date(start_date, plan.start_date.as_deref())?;
                let target_date = changed_date(target_date, plan.target_date.as_deref())?;
                check_date_order(
                    title.as_deref().unwrap_or(&plan.title),
                    start_date.as_deref().or(plan.start_date.as_deref()),
                    target_date.as_deref().or(plan.target_date.as_deref()),
                )?;
                let unchanged = title.is_none()
                    && description.is_none()
                    && status.is_none()
                    && start_date.is_none()
                    && target_date.is_none();
                (!unchanged).then_some(DraftOperation::UpdatePlan {
                    plan_id,
                    title,
                    description,
                    status,
                    start_date,
                    target_date,
                })
            }
            DraftOperation::CreateMilestone {
                plan,
                title,
                description,
                target_date,
            } => {
                self.check_plan_reference(Some(&plan))?;
                Some(DraftOperation::CreateMilestone {
                    plan,
                    title: clean_title(&self.spelled(title))?,
                    description: clean_optional_description(description)?,
                    target_date: target_date.map(|day| check_day(&day)).transpose()?,
                })
            }
            DraftOperation::UpdateMilestone {
                milestone_id,
                title,
                description,
                target_date,
                status,
            } => {
                let milestone = self.milestone(&milestone_id)?;
                let title =
                    changed_title(title.map(|title| self.spelled(title)), &milestone.title)?;
                let description = changed_description(description, &milestone.description)?;
                let target_date = changed_date(target_date, milestone.target_date.as_deref())?;
                let status = status.filter(|status| *status != milestone.status);
                let unchanged = title.is_none()
                    && description.is_none()
                    && target_date.is_none()
                    && status.is_none();
                (!unchanged).then_some(DraftOperation::UpdateMilestone {
                    milestone_id,
                    title,
                    description,
                    target_date,
                    status,
                })
            }
            DraftOperation::DeleteMilestone { milestone_id } => {
                self.milestone(&milestone_id)?;
                Some(DraftOperation::DeleteMilestone { milestone_id })
            }
            DraftOperation::CreateTask {
                title,
                description,
                plan,
                milestone,
                do_on,
                due_by,
                status,
                priority,
            } => {
                self.check_plan_reference(plan.as_ref())?;
                self.check_milestone_reference(milestone.as_ref())?;
                Some(DraftOperation::CreateTask {
                    title: clean_title(&self.spelled(title))?,
                    description: clean_optional_description(description)?,
                    plan,
                    milestone,
                    do_on: do_on.map(|day| check_day(&day)).transpose()?,
                    due_by: due_by.map(|day| check_day(&day)).transpose()?,
                    status,
                    priority,
                })
            }
            DraftOperation::UpdateTask {
                task_id,
                title,
                description,
                status,
                priority,
            } => {
                let task = self.task(&task_id)?;
                let title = changed_title(title.map(|title| self.spelled(title)), &task.title)?;
                let description = changed_description(description, &task.description)?;
                let status = status.filter(|status| *status != task.status);
                let priority = priority.filter(|priority| *priority != task.priority);
                let unchanged = title.is_none()
                    && description.is_none()
                    && status.is_none()
                    && priority.is_none();
                (!unchanged).then_some(DraftOperation::UpdateTask {
                    task_id,
                    title,
                    description,
                    status,
                    priority,
                })
            }
            DraftOperation::ScheduleTask {
                task_id,
                do_on,
                due_by,
            } => {
                let task = self.task(&task_id)?;
                let do_on = changed_day(do_on, task.scheduled_day.as_deref())?;
                let due_by = changed_day(due_by, task.due_date.as_deref())?;
                (do_on.is_some() || due_by.is_some()).then_some(DraftOperation::ScheduleTask {
                    task_id,
                    do_on,
                    due_by,
                })
            }
            DraftOperation::SetTaskPlan {
                task_id,
                plan,
                milestone,
            } => {
                let task = self.task(&task_id)?;
                self.check_plan_reference(plan.as_ref())?;
                self.check_milestone_reference(milestone.as_ref())?;
                let unchanged = same_link(plan.as_ref(), task.plan_id.as_deref())
                    && same_link(milestone.as_ref(), task.milestone_id.as_deref());
                (!unchanged).then_some(DraftOperation::SetTaskPlan {
                    task_id,
                    plan,
                    milestone,
                })
            }
            DraftOperation::DeleteTask { task_id } => {
                self.task(&task_id)?;
                Some(DraftOperation::DeleteTask { task_id })
            }
        })
    }

    fn event_edit(
        &self,
        event: &ScheduleEvent,
        start_at_utc: &str,
        title: Option<String>,
        notes: Option<String>,
        duration_minutes: Option<i64>,
        reminder_change: Option<ReminderChange>,
    ) -> Step<EventEdit> {
        let title = changed_title(title.map(|title| self.spelled(title)), &event.title)?;
        let notes = match notes {
            Some(notes) => {
                let notes = without_target(clean_notes(&notes)?, &event.title);
                (notes != event.notes).then_some(notes)
            }
            None => None,
        };
        let duration_minutes = duration_minutes
            .map(check_duration)
            .transpose()?
            .filter(|minutes| *minutes != event.duration_minutes);
        let reminder_change = match reminder_change {
            None | Some(ReminderChange::Unchanged) => None,
            Some(ReminderChange::Clear) => event
                .reminder_minutes_before
                .is_some()
                .then_some(ReminderChange::Clear),
            Some(ReminderChange::Set { minutes_before }) => {
                let minutes_before =
                    self.check_new_reminder(self.stated_reminder(minutes_before), start_at_utc)?;
                (event.reminder_minutes_before != Some(minutes_before))
                    .then_some(ReminderChange::Set { minutes_before })
            }
        };
        Ok(EventEdit {
            title,
            notes,
            duration_minutes,
            reminder_change,
        })
    }

    /// Drops values the user never stated and asks when the model picked a plan, milestone, or
    /// task the user did not name. A record counts as named when the session mentions its title
    /// or a distinctive word of it, when an earlier turn changed or created it, or, for plans, when
    /// it is the plan being viewed or the plan of a named milestone. Events keep the original
    /// schedule rules, which allow bulk edits such as "push everything back 30 minutes".
    fn ground(&self, drafts: &mut Vec<DraftOperation>, stated: &str) -> Step<()> {
        let days_stated = mentions_day(stated);
        let bulk = is_bulk(self.command);
        let names_a_plan = mentions_plan_word(self.command);
        let known = |id: &str| {
            self.session_ids.iter().any(|known| known == id) || self.active_plan_id == Some(id)
        };
        let created = |kind: fn(&DraftOperation) -> Option<&String>| {
            drafts
                .iter()
                .filter_map(kind)
                .map(|title| fold(title))
                .collect::<HashSet<_>>()
        };
        let created_plans = created(|draft| match draft {
            DraftOperation::CreatePlan { title, .. } => Some(title),
            _ => None,
        });
        let created_milestones = created(|draft| match draft {
            DraftOperation::CreateMilestone { title, .. } => Some(title),
            _ => None,
        });
        // A reference is grounded when the user named the record, touched it earlier, is viewing
        // it, or, for a new title, when this proposal creates it.
        let plan_ok = |reference: &RecordRef| match reference {
            RecordRef::Existing(existing) => {
                known(&existing.id)
                    || self
                        .plan(&existing.id)
                        .is_ok_and(|plan| names(self.said, &plan.title))
            }
            RecordRef::New(new) => {
                created_plans.contains(&fold(&new.new_title)) || names(self.said, &new.new_title)
            }
        };
        let milestone_ok = |reference: &RecordRef| match reference {
            RecordRef::Existing(existing) => {
                known(&existing.id)
                    || self
                        .milestone(&existing.id)
                        .is_ok_and(|milestone| names(self.said, &milestone.title))
            }
            RecordRef::New(new) => {
                created_milestones.contains(&fold(&new.new_title))
                    || names(self.said, &new.new_title)
            }
        };
        let task_named = |id: &str| {
            known(id)
                || self
                    .task(id)
                    .is_ok_and(|task| names(self.said, &task.title))
        };
        // A plan also counts when it is the plan of the milestone the operation uses.
        let plan_of_milestone =
            |plan: &RecordRef, milestone: &Option<RecordRef>| match (plan, milestone) {
                (RecordRef::Existing(plan), Some(RecordRef::Existing(milestone))) => self
                    .milestone(&milestone.id)
                    .is_ok_and(|milestone| milestone.plan_id == plan.id),
                _ => false,
            };
        for draft in drafts.iter_mut() {
            match draft {
                DraftOperation::CreateEvent { title, plan, .. } => {
                    if plan.as_ref().is_some_and(|plan| !plan_ok(plan)) {
                        if names_a_plan {
                            return ask(format!("Which plan should “{title}” go in?"));
                        }
                        *plan = None;
                    }
                }
                DraftOperation::SetEventPlan {
                    event_id,
                    plan: Some(plan),
                } if !plan_ok(plan) => {
                    return ask(format!(
                        "Which plan should “{}” go in?",
                        self.title_of(event_id)
                    ));
                }
                DraftOperation::CreatePlan {
                    start_date,
                    target_date,
                    ..
                } => {
                    if !days_stated {
                        *start_date = None;
                        *target_date = None;
                    }
                }
                DraftOperation::UpdatePlan {
                    plan_id,
                    start_date,
                    target_date,
                    ..
                } => {
                    if !plan_ok(&RecordRef::existing(plan_id.clone())) {
                        return ask("Which plan do you mean?");
                    }
                    if !days_stated {
                        *start_date = None;
                        *target_date = None;
                    }
                }
                DraftOperation::CreateMilestone {
                    plan,
                    title,
                    target_date,
                    ..
                } => {
                    if !plan_ok(plan) {
                        return ask(format!("Which plan should the “{title}” milestone go in?"));
                    }
                    if !days_stated {
                        *target_date = None;
                    }
                }
                DraftOperation::UpdateMilestone {
                    milestone_id,
                    target_date,
                    ..
                } => {
                    if !milestone_ok(&RecordRef::existing(milestone_id.clone())) {
                        return ask("Which milestone do you mean?");
                    }
                    if !days_stated {
                        *target_date = None;
                    }
                }
                DraftOperation::DeleteMilestone { milestone_id }
                    if !milestone_ok(&RecordRef::existing(milestone_id.clone())) =>
                {
                    return ask("Which milestone should I delete?");
                }
                DraftOperation::CreateTask {
                    title,
                    plan,
                    milestone,
                    do_on,
                    due_by,
                    ..
                } => {
                    if milestone
                        .as_ref()
                        .is_some_and(|milestone| !milestone_ok(milestone))
                    {
                        *milestone = None;
                    }
                    if plan
                        .as_ref()
                        .is_some_and(|plan| !plan_ok(plan) && !plan_of_milestone(plan, milestone))
                    {
                        if names_a_plan {
                            return ask(format!("Which plan should “{title}” go in?"));
                        }
                        *plan = None;
                    }
                    if !days_stated {
                        *do_on = None;
                        *due_by = None;
                    }
                }
                DraftOperation::UpdateTask { task_id, .. } if !task_named(task_id) && !bulk => {
                    return ask("Which task do you mean?");
                }
                DraftOperation::ScheduleTask {
                    task_id,
                    do_on,
                    due_by,
                } => {
                    if !task_named(task_id) && !bulk {
                        return ask("Which task do you mean?");
                    }
                    if !days_stated {
                        for change in [do_on, due_by] {
                            if matches!(change, Some(DayChange::Set { .. })) {
                                *change = None;
                            }
                        }
                    }
                }
                DraftOperation::SetTaskPlan {
                    task_id,
                    plan,
                    milestone,
                } => {
                    if !task_named(task_id) && !bulk {
                        return ask("Which task do you mean?");
                    }
                    if milestone
                        .as_ref()
                        .is_some_and(|milestone| !milestone_ok(milestone))
                    {
                        *milestone = None;
                    }
                    if plan
                        .as_ref()
                        .is_some_and(|plan| !plan_ok(plan) && !plan_of_milestone(plan, milestone))
                    {
                        return ask(format!(
                            "Which plan should “{}” move to?",
                            self.title_of(task_id)
                        ));
                    }
                }
                DraftOperation::DeleteTask { task_id } if !task_named(task_id) => {
                    return ask("Which task should I delete?");
                }
                _ => {}
            }
        }
        drafts.retain(|draft| !is_empty_edit(draft));
        if drafts.is_empty() {
            return ask("That would not change anything. What would you like to change?");
        }
        Ok(())
    }

    /// The request's own spelling of a title the model copied from it.
    fn spelled(&self, title: String) -> String {
        user_spelling(self.command, &title).unwrap_or(title)
    }

    /// "Midnight" and "noon" without a clock time are 00:00 and 12:00 on the day the model chose.
    fn stated_clock(&self, start: String) -> String {
        let named = words(self.command);
        let clock = if named.iter().any(|word| word == "midnight") {
            "00:00"
        } else if named.iter().any(|word| word == "noon") {
            "12:00"
        } else {
            return start;
        };
        if mentions_clock_time(self.command) {
            return start;
        }
        match start.split_once('T') {
            Some((day, _)) => format!("{day}T{clock}"),
            None => start,
        }
    }

    /// A reminder requested "when it starts" with no stated offset is at the start.
    fn stated_reminder(&self, minutes: i64) -> i64 {
        if wants_reminder_at_start(self.command) {
            0
        } else {
            minutes
        }
    }

    fn check_conflicts(&self, drafts: &[DraftOperation]) -> Step<()> {
        let mut touched: HashMap<&str, Vec<Touch>> = HashMap::new();
        let mut used_milestones = HashSet::new();
        for draft in drafts {
            let (id, touch) = match draft {
                DraftOperation::UpdateEvent { event_id, .. }
                | DraftOperation::RescheduleEvent { event_id, .. } => (event_id, Touch::Edit),
                DraftOperation::DeleteEvent { event_id } => (event_id, Touch::Delete),
                DraftOperation::SetEventPlan { event_id, .. } => (event_id, Touch::Link),
                DraftOperation::UpdatePlan { plan_id, .. } => (plan_id, Touch::Edit),
                DraftOperation::UpdateMilestone { milestone_id, .. } => (milestone_id, Touch::Edit),
                DraftOperation::DeleteMilestone { milestone_id } => (milestone_id, Touch::Delete),
                DraftOperation::UpdateTask { task_id, .. } => (task_id, Touch::Edit),
                DraftOperation::ScheduleTask { task_id, .. } => (task_id, Touch::Schedule),
                DraftOperation::SetTaskPlan {
                    task_id, milestone, ..
                } => {
                    if let Some(RecordRef::Existing(existing)) = milestone {
                        used_milestones.insert(existing.id.as_str());
                    }
                    (task_id, Touch::Link)
                }
                DraftOperation::DeleteTask { task_id } => (task_id, Touch::Delete),
                DraftOperation::CreateTask {
                    milestone: Some(RecordRef::Existing(existing)),
                    ..
                } => {
                    used_milestones.insert(existing.id.as_str());
                    continue;
                }
                _ => continue,
            };
            touched.entry(id.as_str()).or_default().push(touch);
        }
        for (id, touches) in &touched {
            let title = self.title_of(id);
            if touches.contains(&Touch::Delete)
                && (touches.len() > 1 || used_milestones.contains(id))
            {
                return ask(format!("Should I change “{title}” or delete it?"));
            }
            let mut kinds = HashSet::new();
            if touches.iter().any(|touch| !kinds.insert(*touch)) {
                return ask(format!(
                    "That asks for more than one change of the same kind to “{title}”. Which one do you want?"
                ));
            }
        }
        Ok(())
    }

    /// Resolves `newTitle` plan references: to a plan created in the same proposal, or to the
    /// one existing plan with that exact title. Anything else needs the user's answer.
    fn resolve_plan_titles(&self, drafts: &mut [DraftOperation]) -> Step<()> {
        let mut created = HashSet::new();
        for draft in drafts.iter() {
            if let DraftOperation::CreatePlan { title, .. } = draft {
                if !created.insert(fold(title)) {
                    return ask(format!("Should I create just one plan named “{title}”?"));
                }
                if let Some(existing) = self
                    .candidates
                    .plans
                    .iter()
                    .find(|plan| fold(&plan.title) == fold(title))
                {
                    return ask(format!(
                        "You already have a plan named “{}”. Should I use it instead of creating another?",
                        existing.title
                    ));
                }
            }
        }
        for draft in drafts.iter_mut() {
            let reference = match draft {
                DraftOperation::CreateMilestone { plan, .. } => Some(plan),
                DraftOperation::CreateEvent { plan, .. }
                | DraftOperation::SetEventPlan { plan, .. }
                | DraftOperation::CreateTask { plan, .. }
                | DraftOperation::SetTaskPlan { plan, .. } => plan.as_mut(),
                _ => None,
            };
            let Some(reference) = reference else {
                continue;
            };
            let RecordRef::New(new) = reference else {
                continue;
            };
            let title = new.new_title.clone();
            let key = fold(&title);
            if created.contains(&key) {
                continue;
            }
            let matches = self
                .candidates
                .plans
                .iter()
                .filter(|plan| fold(&plan.title) == key)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [plan] => *reference = RecordRef::existing(plan.id.clone()),
                [] => {
                    return ask(format!(
                        "I couldn't find a plan named “{title}”. Should I create it?"
                    ))
                }
                _ => {
                    return ask(format!(
                        "More than one plan is named “{title}”. Which one do you mean?"
                    ))
                }
            }
        }
        Ok(())
    }

    /// Resolves milestone references on tasks, filling in the plan a milestone belongs to and
    /// asking when a milestone and plan do not fit together.
    fn resolve_milestone_titles(&self, drafts: &mut [DraftOperation]) -> Step<()> {
        let mut created: HashMap<(PlanKey, String), String> = HashMap::new();
        for draft in drafts.iter() {
            let DraftOperation::CreateMilestone { plan, title, .. } = draft else {
                continue;
            };
            let plan_key = PlanKey::from(plan);
            if let PlanKey::Existing(plan_id) = &plan_key {
                if self.candidates.milestones.iter().any(|milestone| {
                    milestone.plan_id == *plan_id && fold(&milestone.title) == fold(title)
                }) {
                    return ask(format!(
                        "“{}” already has a milestone named “{title}”. Should I use that one?",
                        self.plan_title(plan_id)
                    ));
                }
            }
            if created
                .insert((plan_key, fold(title)), title.clone())
                .is_some()
            {
                return ask(format!(
                    "Should I create just one milestone named “{title}”?"
                ));
            }
        }
        for draft in drafts.iter_mut() {
            let (task_title, plan, milestone) = match draft {
                DraftOperation::CreateTask {
                    title,
                    plan,
                    milestone,
                    ..
                } => (title.clone(), plan, milestone),
                DraftOperation::SetTaskPlan {
                    task_id,
                    plan,
                    milestone,
                } => (self.title_of(task_id), plan, milestone),
                _ => continue,
            };
            let Some(reference) = milestone.as_mut() else {
                continue;
            };
            match reference {
                RecordRef::Existing(existing) => {
                    let found = self.milestone(&existing.id)?;
                    match plan.as_ref() {
                        None => *plan = Some(RecordRef::existing(found.plan_id.clone())),
                        Some(RecordRef::Existing(plan_ref)) if plan_ref.id == found.plan_id => {}
                        Some(_) => {
                            return ask(format!(
                            "“{}” is a milestone in “{}”. Which plan should “{task_title}” go in?",
                            found.title,
                            self.plan_title(&found.plan_id)
                        ))
                        }
                    }
                }
                RecordRef::New(new) => {
                    let key = fold(&new.new_title);
                    let plan_key = plan.as_ref().map(PlanKey::from);
                    let created_in = created
                        .keys()
                        .filter(|(created_plan, title)| {
                            *title == key
                                && plan_key.as_ref().is_none_or(|plan| plan == created_plan)
                        })
                        .map(|(created_plan, _)| created_plan.clone())
                        .collect::<Vec<_>>();
                    if let [created_plan] = created_in.as_slice() {
                        if plan.is_none() {
                            *plan = Some(created_plan.reference());
                        }
                        continue;
                    }
                    let existing = self
                        .candidates
                        .milestones
                        .iter()
                        .filter(|milestone| {
                            fold(&milestone.title) == key
                                && match &plan_key {
                                    None => true,
                                    Some(PlanKey::Existing(plan_id)) => {
                                        milestone.plan_id == *plan_id
                                    }
                                    Some(PlanKey::New(_)) => false,
                                }
                        })
                        .collect::<Vec<_>>();
                    match existing.as_slice() {
                        [found] => {
                            if plan.is_none() {
                                *plan = Some(RecordRef::existing(found.plan_id.clone()));
                            }
                            *reference = RecordRef::existing(found.id.clone());
                        }
                        [] => {
                            return ask(format!(
                                "I couldn't find a milestone named “{}”. Should I create it?",
                                new.new_title
                            ))
                        }
                        _ => {
                            return ask(format!(
                                "More than one milestone is named “{}”. Which plan's milestone do you mean?",
                                new.new_title
                            ))
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Every task must keep a plan, a scheduled day, or a due date after the proposal.
    fn check_task_homes(&self, drafts: &[DraftOperation]) -> Step<()> {
        let mut edited: Vec<&str> = Vec::new();
        for draft in drafts {
            match draft {
                DraftOperation::CreateTask {
                    title,
                    plan: None,
                    do_on: None,
                    due_by: None,
                    ..
                } => {
                    return ask(format!(
                        "Which day should “{title}” go on, or which plan does it belong to?"
                    ))
                }
                DraftOperation::ScheduleTask { task_id, .. }
                | DraftOperation::SetTaskPlan { task_id, .. } => edited.push(task_id.as_str()),
                _ => {}
            }
        }
        for task_id in edited {
            let task = self.task(task_id)?;
            let mut has_plan = task.plan_id.is_some();
            let mut scheduled = task.scheduled_day.is_some();
            let mut due = task.due_date.is_some();
            for draft in drafts {
                match draft {
                    DraftOperation::ScheduleTask {
                        task_id: id,
                        do_on,
                        due_by,
                    } if id == task_id => {
                        scheduled = next_has_day(do_on.as_ref(), scheduled);
                        due = next_has_day(due_by.as_ref(), due);
                    }
                    DraftOperation::SetTaskPlan {
                        task_id: id, plan, ..
                    } if id == task_id => has_plan = plan.is_some(),
                    _ => {}
                }
            }
            if !has_plan && !scheduled && !due {
                return ask(format!(
                    "“{}” needs a day, a due date, or a plan. Where should it go?",
                    task.title
                ));
            }
        }
        Ok(())
    }

    fn mutation(&self, draft: &DraftOperation) -> Step<MutationOperation> {
        Ok(match draft.clone() {
            DraftOperation::CreateEvent {
                title,
                start,
                notes,
                duration_minutes,
                reminder_minutes_before,
                plan,
            } => MutationOperation::CreateEvent {
                title,
                notes: notes.unwrap_or_default(),
                start_at_utc: self.utc_start(&start)?,
                time_zone: self.time_zone.to_string(),
                duration_minutes: duration_minutes.unwrap_or(DEFAULT_EVENT_MINUTES),
                reminder_minutes_before,
                plan,
            },
            DraftOperation::UpdateEvent {
                event_id,
                title,
                notes,
                duration_minutes,
                reminder_change,
            } => MutationOperation::UpdateEvent {
                expected_revision: self.event(&event_id)?.revision,
                event_id,
                title,
                notes,
                duration_minutes,
                reminder_change: reminder_change.unwrap_or_default(),
            },
            DraftOperation::RescheduleEvent {
                event_id,
                start,
                title,
                notes,
                duration_minutes,
                reminder_change,
            } => MutationOperation::RescheduleEvent {
                expected_revision: self.event(&event_id)?.revision,
                event_id,
                title,
                notes,
                start_at_utc: self.utc_start(&start)?,
                time_zone: self.time_zone.to_string(),
                duration_minutes,
                reminder_change: reminder_change.unwrap_or_default(),
            },
            DraftOperation::DeleteEvent { event_id } => MutationOperation::DeleteEvent {
                expected_revision: self.event(&event_id)?.revision,
                event_id,
            },
            DraftOperation::SetEventPlan { event_id, plan } => MutationOperation::SetEventPlan {
                expected_revision: self.event(&event_id)?.revision,
                event_id,
                plan,
            },
            DraftOperation::CreatePlan {
                title,
                description,
                status,
                start_date,
                target_date,
            } => MutationOperation::CreatePlan {
                title,
                description: description.unwrap_or_default(),
                status: status.unwrap_or_default(),
                start_date,
                target_date,
            },
            DraftOperation::UpdatePlan {
                plan_id,
                title,
                description,
                status,
                start_date,
                target_date,
            } => MutationOperation::UpdatePlan {
                expected_revision: self.plan(&plan_id)?.revision,
                plan_id,
                title,
                description,
                status,
                start_date,
                target_date,
            },
            DraftOperation::CreateMilestone {
                plan,
                title,
                description,
                target_date,
            } => MutationOperation::CreateMilestone {
                plan,
                title,
                description: description.unwrap_or_default(),
                target_date,
            },
            DraftOperation::UpdateMilestone {
                milestone_id,
                title,
                description,
                target_date,
                status,
            } => MutationOperation::UpdateMilestone {
                expected_revision: self.milestone(&milestone_id)?.revision,
                milestone_id,
                title,
                description,
                target_date,
                status,
            },
            DraftOperation::DeleteMilestone { milestone_id } => {
                MutationOperation::DeleteMilestone {
                    expected_revision: self.milestone(&milestone_id)?.revision,
                    milestone_id,
                }
            }
            DraftOperation::CreateTask {
                title,
                description,
                plan,
                milestone,
                do_on,
                due_by,
                status,
                priority,
            } => MutationOperation::CreateTask {
                title,
                description: description.unwrap_or_default(),
                plan,
                milestone,
                scheduled_day: do_on,
                due_date: due_by,
                status: status.unwrap_or_default(),
                priority: priority.unwrap_or_default(),
            },
            DraftOperation::UpdateTask {
                task_id,
                title,
                description,
                status,
                priority,
            } => MutationOperation::UpdateTask {
                expected_revision: self.task(&task_id)?.revision,
                task_id,
                title,
                description,
                status,
                priority,
            },
            DraftOperation::ScheduleTask {
                task_id,
                do_on,
                due_by,
            } => MutationOperation::ScheduleTask {
                expected_revision: self.task(&task_id)?.revision,
                task_id,
                scheduled_day: do_on.unwrap_or_default(),
                due_date: due_by.unwrap_or_default(),
            },
            DraftOperation::SetTaskPlan {
                task_id,
                plan,
                milestone,
            } => MutationOperation::SetTaskPlan {
                expected_revision: self.task(&task_id)?.revision,
                task_id,
                plan,
                milestone,
            },
            DraftOperation::DeleteTask { task_id } => MutationOperation::DeleteTask {
                expected_revision: self.task(&task_id)?.revision,
                task_id,
            },
        })
    }

    /// The existing records a proposal touches or links to, with titles for the preview.
    fn references(&self, operations: &[MutationOperation]) -> Vec<ProposalReference> {
        let mut seen = HashSet::new();
        let mut references = Vec::new();
        let mut add = |kind: RecordKind, id: &str| {
            if seen.insert(id.to_string()) {
                references.push(ProposalReference {
                    id: id.to_string(),
                    kind,
                    title: self.title_of(id),
                });
            }
        };
        for operation in operations {
            let (target, plan, milestone) = match operation {
                MutationOperation::CreateEvent { plan, .. } => (None, plan.as_ref(), None),
                MutationOperation::UpdateEvent { event_id, .. }
                | MutationOperation::RescheduleEvent { event_id, .. }
                | MutationOperation::DeleteEvent { event_id, .. } => {
                    (Some((RecordKind::Event, event_id)), None, None)
                }
                MutationOperation::SetEventPlan { event_id, plan, .. } => {
                    (Some((RecordKind::Event, event_id)), plan.as_ref(), None)
                }
                MutationOperation::CreatePlan { .. } => (None, None, None),
                MutationOperation::UpdatePlan { plan_id, .. } => {
                    (Some((RecordKind::Plan, plan_id)), None, None)
                }
                MutationOperation::CreateMilestone { plan, .. } => (None, Some(plan), None),
                MutationOperation::UpdateMilestone { milestone_id, .. }
                | MutationOperation::DeleteMilestone { milestone_id, .. } => {
                    (Some((RecordKind::Milestone, milestone_id)), None, None)
                }
                MutationOperation::CreateTask {
                    plan, milestone, ..
                } => (None, plan.as_ref(), milestone.as_ref()),
                MutationOperation::UpdateTask { task_id, .. }
                | MutationOperation::ScheduleTask { task_id, .. }
                | MutationOperation::DeleteTask { task_id, .. } => {
                    (Some((RecordKind::Task, task_id)), None, None)
                }
                MutationOperation::SetTaskPlan {
                    task_id,
                    plan,
                    milestone,
                    ..
                } => (
                    Some((RecordKind::Task, task_id)),
                    plan.as_ref(),
                    milestone.as_ref(),
                ),
            };
            if let Some((kind, id)) = target {
                add(kind, id);
            }
            if let Some(RecordRef::Existing(existing)) = plan {
                add(RecordKind::Plan, &existing.id);
            }
            if let Some(RecordRef::Existing(existing)) = milestone {
                add(RecordKind::Milestone, &existing.id);
            }
        }
        references
    }

    fn utc_start(&self, local: &str) -> Step<String> {
        let unclear = "Which date and time did you mean?";
        let Some((day, time)) = local.split_once('T') else {
            return ask(unclear);
        };
        let resolution = PlannerDatabase::resolve_local_datetime(&LocalDateTimeInput {
            day: day.into(),
            time: time.into(),
            time_zone: self.time_zone.into(),
        });
        let moment = || {
            let date = NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .map(|date| date.format("%A, %B %-d").to_string())
                .unwrap_or_else(|_| day.to_string());
            let clock = NaiveTime::parse_from_str(time, "%H:%M")
                .map(|clock| clock.format("%-I:%M %p").to_string())
                .unwrap_or_else(|_| time.to_string());
            (date, clock)
        };
        match resolution {
            Ok(LocalDateTimeResolution::Resolved { start_at_utc }) => Ok(start_at_utc),
            Ok(LocalDateTimeResolution::Ambiguous { .. }) => {
                let (date, clock) = moment();
                ask(format!(
                    "{clock} happens twice on {date} because the clocks change. Should I use the earlier or later one?"
                ))
            }
            Ok(LocalDateTimeResolution::Nonexistent { .. }) => {
                let (date, clock) = moment();
                ask(format!(
                    "{clock} does not exist on {date} because the clocks move forward. What time should I use?"
                ))
            }
            Err(AppError::Validation(_)) => ask(unclear),
            Err(error) => Err(Stop::Invalid(error)),
        }
    }

    fn check_new_reminder(&self, minutes: i64, start_at_utc: &str) -> Step<i64> {
        if !(0..=MAX_REMINDER_MINUTES).contains(&minutes) {
            return ask(
                "Reminders can be at the start time or up to 7 days before. When should it remind you?",
            );
        }
        let start = DateTime::parse_from_rfc3339(start_at_utc)
            .map_err(|_| invalid("an event start time could not be read"))?
            .with_timezone(&Utc);
        if start - Duration::minutes(minutes) <= self.now {
            return ask(
                "That reminder time has already passed. Should I use a shorter reminder or none?",
            );
        }
        Ok(minutes)
    }

    fn check_plan_reference(&self, reference: Option<&RecordRef>) -> Step<()> {
        match reference {
            Some(RecordRef::Existing(existing)) => self.plan(&existing.id).map(|_| ()),
            Some(RecordRef::New(new)) => clean_title(&new.new_title).map(|_| ()),
            None => Ok(()),
        }
    }

    fn check_milestone_reference(&self, reference: Option<&RecordRef>) -> Step<()> {
        match reference {
            Some(RecordRef::Existing(existing)) => self.milestone(&existing.id).map(|_| ()),
            Some(RecordRef::New(new)) => clean_title(&new.new_title).map(|_| ()),
            None => Ok(()),
        }
    }

    fn event(&self, id: &str) -> Step<&ScheduleEvent> {
        self.candidates
            .events
            .iter()
            .find(|event| event.id == id)
            .ok_or_else(|| invalid("an operation referenced an event outside the request context"))
    }

    fn plan(&self, id: &str) -> Step<&Plan> {
        self.candidates
            .plans
            .iter()
            .find(|plan| plan.id == id)
            .ok_or_else(|| invalid("an operation referenced a plan outside the request context"))
    }

    fn milestone(&self, id: &str) -> Step<&Milestone> {
        self.candidates
            .milestones
            .iter()
            .find(|milestone| milestone.id == id)
            .ok_or_else(|| {
                invalid("an operation referenced a milestone outside the request context")
            })
    }

    fn task(&self, id: &str) -> Step<&Task> {
        self.candidates
            .tasks
            .iter()
            .find(|task| task.id == id)
            .ok_or_else(|| invalid("an operation referenced a task outside the request context"))
    }

    fn plan_title(&self, id: &str) -> String {
        self.title_of(id)
    }

    fn title_of(&self, id: &str) -> String {
        let candidates = self.candidates;
        candidates
            .events
            .iter()
            .find(|event| event.id == id)
            .map(|event| event.title.clone())
            .or_else(|| {
                candidates
                    .plans
                    .iter()
                    .find(|plan| plan.id == id)
                    .map(|plan| plan.title.clone())
            })
            .or_else(|| {
                candidates
                    .milestones
                    .iter()
                    .find(|milestone| milestone.id == id)
                    .map(|milestone| milestone.title.clone())
            })
            .or_else(|| {
                candidates
                    .tasks
                    .iter()
                    .find(|task| task.id == id)
                    .map(|task| task.title.clone())
            })
            .unwrap_or_else(|| "that item".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Touch {
    Edit,
    Schedule,
    Link,
    Delete,
}

/// A plan a milestone or task points at: an existing ID or a title created in the proposal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PlanKey {
    Existing(String),
    New(String),
}

impl From<&RecordRef> for PlanKey {
    fn from(reference: &RecordRef) -> Self {
        match reference {
            RecordRef::Existing(existing) => Self::Existing(existing.id.clone()),
            RecordRef::New(new) => Self::New(fold(&new.new_title)),
        }
    }
}

impl PlanKey {
    fn reference(&self) -> RecordRef {
        match self {
            Self::Existing(id) => RecordRef::existing(id.clone()),
            Self::New(title) => RecordRef::new_title(title.clone()),
        }
    }
}

struct EventEdit {
    title: Option<String>,
    notes: Option<String>,
    duration_minutes: Option<i64>,
    reminder_change: Option<ReminderChange>,
}

impl EventEdit {
    fn into_update(self, event_id: String) -> Option<DraftOperation> {
        let unchanged = self.title.is_none()
            && self.notes.is_none()
            && self.duration_minutes.is_none()
            && self.reminder_change.is_none();
        (!unchanged).then_some(DraftOperation::UpdateEvent {
            event_id,
            title: self.title,
            notes: self.notes,
            duration_minutes: self.duration_minutes,
            reminder_change: self.reminder_change,
        })
    }
}

enum Carried {
    Merged(Vec<DraftOperation>),
    Unchanged(Vec<DraftOperation>),
}

/// Merges follow-up operations into the pending ones about the same subject, keeping the rest of
/// the pending proposal.
fn merge_into(pending: &[DraftOperation], new: Vec<DraftOperation>) -> Vec<DraftOperation> {
    let mut merged = pending.to_vec();
    for operation in new {
        match merged
            .iter()
            .position(|existing| same_subject(existing, &operation))
        {
            Some(index) => {
                let existing = merged.remove(index);
                merged.insert(index, combine(existing, operation));
            }
            None => merged.push(operation),
        }
    }
    merged
}

/// Applies an edit the model aimed at an existing record to the one new task or event a pending
/// proposal would create. Returns `false` when the edit does not fit that shape.
fn edit_pending_create(merged: &mut [DraftOperation], operation: &DraftOperation) -> bool {
    let creates_task = |draft: &DraftOperation| matches!(draft, DraftOperation::CreateTask { .. });
    let creates_event =
        |draft: &DraftOperation| matches!(draft, DraftOperation::CreateEvent { .. });
    let task_index = match merged.iter().filter(|draft| creates_task(draft)).count() {
        1 => merged.iter().position(creates_task),
        _ => None,
    };
    let event_index = match merged.iter().filter(|draft| creates_event(draft)).count() {
        1 => merged.iter().position(creates_event),
        _ => None,
    };
    match operation {
        DraftOperation::UpdateTask {
            title,
            description,
            status,
            priority,
            ..
        } => {
            let Some(DraftOperation::CreateTask {
                title: new_title,
                description: new_description,
                status: new_status,
                priority: new_priority,
                ..
            }) = task_index.map(|index| &mut merged[index])
            else {
                return false;
            };
            if let Some(title) = title {
                *new_title = title.clone();
            }
            if description.is_some() {
                *new_description = description.clone();
            }
            *new_status = status.or(*new_status);
            *new_priority = priority.or(*new_priority);
            true
        }
        DraftOperation::ScheduleTask { do_on, due_by, .. } => {
            let Some(DraftOperation::CreateTask {
                do_on: new_do_on,
                due_by: new_due_by,
                ..
            }) = task_index.map(|index| &mut merged[index])
            else {
                return false;
            };
            apply_day(do_on, new_do_on);
            apply_day(due_by, new_due_by);
            true
        }
        DraftOperation::UpdateEvent {
            title,
            notes,
            duration_minutes,
            reminder_change,
            ..
        }
        | DraftOperation::RescheduleEvent {
            title,
            notes,
            duration_minutes,
            reminder_change,
            ..
        } => {
            let Some(DraftOperation::CreateEvent {
                title: new_title,
                start: new_start,
                notes: new_notes,
                duration_minutes: new_duration,
                reminder_minutes_before: new_reminder,
                ..
            }) = event_index.map(|index| &mut merged[index])
            else {
                return false;
            };
            if let DraftOperation::RescheduleEvent { start, .. } = operation {
                *new_start = start.clone();
            }
            if let Some(title) = title {
                *new_title = title.clone();
            }
            if notes.is_some() {
                *new_notes = notes.clone();
            }
            *new_duration = duration_minutes.or(*new_duration);
            match reminder_change {
                Some(ReminderChange::Set { minutes_before }) => {
                    *new_reminder = Some(*minutes_before)
                }
                Some(ReminderChange::Clear) => *new_reminder = None,
                None | Some(ReminderChange::Unchanged) => {}
            }
            true
        }
        _ => false,
    }
}

fn apply_day(change: &Option<DayChange>, day: &mut Option<String>) {
    match change {
        Some(DayChange::Set { day: new_day }) => *day = Some(new_day.clone()),
        Some(DayChange::Clear) => *day = None,
        None | Some(DayChange::Unchanged) => {}
    }
}

/// What an operation is about: a record and the aspect it changes, or a new record's title.
fn subject(operation: &DraftOperation) -> (&'static str, String) {
    match operation {
        DraftOperation::UpdateEvent { event_id, .. }
        | DraftOperation::RescheduleEvent { event_id, .. }
        | DraftOperation::DeleteEvent { event_id } => ("event", event_id.clone()),
        DraftOperation::SetEventPlan { event_id, .. } => ("event-plan", event_id.clone()),
        DraftOperation::CreateEvent { title, .. } => ("new-event", fold(title)),
        DraftOperation::CreatePlan { title, .. } => ("new-plan", fold(title)),
        DraftOperation::UpdatePlan { plan_id, .. } => ("plan", plan_id.clone()),
        DraftOperation::CreateMilestone { title, .. } => ("new-milestone", fold(title)),
        DraftOperation::UpdateMilestone { milestone_id, .. }
        | DraftOperation::DeleteMilestone { milestone_id } => ("milestone", milestone_id.clone()),
        DraftOperation::CreateTask { title, .. } => ("new-task", fold(title)),
        DraftOperation::UpdateTask { task_id, .. } => ("task", task_id.clone()),
        DraftOperation::ScheduleTask { task_id, .. } => ("task-days", task_id.clone()),
        DraftOperation::SetTaskPlan { task_id, .. } => ("task-plan", task_id.clone()),
        DraftOperation::DeleteTask { task_id } => ("task-delete", task_id.clone()),
    }
}

fn same_subject(left: &DraftOperation, right: &DraftOperation) -> bool {
    let (left_kind, left_id) = subject(left);
    let (right_kind, right_id) = subject(right);
    left_id == right_id
        && (left_kind == right_kind
            || (left_kind.starts_with("task") && right_kind == "task-delete")
            || (left_kind == "task-delete" && right_kind.starts_with("task")))
}

/// Merges a follow-up operation into the pending one about the same subject; new values win.
fn combine(pending: DraftOperation, new: DraftOperation) -> DraftOperation {
    use DraftOperation as Op;
    match (pending, new) {
        (
            Op::RescheduleEvent {
                start,
                title,
                notes,
                duration_minutes,
                reminder_change,
                ..
            },
            Op::UpdateEvent {
                event_id,
                title: new_title,
                notes: new_notes,
                duration_minutes: new_duration,
                reminder_change: new_reminder,
            },
        ) => Op::RescheduleEvent {
            event_id,
            start,
            title: new_title.or(title),
            notes: new_notes.or(notes),
            duration_minutes: new_duration.or(duration_minutes),
            reminder_change: new_reminder.or(reminder_change),
        },
        (
            Op::UpdateEvent {
                title,
                notes,
                duration_minutes,
                reminder_change,
                ..
            }
            | Op::RescheduleEvent {
                title,
                notes,
                duration_minutes,
                reminder_change,
                ..
            },
            Op::RescheduleEvent {
                event_id,
                start,
                title: new_title,
                notes: new_notes,
                duration_minutes: new_duration,
                reminder_change: new_reminder,
            },
        ) => Op::RescheduleEvent {
            event_id,
            start,
            title: new_title.or(title),
            notes: new_notes.or(notes),
            duration_minutes: new_duration.or(duration_minutes),
            reminder_change: new_reminder.or(reminder_change),
        },
        (
            Op::UpdateEvent {
                title,
                notes,
                duration_minutes,
                reminder_change,
                ..
            },
            Op::UpdateEvent {
                event_id,
                title: new_title,
                notes: new_notes,
                duration_minutes: new_duration,
                reminder_change: new_reminder,
            },
        ) => Op::UpdateEvent {
            event_id,
            title: new_title.or(title),
            notes: new_notes.or(notes),
            duration_minutes: new_duration.or(duration_minutes),
            reminder_change: new_reminder.or(reminder_change),
        },
        (
            Op::CreateEvent {
                notes,
                duration_minutes,
                reminder_minutes_before,
                plan,
                ..
            },
            Op::CreateEvent {
                title,
                start,
                notes: new_notes,
                duration_minutes: new_duration,
                reminder_minutes_before: new_reminder,
                plan: new_plan,
            },
        ) => Op::CreateEvent {
            title,
            start,
            notes: new_notes.or(notes),
            duration_minutes: new_duration.or(duration_minutes),
            reminder_minutes_before: new_reminder.or(reminder_minutes_before),
            plan: new_plan.or(plan),
        },
        (
            Op::CreatePlan {
                description,
                status,
                start_date,
                target_date,
                ..
            },
            Op::CreatePlan {
                title,
                description: new_description,
                status: new_status,
                start_date: new_start,
                target_date: new_target,
            },
        ) => Op::CreatePlan {
            title,
            description: new_description.or(description),
            status: new_status.or(status),
            start_date: new_start.or(start_date),
            target_date: new_target.or(target_date),
        },
        (
            Op::CreateMilestone {
                description,
                target_date,
                ..
            },
            Op::CreateMilestone {
                plan,
                title,
                description: new_description,
                target_date: new_target,
            },
        ) => Op::CreateMilestone {
            plan,
            title,
            description: new_description.or(description),
            target_date: new_target.or(target_date),
        },
        (
            Op::CreateTask {
                description,
                plan,
                milestone,
                do_on,
                due_by,
                status,
                priority,
                ..
            },
            Op::CreateTask {
                title,
                description: new_description,
                plan: new_plan,
                milestone: new_milestone,
                do_on: new_do_on,
                due_by: new_due_by,
                status: new_status,
                priority: new_priority,
            },
        ) => Op::CreateTask {
            title,
            description: new_description.or(description),
            plan: new_plan.or(plan),
            milestone: new_milestone.or(milestone),
            do_on: new_do_on.or(do_on),
            due_by: new_due_by.or(due_by),
            status: new_status.or(status),
            priority: new_priority.or(priority),
        },
        (
            Op::UpdateTask {
                title,
                description,
                status,
                priority,
                ..
            },
            Op::UpdateTask {
                task_id,
                title: new_title,
                description: new_description,
                status: new_status,
                priority: new_priority,
            },
        ) => Op::UpdateTask {
            task_id,
            title: new_title.or(title),
            description: new_description.or(description),
            status: new_status.or(status),
            priority: new_priority.or(priority),
        },
        (
            Op::ScheduleTask { do_on, due_by, .. },
            Op::ScheduleTask {
                task_id,
                do_on: new_do_on,
                due_by: new_due_by,
            },
        ) => Op::ScheduleTask {
            task_id,
            do_on: new_do_on.or(do_on),
            due_by: new_due_by.or(due_by),
        },
        (
            Op::UpdatePlan {
                title,
                description,
                status,
                start_date,
                target_date,
                ..
            },
            Op::UpdatePlan {
                plan_id,
                title: new_title,
                description: new_description,
                status: new_status,
                start_date: new_start,
                target_date: new_target,
            },
        ) => Op::UpdatePlan {
            plan_id,
            title: new_title.or(title),
            description: new_description.or(description),
            status: new_status.or(status),
            start_date: new_start.or(start_date),
            target_date: new_target.or(target_date),
        },
        (
            Op::UpdateMilestone {
                title,
                description,
                target_date,
                status,
                ..
            },
            Op::UpdateMilestone {
                milestone_id,
                title: new_title,
                description: new_description,
                target_date: new_target,
                status: new_status,
            },
        ) => Op::UpdateMilestone {
            milestone_id,
            title: new_title.or(title),
            description: new_description.or(description),
            target_date: new_target.or(target_date),
            status: new_status.or(status),
        },
        (_, new) => new,
    }
}

fn is_empty_edit(operation: &DraftOperation) -> bool {
    match operation {
        DraftOperation::UpdatePlan {
            title,
            description,
            status,
            start_date,
            target_date,
            ..
        } => {
            title.is_none()
                && description.is_none()
                && status.is_none()
                && start_date.is_none()
                && target_date.is_none()
        }
        DraftOperation::UpdateMilestone {
            title,
            description,
            target_date,
            status,
            ..
        } => title.is_none() && description.is_none() && target_date.is_none() && status.is_none(),
        DraftOperation::ScheduleTask { do_on, due_by, .. } => do_on.is_none() && due_by.is_none(),
        _ => false,
    }
}

/// Folds an `update_event` into a `reschedule_event` for the same event when their fields agree,
/// so "move it and rename it" becomes one reviewable change.
fn merge_event_edits(mut drafts: Vec<DraftOperation>) -> Vec<DraftOperation> {
    let mut index = 0;
    while index < drafts.len() {
        let DraftOperation::UpdateEvent {
            event_id,
            title,
            notes,
            duration_minutes,
            reminder_change,
        } = &drafts[index]
        else {
            index += 1;
            continue;
        };
        let (event_id, title, notes, duration_minutes, reminder_change) = (
            event_id.clone(),
            title.clone(),
            notes.clone(),
            *duration_minutes,
            reminder_change.clone(),
        );
        let target = drafts.iter().position(|draft| {
            matches!(draft, DraftOperation::RescheduleEvent { event_id: id, .. } if *id == event_id)
        });
        let Some(target) = target else {
            index += 1;
            continue;
        };
        let DraftOperation::RescheduleEvent {
            title: move_title,
            notes: move_notes,
            duration_minutes: move_duration,
            reminder_change: move_reminder,
            ..
        } = &mut drafts[target]
        else {
            unreachable!("the target was matched as a reschedule");
        };
        let compatible = agrees(move_title, &title)
            && agrees(move_notes, &notes)
            && agrees(move_duration, &duration_minutes)
            && agrees(move_reminder, &reminder_change);
        if !compatible {
            index += 1;
            continue;
        }
        if move_title.is_none() {
            *move_title = title;
        }
        if move_notes.is_none() {
            *move_notes = notes;
        }
        if move_duration.is_none() {
            *move_duration = duration_minutes;
        }
        if move_reminder.is_none() {
            *move_reminder = reminder_change;
        }
        drafts.remove(index);
    }
    drafts
}

/// Notes without a trailing " to <event title>" copied from a request like "add notes … to X".
fn without_target(notes: String, event_title: &str) -> String {
    if !notes.is_ascii() || !event_title.is_ascii() {
        return notes;
    }
    let lower = notes.to_ascii_lowercase();
    let title = event_title.to_ascii_lowercase();
    [" to ", " for ", " on "]
        .iter()
        .find_map(|joiner| {
            let suffix = format!("{joiner}{title}");
            lower
                .ends_with(&suffix)
                .then(|| notes[..notes.len() - suffix.len()].trim_end().to_string())
        })
        .filter(|trimmed| !trimmed.is_empty())
        .unwrap_or(notes)
}

fn agrees<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn same_link(reference: Option<&RecordRef>, current: Option<&str>) -> bool {
    match reference {
        None => current.is_none(),
        Some(RecordRef::Existing(existing)) => current == Some(existing.id.as_str()),
        Some(RecordRef::New(_)) => false,
    }
}

fn next_has_day(change: Option<&DayChange>, current: bool) -> bool {
    match change {
        None | Some(DayChange::Unchanged) => current,
        Some(DayChange::Clear) => false,
        Some(DayChange::Set { .. }) => true,
    }
}

fn check_day(value: &str) -> Step<String> {
    match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        Ok(date) => Ok(date.format("%Y-%m-%d").to_string()),
        Err(_) => ask(format!(
            "{value} is not a real date. Which day did you mean?"
        )),
    }
}

fn check_duration(minutes: i64) -> Step<i64> {
    if (5..=1440).contains(&minutes) {
        Ok(minutes)
    } else {
        ask("Events can last from 5 minutes to 24 hours. How long should it be?")
    }
}

fn check_date_order(title: &str, start: Option<&str>, target: Option<&str>) -> Step<()> {
    match (start, target) {
        (Some(start), Some(target)) if start > target => ask(format!(
            "The start date for “{title}” would be after its target date. Which dates should it use?"
        )),
        _ => Ok(()),
    }
}

fn changed_title(value: Option<String>, current: &str) -> Step<Option<String>> {
    value
        .map(|title| clean_title(&title))
        .transpose()
        .map(|title| title.filter(|title| title != current))
}

fn changed_description(value: Option<String>, current: &str) -> Step<Option<String>> {
    value
        .map(|description| clean_description(&description))
        .transpose()
        .map(|description| description.filter(|description| description != current))
}

fn changed_date(value: Option<String>, current: Option<&str>) -> Step<Option<String>> {
    value
        .map(|day| check_day(&day))
        .transpose()
        .map(|day| day.filter(|day| Some(day.as_str()) != current))
}

fn changed_day(change: Option<DayChange>, current: Option<&str>) -> Step<Option<DayChange>> {
    Ok(match change {
        None | Some(DayChange::Unchanged) => None,
        Some(DayChange::Clear) => current.is_some().then_some(DayChange::Clear),
        Some(DayChange::Set { day }) => {
            let day = check_day(&day)?;
            (current != Some(day.as_str())).then_some(DayChange::Set { day })
        }
    })
}

fn clean_title(value: &str) -> Step<String> {
    let title = sentence_start(value.trim());
    if title.is_empty() {
        return ask("What should it be called?");
    }
    if title.chars().count() > MAX_TITLE_LENGTH {
        return ask("That title is longer than 140 characters. What shorter title should I use?");
    }
    Ok(title)
}

fn clean_notes(value: &str) -> Step<String> {
    let notes = sentence_start(value.trim());
    if notes.chars().count() > MAX_NOTES_LENGTH {
        return ask("Those notes are longer than 800 characters. Can you shorten them?");
    }
    Ok(notes)
}

fn clean_description(value: &str) -> Step<String> {
    let description = sentence_start(value.trim());
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return ask("That description is longer than 2,000 characters. Can you shorten it?");
    }
    Ok(description)
}

fn clean_optional_description(value: Option<String>) -> Step<Option<String>> {
    value
        .map(|description| clean_description(&description))
        .transpose()
        .map(|description| description.filter(|description| !description.is_empty()))
}

/// Capitalizes the first letter when the first word is plain lowercase, so "dentist" becomes
/// "Dentist" while names like "iPhone repair" keep their casing.
fn sentence_start(value: &str) -> String {
    let first_word = value.split_whitespace().next().unwrap_or_default();
    let plain_lowercase = first_word
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && first_word
            .chars()
            .all(|character| character.is_ascii_lowercase() || matches!(character, '\'' | '-'));
    if !plain_lowercase {
        return value.to_string();
    }
    let mut characters = value.chars();
    characters
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
        .unwrap_or_default()
}

fn summary_text(value: &str) -> String {
    let summary = value.trim();
    if summary.is_empty() {
        return "Review these changes.".into();
    }
    if summary.chars().count() <= SUMMARY_LIMIT {
        return summary.to_string();
    }
    let mut shortened = summary.chars().take(SUMMARY_LIMIT - 1).collect::<String>();
    shortened.push('…');
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReminderStatus;
    use serde_json::json;

    const EVENT: &str = "30bb9c6a-4020-45a6-806b-5eb71c7ae76f";
    const PLAN: &str = "5d5a3e1c-2d67-4a7e-9d3c-6c9a1f0d8e21";
    const OTHER_PLAN: &str = "8e0c3f4b-1a2d-4f5e-8b7c-9d0e1f2a3b4c";
    const MILESTONE: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
    const TASK: &str = "f0e1d2c3-b4a5-4968-8776-655443322110";

    fn candidates() -> PlannerCandidates {
        let stamp = "2026-09-01T10:00:00.000Z".to_string();
        PlannerCandidates {
            events: vec![ScheduleEvent {
                id: EVENT.into(),
                title: "Client meeting".into(),
                notes: "Bring samples".into(),
                start_at_utc: "2030-05-10T18:00:00.000Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 60,
                reminder_minutes_before: Some(15),
                reminder_status: ReminderStatus::Scheduled,
                plan_id: None,
                location: String::new(),
                workstream_id: None,
                owner_id: None,
                revision: 3,
                created_at: stamp.clone(),
                updated_at: stamp.clone(),
            }],
            plans: [
                (PLAN, "Streamer Charity Week"),
                (OTHER_PLAN, "Home renovation"),
            ]
            .into_iter()
            .map(|(id, title)| Plan {
                id: id.into(),
                title: title.into(),
                description: String::new(),
                status: PlanStatus::Active,
                start_date: None,
                target_date: Some("2026-10-16".into()),
                color: None,
                archived: false,
                revision: 2,
                created_at: stamp.clone(),
                updated_at: stamp.clone(),
            })
            .collect(),
            milestones: vec![Milestone {
                id: MILESTONE.into(),
                plan_id: PLAN.into(),
                title: "Venue locked".into(),
                description: String::new(),
                target_date: Some("2026-10-05".into()),
                status: MilestoneStatus::Pending,
                workstream_id: None,
                sort_order: 0,
                revision: 1,
                created_at: stamp.clone(),
                updated_at: stamp.clone(),
            }],
            tasks: vec![Task {
                id: TASK.into(),
                title: "Pick paint colors".into(),
                description: String::new(),
                plan_id: Some(OTHER_PLAN.into()),
                milestone_id: None,
                workstream_id: None,
                owner_id: None,
                due_date: None,
                scheduled_day: None,
                status: TaskStatus::Todo,
                priority: TaskPriority::Normal,
                completed_at: None,
                sort_order: 0,
                revision: 4,
                created_at: stamp.clone(),
                updated_at: stamp,
            }],
        }
    }

    fn resolve(command: &str, reply: serde_json::Value) -> Resolution {
        let candidates = candidates();
        let draft = serde_json::from_value::<Draft>(reply).expect("valid draft");
        Resolver {
            command,
            said: command,
            time_zone: "America/New_York",
            candidates: &candidates,
            active_plan_id: None,
            session_ids: &[],
            pending: &[],
            recent_ids: &[],
            now: "2026-09-14T12:00:00Z".parse().unwrap(),
        }
        .resolve(draft)
        .expect("no contract violation")
    }

    fn proposal(command: &str, operations: serde_json::Value) -> ResolvedProposal {
        match resolve(
            command,
            json!({ "kind": "proposal", "summary": "Change it", "operations": operations }),
        ) {
            Resolution::Proposal(proposal) => proposal,
            Resolution::Clarification(question) => panic!("unexpected question: {question}"),
        }
    }

    fn question(command: &str, operations: serde_json::Value) -> String {
        match resolve(
            command,
            json!({ "kind": "proposal", "summary": "Change it", "operations": operations }),
        ) {
            Resolution::Clarification(question) => question,
            Resolution::Proposal(proposal) => panic!("unexpected proposal: {proposal:?}"),
        }
    }

    #[test]
    fn local_times_become_utc_and_clock_changes_need_an_answer() {
        let created = proposal(
            "add dentist tomorrow at 2 pm",
            json!([{ "type": "create_event", "title": "dentist", "start": "2026-08-13T14:00" }]),
        );
        assert_eq!(
            created.operations,
            [MutationOperation::CreateEvent {
                title: "Dentist".into(),
                notes: String::new(),
                start_at_utc: "2026-08-13T18:00:00.000Z".into(),
                time_zone: "America/New_York".into(),
                duration_minutes: 60,
                reminder_minutes_before: None,
                plan: None,
            }]
        );
        assert!(question(
            "add maintenance at 1:30 am",
            json!([{ "type": "create_event", "title": "Maintenance", "start": "2026-11-01T01:30" }]),
        )
        .contains("happens twice"));
        assert!(question(
            "add check at 2:30 am",
            json!([{ "type": "create_event", "title": "Check", "start": "2026-03-08T02:30" }]),
        )
        .contains("does not exist"));
        assert!(question(
            "add a break for 2 minutes",
            json!([{ "type": "create_event", "title": "Break", "start": "2026-08-13T09:00", "durationMinutes": 2 }]),
        )
        .contains("5 minutes"));
    }

    #[test]
    fn unchanged_values_are_dropped_and_a_proposal_that_changes_nothing_asks() {
        let renamed = proposal(
            "rename client meeting to Client review",
            json!([{
                "type": "update_event", "eventId": EVENT, "title": "Client review",
                "notes": "", "durationMinutes": 60, "reminderChange": { "action": "set", "minutesBefore": 15 }
            }]),
        );
        assert_eq!(
            renamed.operations,
            [MutationOperation::UpdateEvent {
                event_id: EVENT.into(),
                expected_revision: 3,
                title: Some("Client review".into()),
                notes: None,
                duration_minutes: None,
                reminder_change: ReminderChange::Unchanged,
            }]
        );
        assert_eq!(renamed.references[0].title, "Client meeting");
        let cleared = proposal(
            "remove the notes from client meeting",
            json!([{ "type": "update_event", "eventId": EVENT, "notes": "" }]),
        );
        assert!(matches!(
            &cleared.operations[0],
            MutationOperation::UpdateEvent { notes: Some(notes), .. } if notes.is_empty()
        ));
        question(
            "leave the client meeting where it is",
            json!([{ "type": "reschedule_event", "eventId": EVENT, "start": "2030-05-10T14:00" }]),
        );
        question(
            "paint colors are still todo",
            json!([{ "type": "update_task", "taskId": TASK, "status": "todo" }]),
        );
    }

    #[test]
    fn a_move_and_rename_of_one_event_merge_and_contradictions_ask() {
        let merged = proposal(
            "move client meeting to 3 pm and rename it Client review",
            json!([
                { "type": "reschedule_event", "eventId": EVENT, "start": "2030-05-10T15:00" },
                { "type": "update_event", "eventId": EVENT, "title": "Client review" }
            ]),
        );
        assert!(matches!(
            merged.operations.as_slice(),
            [MutationOperation::RescheduleEvent { title: Some(title), .. }] if title == "Client review"
        ));
        assert!(question(
            "move client meeting to 3 pm and delete it",
            json!([
                { "type": "reschedule_event", "eventId": EVENT, "start": "2030-05-10T15:00" },
                { "type": "delete_event", "eventId": EVENT }
            ]),
        )
        .contains("or delete it"));
    }

    #[test]
    fn plan_titles_resolve_to_new_or_existing_plans_or_ask() {
        let compound = proposal(
            "start a bake sale plan with a flour task and a call",
            json!([
                { "type": "create_plan", "title": "Bake sale", "targetDate": "2026-10-03" },
                { "type": "create_task", "title": "Buy flour", "plan": { "newTitle": "bake sale" }, "dueBy": "2026-10-01" },
                { "type": "create_event", "title": "Planning call", "start": "2026-09-18T15:00", "plan": { "newTitle": "Bake sale" } }
            ]),
        );
        assert_eq!(compound.operations.len(), 3);
        let repaired = proposal(
            "add order banners to the charity week plan by October 8",
            json!([{ "type": "create_task", "title": "Order banners", "plan": { "newTitle": "Streamer charity week" }, "dueBy": "2026-10-08" }]),
        );
        assert!(matches!(
            &repaired.operations[0],
            MutationOperation::CreateTask { plan: Some(RecordRef::Existing(existing)), .. } if existing.id == PLAN
        ));
        assert!(question(
            "add a task to the wedding plan",
            json!([{ "type": "create_task", "title": "Book band", "plan": { "newTitle": "Wedding" } }]),
        )
        .contains("couldn't find a plan"));
        assert!(question(
            "create a Home renovation plan",
            json!([{ "type": "create_plan", "title": "Home renovation" }]),
        )
        .contains("already have a plan"));
    }

    #[test]
    fn milestones_bring_their_plan_and_must_match_the_task_plan() {
        let inferred = proposal(
            "add confirm deposit to the venue locked milestone by Friday",
            json!([{ "type": "create_task", "title": "Confirm deposit", "milestone": { "id": MILESTONE }, "dueBy": "2026-09-18" }]),
        );
        assert!(matches!(
            &inferred.operations[0],
            MutationOperation::CreateTask { plan: Some(RecordRef::Existing(existing)), .. } if existing.id == PLAN
        ));
        assert!(question(
            "move paint colors to venue locked in home renovation",
            json!([{ "type": "set_task_plan", "taskId": TASK, "plan": { "id": OTHER_PLAN }, "milestone": { "id": MILESTONE } }]),
        )
        .contains("is a milestone in"));
        let by_title = proposal(
            "move paint colors to the venue locked milestone",
            json!([{ "type": "set_task_plan", "taskId": TASK, "plan": null, "milestone": { "newTitle": "Venue locked" } }]),
        );
        assert!(matches!(
            &by_title.operations[0],
            MutationOperation::SetTaskPlan {
                plan: Some(RecordRef::Existing(plan)),
                milestone: Some(RecordRef::Existing(milestone)),
                ..
            } if plan.id == PLAN && milestone.id == MILESTONE
        ));
    }

    #[test]
    fn tasks_keep_a_home() {
        assert!(question(
            "add call mom to my tasks",
            json!([{ "type": "create_task", "title": "call mom" }]),
        )
        .contains("Which day should “Call mom” go on"));
        assert!(question(
            "take paint colors out of its plan",
            json!([{ "type": "set_task_plan", "taskId": TASK, "plan": null }]),
        )
        .contains("needs a day"));
        let dated = proposal(
            "take paint colors out of its plan and do it tomorrow",
            json!([
                { "type": "set_task_plan", "taskId": TASK, "plan": null },
                { "type": "schedule_task", "taskId": TASK, "doOn": { "action": "set", "day": "2026-09-15" } }
            ]),
        );
        assert_eq!(dated.operations.len(), 2);
    }

    #[test]
    fn malformed_replies_break_the_contract() {
        for reply in [
            json!({ "kind": "proposal", "summary": "Hi", "operations": [], "untrusted": true }),
            json!({ "kind": "proposal", "summary": "Hi", "operations": [{ "type": "drop_table" }] }),
            json!({ "kind": "proposal", "summary": "Hi", "operations": [{ "type": "delete_task", "taskId": TASK, "expectedRevision": 1 }] }),
            json!({ "kind": "clarification", "question": "Which?", "operations": [] }),
        ] {
            assert!(serde_json::from_value::<Draft>(reply).is_err());
        }
        let candidates = candidates();
        let resolver = Resolver {
            command: "delete it",
            said: "delete it",
            time_zone: "America/New_York",
            candidates: &candidates,
            active_plan_id: None,
            session_ids: &[],
            pending: &[],
            recent_ids: &[],
            now: Utc::now(),
        };
        let unknown = serde_json::from_value::<Draft>(json!({
            "kind": "proposal", "summary": "Delete",
            "operations": [{ "type": "delete_event", "eventId": "11111111-1111-4111-8111-111111111111" }]
        }))
        .unwrap();
        assert!(matches!(
            resolver.resolve(unknown),
            Err(AppError::InvalidModelResponse(_))
        ));
    }

    fn resolve_with(
        command: &str,
        said: &str,
        pending: &[DraftOperation],
        recent_ids: &[String],
        reply: serde_json::Value,
    ) -> Resolution {
        let candidates = candidates();
        Resolver {
            command,
            said,
            time_zone: "America/New_York",
            candidates: &candidates,
            active_plan_id: None,
            session_ids: recent_ids,
            pending,
            recent_ids,
            now: "2026-09-14T12:00:00Z".parse().unwrap(),
        }
        .resolve(serde_json::from_value::<Draft>(reply).expect("valid draft"))
        .expect("no contract violation")
    }

    #[test]
    fn follow_ups_merge_into_the_pending_proposal() {
        let pending = [DraftOperation::RescheduleEvent {
            event_id: EVENT.into(),
            start: "2030-05-11T15:00".into(),
            title: None,
            notes: None,
            duration_minutes: None,
            reminder_change: None,
        }];
        let Resolution::Proposal(renamed) = resolve_with(
            "rename it to Client review",
            "move client meeting to 3 pm tomorrow\nrename it to Client review",
            &pending,
            &[],
            json!({ "kind": "proposal", "summary": "Rename", "operations": [
                { "type": "update_event", "eventId": EVENT, "title": "Client review" }
            ]}),
        ) else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            renamed.operations.as_slice(),
            [MutationOperation::RescheduleEvent { title: Some(title), start_at_utc, .. }]
                if title == "Client review" && start_at_utc == "2030-05-11T19:00:00.000Z"
        ));

        let new_task = [DraftOperation::CreateTask {
            title: "Print flyers".into(),
            description: None,
            plan: Some(RecordRef::existing(PLAN)),
            milestone: None,
            do_on: None,
            due_by: Some("2026-10-01".into()),
            status: None,
            priority: None,
        }];
        let Resolution::Proposal(redated) = resolve_with(
            "actually make it due September 30",
            "add a Print flyers task to charity week due October 1\nactually make it due September 30",
            &new_task,
            &[],
            json!({ "kind": "proposal", "summary": "Redate", "operations": [
                { "type": "schedule_task", "taskId": TASK, "dueBy": { "action": "set", "day": "2026-09-30" } }
            ]}),
        ) else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            redated.operations.as_slice(),
            [MutationOperation::CreateTask { title, due_date: Some(due), .. }]
                if title == "Print flyers" && due == "2026-09-30"
        ));
    }

    #[test]
    fn it_means_the_one_task_the_last_proposal_touched() {
        let other = Task {
            id: "0b0c0d0e-0f10-4112-8314-151617181920".into(),
            title: "Buy milk".into(),
            scheduled_day: Some("2026-09-15".into()),
            plan_id: None,
            ..candidates().tasks[0].clone()
        };
        let mut candidates = candidates();
        candidates.tasks.push(other.clone());
        let recent = [TASK.to_string()];
        let resolution = Resolver {
            command: "mark it in progress",
            said: "add a pick paint colors task\nmark it in progress",
            time_zone: "America/New_York",
            candidates: &candidates,
            active_plan_id: None,
            session_ids: &recent,
            pending: &[],
            recent_ids: &recent,
            now: Utc::now(),
        }
        .resolve(
            serde_json::from_value::<Draft>(
                json!({ "kind": "proposal", "summary": "Start", "operations": [
                    { "type": "update_task", "taskId": other.id, "status": "in_progress" }
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        let Resolution::Proposal(proposal) = resolution else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            proposal.operations.as_slice(),
            [MutationOperation::UpdateTask { task_id, .. }] if task_id == TASK
        ));
    }

    #[test]
    fn values_the_request_never_stated_are_dropped_or_questioned() {
        let Resolution::Proposal(day_task) = resolve_with(
            "add pick up badges to my tasks for Thursday",
            "add pick up badges to my tasks for Thursday",
            &[],
            &[],
            json!({ "kind": "proposal", "summary": "Add", "operations": [
                { "type": "create_task", "title": "Pick up badges", "plan": { "id": OTHER_PLAN }, "doOn": "2026-09-17" }
            ]}),
        ) else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            day_task.operations.as_slice(),
            [MutationOperation::CreateTask { plan: None, scheduled_day: Some(day), .. }] if day == "2026-09-17"
        ));
        let Resolution::Proposal(new_title) = resolve_with(
            "add pick up badges to my tasks for Thursday",
            "add pick up badges to my tasks for Thursday",
            &[],
            &[],
            json!({ "kind": "proposal", "summary": "Add", "operations": [
                { "type": "create_task", "title": "Pick up badges", "plan": { "newTitle": "Home renovation" }, "doOn": "2026-09-17" }
            ]}),
        ) else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            new_title.operations.as_slice(),
            [MutationOperation::CreateTask { plan: None, .. }]
        ));
        for (command, operations) in [
            (
                "leave the client meeting where it is",
                json!([{ "type": "update_event", "eventId": EVENT, "title": "Client meeting", "notes": "Leave it where it is" }]),
            ),
            (
                "add call the caterer to my to-do list",
                json!([{ "type": "create_task", "title": "Call the caterer", "plan": { "id": OTHER_PLAN }, "doOn": "2026-09-15" }]),
            ),
            (
                "add a Book band task to the wedding plan due November 1",
                json!([{ "type": "create_task", "title": "Book band", "plan": { "id": OTHER_PLAN }, "dueBy": "2026-11-01" }]),
            ),
            (
                "mark it done",
                json!([{ "type": "update_task", "taskId": TASK, "status": "done" }]),
            ),
            (
                "delete everything",
                json!([{ "type": "delete_task", "taskId": TASK }]),
            ),
        ] {
            assert!(
                matches!(
                    resolve_with(
                        command,
                        command,
                        &[],
                        &[],
                        json!({ "kind": "proposal", "summary": "Change", "operations": operations })
                    ),
                    Resolution::Clarification(_)
                ),
                "{command}"
            );
        }
        let Resolution::Proposal(timed) = resolve_with(
            "add tea Friday at 10 am and remind me when it starts",
            "add tea Friday at 10 am and remind me when it starts",
            &[],
            &[],
            json!({ "kind": "proposal", "summary": "Add tea", "operations": [
                { "type": "create_event", "title": "tea", "start": "2026-09-18T10:00", "durationMinutes": 30, "reminderMinutesBefore": 10 }
            ]}),
        ) else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            timed.operations.as_slice(),
            [MutationOperation::CreateEvent {
                duration_minutes: 60,
                reminder_minutes_before: Some(0),
                ..
            }]
        ));
        let Resolution::Proposal(midnight) = resolve_with(
            "move client meeting to midnight tomorrow",
            "move client meeting to midnight tomorrow",
            &[],
            &[],
            json!({ "kind": "proposal", "summary": "Move", "operations": [
                { "type": "reschedule_event", "eventId": EVENT, "start": "2030-05-11T23:00" }
            ]}),
        ) else {
            panic!("expected a proposal");
        };
        assert!(matches!(
            midnight.operations.as_slice(),
            [MutationOperation::RescheduleEvent { start_at_utc, .. }]
                if start_at_utc == "2030-05-11T04:00:00.000Z"
        ));
        assert_eq!(
            without_target(
                "use SELECT star FROM metrics to data review".into(),
                "Data review"
            ),
            "use SELECT star FROM metrics"
        );
    }

    #[test]
    fn drafts_round_trip_without_unset_fields() {
        let draft = DraftOperation::CreateTask {
            title: "Book venue".into(),
            description: None,
            plan: Some(RecordRef::existing(PLAN)),
            milestone: None,
            do_on: None,
            due_by: Some("2026-10-03".into()),
            status: None,
            priority: None,
        };
        let value = serde_json::to_value(&draft).unwrap();
        assert_eq!(
            value,
            json!({ "type": "create_task", "title": "Book venue", "plan": { "id": PLAN }, "dueBy": "2026-10-03" })
        );
        assert_eq!(
            serde_json::from_value::<DraftOperation>(value).unwrap(),
            draft
        );
        assert_eq!(sentence_start("post-change check"), "Post-change check");
        assert_eq!(sentence_start("iPhone repair"), "iPhone repair");
    }
}
