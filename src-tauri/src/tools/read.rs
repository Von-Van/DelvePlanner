//! The read tools themselves. Each takes typed arguments, reads through existing
//! `PlannerDatabase` queries, and returns a narrow view of the records: ids, revisions, titles,
//! dates, and statuses. Free text the user wrote (descriptions, notes) is included only when the
//! session's privacy policy allows it, and external calendar events are reduced to busy times
//! with no titles, so text from outside the planner never reaches the model through these tools.

use super::error::ToolError;
use super::{BusySource, PrivacyPolicy, ToolSources};
use crate::calendar::{parse_zone, DayRange};
use crate::capacity;
use crate::model::{
    Capacity, ExternalEvent, Milestone, MilestoneStatus, Plan, PlanStatus, ScheduleEvent,
    ScheduledBlock, Task, TaskPriority, TaskStatus, TimeRange, Workstream,
};
use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Most items any one list in a result carries; longer lists are cut and marked `truncated`.
pub const MAX_DAY_ITEMS: usize = 100;
pub const MAX_PROJECTS: usize = 50;
pub const MAX_MILESTONES_PER_PROJECT: usize = 20;
pub const MAX_PROJECT_TASKS: usize = 200;
pub const MAX_SEARCH_LIMIT: u32 = 50;
pub const DEFAULT_SEARCH_LIMIT: u32 = 20;
pub const MAX_QUERY_LENGTH: usize = 200;
pub const MAX_RECENT_ACTIONS: u32 = 20;

// ---------------------------------------------------------------------------------------------
// Views

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub plan_id: Option<String>,
    pub milestone_id: Option<String>,
    pub workstream_id: Option<String>,
    pub due_date: Option<String>,
    pub scheduled_day: Option<String>,
    pub planned_week: Option<String>,
    pub estimated_minutes: Option<i64>,
    /// Ids of tasks this one waits on.
    pub waiting_on: Vec<String>,
    pub recurring: bool,
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl TaskView {
    fn new(task: &Task, privacy: PrivacyPolicy) -> Self {
        Self {
            id: task.id.clone(),
            revision: task.revision,
            title: task.title.clone(),
            status: task.status,
            priority: task.priority,
            plan_id: task.plan_id.clone(),
            milestone_id: task.milestone_id.clone(),
            workstream_id: task.workstream_id.clone(),
            due_date: task.due_date.clone(),
            scheduled_day: task.scheduled_day.clone(),
            planned_week: task.planned_week.clone(),
            estimated_minutes: task.estimated_minutes,
            waiting_on: task.waiting_on.clone(),
            recurring: task.recurrence.is_some(),
            completed_at: task.completed_at.clone(),
            description: privacy.text(&task.description),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventView {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub start_at_utc: String,
    pub end_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
    pub plan_id: Option<String>,
    pub workstream_id: Option<String>,
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl EventView {
    fn new(event: &ScheduleEvent, privacy: PrivacyPolicy) -> Self {
        Self {
            id: event.id.clone(),
            revision: event.revision,
            title: event.title.clone(),
            end_at_utc: end_of(&event.start_at_utc, event.duration_minutes),
            start_at_utc: event.start_at_utc.clone(),
            time_zone: event.time_zone.clone(),
            duration_minutes: event.duration_minutes,
            plan_id: event.plan_id.clone(),
            workstream_id: event.workstream_id.clone(),
            location: event.location.clone(),
            notes: privacy.text(&event.notes),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MilestoneView {
    pub id: String,
    pub revision: i64,
    pub plan_id: String,
    pub title: String,
    pub status: MilestoneStatus,
    pub target_date: Option<String>,
    pub workstream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl MilestoneView {
    fn new(milestone: &Milestone, privacy: PrivacyPolicy) -> Self {
        Self {
            id: milestone.id.clone(),
            revision: milestone.revision,
            plan_id: milestone.plan_id.clone(),
            title: milestone.title.clone(),
            status: milestone.status,
            target_date: milestone.target_date.clone(),
            workstream_id: milestone.workstream_id.clone(),
            description: privacy.text(&milestone.description),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkstreamView {
    pub id: String,
    pub revision: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl WorkstreamView {
    fn new(workstream: &Workstream, privacy: PrivacyPolicy) -> Self {
        Self {
            id: workstream.id.clone(),
            revision: workstream.revision,
            name: workstream.name.clone(),
            description: privacy.text(&workstream.description),
        }
    }
}

/// A plan without its links, which are opened in the browser and never sent to a model.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanView {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub status: PlanStatus,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PlanView {
    fn new(plan: &Plan, privacy: PrivacyPolicy) -> Self {
        Self {
            id: plan.id.clone(),
            revision: plan.revision,
            title: plan.title.clone(),
            status: plan.status,
            start_date: plan.start_date.clone(),
            target_date: plan.target_date.clone(),
            archived: plan.archived,
            description: privacy.text(&plan.description),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockView {
    pub id: String,
    pub revision: i64,
    pub task_id: String,
    pub task_title: String,
    pub start_at_utc: String,
    pub end_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
}

impl BlockView {
    fn new(scheduled: &ScheduledBlock) -> Self {
        let block = &scheduled.block;
        Self {
            id: block.id.clone(),
            revision: block.revision,
            task_id: block.task_id.clone(),
            task_title: scheduled.task.title.clone(),
            end_at_utc: end_of(&block.start_at_utc, block.duration_minutes),
            start_at_utc: block.start_at_utc.clone(),
            time_zone: block.time_zone.clone(),
            duration_minutes: block.duration_minutes,
        }
    }
}

/// Busy time from a read-only calendar. Titles and locations are left out: they are text from
/// outside the planner and the model only needs to know the time is taken.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusyView {
    pub all_day: bool,
    pub start_at_utc: Option<String>,
    pub end_at_utc: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub tentative: bool,
}

impl BusyView {
    fn new(event: &ExternalEvent) -> Self {
        Self {
            all_day: event.all_day,
            start_at_utc: event.start_at_utc.clone(),
            end_at_utc: event.end_at_utc.clone(),
            start_date: event.start_date.clone(),
            end_date: event.end_date.clone(),
            tentative: event.tentative,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// get_day_context

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DayContextArgs {
    pub date: String,
    pub time_zone: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayContext {
    pub date: String,
    pub time_zone: String,
    pub events: Vec<EventView>,
    pub scheduled_tasks: Vec<TaskView>,
    /// Tasks due this day that are not also scheduled onto it.
    pub due_tasks: Vec<TaskView>,
    pub milestones: Vec<MilestoneView>,
    pub task_blocks: Vec<BlockView>,
    pub external_busy: Vec<BusyView>,
    /// Whether a visible calendar has a problem, so some busy time may be missing.
    pub calendars_incomplete: bool,
    pub working_minutes: i64,
    pub available_minutes: i64,
    pub planned_minutes: i64,
    /// Working time with no event, busy calendar event, or time block.
    pub free: Vec<TimeRange>,
    pub truncated: bool,
}

pub fn get_day_context(
    sources: &ToolSources<'_>,
    privacy: PrivacyPolicy,
    args: DayContextArgs,
) -> Result<DayContext, ToolError> {
    let (day, zone) = day_and_zone(&args.date, &args.time_zone)?;
    let date = day.to_string();
    let agenda = sources.database.agenda(&date, 1, &args.time_zone)?;
    let capacity = capacity_for(sources, day, 1, zone, &args.time_zone)?;
    let (busy, _) = external_busy(sources.calendars, &date, 1, &args.time_zone);
    let today = capacity.days.into_iter().next();
    let mut truncated = false;
    let events = capped(agenda.events, MAX_DAY_ITEMS, &mut truncated)
        .iter()
        .map(|event| EventView::new(event, privacy))
        .collect();
    let scheduled_tasks = capped(agenda.tasks, MAX_DAY_ITEMS, &mut truncated)
        .iter()
        .map(|task| TaskView::new(task, privacy))
        .collect();
    let due_tasks = capped(agenda.due_tasks, MAX_DAY_ITEMS, &mut truncated)
        .iter()
        .map(|task| TaskView::new(task, privacy))
        .collect();
    let milestones = capped(agenda.milestones, MAX_DAY_ITEMS, &mut truncated)
        .iter()
        .map(|milestone| MilestoneView::new(milestone, privacy))
        .collect();
    let task_blocks = capped(agenda.blocks, MAX_DAY_ITEMS, &mut truncated)
        .iter()
        .map(BlockView::new)
        .collect();
    let external_busy = capped(
        busy.into_iter().filter(|event| event.busy).collect(),
        MAX_DAY_ITEMS,
        &mut truncated,
    )
    .iter()
    .map(BusyView::new)
    .collect();
    Ok(DayContext {
        date,
        time_zone: args.time_zone,
        events,
        scheduled_tasks,
        due_tasks,
        milestones,
        task_blocks,
        external_busy,
        calendars_incomplete: capacity.calendars_incomplete,
        working_minutes: today.as_ref().map_or(0, |day| day.working_minutes),
        available_minutes: today.as_ref().map_or(0, |day| day.available_minutes),
        planned_minutes: today.as_ref().map_or(0, |day| day.planned_minutes),
        free: today.map(|day| day.free).unwrap_or_default(),
        truncated,
    })
}

// ---------------------------------------------------------------------------------------------
// list_active_projects

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveProjectsArgs {
    /// Also list plans still being planned. Defaults to false.
    #[serde(default)]
    pub include_planning: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub status: PlanStatus,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub task_count: i64,
    pub open_task_count: i64,
    /// Open tasks whose due date is before `asOf`.
    pub overdue_task_count: i64,
    pub milestones: Vec<MilestoneView>,
    pub milestones_truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveProjects {
    /// The local day overdue counts were worked out for.
    pub as_of: String,
    pub plans: Vec<ProjectSummary>,
    pub truncated: bool,
}

pub fn list_active_projects(
    sources: &ToolSources<'_>,
    privacy: PrivacyPolicy,
    today: &str,
    args: ActiveProjectsArgs,
) -> Result<ActiveProjects, ToolError> {
    let summaries = sources.database.list_plans(today)?;
    let wanted: Vec<_> = summaries
        .into_iter()
        .filter(|summary| {
            !summary.plan.archived
                && (summary.plan.status == PlanStatus::Active
                    || (args.include_planning && summary.plan.status == PlanStatus::Planning))
        })
        .collect();
    let mut truncated = false;
    let mut plans = Vec::new();
    for summary in capped(wanted, MAX_PROJECTS, &mut truncated) {
        let workspace = sources.database.plan_workspace(&summary.plan.id)?;
        let mut milestones_truncated = false;
        let milestones = capped(
            workspace.milestones,
            MAX_MILESTONES_PER_PROJECT,
            &mut milestones_truncated,
        )
        .iter()
        .map(|milestone| MilestoneView::new(milestone, privacy))
        .collect();
        let plan = summary.plan;
        plans.push(ProjectSummary {
            id: plan.id,
            revision: plan.revision,
            title: plan.title,
            status: plan.status,
            start_date: plan.start_date,
            target_date: plan.target_date,
            task_count: summary.task_count,
            open_task_count: summary.task_count - summary.completed_task_count,
            overdue_task_count: summary.overdue_task_count,
            milestones,
            milestones_truncated,
        });
    }
    Ok(ActiveProjects {
        as_of: today.to_string(),
        plans,
        truncated,
    })
}

// ---------------------------------------------------------------------------------------------
// get_project_context

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectContextArgs {
    pub plan_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub plan: PlanView,
    pub milestones: Vec<MilestoneView>,
    pub workstreams: Vec<WorkstreamView>,
    /// Open tasks first, then finished ones.
    pub tasks: Vec<TaskView>,
    pub truncated: bool,
}

pub fn get_project_context(
    sources: &ToolSources<'_>,
    privacy: PrivacyPolicy,
    args: ProjectContextArgs,
) -> Result<ProjectContext, ToolError> {
    let id = args.plan_id.trim();
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(ToolError::invalid("planId must be a plan id."));
    }
    let workspace = sources
        .database
        .plan_workspace(id)
        .map_err(|error| match error {
            crate::error::AppError::NotFound => ToolError::not_found("No plan has that id."),
            other => other.into(),
        })?;
    let mut truncated = false;
    Ok(ProjectContext {
        plan: PlanView::new(&workspace.plan, privacy),
        milestones: workspace
            .milestones
            .iter()
            .map(|milestone| MilestoneView::new(milestone, privacy))
            .collect(),
        workstreams: workspace
            .workstreams
            .iter()
            .map(|workstream| WorkstreamView::new(workstream, privacy))
            .collect(),
        tasks: capped(workspace.tasks, MAX_PROJECT_TASKS, &mut truncated)
            .iter()
            .map(|task| TaskView::new(task, privacy))
            .collect(),
        truncated,
    })
}

// ---------------------------------------------------------------------------------------------
// search_tasks

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchTasksArgs {
    /// Words that must all appear in the title, ignoring case.
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    /// Only tasks due strictly before this local day.
    #[serde(default)]
    pub due_before: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSearch {
    pub tasks: Vec<TaskView>,
    /// Every match, including those past `limit`.
    pub total_matches: usize,
    pub truncated: bool,
}

pub fn search_tasks(
    sources: &ToolSources<'_>,
    privacy: PrivacyPolicy,
    args: SearchTasksArgs,
) -> Result<TaskSearch, ToolError> {
    let limit = args.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(ToolError::invalid(format!(
            "limit must be between 1 and {MAX_SEARCH_LIMIT}."
        )));
    }
    let terms: Vec<String> = match args.query.as_deref() {
        Some(query) if query.chars().count() > MAX_QUERY_LENGTH => {
            return Err(ToolError::invalid(format!(
                "query can be at most {MAX_QUERY_LENGTH} characters."
            )));
        }
        Some(query) => query.split_whitespace().map(str::to_lowercase).collect(),
        None => Vec::new(),
    };
    if let Some(plan_id) = &args.plan_id {
        if uuid::Uuid::parse_str(plan_id).is_err() {
            return Err(ToolError::invalid("planId must be a plan id."));
        }
    }
    let due_before = args
        .due_before
        .as_deref()
        .map(parse_day)
        .transpose()?
        .map(|day| day.to_string());
    let mut matches: Vec<Task> = sources
        .database
        .export_bundle()?
        .tasks
        .into_iter()
        .filter(|task| {
            let title = task.title.to_lowercase();
            terms.iter().all(|term| title.contains(term.as_str()))
                && args
                    .plan_id
                    .as_ref()
                    .is_none_or(|plan| task.plan_id.as_ref() == Some(plan))
                && args.status.is_none_or(|status| task.status == status)
                && due_before.as_ref().is_none_or(|before| {
                    task.due_date
                        .as_ref()
                        .is_some_and(|due| due.as_str() < before.as_str())
                })
        })
        .collect();
    // Open work first, then by due date with undated tasks last, then by title.
    matches.sort_by(|a, b| {
        (a.status == TaskStatus::Done)
            .cmp(&(b.status == TaskStatus::Done))
            .then_with(|| a.due_date.is_none().cmp(&b.due_date.is_none()))
            .then_with(|| a.due_date.cmp(&b.due_date))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
    let total_matches = matches.len();
    let mut truncated = false;
    Ok(TaskSearch {
        tasks: capped(matches, limit as usize, &mut truncated)
            .iter()
            .map(|task| TaskView::new(task, privacy))
            .collect(),
        total_matches,
        truncated,
    })
}

// ---------------------------------------------------------------------------------------------
// get_capacity

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapacityArgs {
    pub week_start: String,
    pub time_zone: String,
}

/// Capacity for the seven local days starting at `weekStart`, in the planner's own shape.
pub fn get_capacity(sources: &ToolSources<'_>, args: CapacityArgs) -> Result<Capacity, ToolError> {
    let (day, zone) = day_and_zone(&args.week_start, &args.time_zone)?;
    capacity_for(sources, day, 7, zone, &args.time_zone)
}

// ---------------------------------------------------------------------------------------------
// get_recent_actions

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecentActionsArgs {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentActions {
    pub actions: Vec<serde_json::Value>,
    /// False until agent runs and their outcomes are stored; an empty list then means "not
    /// recorded yet", not "nothing happened".
    pub history_available: bool,
}

pub fn get_recent_actions(args: RecentActionsArgs) -> Result<RecentActions, ToolError> {
    let limit = args.limit.unwrap_or(10);
    if !(1..=MAX_RECENT_ACTIONS).contains(&limit) {
        return Err(ToolError::invalid(format!(
            "limit must be between 1 and {MAX_RECENT_ACTIONS}."
        )));
    }
    Ok(RecentActions {
        actions: Vec::new(),
        history_available: false,
    })
}

// ---------------------------------------------------------------------------------------------
// Helpers

fn parse_day(value: &str) -> Result<NaiveDate, ToolError> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| ToolError::invalid(format!("'{value}' is not a YYYY-MM-DD date.")))
}

fn day_and_zone(day: &str, time_zone: &str) -> Result<(NaiveDate, Tz), ToolError> {
    let day = parse_day(day)?;
    let zone = parse_zone(time_zone)
        .map_err(|_| ToolError::invalid(format!("'{time_zone}' is not an IANA time zone.")))?;
    Ok((day, zone))
}

fn external_busy(
    calendars: &dyn BusySource,
    first_day: &str,
    days: u32,
    time_zone: &str,
) -> (Vec<ExternalEvent>, bool) {
    DayRange::new(first_day, days, time_zone)
        .and_then(|range| calendars.busy(&range))
        .unwrap_or_else(|_| (Vec::new(), true))
}

fn capacity_for(
    sources: &ToolSources<'_>,
    first_day: NaiveDate,
    days: u32,
    zone: Tz,
    time_zone: &str,
) -> Result<Capacity, ToolError> {
    let range = DayRange::new(&first_day.to_string(), days, time_zone)?;
    let facts =
        sources
            .database
            .capacity_facts(&range.first_day, &range.end_day, time_zone, None)?;
    let profile = sources.database.planning_profile()?;
    let (busy, incomplete) = external_busy(sources.calendars, &range.first_day, days, time_zone);
    Ok(capacity::capacity(
        first_day, days, zone, facts, &busy, incomplete, &profile,
    ))
}

fn end_of(start_at_utc: &str, duration_minutes: i64) -> String {
    DateTime::parse_from_rfc3339(start_at_utc)
        .map(|start| {
            (start.with_timezone(&Utc) + Duration::minutes(duration_minutes))
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .unwrap_or_default()
}

fn capped<T>(mut items: Vec<T>, max: usize, truncated: &mut bool) -> Vec<T> {
    if items.len() > max {
        items.truncate(max);
        *truncated = true;
    }
    items
}
