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
    /// An account DayPlan can read calendars from. Both are read-only: DayPlan asks for
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
                "DayPlan couldn't reach the calendar service. It will try again automatically."
            }
            Self::RateLimited => {
                "The calendar service asked DayPlan to slow down. It will try again later."
            }
            Self::ServerError => {
                "The calendar service had a problem. DayPlan will try again automatically."
            }
            Self::LinkUnavailable => {
                "DayPlan couldn't read this calendar's link from the system keychain. Paste the link again."
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

/// The hours DayPlan counts as available for planned work, in the viewer's local time.
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

/// An account DayPlan reads calendars from. Its tokens stay in the system keychain and never
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

/// A calendar offered by a connected account, for choosing which ones DayPlan shows.
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
    pub planned_week: Option<String>,
    #[serde(default)]
    pub estimated_minutes: Option<i64>,
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
    pub planned_week: Option<String>,
    pub estimated_minutes: Option<i64>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
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

/// One local day's planning hours against what already fills them. Busy time is DayPlan events
/// and busy events from visible calendars inside working hours; time blocks are planned work,
/// so they reduce `free` but not `available_minutes`.
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

/// Planned work against available time for a window of local days. DayPlan reports these numbers
/// and never rearranges anything because of them.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub working_hours: WorkingHours,
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
    /// The plans and milestones this operation needs the same proposal to create. An operation
    /// refers to one by title until it exists, which is what makes it depend on another.
    pub fn new_references(&self) -> Vec<(RecordKind, &str)> {
        fn title(reference: Option<&RecordRef>) -> Option<&str> {
            match reference {
                Some(RecordRef::New(NewRef { new_title })) => Some(new_title.as_str()),
                _ => None,
            }
        }
        let (plan, milestone) = match self {
            Self::CreateEvent { plan, .. } | Self::SetEventPlan { plan, .. } => {
                (title(plan.as_ref()), None)
            }
            Self::CreateMilestone { plan, .. } => (title(Some(plan)), None),
            Self::CreateTask {
                plan, milestone, ..
            }
            | Self::SetTaskPlan {
                plan, milestone, ..
            } => (title(plan.as_ref()), title(milestone.as_ref())),
            _ => (None, None),
        };
        plan.map(|title| (RecordKind::Plan, title))
            .into_iter()
            .chain(milestone.map(|title| (RecordKind::Milestone, title)))
            .collect()
    }
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
        let creates: HashMap<(RecordKind, &str), String> = operations
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| match operation {
                MutationOperation::CreatePlan { title, .. } => {
                    Some(((RecordKind::Plan, title.as_str()), Self::handle(index)))
                }
                MutationOperation::CreateMilestone { title, .. } => {
                    Some(((RecordKind::Milestone, title.as_str()), Self::handle(index)))
                }
                _ => None,
            })
            .collect();
        operations
            .iter()
            .enumerate()
            .map(|(index, operation)| {
                let mut depends_on: Vec<String> = operation
                    .new_references()
                    .into_iter()
                    .filter_map(|(kind, title)| creates.get(&(kind, title)).cloned())
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
