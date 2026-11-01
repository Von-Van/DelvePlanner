//! Assistant conversations, kept on the device so follow-ups work and a conversation survives a
//! restart.
//!
//! Two record kinds live here:
//!
//! - `agent_thread`: one conversation with the assistant (a title and when it was last used).
//! - `agent_message`: one message in a conversation, from the person or the assistant, in order.
//!   An assistant message may point at the `agent_run` that produced it, so its drafts and their
//!   outcome (approved and applied, turned down, …) can be read back.
//!
//! The assistant is never handed a whole conversation. [`PlannerDatabase::conversation_context`]
//! gives it the most recent turns plus a short digest of the older ones, built here from the
//! stored messages without a model call.
//!
//! Like the run records next door, none of this is planner data: it is never exported with the
//! planner and imports leave it alone. Deleting a conversation deletes its messages and the runs
//! (traces, drafts, results) it started, so what the person typed is gone with it.

use super::{now, utc_string, PlannerDatabase};
use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_THREAD_TITLE: &str = "New conversation";
pub const MAX_THREAD_LIST: u32 = 200;
pub const MAX_MESSAGES_PER_THREAD: i64 = 2_000;
/// The most recent turns a request may carry. The person's own setting is usually lower.
pub const MAX_CONTEXT_TURNS: usize = 20;
const MAX_TITLE_CHARS: usize = 120;
const AUTO_TITLE_CHARS: usize = 60;
const MAX_MESSAGE_CHARS: usize = 8_000;
const MAX_PREVIEW_CHARS: usize = 140;
/// Bounds for the digest of older turns: each line is clipped, and the oldest lines are dropped
/// until the whole digest fits.
const SUMMARY_USER_CHARS: usize = 160;
const SUMMARY_ASSISTANT_CHARS: usize = 120;
const MAX_SUMMARY_CHARS: usize = 1_200;

// ---------------------------------------------------------------------------------------------
// Schema

pub(super) fn ensure_conversation_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_thread (
             id TEXT PRIMARY KEY NOT NULL,
             title TEXT NOT NULL,
             titled_by_user INTEGER NOT NULL DEFAULT 0 CHECK(titled_by_user IN (0, 1)),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS agent_thread_updated_idx ON agent_thread(updated_at);

         CREATE TABLE IF NOT EXISTS agent_message (
             id TEXT PRIMARY KEY NOT NULL,
             thread_id TEXT NOT NULL REFERENCES agent_thread(id) ON DELETE CASCADE,
             seq INTEGER NOT NULL CHECK(seq >= 1),
             role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
             text TEXT NOT NULL,
             run_id TEXT REFERENCES agent_run(id) ON DELETE SET NULL,
             run_status TEXT CHECK(run_status IN ('answered', 'drafted', 'needs_clarification',
                                                  'cannot_complete', 'failed', 'cancelled')),
             created_at TEXT NOT NULL,
             UNIQUE(thread_id, seq)
         );
         CREATE INDEX IF NOT EXISTS agent_message_run_idx ON agent_message(run_id);",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            other => Err(bad_text(format!("unknown message role '{other}'"))),
        }
    }
}

/// How the run behind an assistant message ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRunStatus {
    Answered,
    Drafted,
    NeedsClarification,
    CannotComplete,
    Failed,
    Cancelled,
}

impl MessageRunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Drafted => "drafted",
            Self::NeedsClarification => "needs_clarification",
            Self::CannotComplete => "cannot_complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        Ok(match value {
            "answered" => Self::Answered,
            "drafted" => Self::Drafted,
            "needs_clarification" => Self::NeedsClarification,
            "cannot_complete" => Self::CannotComplete,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => return Err(bad_text(format!("unknown run status '{other}'"))),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentThread {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
    /// The start of the newest message, for the conversation list.
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessage {
    pub id: String,
    pub thread_id: String,
    pub seq: i64,
    pub role: MessageRole,
    pub text: String,
    pub run_id: Option<String>,
    pub run_status: Option<MessageRunStatus>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentThreadDetail {
    pub thread: AgentThread,
    pub messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAgentMessage {
    pub role: MessageRole,
    pub text: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub run_status: Option<MessageRunStatus>,
    /// Defaults to now; tests and imports may set it.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// One earlier turn as the agent sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTurn {
    pub role: MessageRole,
    pub text: String,
    pub at: String,
}

/// What a new request in a conversation carries: the most recent turns (oldest first) and a short
/// digest of anything older. The request being asked now is not included.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationContext {
    pub thread_id: String,
    pub turns: Vec<ConversationTurn>,
    pub summary: Option<String>,
    /// How many earlier messages the conversation holds in all.
    pub turns_available: usize,
}

// ---------------------------------------------------------------------------------------------
// Writes

impl PlannerDatabase {
    /// Starts an empty conversation. Without a title it is called "New conversation" until the
    /// person's first message names it.
    pub fn create_agent_thread(&mut self, title: Option<String>) -> AppResult<AgentThread> {
        let id = Uuid::new_v4().to_string();
        let at = now();
        let (title, titled) = match title.as_deref().map(str::trim) {
            None | Some("") => (DEFAULT_THREAD_TITLE.to_string(), false),
            Some(text) => (
                bounded(text, "A conversation title", MAX_TITLE_CHARS)?,
                true,
            ),
        };
        self.connection.execute(
            "INSERT INTO agent_thread (id, title, titled_by_user, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, title, titled, at],
        )?;
        self.agent_thread_summary(&id)
    }

    pub fn rename_agent_thread(&mut self, id: &str, title: &str) -> AppResult<AgentThread> {
        let title = bounded(title.trim(), "A conversation title", MAX_TITLE_CHARS)?;
        let changed = self.connection.execute(
            "UPDATE agent_thread SET title = ?1, titled_by_user = 1 WHERE id = ?2",
            params![title, id],
        )?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        self.agent_thread_summary(id)
    }

    /// Adds a message to the end of a conversation. The first message from the person names an
    /// untitled conversation.
    pub fn append_agent_message(
        &mut self,
        thread_id: &str,
        input: NewAgentMessage,
    ) -> AppResult<AgentMessage> {
        let text = bounded(input.text.trim(), "A message", MAX_MESSAGE_CHARS)?;
        if input.role == MessageRole::User && input.run_status.is_some() {
            return Err(invalid("Only an assistant message has a run status."));
        }
        let created_at = match input.created_at {
            Some(value) => timestamp(&value)?,
            None => now(),
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let titled: Option<bool> = transaction
            .query_row(
                "SELECT titled_by_user FROM agent_thread WHERE id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;
        let titled = titled.ok_or(AppError::NotFound)?;
        let (count, last_seq): (i64, i64) = transaction.query_row(
            "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM agent_message WHERE thread_id = ?1",
            params![thread_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count >= MAX_MESSAGES_PER_THREAD {
            return Err(invalid(
                "This conversation is full. Start a new conversation to keep going.",
            ));
        }
        if let Some(run_id) = &input.run_id {
            let source: Option<String> = transaction
                .query_row(
                    "SELECT source FROM agent_run WHERE id = ?1",
                    params![run_id],
                    |row| row.get(0),
                )
                .optional()?;
            match source.as_deref() {
                None => return Err(AppError::NotFound),
                Some("assistant") => {}
                Some(_) => return Err(invalid("A benchmark run can't join a conversation.")),
            }
        }
        let id = Uuid::new_v4().to_string();
        let seq = last_seq + 1;
        transaction.execute(
            "INSERT INTO agent_message (id, thread_id, seq, role, text, run_id, run_status,
                 created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                thread_id,
                seq,
                input.role.as_str(),
                text,
                input.run_id,
                input.run_status.map(MessageRunStatus::as_str),
                created_at,
            ],
        )?;
        transaction.execute(
            "UPDATE agent_thread SET updated_at = MAX(updated_at, ?1) WHERE id = ?2",
            params![created_at, thread_id],
        )?;
        if !titled && input.role == MessageRole::User && count == 0 {
            transaction.execute(
                "UPDATE agent_thread SET title = ?1 WHERE id = ?2",
                params![clip(&text, AUTO_TITLE_CHARS), thread_id],
            )?;
        }
        transaction.commit()?;
        self.agent_message(&id)
    }

    /// Records how the run behind an assistant message ended, once it has.
    pub fn set_agent_message_run(
        &mut self,
        message_id: &str,
        run_id: Option<&str>,
        run_status: MessageRunStatus,
    ) -> AppResult<AgentMessage> {
        let changed = self.connection.execute(
            "UPDATE agent_message SET run_id = COALESCE(?1, run_id), run_status = ?2
             WHERE id = ?3 AND role = 'assistant'",
            params![run_id, run_status.as_str(), message_id],
        )?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        self.agent_message(message_id)
    }

    /// Deletes a conversation, its messages, and the assistant runs it started (their traces,
    /// drafts and results), so nothing the person typed in it is left behind.
    pub fn delete_agent_thread(&mut self, id: &str) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM agent_run WHERE source = 'assistant' AND id IN (
                 SELECT run_id FROM agent_message WHERE thread_id = ?1 AND run_id IS NOT NULL
             )",
            params![id],
        )?;
        let changed = transaction.execute("DELETE FROM agent_thread WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        transaction.commit()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Reads

impl PlannerDatabase {
    /// Conversations, most recently used first.
    pub fn agent_threads(&self, limit: Option<u32>) -> AppResult<Vec<AgentThread>> {
        let limit = limit.unwrap_or(50);
        if !(1..=MAX_THREAD_LIST).contains(&limit) {
            return Err(invalid(&format!(
                "A list limit must be between 1 and {MAX_THREAD_LIST}."
            )));
        }
        let mut statement = self.connection.prepare(&format!(
            "{THREAD_SELECT} ORDER BY t.updated_at DESC, t.rowid DESC LIMIT ?1"
        ))?;
        let threads = statement
            .query_map(params![limit], thread_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(threads)
    }

    /// One conversation with all its messages, oldest first.
    pub fn agent_thread(&self, id: &str) -> AppResult<AgentThreadDetail> {
        let thread = self.agent_thread_summary(id)?;
        let mut statement = self.connection.prepare(&format!(
            "{MESSAGE_SELECT} WHERE thread_id = ?1 ORDER BY seq"
        ))?;
        let messages = statement
            .query_map(params![id], message_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(AgentThreadDetail { thread, messages })
    }

    /// What the next request in a conversation should carry: the last `turns` messages, each
    /// with what came of any change it drafted, plus a short digest of the older ones.
    pub fn conversation_context(
        &self,
        thread_id: &str,
        turns: usize,
    ) -> AppResult<ConversationContext> {
        self.agent_thread_summary(thread_id)?;
        let turns = turns.min(MAX_CONTEXT_TURNS);
        let mut statement = self.connection.prepare(
            "SELECT m.role, m.text, m.created_at, m.run_status,
                    (SELECT COUNT(*) FROM agent_action_draft d
                       JOIN agent_action_result x ON x.draft_id = d.id
                      WHERE d.run_id = m.run_id AND x.execution_state = 'applied'),
                    (SELECT COUNT(*) FROM agent_action_draft d
                      WHERE d.run_id = m.run_id AND d.state = 'rejected'),
                    (SELECT COUNT(*) FROM agent_action_draft d
                       LEFT JOIN agent_action_result x ON x.draft_id = d.id
                      WHERE d.run_id = m.run_id
                        AND (d.state IN ('stale', 'expired')
                             OR x.execution_state IN ('failed', 'stale')))
             FROM agent_message m WHERE m.thread_id = ?1 ORDER BY m.seq",
        )?;
        let messages = statement
            .query_map(params![thread_id], |row| {
                let role = MessageRole::parse(&row.get::<_, String>(0)?)?;
                let text: String = row.get(1)?;
                let status = row
                    .get::<_, Option<String>>(3)?
                    .as_deref()
                    .map(MessageRunStatus::parse)
                    .transpose()?;
                let outcome = DraftOutcome {
                    applied: row.get(4)?,
                    rejected: row.get(5)?,
                    not_applied: row.get(6)?,
                };
                Ok(ConversationTurn {
                    role,
                    text: with_outcome(role, text, status, outcome),
                    at: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let available = messages.len();
        let split = available.saturating_sub(turns);
        let summary = summarize(&messages[..split]);
        Ok(ConversationContext {
            thread_id: thread_id.to_string(),
            turns: messages[split..].to_vec(),
            summary,
            turns_available: available,
        })
    }

    fn agent_thread_summary(&self, id: &str) -> AppResult<AgentThread> {
        self.connection
            .query_row(
                &format!("{THREAD_SELECT} WHERE t.id = ?1"),
                params![id],
                thread_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }

    fn agent_message(&self, id: &str) -> AppResult<AgentMessage> {
        self.connection
            .query_row(
                &format!("{MESSAGE_SELECT} WHERE id = ?1"),
                params![id],
                message_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers

const THREAD_SELECT: &str = "SELECT t.id, t.title, t.created_at, t.updated_at,
        (SELECT COUNT(*) FROM agent_message m WHERE m.thread_id = t.id),
        (SELECT m.text FROM agent_message m WHERE m.thread_id = t.id ORDER BY m.seq DESC LIMIT 1)
     FROM agent_thread t";

const MESSAGE_SELECT: &str =
    "SELECT id, thread_id, seq, role, text, run_id, run_status, created_at FROM agent_message";

fn thread_from_row(row: &Row<'_>) -> rusqlite::Result<AgentThread> {
    Ok(AgentThread {
        id: row.get(0)?,
        title: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        message_count: row.get(4)?,
        preview: row
            .get::<_, Option<String>>(5)?
            .map(|text| clip(&text, MAX_PREVIEW_CHARS)),
    })
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<AgentMessage> {
    Ok(AgentMessage {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        seq: row.get(2)?,
        role: MessageRole::parse(&row.get::<_, String>(3)?)?,
        text: row.get(4)?,
        run_id: row.get(5)?,
        run_status: row
            .get::<_, Option<String>>(6)?
            .as_deref()
            .map(MessageRunStatus::parse)
            .transpose()?,
        created_at: row.get(7)?,
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct DraftOutcome {
    applied: i64,
    rejected: i64,
    not_applied: i64,
}

/// An assistant turn that drafted changes says what the person did with them, so "move it 30
/// minutes later" starts from the approved time, not the old one.
fn with_outcome(
    role: MessageRole,
    text: String,
    status: Option<MessageRunStatus>,
    outcome: DraftOutcome,
) -> String {
    if role != MessageRole::Assistant || status != Some(MessageRunStatus::Drafted) {
        return text;
    }
    let mut notes = Vec::new();
    if outcome.applied > 0 {
        notes.push(match outcome.applied {
            1 => "The person approved this change and it was applied.".to_string(),
            n => format!("The person approved {n} of these changes and they were applied."),
        });
    }
    if outcome.rejected > 0 {
        notes.push("The person turned down the drafted change.".to_string());
    }
    if outcome.not_applied > 0 && outcome.applied == 0 {
        notes.push("The drafted change was not applied.".to_string());
    }
    if outcome.applied == 0 && outcome.rejected == 0 && outcome.not_applied == 0 {
        notes.push("The drafted change has not been approved yet.".to_string());
    }
    format!("{text} [{}]", notes.join(" "))
}

/// A short digest of turns that no longer fit the window: what the person asked and how the
/// assistant answered, one clipped line each, newest kept when space runs out. Built from the
/// stored messages, so there is no model call and nothing new is made up.
fn summarize(older: &[ConversationTurn]) -> Option<String> {
    if older.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = older
        .iter()
        .map(|turn| match turn.role {
            MessageRole::User => format!("- Person: {}", clip(&turn.text, SUMMARY_USER_CHARS)),
            MessageRole::Assistant => {
                format!("- Assistant: {}", clip(&turn.text, SUMMARY_ASSISTANT_CHARS))
            }
        })
        .collect();
    let mut dropped = 0;
    while lines.len() > 1
        && lines.iter().map(|line| line.len() + 1).sum::<usize>() > MAX_SUMMARY_CHARS
    {
        lines.remove(0);
        dropped += 1;
    }
    let mut summary = String::from("Earlier in this conversation, in short (oldest first):\n");
    if dropped > 0 {
        summary.push_str(&format!("- ({dropped} earlier messages left out)\n"));
    }
    summary.push_str(&lines.join("\n"));
    Some(summary)
}

/// One line, at most `max` characters, ending in "…" when cut.
fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn bounded(value: &str, what: &str, max: usize) -> AppResult<String> {
    if value.is_empty() {
        return Err(invalid(&format!("{what} can't be empty.")));
    }
    if value.chars().count() > max {
        return Err(invalid(&format!("{what} is longer than {max} characters.")));
    }
    Ok(value.to_string())
}

fn timestamp(value: &str) -> AppResult<String> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .map(|time| utc_string(time.with_timezone(&chrono::Utc)))
        .map_err(|_| invalid(&format!("'{value}' is not an ISO-8601 time with a zone.")))
}

fn invalid(message: &str) -> AppError {
    AppError::Validation(message.to_string())
}

fn bad_text(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        ActionOutcome, DraftDecision, DraftTarget, ExecutionState, NewActionDraft, NewAgentRun,
        RunSource,
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn path() -> PathBuf {
        tempdir().unwrap().keep().join("dayplan.sqlite3")
    }

    fn user(text: &str) -> NewAgentMessage {
        NewAgentMessage {
            role: MessageRole::User,
            text: text.into(),
            run_id: None,
            run_status: None,
            created_at: None,
        }
    }

    fn assistant(text: &str, run_id: Option<&str>, status: MessageRunStatus) -> NewAgentMessage {
        NewAgentMessage {
            role: MessageRole::Assistant,
            text: text.into(),
            run_id: run_id.map(str::to_string),
            run_status: Some(status),
            created_at: None,
        }
    }

    fn run(database: &mut PlannerDatabase, goal: &str) -> String {
        database
            .start_agent_run(NewAgentRun {
                source: RunSource::Assistant,
                experiment_run_id: None,
                case_id: None,
                case_family: None,
                goal: goal.into(),
                architecture_id: "agent".into(),
                model_name: "qwen3:8b".into(),
                model_digest: None,
                trigger: "user_message".into(),
                budget: json!({}),
                started_at: None,
            })
            .unwrap()
            .id
    }

    fn approved_and_applied(database: &mut PlannerDatabase, run_id: &str) {
        let draft = database
            .record_action_draft(
                run_id,
                NewActionDraft {
                    op: "reschedule_event".into(),
                    target: Some(DraftTarget {
                        id: "event-gym".into(),
                        revision: 1,
                    }),
                    payload: json!({ "startAtUtc": "2026-11-02T00:00:00.000Z" }),
                    evidence: vec!["call-1".into()],
                    expires_at: "2999-01-01T00:00:00Z".into(),
                },
            )
            .unwrap();
        database
            .decide_action_draft(&draft.id, DraftDecision::Approve, &draft.payload_digest)
            .unwrap();
        database
            .record_action_result(
                &draft.id,
                ActionOutcome {
                    execution_state: ExecutionState::Applied,
                    effects: json!([]),
                    error: None,
                },
            )
            .unwrap();
    }

    fn table_count(path: &Path, table: &str) -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_conversation_reloads_after_a_restart() {
        let path = path();
        let thread_id = {
            let mut database = PlannerDatabase::open(&path).unwrap();
            let thread = database.create_agent_thread(None).unwrap();
            assert_eq!(thread.title, DEFAULT_THREAD_TITLE);
            database
                .append_agent_message(&thread.id, user("Move gym to 6 pm today"))
                .unwrap();
            let run_id = run(&mut database, "Move gym to 6 pm today");
            database
                .append_agent_message(
                    &thread.id,
                    assistant(
                        "Drafted: move Gym to 6:00 pm.",
                        Some(&run_id),
                        MessageRunStatus::Drafted,
                    ),
                )
                .unwrap();
            approved_and_applied(&mut database, &run_id);
            thread.id
        };

        let database = PlannerDatabase::open(&path).unwrap();
        let threads = database.agent_threads(None).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title, "Move gym to 6 pm today");
        assert_eq!(threads[0].message_count, 2);
        let detail = database.agent_thread(&thread_id).unwrap();
        let texts: Vec<_> = detail.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Move gym to 6 pm today", "Drafted: move Gym to 6:00 pm."]
        );
        assert_eq!(detail.messages[1].seq, 2);
        assert_eq!(
            detail.messages[1].run_status,
            Some(MessageRunStatus::Drafted)
        );

        // The next request carries both turns, and knows the drafted move was applied.
        let context = database.conversation_context(&thread_id, 6).unwrap();
        assert_eq!(context.turns.len(), 2);
        assert_eq!(context.summary, None);
        assert!(context.turns[1]
            .text
            .ends_with("[The person approved this change and it was applied.]"));
    }

    #[test]
    fn older_turns_become_a_short_digest() {
        let mut database = PlannerDatabase::open(&path()).unwrap();
        let thread = database.create_agent_thread(Some("Week".into())).unwrap();
        for index in 1..=10 {
            database
                .append_agent_message(&thread.id, user(&format!("Question {index}")))
                .unwrap();
            database
                .append_agent_message(
                    &thread.id,
                    assistant(&format!("Answer {index}"), None, MessageRunStatus::Answered),
                )
                .unwrap();
        }
        let context = database.conversation_context(&thread.id, 4).unwrap();
        assert_eq!(context.turns_available, 20);
        let recent: Vec<_> = context.turns.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            recent,
            ["Question 9", "Answer 9", "Question 10", "Answer 10"]
        );
        let summary = context.summary.unwrap();
        assert!(summary.contains("- Person: Question 1\n- Assistant: Answer 1"));
        assert!(summary.contains("- Assistant: Answer 8"));
        assert!(!summary.contains("Question 9"));

        // A long history keeps the digest bounded, newest lines first to stay.
        let long = "word ".repeat(400);
        for _ in 0..30 {
            database
                .append_agent_message(&thread.id, user(&long))
                .unwrap();
        }
        let summary = database
            .conversation_context(&thread.id, 2)
            .unwrap()
            .summary
            .unwrap();
        assert!(summary.len() <= MAX_SUMMARY_CHARS + 200);
        assert!(summary.contains("earlier messages left out"));
        assert!(!summary.contains("Question 1\n"));

        // No window at all: everything is digest; the cap on turns still holds.
        let none = database.conversation_context(&thread.id, 0).unwrap();
        assert!(none.turns.is_empty());
        let capped = database.conversation_context(&thread.id, 500).unwrap();
        assert_eq!(capped.turns.len(), MAX_CONTEXT_TURNS);
    }

    #[test]
    fn draft_outcomes_are_spelled_out_for_the_agent() {
        let mut database = PlannerDatabase::open(&path()).unwrap();
        let thread = database.create_agent_thread(None).unwrap();
        let pending = run(&mut database, "Move standup");
        database
            .append_agent_message(
                &thread.id,
                assistant("Drafted a move.", Some(&pending), MessageRunStatus::Drafted),
            )
            .unwrap();
        let rejected = run(&mut database, "Delete gym");
        let draft = database
            .record_action_draft(
                &rejected,
                NewActionDraft {
                    op: "delete_event".into(),
                    target: Some(DraftTarget {
                        id: "event-gym".into(),
                        revision: 2,
                    }),
                    payload: json!({}),
                    evidence: vec!["call-1".into()],
                    expires_at: "2999-01-01T00:00:00Z".into(),
                },
            )
            .unwrap();
        database
            .decide_action_draft(&draft.id, DraftDecision::Reject, &draft.payload_digest)
            .unwrap();
        database
            .append_agent_message(
                &thread.id,
                assistant(
                    "Drafted a delete.",
                    Some(&rejected),
                    MessageRunStatus::Drafted,
                ),
            )
            .unwrap();
        database
            .append_agent_message(
                &thread.id,
                assistant("It's at 6.", None, MessageRunStatus::Answered),
            )
            .unwrap();
        let turns = database.conversation_context(&thread.id, 6).unwrap().turns;
        assert!(turns[0]
            .text
            .ends_with("[The drafted change has not been approved yet.]"));
        assert!(turns[1]
            .text
            .ends_with("[The person turned down the drafted change.]"));
        assert_eq!(turns[2].text, "It's at 6.");
    }

    #[test]
    fn deleting_a_conversation_removes_its_messages_and_runs() {
        let path = path();
        let mut database = PlannerDatabase::open(&path).unwrap();
        let keep = database.create_agent_thread(None).unwrap();
        let kept_run = run(&mut database, "Keep me");
        database
            .append_agent_message(&keep.id, user("Keep me"))
            .unwrap();
        database
            .append_agent_message(
                &keep.id,
                assistant("Kept.", Some(&kept_run), MessageRunStatus::Answered),
            )
            .unwrap();

        let gone = database.create_agent_thread(None).unwrap();
        let gone_run = run(&mut database, "Private request");
        database
            .append_agent_message(&gone.id, user("Private request"))
            .unwrap();
        database
            .append_agent_message(
                &gone.id,
                assistant("Drafted.", Some(&gone_run), MessageRunStatus::Drafted),
            )
            .unwrap();
        approved_and_applied(&mut database, &gone_run);

        database.delete_agent_thread(&gone.id).unwrap();
        assert!(matches!(
            database.agent_thread(&gone.id),
            Err(AppError::NotFound)
        ));
        assert!(matches!(
            database.agent_run(&gone_run),
            Err(AppError::NotFound)
        ));
        assert!(database.agent_run(&kept_run).is_ok());
        assert_eq!(table_count(&path, "agent_message"), 2);
        assert_eq!(table_count(&path, "agent_action_draft"), 0);
        assert!(matches!(
            database.delete_agent_thread(&gone.id),
            Err(AppError::NotFound)
        ));

        database.clear_agent_history().unwrap();
        assert!(database.agent_threads(None).unwrap().is_empty());
        assert_eq!(table_count(&path, "agent_message"), 0);
    }

    #[test]
    fn a_pruned_run_leaves_its_message_in_place() {
        let mut database = PlannerDatabase::open(&path()).unwrap();
        let thread = database.create_agent_thread(None).unwrap();
        let run_id = run(&mut database, "What's on today?");
        let message = database
            .append_agent_message(
                &thread.id,
                assistant("Two meetings.", Some(&run_id), MessageRunStatus::Answered),
            )
            .unwrap();
        database.delete_agent_run(&run_id).unwrap();
        let detail = database.agent_thread(&thread.id).unwrap();
        assert_eq!(detail.messages[0].id, message.id);
        assert_eq!(detail.messages[0].run_id, None);
        assert_eq!(detail.messages[0].text, "Two meetings.");
    }

    #[test]
    fn messages_and_titles_are_checked() {
        let mut database = PlannerDatabase::open(&path()).unwrap();
        assert!(matches!(
            database.append_agent_message("missing", user("Hi")),
            Err(AppError::NotFound)
        ));
        let thread = database.create_agent_thread(None).unwrap();
        assert!(database
            .append_agent_message(&thread.id, user("   "))
            .is_err());
        assert!(database
            .append_agent_message(&thread.id, user(&"x".repeat(MAX_MESSAGE_CHARS + 1)))
            .is_err());
        let mut status_on_user = user("Hi");
        status_on_user.run_status = Some(MessageRunStatus::Answered);
        assert!(database
            .append_agent_message(&thread.id, status_on_user)
            .is_err());
        assert!(matches!(
            database.append_agent_message(
                &thread.id,
                assistant("Hi", Some("no-such-run"), MessageRunStatus::Answered)
            ),
            Err(AppError::NotFound)
        ));

        // An assistant reply first doesn't name the conversation; the person's first message does,
        // clipped to one line; a later message or a chosen title is never overwritten.
        database
            .append_agent_message(
                &thread.id,
                assistant("Hello", None, MessageRunStatus::Answered),
            )
            .unwrap();
        database
            .append_agent_message(
                &thread.id,
                user("What should I\nwork on this afternoon given everything due on Friday?"),
            )
            .unwrap();
        assert_eq!(
            database.agent_threads(None).unwrap()[0].title,
            DEFAULT_THREAD_TITLE
        );
        let fresh = database.create_agent_thread(None).unwrap();
        database
            .append_agent_message(
                &fresh.id,
                user("What should I\nwork on this afternoon given everything due on Friday?"),
            )
            .unwrap();
        let titled = database.agent_thread(&fresh.id).unwrap().thread;
        assert_eq!(
            titled.title,
            "What should I work on this afternoon given everything due o…"
        );
        assert_eq!(titled.title.chars().count(), AUTO_TITLE_CHARS);
        let renamed = database.rename_agent_thread(&fresh.id, " Friday ").unwrap();
        assert_eq!(renamed.title, "Friday");
        assert!(database.rename_agent_thread(&fresh.id, "").is_err());
        assert!(matches!(
            database.rename_agent_thread("missing", "x"),
            Err(AppError::NotFound)
        ));

        // Most recently used first.
        database
            .append_agent_message(&thread.id, user("Back to this one"))
            .unwrap();
        let order: Vec<_> = database
            .agent_threads(None)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(order[0], thread.id);
        assert!(database.agent_threads(Some(0)).is_err());
        assert!(database.agent_threads(Some(MAX_THREAD_LIST + 1)).is_err());
    }

    #[test]
    fn a_benchmark_run_cant_join_a_conversation() {
        let mut database = PlannerDatabase::open(&path()).unwrap();
        let thread = database.create_agent_thread(None).unwrap();
        let experiment = database
            .start_experiment_run(crate::db::NewExperimentRun {
                kind: RunSource::Benchmark,
                benchmark_version: "agent_bench 0.1".into(),
                config_hash: "cfg".into(),
                architecture_id: "agent".into(),
                model_name: "m".into(),
                model_digest: None,
                runtime_version: None,
                hardware: None,
                split: None,
                result_path: None,
                notes: None,
                started_at: None,
            })
            .unwrap();
        let case = database
            .start_agent_run(NewAgentRun {
                source: RunSource::Benchmark,
                experiment_run_id: Some(experiment.id),
                case_id: Some("c".into()),
                case_family: None,
                goal: "g".into(),
                architecture_id: "agent".into(),
                model_name: "m".into(),
                model_digest: None,
                trigger: "benchmark".into(),
                budget: json!({}),
                started_at: None,
            })
            .unwrap();
        assert!(database
            .append_agent_message(
                &thread.id,
                assistant("x", Some(&case.id), MessageRunStatus::Answered)
            )
            .is_err());
    }
}
