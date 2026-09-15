use serde::{Deserialize, Serialize};

pub const MAX_TITLE_LENGTH: usize = 140;
pub const MAX_NOTES_LENGTH: usize = 800;
pub const MAX_DESCRIPTION_LENGTH: usize = 2_000;
pub const MAX_NAME_LENGTH: usize = 80;
pub const MAX_LOCATION_LENGTH: usize = 140;
pub const MAX_EMAIL_LENGTH: usize = 254;
pub const MAX_COMMAND_LENGTH: usize = 1_000;
pub const MAX_OPERATIONS: usize = 12;
pub const MAX_REMINDER_MINUTES: i64 = 7 * 24 * 60;

/// Declares a closed enum whose serde name and SQLite text value come from one literal.
macro_rules! stored_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$variant_meta])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

stored_enum! {
    #[derive(Default)]
    pub enum PlanStatus {
        #[default]
        Planning => "planning",
        Active => "active",
        OnHold => "on_hold",
        Complete => "complete",
        Cancelled => "cancelled",
    }
}

stored_enum! {
    /// A small fixed palette keeps plan labels legible against DayPlan's theme.
    pub enum PlanColor {
        Sage => "sage",
        Clay => "clay",
        Ochre => "ochre",
        Lake => "lake",
        Plum => "plum",
        Stone => "stone",
    }
}

stored_enum! {
    /// Only user decisions are stored; "upcoming", "overdue", and "at risk" are derived from dates.
    #[derive(Default)]
    pub enum MilestoneStatus {
        #[default]
        Pending => "pending",
        Complete => "complete",
        Skipped => "skipped",
    }
}

stored_enum! {
    #[derive(Default)]
    pub enum TaskStatus {
        #[default]
        Todo => "todo",
        InProgress => "in_progress",
        Blocked => "blocked",
        Done => "done",
    }
}

stored_enum! {
    #[derive(Default)]
    pub enum TaskPriority {
        Low => "low",
        #[default]
        Normal => "normal",
        High => "high",
        Critical => "critical",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleEvent {
    pub id: String,
    pub title: String,
    pub notes: String,
    pub start_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
    #[serde(default)]
    pub reminder_minutes_before: Option<i64>,
    #[serde(default)]
    pub reminder_status: ReminderStatus,
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub workstream_id: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Someone who can own tasks and events. People are local labels, not accounts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Person {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub email: Option<String>,
    pub notes: String,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A named stream of related work inside one plan, such as Production or Sponsors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Workstream {
    pub id: String,
    pub plan_id: String,
    pub name: String,
    pub description: String,
    pub sort_order: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A long-range container for milestones, tasks, and events. Dates are optional local days.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: PlanStatus,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub color: Option<PlanColor>,
    pub archived: bool,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Milestone {
    pub id: String,
    pub plan_id: String,
    pub title: String,
    pub description: String,
    pub target_date: Option<String>,
    pub status: MilestoneStatus,
    #[serde(default)]
    pub workstream_id: Option<String>,
    pub sort_order: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Work that needs doing. A task may be scheduled onto a day, due by a day, or live only in a plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub plan_id: Option<String>,
    pub milestone_id: Option<String>,
    #[serde(default)]
    pub workstream_id: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    pub due_date: Option<String>,
    pub scheduled_day: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub completed_at: Option<String>,
    pub sort_order: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// The day-bound task shape used by schema 1–2 databases and export formats 1–2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyDailyTask {
    pub id: String,
    pub title: String,
    pub day: String,
    pub completed: bool,
    pub completed_at: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEventInput {
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub start_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
    #[serde(default)]
    pub reminder_minutes_before: Option<i64>,
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub workstream_id: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateEventInput {
    pub id: String,
    pub revision: i64,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub start_at_utc: Option<String>,
    pub time_zone: Option<String>,
    pub duration_minutes: Option<i64>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub reminder_change: ReminderChange,
    #[serde(default)]
    pub plan_change: LinkChange,
    #[serde(default)]
    pub workstream_change: LinkChange,
    #[serde(default)]
    pub owner_change: LinkChange,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RescheduleEventInput {
    pub id: String,
    pub revision: i64,
    pub start_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
    #[serde(default)]
    pub reminder_change: ReminderChange,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePlanInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: PlanStatus,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub target_date: Option<String>,
    #[serde(default)]
    pub color: Option<PlanColor>,
}

/// Replaces every editable plan field under one revision check.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePlanInput {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub description: String,
    pub status: PlanStatus,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub color: Option<PlanColor>,
    pub archived: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMilestoneInput {
    pub plan_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub target_date: Option<String>,
    #[serde(default)]
    pub status: MilestoneStatus,
    #[serde(default)]
    pub workstream_id: Option<String>,
}

/// Replaces every editable milestone field under one revision check. Milestones never move plans.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMilestoneInput {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub description: String,
    pub target_date: Option<String>,
    pub status: MilestoneStatus,
    pub workstream_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTaskInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub milestone_id: Option<String>,
    #[serde(default)]
    pub workstream_id: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub scheduled_day: Option<String>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: TaskPriority,
}

/// Replaces every editable task field under one revision check.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTaskInput {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub description: String,
    pub plan_id: Option<String>,
    pub milestone_id: Option<String>,
    pub workstream_id: Option<String>,
    pub owner_id: Option<String>,
    pub due_date: Option<String>,
    pub scheduled_day: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePersonInput {
    pub display_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePersonInput {
    pub id: String,
    pub revision: i64,
    pub display_name: String,
    pub role: String,
    pub email: Option<String>,
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorkstreamInput {
    pub plan_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Workstreams never move between plans.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWorkstreamInput {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSummary {
    pub plan: Plan,
    pub milestone_count: i64,
    pub task_count: i64,
    pub completed_task_count: i64,
    /// Open tasks whose due date is before the day the list was requested for.
    pub overdue_task_count: i64,
    pub next_milestone: Option<Milestone>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanWorkspace {
    pub plan: Plan,
    pub workstreams: Vec<Workstream>,
    pub milestones: Vec<Milestone>,
    pub tasks: Vec<Task>,
    pub events: Vec<ScheduleEvent>,
}

/// A person with the open work and upcoming events they own, soonest first.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonSummary {
    pub person: Person,
    pub open_task_count: i64,
    pub open_tasks: Vec<Task>,
    pub upcoming_events: Vec<ScheduleEvent>,
}

/// Everything dated inside a window of local days, for the Today and Week views.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Agenda {
    pub events: Vec<ScheduleEvent>,
    pub tasks: Vec<Task>,
    pub due_tasks: Vec<Task>,
    pub milestones: Vec<Milestone>,
}

/// What a permanent plan deletion removed or detached, for confirmation feedback.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanDeletion {
    pub deleted_workstreams: usize,
    pub deleted_milestones: usize,
    pub deleted_tasks: usize,
    pub detached_tasks: usize,
    pub detached_events: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalDateTimeInput {
    pub day: String,
    pub time: String,
    pub time_zone: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LocalDateTimeResolution {
    Resolved { start_at_utc: String },
    Ambiguous { options: Vec<LocalTimeOption> },
    Nonexistent { message: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalTimeOption {
    pub start_at_utc: String,
    pub utc_offset_minutes: i32,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportBundle {
    pub format_version: u32,
    pub exported_at: String,
    #[serde(default)]
    pub people: Vec<Person>,
    pub plans: Vec<Plan>,
    #[serde(default)]
    pub workstreams: Vec<Workstream>,
    pub milestones: Vec<Milestone>,
    pub events: Vec<ScheduleEvent>,
    pub tasks: Vec<Task>,
}

/// Export formats 1 and 2, which predate plans and stored day-bound tasks.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyExportBundle {
    pub format_version: u32,
    pub exported_at: String,
    pub events: Vec<ScheduleEvent>,
    pub tasks: Vec<LegacyDailyTask>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub person_count: usize,
    pub plan_count: usize,
    pub workstream_count: usize,
    pub milestone_count: usize,
    pub event_count: usize,
    pub task_count: usize,
    pub earliest_day: Option<String>,
    pub latest_day: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub name: String,
    pub created_at: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStatus {
    pub ready: bool,
    pub schema_version: u32,
    pub error: Option<crate::error::CommandError>,
    pub backups: Vec<BackupInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReminderChange {
    #[default]
    Unchanged,
    Clear,
    Set {
        #[serde(rename = "minutesBefore")]
        minutes_before: i64,
    },
}

/// A typed edit to an optional link such as an event's plan, workstream, or owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkChange {
    #[default]
    Unchanged,
    Clear,
    Set {
        id: String,
    },
}

impl LinkChange {
    pub fn apply(&self, current: Option<String>) -> Option<String> {
        match self {
            Self::Unchanged => current,
            Self::Clear => None,
            Self::Set { id } => Some(id.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReminderStatus {
    #[default]
    None,
    Pending,
    Scheduled,
    NeedsPermission,
    Error,
    Expired,
}

/// Refers to an existing record by ID, or to a plan or milestone created earlier in the same
/// proposal by its exact title.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RecordRef {
    Existing(ExistingRef),
    New(NewRef),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExistingRef {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewRef {
    pub new_title: String,
}

impl RecordRef {
    pub fn existing(id: impl Into<String>) -> Self {
        Self::Existing(ExistingRef { id: id.into() })
    }

    pub fn new_title(title: impl Into<String>) -> Self {
        Self::New(NewRef {
            new_title: title.into(),
        })
    }
}

/// A typed edit to an optional local day such as a task's scheduled day or due date.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DayChange {
    #[default]
    Unchanged,
    Clear,
    Set {
        day: String,
    },
}

/// The closed set of changes the local planner may propose. Nothing here is applied until the
/// user approves the whole proposal, which then commits in one transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MutationOperation {
    CreateEvent {
        title: String,
        notes: String,
        start_at_utc: String,
        time_zone: String,
        duration_minutes: i64,
        reminder_minutes_before: Option<i64>,
        #[serde(default)]
        plan: Option<RecordRef>,
    },
    UpdateEvent {
        event_id: String,
        expected_revision: i64,
        title: Option<String>,
        notes: Option<String>,
        duration_minutes: Option<i64>,
        reminder_change: ReminderChange,
    },
    DeleteEvent {
        event_id: String,
        expected_revision: i64,
    },
    RescheduleEvent {
        event_id: String,
        expected_revision: i64,
        title: Option<String>,
        notes: Option<String>,
        start_at_utc: String,
        time_zone: String,
        duration_minutes: Option<i64>,
        reminder_change: ReminderChange,
    },
    /// Moves an event into a plan or out of plans; its workstream is cleared when the plan changes.
    SetEventPlan {
        event_id: String,
        expected_revision: i64,
        plan: Option<RecordRef>,
    },
    CreatePlan {
        title: String,
        description: String,
        status: PlanStatus,
        start_date: Option<String>,
        target_date: Option<String>,
    },
    UpdatePlan {
        plan_id: String,
        expected_revision: i64,
        title: Option<String>,
        description: Option<String>,
        status: Option<PlanStatus>,
        start_date: Option<String>,
        target_date: Option<String>,
    },
    CreateMilestone {
        plan: RecordRef,
        title: String,
        description: String,
        target_date: Option<String>,
    },
    UpdateMilestone {
        milestone_id: String,
        expected_revision: i64,
        title: Option<String>,
        description: Option<String>,
        target_date: Option<String>,
        status: Option<MilestoneStatus>,
    },
    DeleteMilestone {
        milestone_id: String,
        expected_revision: i64,
    },
    CreateTask {
        title: String,
        description: String,
        plan: Option<RecordRef>,
        milestone: Option<RecordRef>,
        scheduled_day: Option<String>,
        due_date: Option<String>,
        status: TaskStatus,
        priority: TaskPriority,
    },
    UpdateTask {
        task_id: String,
        expected_revision: i64,
        title: Option<String>,
        description: Option<String>,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
    },
    ScheduleTask {
        task_id: String,
        expected_revision: i64,
        scheduled_day: DayChange,
        due_date: DayChange,
    },
    /// Moves a task into a plan (and optionally one of its milestones), or out of plans.
    /// The task's workstream is cleared when its plan changes.
    SetTaskPlan {
        task_id: String,
        expected_revision: i64,
        plan: Option<RecordRef>,
        milestone: Option<RecordRef>,
    },
    DeleteTask {
        task_id: String,
        expected_revision: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelResponse {
    Proposal {
        summary: String,
        operations: Vec<MutationOperation>,
    },
    Clarification {
        question: String,
    },
}

impl ModelResponse {
    pub fn proposal(summary: impl Into<String>, operations: Vec<MutationOperation>) -> Self {
        Self::Proposal {
            summary: summary.into(),
            operations,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PlannerResponse {
    Proposal {
        proposal_id: String,
        summary: String,
        operations: Vec<MutationOperation>,
        /// Titles of the existing records the operations reference, for a readable preview.
        references: Vec<ProposalReference>,
        expires_at: String,
    },
    Clarification {
        question: String,
    },
}

stored_enum! {
    pub enum RecordKind {
        Event => "event",
        Plan => "plan",
        Milestone => "milestone",
        Task => "task",
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProposalReference {
    pub id: String,
    pub kind: RecordKind,
    pub title: String,
}

/// The records an applied proposal created or changed.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppliedProposal {
    pub event_ids: Vec<String>,
    pub plan_ids: Vec<String>,
    pub milestone_ids: Vec<String>,
    pub task_ids: Vec<String>,
}

/// The records the planner may reference for one request, chosen by relevance.
#[derive(Debug, Clone, Default)]
pub struct PlannerCandidates {
    pub events: Vec<ScheduleEvent>,
    pub plans: Vec<Plan>,
    pub milestones: Vec<Milestone>,
    pub tasks: Vec<Task>,
}
