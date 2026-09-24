use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_TITLE_LENGTH: usize = 140;
pub const MAX_NOTES_LENGTH: usize = 800;
pub const MAX_DESCRIPTION_LENGTH: usize = 2_000;
pub const MAX_NAME_LENGTH: usize = 80;
pub const MAX_LOCATION_LENGTH: usize = 140;
pub const MAX_EMAIL_LENGTH: usize = 254;
pub const MAX_COMMAND_LENGTH: usize = 1_000;
pub const MAX_OPERATIONS: usize = 12;
pub const MAX_REMINDER_MINUTES: i64 = 7 * 24 * 60;
pub const MAX_ESTIMATE_MINUTES: i64 = 24 * 60;
pub const MAX_TASK_MOVES: usize = 500;
pub const MAX_BLOCK_CHANGES: usize = 200;
pub const MAX_CALENDAR_NAME_LENGTH: usize = 80;
pub const MAX_CALENDAR_LINK_LENGTH: usize = 1_200;
pub const MAX_CALENDARS: usize = 20;
pub const MAX_CHECKLIST_ITEMS: usize = 30;
pub const MAX_WAITING_ON: usize = 10;
pub const MAX_RECURRENCE_INTERVAL: u32 = 99;
pub const MAX_PLAN_LINKS: usize = 20;
pub const MAX_LINK_URL_LENGTH: usize = 2_048;

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
    /// A small fixed palette keeps plan labels legible against Delve Planner's theme.
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

stored_enum! {
    /// How often a repeating task comes back.
    pub enum RecurrenceFrequency {
        Daily => "daily",
        /// Monday through Friday.
        Weekdays => "weekdays",
        Weekly => "weekly",
        Monthly => "monthly",
    }
}

/// How a task repeats. Only the open occurrence carries the rule: finishing it creates the next
/// occurrence, which takes the rule forward, so there is never more than one open copy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recurrence {
    pub frequency: RecurrenceFrequency,
    /// Every this many days, weeks, or months. Weekday rules always step one working day.
    #[serde(default = "one")]
    pub interval: u32,
    /// For weekly rules, the ISO weekdays it lands on, Monday = 1 through Sunday = 7.
    #[serde(default)]
    pub weekdays: Vec<u8>,
    /// For monthly rules, the day of the month, moved to the last day in shorter months.
    #[serde(default)]
    pub month_day: Option<u8>,
}

fn one() -> u32 {
    1
}

/// One line of a task's checklist: steps inside the task rather than tasks of their own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChecklistItem {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

/// A page that belongs with a plan, such as a booking, a shared document, or a ticket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanLink {
    #[serde(default)]
    pub title: String,
    pub url: String,
}

stored_enum! {
    /// Where a read-only calendar comes from.
    pub enum CalendarKind {
        /// A subscription to an iCalendar link, refreshed in the background.
        IcsLink => "ics_link",
        /// An imported .ics file: a snapshot that changes only when a newer file replaces it.
        IcsFile => "ics_file",
        /// A calendar in a connected Google account.
        Google => "google",
        /// A calendar in a connected Microsoft account.
        Microsoft => "microsoft",
    }
}

stored_enum! {
    /// An account Delve Planner can read calendars from. Both are read-only: Delve Planner asks for
    /// read-only scopes and never calls an endpoint that changes a calendar.
    pub enum CalendarProvider {
        Google => "google",
        Microsoft => "microsoft",
    }
}

impl CalendarProvider {
    pub fn calendar_kind(self) -> CalendarKind {
        match self {
            Self::Google => CalendarKind::Google,
            Self::Microsoft => CalendarKind::Microsoft,
        }
    }

    /// What the account is called in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft => "Outlook",
        }
    }
}

stored_enum! {
    /// Why a calendar could not be read or refreshed. Its cached events stay until it recovers.
    pub enum CalendarProblem {
        NotACalendar => "not_a_calendar",
        SignInExpired => "sign_in_expired",
        TooLarge => "too_large",
        LinkNotFound => "link_not_found",
        LinkRefused => "link_refused",
        Unreachable => "unreachable",
        RateLimited => "rate_limited",
        ServerError => "server_error",
        LinkUnavailable => "link_unavailable",
    }
}

impl CalendarProblem {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotACalendar => "That isn't an iCalendar (.ics) calendar.",
            Self::SignInExpired => {
                "This account's sign-in expired or was revoked. Sign in again to keep it up to date."
            }
            Self::TooLarge => "That calendar is larger than the 20 MB limit.",
            Self::LinkNotFound => {
                "The calendar link no longer works. It may have been reset; paste the new link."
            }
            Self::LinkRefused => {
                "The calendar service refused the link. Check that the calendar is still shared."
            }
            Self::Unreachable => {
                "Delve Planner couldn't reach the calendar service. It will try again automatically."
            }
            Self::RateLimited => {
                "The calendar service asked Delve Planner to slow down. It will try again later."
            }
            Self::ServerError => {
                "The calendar service had a problem. Delve Planner will try again automatically."
            }
            Self::LinkUnavailable => {
                "Delve Planner couldn't read this calendar's link from the system keychain. Paste the link again."
            }
        }
    }

    /// Problems that clear up on their own, as opposed to ones that need the user to act.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Unreachable | Self::RateLimited | Self::ServerError
        )
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
    /// Pages that belong with the plan. Opened in the browser; never sent to the planner.
    #[serde(default)]
    pub links: Vec<PlanLink>,
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

/// Work that needs doing. A task lives in a plan, is chosen for a week, is scheduled onto a day,
/// or is due by a day; one record moves between those horizons and is never copied.
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
    /// The first local day of the week the task was chosen for, with no day picked yet.
    #[serde(default)]
    pub planned_week: Option<String>,
    #[serde(default)]
    pub estimated_minutes: Option<i64>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub completed_at: Option<String>,
    /// How the task repeats. Only the open occurrence of a repeating task has one.
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    #[serde(default)]
    pub checklist: Vec<ChecklistItem>,
    /// Tasks this one waits on. A hint shown beside the task; nothing is blocked automatically.
    #[serde(default)]
    pub waiting_on: Vec<String>,
    pub sort_order: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Something captured before it is organized. Converting it into a task, plan, or event removes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxItem {
    pub id: String,
    pub text: String,
    pub notes: String,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Time reserved for a task. Moving or deleting a block never changes its task, and deleting the
/// task deletes its blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskBlock {
    pub id: String,
    pub task_id: String,
    pub start_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A time block with the task it reserves time for.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledBlock {
    pub block: TaskBlock,
    pub task: Task,
}

/// The hours Delve Planner counts as available for planned work, in the viewer's local time.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkingHours {
    /// ISO weekdays in ascending order, Monday = 1 through Sunday = 7.
    pub days: Vec<u8>,
    /// Minutes after local midnight.
    pub start_minute: i64,
    pub end_minute: i64,
    pub revision: i64,
    pub updated_at: String,
}

/// An account Delve Planner reads calendars from. Its tokens stay in the system keychain and never
/// reach SQLite or the renderer.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAccount {
    pub id: String,
    pub provider: CalendarProvider,
    /// The signed-in address, such as someone@gmail.com.
    pub label: String,
    pub problem: Option<CalendarIssue>,
    pub calendar_count: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A calendar offered by a connected account, for choosing which ones Delve Planner shows.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCalendar {
    pub id: String,
    pub name: String,
    /// The account's main calendar, ticked by default.
    pub primary: bool,
    pub already_added: bool,
}

/// A freshly connected account and the calendars it offers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectedAccount {
    pub account: CalendarAccount,
    pub calendars: Vec<RemoteCalendar>,
}

/// A read-only calendar from another service. Its link or account tokens, when it has them, stay
/// in the system keychain and never reach SQLite or the renderer.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: String,
    pub kind: CalendarKind,
    /// The connected account this calendar belongs to, for account calendars.
    pub account_id: Option<String>,
    pub name: String,
    pub color: PlanColor,
    pub visible: bool,
    /// The link's host, such as calendar.google.com, or the imported file's name.
    pub source_label: String,
    pub problem: Option<CalendarIssue>,
    pub last_synced_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub event_count: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalendarIssue {
    pub code: CalendarProblem,
    pub message: String,
    pub retryable: bool,
}

impl From<CalendarProblem> for CalendarIssue {
    fn from(problem: CalendarProblem) -> Self {
        Self {
            code: problem,
            message: problem.message().into(),
            retryable: problem.retryable(),
        }
    }
}

/// One occurrence of an event from a read-only calendar. Timed events carry UTC instants;
/// all-day events carry local dates with an exclusive end.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEvent {
    pub calendar_id: String,
    pub key: String,
    pub title: String,
    pub location: String,
    pub all_day: bool,
    pub start_at_utc: Option<String>,
    pub end_at_utc: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub busy: bool,
    pub tentative: bool,
    pub recurring: bool,
}

/// Every calendar, and the events of the visible ones inside a window of local days.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAgenda {
    pub calendars: Vec<Calendar>,
    pub events: Vec<ExternalEvent>,
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
    #[serde(default)]
    pub links: Vec<PlanLink>,
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
    pub links: Vec<PlanLink>,
}

impl UpdatePlanInput {
    /// A replacement that keeps every field of `plan`, for callers that change only some.
    pub fn keeping(plan: &Plan) -> Self {
        Self {
            id: plan.id.clone(),
            revision: plan.revision,
            title: plan.title.clone(),
            description: plan.description.clone(),
            status: plan.status,
            start_date: plan.start_date.clone(),
            target_date: plan.target_date.clone(),
            color: plan.color,
            archived: plan.archived,
            links: plan.links.clone(),
        }
    }
}

/// The kinds of plan Delve Planner can start from, each with a few workstreams to fill in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanTemplate {
    Event,
    Trip,
    Move,
    Research,
    JobSearch,
    PersonalProject,
}

impl PlanTemplate {
    pub const ALL: [Self; 6] = [
        Self::Event,
        Self::Trip,
        Self::Move,
        Self::Research,
        Self::JobSearch,
        Self::PersonalProject,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Event => "Event",
            Self::Trip => "Trip",
            Self::Move => "Move",
            Self::Research => "Research or school project",
            Self::JobSearch => "Job search",
            Self::PersonalProject => "Personal project",
        }
    }

    /// The workstreams a plan of this kind starts with. A handful, not an exhaustive list: they
    /// are a first shape to rename, remove, or add to.
    pub fn workstreams(self) -> &'static [&'static str] {
        match self {
            Self::Event => &["Venue and logistics", "Program", "Promotion", "Budget"],
            Self::Trip => &["Travel", "Lodging", "Itinerary", "Packing"],
            Self::Move => &[
                "Finding a place",
                "Packing and movers",
                "Utilities and address changes",
                "Settling in",
            ],
            Self::Research => &["Reading and research", "Analysis", "Writing", "Submission"],
            Self::JobSearch => &[
                "Applications",
                "Networking",
                "Interview prep",
                "Résumé and portfolio",
            ],
            Self::PersonalProject => &["Planning", "Making", "Finishing"],
        }
    }
}

/// A template as the plan editor offers it: what it's called and what it starts with.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanTemplateInfo {
    pub template: PlanTemplate,
    pub label: &'static str,
    pub workstreams: &'static [&'static str],
}

/// An open task in a word, for choosing what another task waits on and showing the wait.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskReference {
    pub id: String,
    pub title: String,
    pub plan_id: Option<String>,
    pub scheduled_day: Option<String>,
    pub due_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DuplicatePlanInput {
    pub id: String,
    pub title: String,
    /// Days to move every date by; negative moves them earlier.
    #[serde(default)]
    pub shift_days: i64,
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
    pub planned_week: Option<String>,
    #[serde(default)]
    pub estimated_minutes: Option<i64>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: TaskPriority,
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    #[serde(default)]
    pub checklist: Vec<ChecklistItem>,
    #[serde(default)]
    pub waiting_on: Vec<String>,
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
    pub planned_week: Option<String>,
    pub estimated_minutes: Option<i64>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub recurrence: Option<Recurrence>,
    pub checklist: Vec<ChecklistItem>,
    pub waiting_on: Vec<String>,
}

impl UpdateTaskInput {
    /// A replacement that keeps every field of `task`, for callers that change only some. Going
    /// through here means a field added later can't be dropped by a caller that never heard of it.
    pub fn keeping(task: &Task) -> Self {
        Self {
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
            planned_week: task.planned_week.clone(),
            estimated_minutes: task.estimated_minutes,
            status: task.status,
            priority: task.priority,
            recurrence: task.recurrence.clone(),
            checklist: task.checklist.clone(),
            waiting_on: task.waiting_on.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateInboxItemInput {
    pub text: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateInboxItemInput {
    pub id: String,
    pub revision: i64,
    pub text: String,
    pub notes: String,
}

/// What an inbox item becomes. The new record is created and the item removed in one transaction.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum InboxTarget {
    Task { task: CreateTaskInput },
    Plan { plan: CreatePlanInput },
    Event { event: CreateEventInput },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessInboxItemInput {
    pub id: String,
    pub revision: i64,
    pub target: InboxTarget,
}

/// The record an inbox item was converted into.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InboxConversion {
    pub kind: RecordKind,
    pub id: String,
}

/// Moves one task between planning horizons: onto a day, into a week's pool, out of both, or done.
/// Every move in a batch is applied in one transaction.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskMove {
    pub id: String,
    pub revision: i64,
    #[serde(default)]
    pub scheduled_day: DayChange,
    #[serde(default)]
    pub planned_week: DayChange,
    #[serde(default)]
    pub status: Option<TaskStatus>,
}

/// Open work plus everything chosen for or scheduled in one week, for weekly and daily planning.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningBoard {
    pub tasks: Vec<Task>,
    pub milestones: Vec<Milestone>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTaskBlockInput {
    pub task_id: String,
    pub start_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTaskBlockInput {
    pub id: String,
    pub revision: i64,
    pub start_at_utc: String,
    pub time_zone: String,
    pub duration_minutes: i64,
}

/// Identifies one revision of a record, for batch changes that must not overwrite newer edits.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordVersion {
    pub id: String,
    pub revision: i64,
}

/// Something Delve Planner worked out from what is already on this device. Every observation says
/// what it is based on, so the user can judge it rather than take it on trust.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub kind: ObservationKind,
    /// The finding in one line: "Work takes about a third longer than you estimate".
    pub summary: String,
    /// What it was worked out from: "12 finished tasks with an estimate and blocked time".
    pub evidence: String,
    /// How many records it rests on, so a thin observation can say so.
    pub sample: i64,
}

stored_enum! {
    /// The observations Delve Planner can make, each computed in Rust from local records only.
    pub enum ObservationKind {
        EstimateAccuracy => "estimate_accuracy",
        UsualStart => "usual_start",
        TypicalDailyLoad => "typical_daily_load",
        DeferredDays => "deferred_days",
        RepeatedlyMoved => "repeatedly_moved",
        SlippingPlan => "slipping_plan",
    }
}

/// How the user plans, as far as they have chosen to say. Every field is optional: an empty
/// profile is a valid one, and capacity and the planner simply have less to go on.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanningProfile {
    /// The hours the user prefers to plan work into, inside their working hours.
    pub preferred_start_minute: Option<i64>,
    pub preferred_end_minute: Option<i64>,
    /// The most work the user wants planned into one day.
    pub max_planned_minutes: Option<i64>,
    /// How long a stretch of focused work should be, and the break after it.
    pub focus_minutes: Option<i64>,
    pub break_minutes: Option<i64>,
    /// ISO weekdays the user would rather not work, Monday = 1 through Sunday = 7.
    pub no_work_days: Vec<u8>,
    /// When in the day demanding work belongs, if the user has a preference.
    pub energy: Option<DayPreference>,
    /// Observations the user has switched off. They are neither computed nor shown.
    pub muted_observations: Vec<ObservationKind>,
    pub revision: i64,
    pub updated_at: String,
}

stored_enum! {
    /// When the user would rather do demanding work.
    pub enum DayPreference {
        Morning => "morning",
        Afternoon => "afternoon",
        Evening => "evening",
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePlanningProfileInput {
    pub revision: i64,
    pub preferred_start_minute: Option<i64>,
    pub preferred_end_minute: Option<i64>,
    pub max_planned_minutes: Option<i64>,
    pub focus_minutes: Option<i64>,
    pub break_minutes: Option<i64>,
    pub no_work_days: Vec<u8>,
    pub energy: Option<DayPreference>,
    pub muted_observations: Vec<ObservationKind>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWorkingHoursInput {
    pub revision: i64,
    pub days: Vec<u8>,
    pub start_minute: i64,
    pub end_minute: i64,
}

/// A span of time between two UTC instants.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimeRange {
    pub start_at_utc: String,
    pub end_at_utc: String,
}

/// One local day's planning hours against what already fills them. Busy time is Delve Planner
/// events and busy events from visible calendars inside working hours; time blocks are planned
/// work, so they reduce `free` but not `available_minutes`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DayCapacity {
    pub day: String,
    pub working_minutes: i64,
    pub busy_minutes: i64,
    pub available_minutes: i64,
    /// Estimates of the open tasks scheduled on this day.
    pub planned_minutes: i64,
    pub unestimated_tasks: i64,
    pub blocked_minutes: i64,
    /// Working time with no event, busy calendar event, or time block.
    pub free: Vec<TimeRange>,
}

/// Planned work against available time for a window of local days. Delve Planner reports these
/// numbers and never rearranges anything because of them.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub working_hours: WorkingHours,
    /// The most work the user wants planned into one day, from their planning profile.
    pub planned_limit_minutes: Option<i64>,
    pub days: Vec<DayCapacity>,
    /// Estimates of open tasks chosen for a week that starts in the window, with no day yet.
    pub pooled_minutes: i64,
    pub pooled_unestimated_tasks: i64,
    /// Whether a visible calendar has a problem, so some busy time may be missing.
    pub calendars_incomplete: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeCalendarInput {
    /// Empty to use the name the calendar gives itself.
    #[serde(default)]
    pub name: String,
    pub link: String,
    pub color: PlanColor,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCalendarInput {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub color: PlanColor,
    pub visible: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceCalendarLinkInput {
    pub id: String,
    pub revision: i64,
    pub link: String,
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
    /// Time blocks overlapping the window, with their tasks.
    pub blocks: Vec<ScheduledBlock>,
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
    #[serde(default)]
    pub inbox_items: Vec<InboxItem>,
    #[serde(default)]
    pub task_blocks: Vec<TaskBlock>,
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
    pub inbox_item_count: usize,
    pub task_block_count: usize,
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

/// The inbox item a new record is made from. Applying the proposal removes the item, and an
/// item edited or removed since the proposal was made fails the whole proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxSource {
    pub item_id: String,
    pub expected_revision: i64,
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

impl DayChange {
    pub fn apply(&self, current: Option<String>) -> Option<String> {
        match self {
            Self::Unchanged => current,
            Self::Clear => None,
            Self::Set { day } => Some(day.clone()),
        }
    }
}

/// A typed edit to a task's estimate, so "no change" and "no estimate" stay distinct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum EstimateChange {
    #[default]
    Unchanged,
    Clear,
    Set {
        minutes: i64,
    },
}

impl EstimateChange {
    pub fn apply(&self, current: Option<i64>) -> Option<i64> {
        match self {
            Self::Unchanged => current,
            Self::Clear => None,
            Self::Set { minutes } => Some(*minutes),
        }
    }
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
        #[serde(default)]
        from_inbox: Option<InboxSource>,
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
        #[serde(default)]
        from_inbox: Option<InboxSource>,
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
        /// One of the task's plan's workstreams, existing or created in the same proposal.
        #[serde(default)]
        workstream: Option<RecordRef>,
        #[serde(default)]
        from_inbox: Option<InboxSource>,
    },
    UpdateTask {
        task_id: String,
        expected_revision: i64,
        title: Option<String>,
        description: Option<String>,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        /// How long the work is expected to take, which capacity counts as planned work.
        estimate: EstimateChange,
    },
    ScheduleTask {
        task_id: String,
        expected_revision: i64,
        scheduled_day: DayChange,
        due_date: DayChange,
        /// The week a task is chosen for, as any day inside it. Weekly planning picks work for a
        /// week before it knows which day it lands on.
        planned_week: DayChange,
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
    /// Adds a named group of work to a plan, which tasks in the same proposal may then join.
    CreateWorkstream { plan: RecordRef, name: String },
    /// Puts a task in one of its plan's workstreams, or takes it out of its workstream.
    SetTaskWorkstream {
        task_id: String,
        expected_revision: i64,
        workstream: Option<RecordRef>,
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

impl MutationOperation {
    /// The plans, milestones, and workstreams this operation needs the same proposal to create.
    /// An operation refers to one by title until it exists, which is what makes it depend on
    /// another.
    pub fn new_references(&self) -> Vec<(RecordKind, &str)> {
        self.references()
            .into_iter()
            .filter_map(|(kind, reference)| match reference {
                RecordRef::New(NewRef { new_title }) => Some((kind, new_title.as_str())),
                RecordRef::Existing(_) => None,
            })
            .collect()
    }

    /// Every plan, milestone, and workstream reference in the operation, with its kind.
    pub fn references(&self) -> Vec<(RecordKind, &RecordRef)> {
        let (plan, milestone, workstream) = match self {
            Self::CreateEvent { plan, .. } | Self::SetEventPlan { plan, .. } => {
                (plan.as_ref(), None, None)
            }
            Self::CreateMilestone { plan, .. } | Self::CreateWorkstream { plan, .. } => {
                (Some(plan), None, None)
            }
            Self::CreateTask {
                plan,
                milestone,
                workstream,
                ..
            } => (plan.as_ref(), milestone.as_ref(), workstream.as_ref()),
            Self::SetTaskPlan {
                plan, milestone, ..
            } => (plan.as_ref(), milestone.as_ref(), None),
            Self::SetTaskWorkstream { workstream, .. } => (None, None, workstream.as_ref()),
            _ => (None, None, None),
        };
        [
            (RecordKind::Plan, plan),
            (RecordKind::Milestone, milestone),
            (RecordKind::Workstream, workstream),
        ]
        .into_iter()
        .filter_map(|(kind, reference)| reference.map(|reference| (kind, reference)))
        .collect()
    }

    /// The same references, for rewriting a new title the user renamed before applying.
    pub fn references_mut(&mut self) -> Vec<(RecordKind, &mut RecordRef)> {
        let (plan, milestone, workstream) = match self {
            Self::CreateEvent { plan, .. } | Self::SetEventPlan { plan, .. } => {
                (plan.as_mut(), None, None)
            }
            Self::CreateMilestone { plan, .. } | Self::CreateWorkstream { plan, .. } => {
                (Some(plan), None, None)
            }
            Self::CreateTask {
                plan,
                milestone,
                workstream,
                ..
            } => (plan.as_mut(), milestone.as_mut(), workstream.as_mut()),
            Self::SetTaskPlan {
                plan, milestone, ..
            } => (plan.as_mut(), milestone.as_mut(), None),
            Self::SetTaskWorkstream { workstream, .. } => (None, None, workstream.as_mut()),
            _ => (None, None, None),
        };
        [
            (RecordKind::Plan, plan),
            (RecordKind::Milestone, milestone),
            (RecordKind::Workstream, workstream),
        ]
        .into_iter()
        .filter_map(|(kind, reference)| reference.map(|reference| (kind, reference)))
        .collect()
    }

    /// The new plan, milestone, or workstream this operation creates, by title.
    pub fn creates(&self) -> Option<(RecordKind, &str)> {
        match self {
            Self::CreatePlan { title, .. } => Some((RecordKind::Plan, title)),
            Self::CreateMilestone { title, .. } => Some((RecordKind::Milestone, title)),
            Self::CreateWorkstream { name, .. } => Some((RecordKind::Workstream, name)),
            _ => None,
        }
    }

    /// The inbox item this operation turns into a new record, if any.
    pub fn inbox_source(&self) -> Option<&InboxSource> {
        match self {
            Self::CreateEvent { from_inbox, .. }
            | Self::CreatePlan { from_inbox, .. }
            | Self::CreateTask { from_inbox, .. } => from_inbox.as_ref(),
            _ => None,
        }
    }
}

/// A suggestion the user changed before applying it. Only its values may differ: what it targets,
/// what it links to, and the revision it was proposed against stay as proposed.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuggestionEdit {
    pub id: String,
    pub change: MutationOperation,
}

/// What the planner says about one of its own suggestions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewNote {
    pub reason: Option<String>,
    pub suggested: bool,
}

/// One suggestion inside a proposal, as the user reviews it: a handle their choice refers to, the
/// other suggestions it can't be applied without, and the change itself.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProposedOperation {
    pub id: String,
    /// Suggestions that have to be accepted alongside this one. A task can't join a plan the same
    /// proposal creates unless that plan is created too.
    pub depends_on: Vec<String>,
    /// Why the planner chose this, when the request didn't make it obvious.
    pub reason: Option<String>,
    /// Whether the day or week in it is the planner's own idea, asked for but never stated.
    pub suggested: bool,
    pub change: MutationOperation,
}

impl ProposedOperation {
    /// The handle for the operation at `index`. Positional, because a proposal is reviewed once
    /// and applied once: nothing outlives the pending proposal it belongs to.
    pub fn handle(index: usize) -> String {
        format!("op-{index}")
    }

    /// Wraps a proposal's operations for review, working out which ones depend on which. An
    /// operation depends on another when it refers to a plan or milestone that one creates.
    /// `notes` carries what the planner said about each, in the same order.
    pub fn review(operations: &[MutationOperation], notes: &[ReviewNote]) -> Vec<Self> {
        // Titles match the way applying resolves them, ignoring case and spacing, so "charity
        // WEEK" depends on the suggestion that creates "Charity week".
        let creates: HashMap<(RecordKind, String), String> = operations
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| {
                operation
                    .creates()
                    .map(|(kind, title)| ((kind, crate::db::fold(title)), Self::handle(index)))
            })
            .collect();
        operations
            .iter()
            .enumerate()
            .map(|(index, operation)| {
                let mut depends_on: Vec<String> = operation
                    .new_references()
                    .into_iter()
                    .filter_map(|(kind, title)| {
                        creates.get(&(kind, crate::db::fold(title))).cloned()
                    })
                    .collect();
                depends_on.sort();
                depends_on.dedup();
                let note = notes.get(index).cloned().unwrap_or_default();
                Self {
                    id: Self::handle(index),
                    depends_on,
                    reason: note.reason,
                    suggested: note.suggested,
                    change: operation.clone(),
                }
            })
            .collect()
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
        operations: Vec<ProposedOperation>,
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
        Workstream => "workstream",
        InboxItem => "inbox_item",
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
    pub workstream_ids: Vec<String>,
}

/// The records the planner may reference for one request, chosen by relevance.
#[derive(Debug, Clone, Default)]
pub struct PlannerCandidates {
    pub events: Vec<ScheduleEvent>,
    pub plans: Vec<Plan>,
    pub milestones: Vec<Milestone>,
    pub tasks: Vec<Task>,
    /// The workstreams of the candidate plans, by name only.
    pub workstreams: Vec<Workstream>,
    /// Captured items, only when the request talks about the inbox. Their notes stay behind.
    pub inbox_items: Vec<InboxItem>,
    /// Spare time on the coming days, for requests that ask the planner to choose a day.
    pub planning: Option<PlanningFacts>,
}

/// What the planner may know about the coming days when it's asked to pick one: how much time is
/// left after events and planned work, and which days have no working time at all. Totals only;
/// nothing about what fills a day.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanningFacts {
    pub days: Vec<DayFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayFacts {
    pub day: String,
    /// Working time left once events and the estimates of work already on the day are placed,
    /// never more than the daily limit leaves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spare_minutes: Option<i64>,
    /// A day with no working time: outside working hours or a day off in the planning profile.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
}

impl PlanningFacts {
    pub fn from_capacity(capacity: &Capacity) -> Self {
        let limit = capacity.planned_limit_minutes;
        Self {
            days: capacity
                .days
                .iter()
                .map(|day| {
                    let off = day.working_minutes == 0;
                    let room = limit.map_or(day.available_minutes, |limit| {
                        day.available_minutes.min(limit)
                    });
                    DayFacts {
                        day: day.day.clone(),
                        spare_minutes: (!off).then(|| (room - day.planned_minutes).max(0)),
                        off,
                    }
                })
                .collect(),
        }
    }

    /// Whether `day` has no working time at all.
    pub fn is_off(&self, day: &str) -> bool {
        self.days.iter().any(|facts| facts.day == day && facts.off)
    }
}
