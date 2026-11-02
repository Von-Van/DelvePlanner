//! Heads-ups: things worth the person's attention that the assistant brings up before it is
//! asked, such as a deadline with no time planned or two commitments at the same time.
//!
//! Fixed rules in [`crate::heads_up`] decide what is worth saying. This file keeps what they found:
//!
//! - `agent_suggestion`: one heads-up, keyed by a dedupe key that names exactly what it is about
//!   (for example a task and its due date). A key the person dismissed or acted on is never shown
//!   again. One that stops being true is cleared on the next check.
//! - `heads_up_settings`: whether heads-ups are on, and whether urgent ones may send a system
//!   notification.
//!
//! Like the assistant's other records, none of this is planner data: it is never exported and
//! imports leave it alone. The model never writes it.

use super::planning::{task_from_row, TASK_SELECT};
use super::{collect, utc_string, PlannerDatabase};
use crate::error::{AppError, AppResult};
use crate::model::Task;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// At most this many heads-ups are shown at once. Fewer is normal; there is no quota to fill.
pub const MAX_SHOWN_HEADS_UPS: u32 = 3;
/// At most this many heads-ups are newly brought up in one local day, however many rules fire.
pub const MAX_NEW_HEADS_UPS_PER_DAY: i64 = 6;
/// At most this many system notifications are sent in one local day.
pub const MAX_NOTIFICATIONS_PER_DAY: i64 = 2;
/// Finished heads-ups (dismissed, acted on, cleared, expired) are forgotten after this long.
const RESOLVED_RETENTION_DAYS: i64 = 60;
const MAX_TITLE_CHARS: usize = 200;
const MAX_EVIDENCE_CHARS: usize = 600;
const MAX_REQUEST_CHARS: usize = 1_000;

// ---------------------------------------------------------------------------------------------
// Schema

/// Schema 12: heads-ups and their settings.
pub(super) fn ensure_heads_up_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_suggestion (
             id TEXT PRIMARY KEY NOT NULL,
             dedupe_key TEXT NOT NULL UNIQUE,
             kind TEXT NOT NULL
                 CHECK(kind IN ('deadline', 'clash', 'focus_window', 'overloaded',
                               'pushed_back', 'quiet_project')),
             title TEXT NOT NULL CHECK(length(title) > 0),
             evidence TEXT NOT NULL,
             target_kind TEXT CHECK(target_kind IN ('task', 'plan', 'day', 'event', 'block')),
             target_id TEXT,
             action_label TEXT NOT NULL,
             action_request TEXT NOT NULL,
             score INTEGER NOT NULL,
             urgent INTEGER NOT NULL DEFAULT 0 CHECK(urgent IN (0, 1)),
             status TEXT NOT NULL
                 CHECK(status IN ('active', 'snoozed', 'dismissed', 'acted', 'cleared',
                                  'expired')),
             surfaced_at TEXT NOT NULL,
             last_seen_at TEXT NOT NULL,
             expires_at TEXT NOT NULL,
             snoozed_until TEXT,
             resolved_at TEXT,
             notified_at TEXT,
             CHECK((status = 'snoozed') = (snoozed_until IS NOT NULL)),
             CHECK((target_kind IS NULL) = (target_id IS NULL))
         );
         CREATE INDEX IF NOT EXISTS agent_suggestion_status_idx
             ON agent_suggestion(status, score);

         CREATE TABLE IF NOT EXISTS heads_up_settings (
             id INTEGER PRIMARY KEY NOT NULL CHECK(id = 1),
             enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
             notify INTEGER NOT NULL DEFAULT 1 CHECK(notify IN (0, 1))
         );
         INSERT OR IGNORE INTO heads_up_settings (id) VALUES (1);",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Types

/// Which fixed rule found a heads-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsUpKind {
    /// A deadline is near (or just passed) and no time is planned for it.
    Deadline,
    /// Two commitments overlap in the next two days.
    Clash,
    /// A real stretch of free time today, while important work has no time planned.
    FocusWindow,
    /// A coming day holds more planned work than time.
    Overloaded,
    /// Open work keeps being carried off its day.
    PushedBack,
    /// An active project nobody has touched for weeks.
    QuietProject,
}

impl HeadsUpKind {
    /// How many of this kind may show (or be snoozed) at once. A rule may find more; the rest
    /// wait, so dismissing one lets the next come through instead of hiding them all.
    pub fn max_showing(self) -> usize {
        match self {
            // Each deadline is a different piece of work: only the screen's own cap applies.
            Self::Deadline => usize::MAX,
            Self::Clash => 2,
            Self::FocusWindow | Self::Overloaded | Self::PushedBack | Self::QuietProject => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::Clash => "clash",
            Self::FocusWindow => "focus_window",
            Self::Overloaded => "overloaded",
            Self::PushedBack => "pushed_back",
            Self::QuietProject => "quiet_project",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        Ok(match value {
            "deadline" => Self::Deadline,
            "clash" => Self::Clash,
            "focus_window" => Self::FocusWindow,
            "overloaded" => Self::Overloaded,
            "pushed_back" => Self::PushedBack,
            "quiet_project" => Self::QuietProject,
            other => return Err(bad_text(format!("unknown heads-up kind '{other}'"))),
        })
    }
}

/// What a heads-up is about, so acting on it can start from the right record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsUpTargetKind {
    Task,
    Plan,
    Day,
    Event,
    Block,
}

impl HeadsUpTargetKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Plan => "plan",
            Self::Day => "day",
            Self::Event => "event",
            Self::Block => "block",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        Ok(match value {
            "task" => Self::Task,
            "plan" => Self::Plan,
            "day" => Self::Day,
            "event" => Self::Event,
            "block" => Self::Block,
            other => return Err(bad_text(format!("unknown heads-up target '{other}'"))),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadsUpTarget {
    pub kind: HeadsUpTargetKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsUpStatus {
    Active,
    /// Hidden until `snoozed_until`, then shown again if it is still true.
    Snoozed,
    /// The person said not to show it again.
    Dismissed,
    /// The person asked the assistant to deal with it.
    Acted,
    /// It stopped being true (the time got planned, the clash was moved, …).
    Cleared,
    /// What it was about is over.
    Expired,
}

impl HeadsUpStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Snoozed => "snoozed",
            Self::Dismissed => "dismissed",
            Self::Acted => "acted",
            Self::Cleared => "cleared",
            Self::Expired => "expired",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        Ok(match value {
            "active" => Self::Active,
            "snoozed" => Self::Snoozed,
            "dismissed" => Self::Dismissed,
            "acted" => Self::Acted,
            "cleared" => Self::Cleared,
            "expired" => Self::Expired,
            other => return Err(bad_text(format!("unknown heads-up status '{other}'"))),
        })
    }
}

/// What one rule found on one check. Storing it decides whether it is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct NewHeadsUp {
    pub kind: HeadsUpKind,
    /// Names exactly what this is about. The same key is the same heads-up.
    pub key: String,
    pub title: String,
    /// The facts behind it, so it is never a suggestion without a reason.
    pub evidence: String,
    pub target: Option<HeadsUpTarget>,
    /// The button that hands it to the assistant ("Plan it").
    pub action_label: String,
    /// What the assistant is asked when the person presses that button.
    pub action_request: String,
    /// Higher is more worth saying. Only the top few are shown.
    pub score: i64,
    /// Whether it may send a system notification (outside quiet hours, within the daily limit).
    pub urgent: bool,
    pub expires_at: DateTime<Utc>,
}

/// A stored heads-up, as the Assistant screen shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadsUp {
    pub id: String,
    pub kind: HeadsUpKind,
    pub title: String,
    pub evidence: String,
    pub target: Option<HeadsUpTarget>,
    pub action_label: String,
    pub action_request: String,
    pub score: i64,
    pub urgent: bool,
    pub status: HeadsUpStatus,
    pub surfaced_at: String,
    pub expires_at: String,
    pub snoozed_until: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeadsUpSettings {
    /// Whether the assistant looks for heads-ups at all.
    pub enabled: bool,
    /// Whether urgent heads-ups may also send a system notification.
    pub notify: bool,
}

/// What one check changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeadsUpRefresh {
    /// Whether anything shown may have changed, so the screen should reload.
    pub changed: bool,
    /// Heads-ups brought up for the first time (or again, after they had cleared).
    pub surfaced: usize,
    /// Heads-ups the daily limit held back. A later check tries again.
    pub held_back: usize,
}

/// An open task that keeps being carried off its day.
#[derive(Debug, Clone, PartialEq)]
pub struct PushedTask {
    pub task_id: String,
    pub title: String,
    pub moves: i64,
}

/// An active project with open work and no recent change.
#[derive(Debug, Clone, PartialEq)]
pub struct QuietPlan {
    pub plan_id: String,
    pub title: String,
    pub open_tasks: i64,
    pub last_activity: DateTime<Utc>,
}

/// A time block that hasn't ended yet, with its task.
#[derive(Debug, Clone, PartialEq)]
pub struct UpcomingBlock {
    pub id: String,
    pub task_id: String,
    pub task_title: String,
    pub task_done: bool,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// What the heads-up rules read from the planner database. Calendars and capacity are added by
/// [`crate::heads_up::collect`].
#[derive(Debug, Clone, Default)]
pub struct PlannerHeadsUpFacts {
    /// Open tasks due from a week ago through `due_until`, soonest first.
    pub due_tasks: Vec<Task>,
    /// Open tasks that matter now: high or critical priority, or due within a week.
    pub important_open: Vec<Task>,
    /// Time blocks that haven't ended, starting within two weeks.
    pub blocks: Vec<UpcomingBlock>,
    pub pushed_back: Vec<PushedTask>,
    pub quiet_plans: Vec<QuietPlan>,
}

// ---------------------------------------------------------------------------------------------
// Reading the planner

/// How many times a task has to be carried off its day before it counts as pushed back.
pub const PUSHED_BACK_MOVES: i64 = 3;

impl PlannerDatabase {
    /// The planner records the heads-up rules look at, for `today` (a local day) and `now`.
    pub fn heads_up_facts(
        &self,
        now: DateTime<Utc>,
        today: NaiveDate,
        due_until: NaiveDate,
    ) -> AppResult<PlannerHeadsUpFacts> {
        let week_ago = today - Duration::days(7);
        let week_ahead = today + Duration::days(7);
        let mut statement = self.connection.prepare(&format!(
            "{TASK_SELECT} WHERE status <> 'done' AND due_date IS NOT NULL
               AND due_date >= ?1 AND due_date <= ?2
             ORDER BY due_date ASC, sort_order ASC, created_at ASC
             LIMIT 50"
        ))?;
        let due_tasks = collect(statement.query_map(
            params![week_ago.to_string(), due_until.to_string()],
            task_from_row,
        )?)?;

        let mut statement = self.connection.prepare(&format!(
            "{TASK_SELECT} WHERE status <> 'done'
               AND (priority IN ('high', 'critical')
                    OR (due_date IS NOT NULL AND due_date <= ?1))
             ORDER BY CASE priority WHEN 'critical' THEN 0 WHEN 'high' THEN 1 ELSE 2 END,
                      COALESCE(due_date, '9999-12-31') ASC, sort_order ASC
             LIMIT 50"
        ))?;
        let important_open =
            collect(statement.query_map(params![week_ahead.to_string()], task_from_row)?)?;

        let now_text = utc_string(now);
        let horizon = utc_string(now + Duration::days(14));
        let mut statement = self.connection.prepare(
            "SELECT task_blocks.id, task_blocks.task_id, tasks.title, tasks.status,
                    task_blocks.start_at_utc, task_blocks.duration_minutes
             FROM task_blocks JOIN tasks ON tasks.id = task_blocks.task_id
             WHERE julianday(task_blocks.start_at_utc,
                             printf('+%d minutes', task_blocks.duration_minutes))
                   > julianday(?1)
               AND julianday(task_blocks.start_at_utc) < julianday(?2)
             ORDER BY task_blocks.start_at_utc ASC
             LIMIT 500",
        )?;
        let rows = statement
            .query_map(params![now_text, horizon], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let blocks = rows
            .into_iter()
            .filter_map(|(id, task_id, task_title, status, start, minutes)| {
                let start: DateTime<Utc> = start.parse().ok()?;
                Some(UpcomingBlock {
                    id,
                    task_id,
                    task_title,
                    task_done: status == "done",
                    start,
                    end: start + Duration::minutes(minutes),
                })
            })
            .collect();

        let mut statement = self.connection.prepare(
            "SELECT tasks.id, tasks.title, COUNT(*) AS moves
             FROM task_moves JOIN tasks ON tasks.id = task_moves.task_id
             WHERE task_moves.kind = 'carried_forward' AND tasks.status <> 'done'
             GROUP BY tasks.id
             HAVING COUNT(*) >= ?1
             ORDER BY moves DESC, MIN(task_moves.moved_at) ASC
             LIMIT 3",
        )?;
        let pushed_back = statement
            .query_map(params![PUSHED_BACK_MOVES], |row| {
                Ok(PushedTask {
                    task_id: row.get(0)?,
                    title: row.get(1)?,
                    moves: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut statement = self.connection.prepare(
            "SELECT plans.id, plans.title, COUNT(tasks.id),
                    MAX(plans.updated_at,
                        COALESCE((SELECT MAX(updated_at) FROM tasks AS touched
                                  WHERE touched.plan_id = plans.id), plans.updated_at))
             FROM plans JOIN tasks ON tasks.plan_id = plans.id AND tasks.status <> 'done'
             WHERE plans.archived = 0 AND plans.status = 'active'
             GROUP BY plans.id
             ORDER BY plans.title ASC
             LIMIT 100",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let quiet_plans = rows
            .into_iter()
            .filter_map(|(plan_id, title, open_tasks, last)| {
                Some(QuietPlan {
                    plan_id,
                    title,
                    open_tasks,
                    last_activity: last.parse().ok()?,
                })
            })
            .collect();

        Ok(PlannerHeadsUpFacts {
            due_tasks,
            important_open,
            blocks,
            pushed_back,
            quiet_plans,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Storing what the rules found

const HEADS_UP_SELECT: &str = "SELECT id, kind, title, evidence, target_kind, target_id,
        action_label, action_request, score, urgent, status, surfaced_at, expires_at,
        snoozed_until
     FROM agent_suggestion";

impl PlannerDatabase {
    pub fn heads_up_settings(&self) -> AppResult<HeadsUpSettings> {
        Ok(self.connection.query_row(
            "SELECT enabled, notify FROM heads_up_settings WHERE id = 1",
            [],
            |row| {
                Ok(HeadsUpSettings {
                    enabled: row.get::<_, i64>(0)? == 1,
                    notify: row.get::<_, i64>(1)? == 1,
                })
            },
        )?)
    }

    /// Saves the settings. Turning heads-ups off clears the ones on screen; dismissals are kept.
    pub fn update_heads_up_settings(
        &mut self,
        settings: HeadsUpSettings,
        now: DateTime<Utc>,
    ) -> AppResult<HeadsUpSettings> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE heads_up_settings SET enabled = ?1, notify = ?2 WHERE id = 1",
            params![i64::from(settings.enabled), i64::from(settings.notify)],
        )?;
        if !settings.enabled {
            transaction.execute(
                "UPDATE agent_suggestion
                 SET status = 'cleared', snoozed_until = NULL, resolved_at = ?1
                 WHERE status IN ('active', 'snoozed')",
                params![utc_string(now)],
            )?;
        }
        transaction.commit()?;
        self.heads_up_settings()
    }

    /// Records one check's findings. `day_start` is the start of the local day in UTC, for the
    /// daily limit. Returns what changed.
    ///
    /// - A new key is shown, unless the daily limit is used up.
    /// - A key already showing (or snoozed) gets the fresh wording and expiry.
    /// - A key the person dismissed or acted on stays hidden for good.
    /// - A key that had cleared or expired and is true again is shown again, within the limit.
    /// - A showing key the rules no longer find is cleared.
    pub fn record_heads_ups(
        &mut self,
        found: &[NewHeadsUp],
        now: DateTime<Utc>,
        day_start: DateTime<Utc>,
    ) -> AppResult<HeadsUpRefresh> {
        let now_text = utc_string(now);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcome = HeadsUpRefresh {
            changed: tidy(&transaction, now)?,
            ..HeadsUpRefresh::default()
        };
        let mut surfaced_today: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM agent_suggestion WHERE surfaced_at >= ?1",
            params![utc_string(day_start)],
            |row| row.get(0),
        )?;
        let mut ordered: Vec<&NewHeadsUp> = found
            .iter()
            .filter(|heads_up| heads_up.expires_at > now)
            .collect();
        ordered.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.key.cmp(&b.key)));
        let found_keys: HashSet<&str> =
            found.iter().map(|heads_up| heads_up.key.as_str()).collect();
        // What already shows (or is snoozed) and is still true counts against each kind's cap.
        let mut showing_by_kind: HashMap<HeadsUpKind, usize> = HashMap::new();
        {
            let mut statement = transaction.prepare(
                "SELECT kind, dedupe_key FROM agent_suggestion
                 WHERE status IN ('active', 'snoozed')",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        HeadsUpKind::parse(&row.get::<_, String>(0)?)?,
                        row.get::<_, String>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (kind, key) in rows {
                if found_keys.contains(key.as_str()) {
                    *showing_by_kind.entry(kind).or_default() += 1;
                }
            }
        }
        let mut seen = HashSet::new();
        for heads_up in ordered {
            if !seen.insert(heads_up.key.as_str()) {
                continue;
            }
            let existing = transaction
                .query_row(
                    "SELECT id, status, title, evidence, score FROM agent_suggestion
                     WHERE dedupe_key = ?1",
                    params![heads_up.key],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            HeadsUpStatus::parse(&row.get::<_, String>(1)?)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            let title = clip(&heads_up.title, MAX_TITLE_CHARS);
            let evidence = clip(&heads_up.evidence, MAX_EVIDENCE_CHARS);
            let request = clip(&heads_up.action_request, MAX_REQUEST_CHARS);
            let (target_kind, target_id) = match &heads_up.target {
                Some(target) => (Some(target.kind.as_str()), Some(target.id.as_str())),
                None => (None, None),
            };
            match existing {
                None => {
                    if showing_by_kind.get(&heads_up.kind).copied().unwrap_or(0)
                        >= heads_up.kind.max_showing()
                    {
                        continue;
                    }
                    if surfaced_today >= MAX_NEW_HEADS_UPS_PER_DAY {
                        outcome.held_back += 1;
                        continue;
                    }
                    transaction.execute(
                        "INSERT INTO agent_suggestion (
                             id, dedupe_key, kind, title, evidence, target_kind, target_id,
                             action_label, action_request, score, urgent, status, surfaced_at,
                             last_seen_at, expires_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'active', ?12,
                                 ?12, ?13)",
                        params![
                            Uuid::new_v4().to_string(),
                            heads_up.key,
                            heads_up.kind.as_str(),
                            title,
                            evidence,
                            target_kind,
                            target_id,
                            heads_up.action_label,
                            request,
                            heads_up.score,
                            i64::from(heads_up.urgent),
                            now_text,
                            utc_string(heads_up.expires_at),
                        ],
                    )?;
                    surfaced_today += 1;
                    *showing_by_kind.entry(heads_up.kind).or_default() += 1;
                    outcome.surfaced += 1;
                    outcome.changed = true;
                }
                Some((id, status, old_title, old_evidence, old_score)) => match status {
                    HeadsUpStatus::Dismissed | HeadsUpStatus::Acted => {}
                    HeadsUpStatus::Active | HeadsUpStatus::Snoozed => {
                        transaction.execute(
                            "UPDATE agent_suggestion
                             SET title = ?2, evidence = ?3, target_kind = ?4, target_id = ?5,
                                 action_label = ?6, action_request = ?7, score = ?8, urgent = ?9,
                                 last_seen_at = ?10, expires_at = ?11
                             WHERE id = ?1",
                            params![
                                id,
                                title,
                                evidence,
                                target_kind,
                                target_id,
                                heads_up.action_label,
                                request,
                                heads_up.score,
                                i64::from(heads_up.urgent),
                                now_text,
                                utc_string(heads_up.expires_at),
                            ],
                        )?;
                        if status == HeadsUpStatus::Active
                            && (old_title != title
                                || old_evidence != evidence
                                || old_score != heads_up.score)
                        {
                            outcome.changed = true;
                        }
                    }
                    HeadsUpStatus::Cleared | HeadsUpStatus::Expired => {
                        if showing_by_kind.get(&heads_up.kind).copied().unwrap_or(0)
                            >= heads_up.kind.max_showing()
                        {
                            continue;
                        }
                        if surfaced_today >= MAX_NEW_HEADS_UPS_PER_DAY {
                            outcome.held_back += 1;
                            continue;
                        }
                        // Shown again because it is true again. A notification already sent
                        // for this key is not sent twice.
                        transaction.execute(
                            "UPDATE agent_suggestion
                             SET status = 'active', title = ?2, evidence = ?3, target_kind = ?4,
                                 target_id = ?5, action_label = ?6, action_request = ?7,
                                 score = ?8, urgent = ?9, surfaced_at = ?10, last_seen_at = ?10,
                                 expires_at = ?11, resolved_at = NULL
                             WHERE id = ?1",
                            params![
                                id,
                                title,
                                evidence,
                                target_kind,
                                target_id,
                                heads_up.action_label,
                                request,
                                heads_up.score,
                                i64::from(heads_up.urgent),
                                now_text,
                                utc_string(heads_up.expires_at),
                            ],
                        )?;
                        surfaced_today += 1;
                        *showing_by_kind.entry(heads_up.kind).or_default() += 1;
                        outcome.surfaced += 1;
                        outcome.changed = true;
                    }
                },
            }
        }

        // Whatever is showing but no longer found has stopped being true.
        let showing = {
            let mut statement = transaction.prepare(
                "SELECT id, dedupe_key, status FROM agent_suggestion
                 WHERE status IN ('active', 'snoozed')",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, key, status) in showing {
            if found_keys.contains(key.as_str()) {
                continue;
            }
            transaction.execute(
                "UPDATE agent_suggestion
                 SET status = 'cleared', snoozed_until = NULL, resolved_at = ?2
                 WHERE id = ?1",
                params![id, now_text],
            )?;
            if status == "active" {
                outcome.changed = true;
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    /// The heads-ups to show now, most worth saying first, at most `limit`.
    pub fn active_heads_ups(&mut self, now: DateTime<Utc>, limit: u32) -> AppResult<Vec<HeadsUp>> {
        tidy(&self.connection, now)?;
        let mut statement = self.connection.prepare(&format!(
            "{HEADS_UP_SELECT} WHERE status = 'active' AND expires_at > ?1
             ORDER BY score DESC, surfaced_at ASC, id ASC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![utc_string(now), limit.min(MAX_SHOWN_HEADS_UPS)],
            heads_up_from_row,
        )?;
        collect(rows)
    }

    /// Every stored heads-up, newest first, for tests and diagnostics.
    pub fn all_heads_ups(&self) -> AppResult<Vec<HeadsUp>> {
        let mut statement = self.connection.prepare(&format!(
            "{HEADS_UP_SELECT} ORDER BY surfaced_at DESC, id ASC"
        ))?;
        let rows = statement.query_map([], heads_up_from_row)?;
        collect(rows)
    }

    /// "Don't show this again." The same key is never brought up again.
    pub fn dismiss_heads_up(&mut self, id: &str, now: DateTime<Utc>) -> AppResult<()> {
        self.resolve_heads_up(id, HeadsUpStatus::Dismissed, now)
            .map(|_| ())
    }

    /// "Not now." Hidden until `until`, then shown again if it is still true.
    pub fn snooze_heads_up(
        &mut self,
        id: &str,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> AppResult<HeadsUp> {
        if until <= now {
            return Err(AppError::Validation(
                "A heads-up can only be snoozed until later.".into(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE agent_suggestion SET status = 'snoozed', snoozed_until = ?2
             WHERE id = ?1 AND status = 'active'",
            params![id, utc_string(until)],
        )?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        self.heads_up(id)
    }

    /// The person asked the assistant to deal with it. Returns it, with the request to send.
    pub fn act_on_heads_up(&mut self, id: &str, now: DateTime<Utc>) -> AppResult<HeadsUp> {
        self.resolve_heads_up(id, HeadsUpStatus::Acted, now)
    }

    fn resolve_heads_up(
        &mut self,
        id: &str,
        status: HeadsUpStatus,
        now: DateTime<Utc>,
    ) -> AppResult<HeadsUp> {
        let changed = self.connection.execute(
            "UPDATE agent_suggestion
             SET status = ?2, snoozed_until = NULL, resolved_at = ?3
             WHERE id = ?1 AND status IN ('active', 'snoozed')",
            params![id, status.as_str(), utc_string(now)],
        )?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        self.heads_up(id)
    }

    fn heads_up(&self, id: &str) -> AppResult<HeadsUp> {
        self.connection
            .query_row(
                &format!("{HEADS_UP_SELECT} WHERE id = ?1"),
                params![id],
                heads_up_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }

    /// Showing, urgent heads-ups that haven't sent a notification, within what is left of the
    /// day's notification limit.
    pub fn heads_ups_to_notify(
        &self,
        now: DateTime<Utc>,
        day_start: DateTime<Utc>,
    ) -> AppResult<Vec<HeadsUp>> {
        let sent_today: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM agent_suggestion WHERE notified_at >= ?1",
            params![utc_string(day_start)],
            |row| row.get(0),
        )?;
        let left = (MAX_NOTIFICATIONS_PER_DAY - sent_today).max(0);
        if left == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(&format!(
            "{HEADS_UP_SELECT} WHERE status = 'active' AND urgent = 1 AND notified_at IS NULL
               AND expires_at > ?1
             ORDER BY score DESC, surfaced_at ASC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![utc_string(now), left], heads_up_from_row)?;
        collect(rows)
    }

    pub fn mark_heads_up_notified(&mut self, id: &str, now: DateTime<Utc>) -> AppResult<()> {
        self.connection.execute(
            "UPDATE agent_suggestion SET notified_at = ?2 WHERE id = ?1",
            params![id, utc_string(now)],
        )?;
        Ok(())
    }
}

/// Wakes snoozed heads-ups whose time has come, expires showing ones whose subject is over, and forgets
/// long-finished ones. Returns whether anything on screen changed.
fn tidy(connection: &Connection, now: DateTime<Utc>) -> AppResult<bool> {
    let now_text = utc_string(now);
    let woken = connection.execute(
        "UPDATE agent_suggestion SET status = 'active', snoozed_until = NULL
         WHERE status = 'snoozed' AND snoozed_until <= ?1",
        params![now_text],
    )?;
    // A snoozed heads-up keeps its snooze past its expiry: if the next check still finds it (a
    // deadline due today that is overdue tomorrow), it stays hidden until the time promised.
    // Once the snooze ends it wakes above, and expires here if what it was about is over.
    let expired = connection.execute(
        "UPDATE agent_suggestion
         SET status = 'expired', resolved_at = ?1
         WHERE status = 'active' AND expires_at <= ?1",
        params![now_text],
    )?;
    connection.execute(
        "DELETE FROM agent_suggestion
         WHERE status NOT IN ('active', 'snoozed') AND resolved_at < ?1",
        params![utc_string(now - Duration::days(RESOLVED_RETENTION_DAYS))],
    )?;
    Ok(woken > 0 || expired > 0)
}

fn heads_up_from_row(row: &Row<'_>) -> rusqlite::Result<HeadsUp> {
    let target_kind: Option<String> = row.get(4)?;
    let target_id: Option<String> = row.get(5)?;
    let target = match (target_kind, target_id) {
        (Some(kind), Some(id)) => Some(HeadsUpTarget {
            kind: HeadsUpTargetKind::parse(&kind)?,
            id,
        }),
        _ => None,
    };
    Ok(HeadsUp {
        id: row.get(0)?,
        kind: HeadsUpKind::parse(&row.get::<_, String>(1)?)?,
        title: row.get(2)?,
        evidence: row.get(3)?,
        target,
        action_label: row.get(6)?,
        action_request: row.get(7)?,
        score: row.get(8)?,
        urgent: row.get::<_, i64>(9)? == 1,
        status: HeadsUpStatus::parse(&row.get::<_, String>(10)?)?,
        surfaced_at: row.get(11)?,
        expires_at: row.get(12)?,
        snoozed_until: row.get(13)?,
    })
}

fn clip(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut clipped: String = value.chars().take(limit.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

fn bad_text(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::<dyn std::error::Error + Send + Sync>::from(message),
    )
}
