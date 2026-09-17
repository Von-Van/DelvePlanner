//! The calendar cache: read-only calendars and their events in a SQLite file kept apart from the
//! planner database, so it is never exported, backed up, or restored with planner data. Events
//! here never get DayPlan event IDs. Removing a calendar removes everything cached for it.

use super::ics::{CalendarRead, EventTime, SyncWindow};
use crate::error::{AppError, AppResult};
use crate::model::{
    Calendar, CalendarAccount, CalendarIssue, CalendarKind, CalendarProblem, CalendarProvider,
    ExternalEvent, PlanColor, UpdateCalendarInput, MAX_CALENDARS, MAX_CALENDAR_NAME_LENGTH,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use std::fs;
use std::path::Path;

const STORE_VERSION: u32 = 2;
const CALENDAR_SELECT: &str = "SELECT id, kind, name, color, visible, source_label, problem,
        last_synced_at, last_attempt_at, event_count, revision, created_at, updated_at, account_id
     FROM calendars";
const ACCOUNT_SELECT: &str = "SELECT account.id, account.provider, account.label, account.problem,
        (SELECT COUNT(*) FROM calendars WHERE account_id = account.id), account.revision,
        account.created_at, account.updated_at
     FROM calendar_accounts AS account";

pub struct CalendarStore {
    connection: Connection,
}

/// A calendar about to be stored with its first successful read.
pub struct NewCalendar<'a> {
    pub id: &'a str,
    pub kind: CalendarKind,
    pub name: &'a str,
    pub color: PlanColor,
    pub source_label: &'a str,
    /// The connected account and the calendar's ID there, for account calendars.
    pub account_id: Option<&'a str>,
    pub remote_id: Option<&'a str>,
}

/// A connected account about to be stored.
pub struct NewAccount<'a> {
    pub id: &'a str,
    pub provider: CalendarProvider,
    pub label: &'a str,
}

/// Where a calendar's events come from, for the refresh worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarSource {
    pub id: String,
    pub kind: CalendarKind,
    pub account_id: Option<String>,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// The content of a successful read and the events it produced for `window`. Account calendars
/// have no document to keep, because their events are fetched already expanded.
pub struct Snapshot<'a> {
    pub content: Option<&'a str>,
    pub read: &'a CalendarRead,
    pub window: &'a SyncWindow,
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
}

impl CalendarStore {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::with_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> AppResult<Self> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(connection: Connection) -> AppResult<Self> {
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > STORE_VERSION {
            return Err(AppError::UnsupportedDatabaseVersion);
        }
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS calendars (
                 id TEXT PRIMARY KEY NOT NULL,
                 kind TEXT NOT NULL,
                 name TEXT NOT NULL,
                 color TEXT NOT NULL,
                 visible INTEGER NOT NULL DEFAULT 1 CHECK(visible IN (0, 1)),
                 source_label TEXT NOT NULL DEFAULT '',
                 problem TEXT,
                 last_synced_at TEXT,
                 last_attempt_at TEXT,
                 next_refresh_at TEXT,
                 etag TEXT,
                 last_modified TEXT,
                 window_first_day TEXT,
                 window_end_day TEXT,
                 window_zone TEXT,
                 account_id TEXT REFERENCES calendar_accounts(id) ON DELETE CASCADE,
                 remote_id TEXT,
                 event_count INTEGER NOT NULL DEFAULT 0,
                 revision INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS calendar_accounts (
                 id TEXT PRIMARY KEY NOT NULL,
                 provider TEXT NOT NULL,
                 label TEXT NOT NULL,
                 problem TEXT,
                 revision INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS calendar_documents (
                 calendar_id TEXT PRIMARY KEY NOT NULL
                     REFERENCES calendars(id) ON DELETE CASCADE,
                 content TEXT NOT NULL,
                 stored_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS calendar_events (
                 calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
                 instance_key TEXT NOT NULL,
                 title TEXT NOT NULL,
                 location TEXT NOT NULL DEFAULT '',
                 all_day INTEGER NOT NULL CHECK(all_day IN (0, 1)),
                 start_at_utc TEXT,
                 end_at_utc TEXT,
                 start_date TEXT,
                 end_date TEXT,
                 busy INTEGER NOT NULL CHECK(busy IN (0, 1)),
                 tentative INTEGER NOT NULL CHECK(tentative IN (0, 1)),
                 recurring INTEGER NOT NULL CHECK(recurring IN (0, 1)),
                 PRIMARY KEY (calendar_id, instance_key),
                 CHECK((all_day = 0 AND start_at_utc IS NOT NULL AND end_at_utc IS NOT NULL
                        AND start_date IS NULL AND end_date IS NULL)
                    OR (all_day = 1 AND start_date IS NOT NULL AND end_date IS NOT NULL
                        AND start_at_utc IS NULL AND end_at_utc IS NULL))
             );
             CREATE INDEX IF NOT EXISTS calendar_events_timed_idx
                 ON calendar_events(start_at_utc) WHERE all_day = 0;
             CREATE INDEX IF NOT EXISTS calendar_events_dated_idx
                 ON calendar_events(start_date) WHERE all_day = 1;
             COMMIT;",
        )?;
        // Store 1 predates connected accounts, so its calendars table has no link to one.
        for (column, definition) in [
            (
                "account_id",
                "TEXT REFERENCES calendar_accounts(id) ON DELETE CASCADE",
            ),
            ("remote_id", "TEXT"),
        ] {
            let mut columns = connection.prepare("PRAGMA table_info(calendars)")?;
            let existing = columns
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(columns);
            if !existing.iter().any(|name| name == column) {
                connection.execute(
                    &format!("ALTER TABLE calendars ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        connection.pragma_update(None, "user_version", STORE_VERSION)?;
        Ok(Self { connection })
    }

    pub fn list(&self) -> AppResult<Vec<Calendar>> {
        let mut statement = self.connection.prepare(&format!(
            "{CALENDAR_SELECT} ORDER BY created_at ASC, rowid ASC"
        ))?;
        let rows = statement.query_map([], calendar_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn calendar(&self, id: &str) -> AppResult<Option<Calendar>> {
        calendar_by_id(&self.connection, id)
    }

    pub fn create(
        &mut self,
        calendar: NewCalendar<'_>,
        snapshot: &Snapshot<'_>,
        next_refresh: Option<DateTime<Utc>>,
    ) -> AppResult<Calendar> {
        let name = validate_name(calendar.name)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 =
            transaction.query_row("SELECT COUNT(*) FROM calendars", [], |row| row.get(0))?;
        if count as usize >= MAX_CALENDARS {
            return Err(AppError::Validation(format!(
                "DayPlan can show up to {MAX_CALENDARS} calendars. Remove one to add another."
            )));
        }
        let now = timestamp(Utc::now());
        transaction.execute(
            "INSERT INTO calendars (id, kind, name, color, source_label, account_id, remote_id,
                                    created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                calendar.id,
                calendar.kind.as_str(),
                name,
                calendar.color.as_str(),
                calendar.source_label,
                calendar.account_id,
                calendar.remote_id,
                now
            ],
        )?;
        write_snapshot(&transaction, calendar.id, snapshot, next_refresh)?;
        let created = calendar_by_id(&transaction, calendar.id)?.ok_or(AppError::NotFound)?;
        transaction.commit()?;
        Ok(created)
    }

    /// Renames, recolors, hides, or shows a calendar under a revision check.
    pub fn update(&mut self, input: &UpdateCalendarInput) -> AppResult<Calendar> {
        let name = validate_name(&input.name)?;
        let changed = self.connection.execute(
            "UPDATE calendars SET name = ?1, color = ?2, visible = ?3,
                 revision = revision + 1, updated_at = ?4
             WHERE id = ?5 AND revision = ?6",
            params![
                name,
                input.color.as_str(),
                input.visible,
                timestamp(Utc::now()),
                input.id,
                input.revision
            ],
        )?;
        self.after_revision_checked_write(&input.id, changed)
    }

    /// Replaces a calendar's source (a new link or a newer file) and everything read from it.
    pub fn replace_source(
        &mut self,
        id: &str,
        revision: i64,
        source_label: &str,
        snapshot: &Snapshot<'_>,
        next_refresh: Option<DateTime<Utc>>,
    ) -> AppResult<Calendar> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE calendars SET source_label = ?1, etag = NULL, last_modified = NULL,
                 revision = revision + 1, updated_at = ?2
             WHERE id = ?3 AND revision = ?4",
            params![source_label, timestamp(Utc::now()), id, revision],
        )?;
        if changed != 1 {
            return Err(stale_or_missing(&transaction, id)?);
        }
        write_snapshot(&transaction, id, snapshot, next_refresh)?;
        let calendar = calendar_by_id(&transaction, id)?.ok_or(AppError::NotFound)?;
        transaction.commit()?;
        Ok(calendar)
    }

    /// Stores a background refresh. Sync bookkeeping never changes a calendar's revision, so it
    /// can't conflict with an edit made at the same time.
    pub fn store_refresh(
        &mut self,
        id: &str,
        snapshot: &Snapshot<'_>,
        next_refresh: Option<DateTime<Utc>>,
    ) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if calendar_by_id(&transaction, id)?.is_none() {
            return Err(AppError::NotFound);
        }
        write_snapshot(&transaction, id, snapshot, next_refresh)?;
        transaction.commit()?;
        Ok(())
    }

    /// Re-reads stored content for a new window or zone without touching sync bookkeeping.
    pub fn store_window(
        &mut self,
        id: &str,
        read: &CalendarRead,
        window: &SyncWindow,
    ) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if calendar_by_id(&transaction, id)?.is_none() {
            return Err(AppError::NotFound);
        }
        let stored = write_events(&transaction, id, read)?;
        transaction.execute(
            "UPDATE calendars SET window_first_day = ?1, window_end_day = ?2, window_zone = ?3,
                 event_count = ?4
             WHERE id = ?5",
            params![
                window.first_day.to_string(),
                window.end_day.to_string(),
                window.zone.name(),
                stored as i64,
                id
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Records that the service confirmed the cached copy is current.
    pub fn record_unchanged(&mut self, id: &str, next_refresh: DateTime<Utc>) -> AppResult<()> {
        let now = timestamp(Utc::now());
        self.connection.execute(
            "UPDATE calendars SET problem = NULL, last_synced_at = ?1, last_attempt_at = ?1,
                 next_refresh_at = ?2
             WHERE id = ?3",
            params![now, timestamp(next_refresh), id],
        )?;
        Ok(())
    }

    /// Records a failed refresh. Cached events stay until the calendar recovers or is removed.
    pub fn record_problem(
        &mut self,
        id: &str,
        problem: CalendarProblem,
        next_refresh: Option<DateTime<Utc>>,
    ) -> AppResult<()> {
        self.connection.execute(
            "UPDATE calendars SET problem = ?1, last_attempt_at = ?2, next_refresh_at = ?3
             WHERE id = ?4",
            params![
                problem.as_str(),
                timestamp(Utc::now()),
                next_refresh.map(timestamp),
                id
            ],
        )?;
        Ok(())
    }

    /// Deletes a calendar with its events and stored content.
    pub fn remove(&mut self, id: &str, revision: i64) -> AppResult<()> {
        let changed = self.connection.execute(
            "DELETE FROM calendars WHERE id = ?1 AND revision = ?2",
            params![id, revision],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(stale_or_missing(&self.connection, id)?)
        }
    }

    /// Calendars that are fetched and whose next refresh is due. Imported files never are.
    pub fn due_refreshes(&self, now: DateTime<Utc>) -> AppResult<Vec<CalendarSource>> {
        let mut statement = self.connection.prepare(
            "SELECT id, kind, account_id, remote_id, etag, last_modified FROM calendars
             WHERE kind <> ?1 AND (next_refresh_at IS NULL OR next_refresh_at <= ?2)
             ORDER BY next_refresh_at ASC",
        )?;
        let rows = statement.query_map(
            params![CalendarKind::IcsFile.as_str(), timestamp(now)],
            source_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// Where one calendar's events come from.
    pub fn source(&self, id: &str) -> AppResult<Option<CalendarSource>> {
        self.connection
            .query_row(
                "SELECT id, kind, account_id, remote_id, etag, last_modified
                 FROM calendars WHERE id = ?1",
                params![id],
                source_from_row,
            )
            .optional()
            .map_err(AppError::from)
    }

    pub fn list_accounts(&self) -> AppResult<Vec<CalendarAccount>> {
        let mut statement = self
            .connection
            .prepare(&format!("{ACCOUNT_SELECT} ORDER BY account.created_at ASC"))?;
        let rows = statement.query_map([], account_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn account(&self, id: &str) -> AppResult<Option<CalendarAccount>> {
        account_by_id(&self.connection, id)
    }

    pub fn create_account(&mut self, account: NewAccount<'_>) -> AppResult<CalendarAccount> {
        let now = timestamp(Utc::now());
        self.connection.execute(
            "INSERT INTO calendar_accounts (id, provider, label, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![account.id, account.provider.as_str(), account.label, now],
        )?;
        account_by_id(&self.connection, account.id)?.ok_or(AppError::NotFound)
    }

    /// Records that an account needs attention, or that it is working again. Its calendars carry
    /// the same problem, so every view shows it.
    pub fn record_account_problem(
        &mut self,
        id: &str,
        problem: Option<CalendarProblem>,
    ) -> AppResult<()> {
        let now = timestamp(Utc::now());
        let code = problem.map(CalendarProblem::as_str);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE calendar_accounts SET problem = ?1, updated_at = ?2 WHERE id = ?3",
            params![code, now, id],
        )?;
        transaction.execute(
            "UPDATE calendars SET problem = ?1, last_attempt_at = ?2 WHERE account_id = ?3",
            params![code, now, id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Points an account at the address it signed in as, after a reconnect.
    pub fn rename_account(&mut self, id: &str, label: &str) -> AppResult<CalendarAccount> {
        self.connection.execute(
            "UPDATE calendar_accounts SET label = ?1, revision = revision + 1, updated_at = ?2
             WHERE id = ?3",
            params![label, timestamp(Utc::now()), id],
        )?;
        account_by_id(&self.connection, id)?.ok_or(AppError::NotFound)
    }

    /// Deletes an account, and with it every calendar and event cached from it.
    pub fn remove_account(&mut self, id: &str, revision: i64) -> AppResult<()> {
        let changed = self.connection.execute(
            "DELETE FROM calendar_accounts WHERE id = ?1 AND revision = ?2",
            params![id, revision],
        )?;
        if changed == 1 {
            return Ok(());
        }
        Err(if account_by_id(&self.connection, id)?.is_some() {
            AppError::Conflict
        } else {
            AppError::NotFound
        })
    }

    /// The calendars already added from an account, by their ID at the provider.
    pub fn account_remote_ids(&self, account_id: &str) -> AppResult<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT remote_id FROM calendars WHERE account_id = ?1 AND remote_id IS NOT NULL",
        )?;
        let rows = statement.query_map(params![account_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// Calendars with stored content that was read for a different window or zone.
    pub fn stale_windows(&self, window: &SyncWindow) -> AppResult<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT calendars.id FROM calendars
             JOIN calendar_documents ON calendar_documents.calendar_id = calendars.id
             WHERE window_first_day IS NOT ?1 OR window_end_day IS NOT ?2
                OR window_zone IS NOT ?3",
        )?;
        let rows = statement.query_map(
            params![
                window.first_day.to_string(),
                window.end_day.to_string(),
                window.zone.name()
            ],
            |row| row.get(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn content(&self, id: &str) -> AppResult<Option<String>> {
        self.connection
            .query_row(
                "SELECT content FROM calendar_documents WHERE calendar_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(AppError::from)
    }

    /// Events of visible calendars that overlap `[start_utc, end_utc)`, or for all-day events the
    /// local days `[first_day, end_day)`. All-day events come first, then by start.
    pub fn events(
        &self,
        first_day: &str,
        end_day: &str,
        start_utc: &str,
        end_utc: &str,
    ) -> AppResult<Vec<ExternalEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT event.calendar_id, event.instance_key, event.title, event.location,
                    event.all_day, event.start_at_utc, event.end_at_utc, event.start_date,
                    event.end_date, event.busy, event.tentative, event.recurring
             FROM calendar_events AS event
             JOIN calendars AS calendar ON calendar.id = event.calendar_id
             WHERE calendar.visible = 1
               AND ((event.all_day = 0 AND event.start_at_utc < ?4
                     AND (event.end_at_utc > ?3
                          OR (event.end_at_utc = event.start_at_utc
                              AND event.start_at_utc >= ?3)))
                 OR (event.all_day = 1 AND event.start_date < ?2 AND event.end_date > ?1))
             ORDER BY event.all_day DESC, COALESCE(event.start_at_utc, event.start_date) ASC,
                      event.title ASC",
        )?;
        let rows = statement.query_map(params![first_day, end_day, start_utc, end_utc], |row| {
            Ok(ExternalEvent {
                calendar_id: row.get(0)?,
                key: row.get(1)?,
                title: row.get(2)?,
                location: row.get(3)?,
                all_day: row.get(4)?,
                start_at_utc: row.get(5)?,
                end_at_utc: row.get(6)?,
                start_date: row.get(7)?,
                end_date: row.get(8)?,
                busy: row.get(9)?,
                tentative: row.get(10)?,
                recurring: row.get(11)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    fn after_revision_checked_write(&self, id: &str, changed: usize) -> AppResult<Calendar> {
        if changed == 1 {
            calendar_by_id(&self.connection, id)?.ok_or(AppError::NotFound)
        } else {
            Err(stale_or_missing(&self.connection, id)?)
        }
    }
}

/// Replaces a calendar's stored content and events and clears any problem, in the caller's
/// transaction.
fn write_snapshot(
    transaction: &Transaction<'_>,
    id: &str,
    snapshot: &Snapshot<'_>,
    next_refresh: Option<DateTime<Utc>>,
) -> AppResult<()> {
    let now = timestamp(Utc::now());
    if let Some(content) = snapshot.content {
        transaction.execute(
            "INSERT INTO calendar_documents (calendar_id, content, stored_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(calendar_id) DO UPDATE SET content = excluded.content,
                 stored_at = excluded.stored_at",
            params![id, content, now],
        )?;
    }
    let stored = write_events(transaction, id, snapshot.read)?;
    transaction.execute(
        "UPDATE calendars SET problem = NULL, last_synced_at = ?1, last_attempt_at = ?1,
             next_refresh_at = ?2, etag = ?3, last_modified = ?4, window_first_day = ?5,
             window_end_day = ?6, window_zone = ?7, event_count = ?8
         WHERE id = ?9",
        params![
            now,
            next_refresh.map(timestamp),
            snapshot.etag,
            snapshot.last_modified,
            snapshot.window.first_day.to_string(),
            snapshot.window.end_day.to_string(),
            snapshot.window.zone.name(),
            stored as i64,
            id
        ],
    )?;
    Ok(())
}

/// Replaces a calendar's events and returns how many were stored.
fn write_events(transaction: &Transaction<'_>, id: &str, read: &CalendarRead) -> AppResult<usize> {
    transaction.execute(
        "DELETE FROM calendar_events WHERE calendar_id = ?1",
        params![id],
    )?;
    let mut insert = transaction.prepare(
        "INSERT OR IGNORE INTO calendar_events
         (calendar_id, instance_key, title, location, all_day, start_at_utc, end_at_utc,
          start_date, end_date, busy, tentative, recurring)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut stored = 0;
    for event in &read.events {
        let (all_day, start_at, end_at, start_date, end_date) = match event.when {
            EventTime::Timed { start, end } => (
                false,
                Some(timestamp(start)),
                Some(timestamp(end)),
                None,
                None,
            ),
            EventTime::AllDay { start, end } => (
                true,
                None,
                None,
                Some(start.to_string()),
                Some(end.to_string()),
            ),
        };
        stored += insert.execute(params![
            id,
            event.key,
            event.title,
            event.location,
            all_day,
            start_at,
            end_at,
            start_date,
            end_date,
            event.busy,
            event.tentative,
            event.recurring
        ])?;
    }
    Ok(stored)
}

fn validate_name(name: &str) -> AppResult<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if !(1..=MAX_CALENDAR_NAME_LENGTH).contains(&name.chars().count()) {
        return Err(AppError::Validation(format!(
            "Calendar names must be 1–{MAX_CALENDAR_NAME_LENGTH} characters."
        )));
    }
    Ok(name)
}

fn calendar_by_id(connection: &Connection, id: &str) -> AppResult<Option<Calendar>> {
    connection
        .query_row(
            &format!("{CALENDAR_SELECT} WHERE id = ?1"),
            params![id],
            calendar_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn stale_or_missing(connection: &Connection, id: &str) -> AppResult<AppError> {
    Ok(if calendar_by_id(connection, id)?.is_some() {
        AppError::Conflict
    } else {
        AppError::NotFound
    })
}

fn source_from_row(row: &Row<'_>) -> rusqlite::Result<CalendarSource> {
    let kind = row.get::<_, String>(1)?;
    Ok(CalendarSource {
        id: row.get(0)?,
        kind: CalendarKind::parse(&kind).ok_or_else(|| invalid(1, "calendar kind"))?,
        account_id: row.get(2)?,
        remote_id: row.get(3)?,
        etag: row.get(4)?,
        last_modified: row.get(5)?,
    })
}

fn account_by_id(connection: &Connection, id: &str) -> AppResult<Option<CalendarAccount>> {
    connection
        .query_row(
            &format!("{ACCOUNT_SELECT} WHERE account.id = ?1"),
            params![id],
            account_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn account_from_row(row: &Row<'_>) -> rusqlite::Result<CalendarAccount> {
    let provider = row.get::<_, String>(1)?;
    let problem = row.get::<_, Option<String>>(3)?;
    Ok(CalendarAccount {
        id: row.get(0)?,
        provider: CalendarProvider::parse(&provider)
            .ok_or_else(|| invalid(1, "calendar provider"))?,
        label: row.get(2)?,
        problem: problem
            .as_deref()
            .and_then(CalendarProblem::parse)
            .map(CalendarIssue::from),
        calendar_count: row.get(4)?,
        revision: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn calendar_from_row(row: &Row<'_>) -> rusqlite::Result<Calendar> {
    let text = |index: usize| row.get::<_, String>(index);
    let kind = text(1)?;
    let color = text(3)?;
    let problem = row.get::<_, Option<String>>(6)?;
    Ok(Calendar {
        id: text(0)?,
        kind: CalendarKind::parse(&kind).ok_or_else(|| invalid(1, "calendar kind"))?,
        name: text(2)?,
        color: PlanColor::parse(&color).ok_or_else(|| invalid(3, "calendar color"))?,
        visible: row.get(4)?,
        source_label: text(5)?,
        problem: problem
            .as_deref()
            .and_then(CalendarProblem::parse)
            .map(CalendarIssue::from),
        last_synced_at: row.get(7)?,
        last_attempt_at: row.get(8)?,
        event_count: row.get(9)?,
        revision: row.get(10)?,
        created_at: text(11)?,
        updated_at: text(12)?,
        account_id: row.get(13)?,
    })
}

fn invalid(column: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid {what}"),
        )),
    )
}

/// Every stored instant uses one format, so text comparison orders instants correctly.
pub fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::super::ics::{read_calendar, EventDraft};
    use super::*;
    use chrono::NaiveDate;

    const FEED: &str = "BEGIN:VCALENDAR\r
X-WR-CALNAME:Team\r
BEGIN:VEVENT\r
UID:review\r
SUMMARY:Design review\r
DTSTART:20260917T150000Z\r
DTEND:20260917T160000Z\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:trip\r
SUMMARY:Conference\r
DTSTART;VALUE=DATE:20260916\r
DTEND;VALUE=DATE:20260919\r
TRANSP:OPAQUE\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:late\r
SUMMARY:Late call\r
DTSTART:20260918T043000Z\r
DTEND:20260918T053000Z\r
TRANSP:TRANSPARENT\r
END:VEVENT\r
END:VCALENDAR\r
";

    fn window() -> SyncWindow {
        SyncWindow::around(
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            chrono_tz::America::New_York,
        )
    }

    fn create(store: &mut CalendarStore, id: &str, kind: CalendarKind) -> Calendar {
        let window = window();
        let read = read_calendar(FEED, &window).unwrap();
        store
            .create(
                NewCalendar {
                    id,
                    kind,
                    name: "  Team   calendar ",
                    color: PlanColor::Lake,
                    source_label: "calendar.example",
                    account_id: None,
                    remote_id: None,
                },
                &Snapshot {
                    content: Some(FEED),
                    read: &read,
                    window: &window,
                    etag: Some("\"v1\""),
                    last_modified: None,
                },
                None,
            )
            .unwrap()
    }

    fn day_events(store: &CalendarStore, day: &str) -> Vec<String> {
        let (start, end) = match day {
            "2026-09-17" => ("2026-09-17T04:00:00.000Z", "2026-09-18T04:00:00.000Z"),
            "2026-09-18" => ("2026-09-18T04:00:00.000Z", "2026-09-19T04:00:00.000Z"),
            _ => unreachable!(),
        };
        let next = if day == "2026-09-17" {
            "2026-09-18"
        } else {
            "2026-09-19"
        };
        store
            .events(day, next, start, end)
            .unwrap()
            .into_iter()
            .map(|event| event.title)
            .collect()
    }

    #[test]
    fn stores_reads_and_queries_calendar_events_by_local_day() {
        let mut store = CalendarStore::open_in_memory().unwrap();
        let calendar = create(&mut store, "team", CalendarKind::IcsLink);
        assert_eq!(calendar.name, "Team calendar");
        assert_eq!((calendar.event_count, calendar.revision), (3, 1));
        assert!(calendar.problem.is_none() && calendar.last_synced_at.is_some());
        assert_eq!(
            day_events(&store, "2026-09-17"),
            ["Conference", "Design review"]
        );
        assert_eq!(
            day_events(&store, "2026-09-18"),
            ["Conference", "Late call"],
            "04:30 UTC is 00:30 in New York, so the late call lands on the 18th"
        );
        assert_eq!(store.content("team").unwrap().as_deref(), Some(FEED));

        let hidden = store
            .update(&UpdateCalendarInput {
                id: "team".into(),
                revision: 1,
                name: "Team".into(),
                color: PlanColor::Plum,
                visible: false,
            })
            .unwrap();
        assert_eq!((hidden.revision, hidden.visible), (2, false));
        assert!(day_events(&store, "2026-09-17").is_empty());
        assert!(matches!(
            store.update(&UpdateCalendarInput {
                id: "team".into(),
                revision: 1,
                name: "Stale".into(),
                color: PlanColor::Plum,
                visible: true,
            }),
            Err(AppError::Conflict)
        ));
    }

    #[test]
    fn refresh_bookkeeping_keeps_revisions_and_events_until_recovery() {
        let mut store = CalendarStore::open_in_memory().unwrap();
        create(&mut store, "team", CalendarKind::IcsLink);
        let now = Utc::now();
        assert_eq!(
            store.due_refreshes(now).unwrap()[0].etag.as_deref(),
            Some("\"v1\"")
        );
        store
            .record_problem("team", CalendarProblem::LinkNotFound, Some(now))
            .unwrap();
        let broken = store.calendar("team").unwrap().unwrap();
        assert_eq!(
            broken.problem.map(|issue| (issue.code, issue.retryable)),
            Some((CalendarProblem::LinkNotFound, false))
        );
        assert_eq!(broken.revision, 1);
        assert_eq!(
            broken.event_count, 3,
            "cached events stay while it's broken"
        );

        let window = window();
        let empty = CalendarRead {
            name: None,
            events: Vec::<EventDraft>::new(),
        };
        store
            .store_refresh(
                "team",
                &Snapshot {
                    content: Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"),
                    read: &empty,
                    window: &window,
                    etag: None,
                    last_modified: None,
                },
                Some(now + chrono::Duration::minutes(30)),
            )
            .unwrap();
        let recovered = store.calendar("team").unwrap().unwrap();
        assert_eq!(
            (recovered.problem, recovered.event_count, recovered.revision),
            (None, 0, 1)
        );
        assert!(store.due_refreshes(now).unwrap().is_empty());
        assert!(store.stale_windows(&window).unwrap().is_empty());
        let tomorrow =
            SyncWindow::around(NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(), window.zone);
        assert_eq!(store.stale_windows(&tomorrow).unwrap(), ["team"]);
    }

    #[test]
    fn removing_a_calendar_removes_everything_cached_for_it() {
        let mut store = CalendarStore::open_in_memory().unwrap();
        create(&mut store, "team", CalendarKind::IcsFile);
        assert!(
            store.due_refreshes(Utc::now()).unwrap().is_empty(),
            "files never refresh"
        );
        assert!(matches!(store.remove("team", 7), Err(AppError::Conflict)));
        store.remove("team", 1).unwrap();
        for table in ["calendars", "calendar_documents", "calendar_events"] {
            let count: i64 = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} is empty");
        }
        assert!(matches!(store.remove("team", 1), Err(AppError::NotFound)));
    }

    #[test]
    fn limits_calendar_count_and_names() {
        let mut store = CalendarStore::open_in_memory().unwrap();
        for index in 0..MAX_CALENDARS {
            create(
                &mut store,
                &format!("calendar-{index}"),
                CalendarKind::IcsFile,
            );
        }
        let window = window();
        let read = read_calendar(FEED, &window).unwrap();
        let snapshot = Snapshot {
            content: Some(FEED),
            read: &read,
            window: &window,
            etag: None,
            last_modified: None,
        };
        let new = |id| NewCalendar {
            id,
            kind: CalendarKind::IcsFile,
            name: "One too many",
            color: PlanColor::Sage,
            source_label: "extra.ics",
            account_id: None,
            remote_id: None,
        };
        assert!(matches!(
            store.create(new("extra"), &snapshot, None),
            Err(AppError::Validation(_))
        ));
        let mut fresh = CalendarStore::open_in_memory().unwrap();
        assert!(matches!(
            fresh.create(
                NewCalendar {
                    name: "   ",
                    ..new("blank")
                },
                &snapshot,
                None
            ),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn reopening_a_store_keeps_its_calendars() {
        let path = tempfile::tempdir()
            .unwrap()
            .keep()
            .join("calendars.sqlite3");
        let mut store = CalendarStore::open(&path).unwrap();
        create(&mut store, "team", CalendarKind::IcsLink);
        drop(store);
        let reopened = CalendarStore::open(&path).unwrap();
        assert_eq!(reopened.list().unwrap().len(), 1);
    }
}
