//! What the person thought of an assistant reply, and the lessons they chose to keep
//! (milestone M6).
//!
//! One record kind lives here, `agent_feedback`: "helpful" or "not helpful because …" on one
//! assistant run, about its answer or one of its proposed changes. It points at the `agent_run`,
//! so the feedback sits next to the run's full trace and is deleted with it. (The table can also
//! hold feedback on a heads-up, with `suggestion_id` instead of a run; nothing records that yet.)
//!
//! Feedback alone never changes what the assistant does later. A "not helpful" with a reason is
//! offered back to the person as a lesson they can keep, in their own words:
//!
//! - **Just this once** (`this_time`): nothing is remembered. The reason is carried into the same
//!   conversation only, so a retry there takes it into account, and no later run sees it.
//! - **Remember for next time** (`lasting`): the person confirms (and may edit) the lesson, and it
//!   is stored through [`PlannerDatabase::add_memory`] as something they stated. It is then a
//!   confirmed preference like any other: listed by `get_preferences`, visible, correctable and
//!   deletable in Settings › Memory.
//!
//! Until the person picks one, the feedback is `pending` and treated as "just this once".
//! Deleting the remembered lesson also erases the reason here, so its words don't outlive it.

use super::{now, MemoryKind, MemorySource, NewMemory, PlannerDatabase, MAX_MEMORY_CHARS};
use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The longest reason kept; a lesson is a memory, which has the same limit.
pub const MAX_FEEDBACK_REASON_CHARS: usize = MAX_MEMORY_CHARS;
/// The longest description of what the feedback is about (a change's title, as shown).
pub const MAX_FEEDBACK_ABOUT_CHARS: usize = 200;
/// The most feedback one run can collect, so a stuck button can't fill the database.
pub const MAX_FEEDBACK_PER_RUN: i64 = 20;
/// The longest reason quoted back to the assistant in a conversation.
const NOTE_REASON_CHARS: usize = 200;
/// The longest change description quoted back with it, so the note stays short enough to keep
/// whole when the conversation window shortens a long reply.
const NOTE_ABOUT_CHARS: usize = 80;

pub(super) fn ensure_feedback_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_feedback (
             id TEXT PRIMARY KEY NOT NULL,
             run_id TEXT REFERENCES agent_run(id) ON DELETE CASCADE,
             suggestion_id TEXT,
             target TEXT NOT NULL CHECK(target IN ('answer', 'change', 'suggestion')),
             about TEXT,
             rating TEXT NOT NULL CHECK(rating IN ('helpful', 'not_helpful')),
             reason TEXT,
             scope TEXT NOT NULL CHECK(scope IN ('pending', 'this_time', 'lasting')),
             memory_id TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             CHECK(scope != 'pending' OR reason IS NOT NULL),
             CHECK(memory_id IS NULL OR scope = 'lasting'),
             CHECK((target = 'suggestion') = (suggestion_id IS NOT NULL)),
             CHECK((run_id IS NULL) = (suggestion_id IS NOT NULL))
         );
         CREATE INDEX IF NOT EXISTS agent_feedback_run_idx ON agent_feedback(run_id);

         CREATE TRIGGER IF NOT EXISTS agent_feedback_forget_lesson
         AFTER DELETE ON agent_memory
         BEGIN
             UPDATE agent_feedback SET reason = NULL, memory_id = NULL
             WHERE memory_id = OLD.id;
         END;",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    Helpful,
    NotHelpful,
}

/// What the feedback is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTarget {
    /// The reply as a whole.
    Answer,
    /// One proposed change; `about` names it.
    Change,
    /// A heads-up the assistant raised on its own.
    Suggestion,
}

/// Whether a lesson from the feedback is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackScope {
    /// Waiting for the person to choose; treated like `this_time`.
    Pending,
    /// Nothing is remembered.
    ThisTime,
    /// Kept as a confirmed memory.
    Lasting,
}

macro_rules! text_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
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

text_enum!(FeedbackRating { Helpful => "helpful", NotHelpful => "not_helpful" });
text_enum!(FeedbackTarget { Answer => "answer", Change => "change", Suggestion => "suggestion" });
text_enum!(FeedbackScope { Pending => "pending", ThisTime => "this_time", Lasting => "lasting" });

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFeedback {
    pub id: String,
    /// The run the feedback is about; null only for feedback on a heads-up.
    pub run_id: Option<String>,
    pub target: FeedbackTarget,
    pub about: Option<String>,
    pub rating: FeedbackRating,
    /// Null when none was given, or after the lesson made from it was deleted.
    pub reason: Option<String>,
    pub scope: FeedbackScope,
    /// The memory the lesson became, while it exists.
    pub memory_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewFeedback {
    pub run_id: String,
    pub target: FeedbackTarget,
    pub about: Option<String>,
    pub rating: FeedbackRating,
    pub reason: Option<String>,
}

/// The lesson offered back to the person: their own reason, tidied, as a preference.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonProposal {
    pub kind: MemoryKind,
    pub content: String,
}

// ---------------------------------------------------------------------------------------------
// Writes

impl PlannerDatabase {
    /// Records feedback on one assistant run. A "not helpful" with a reason waits for the person
    /// to choose between "just this once" and "remember"; everything else is settled at once.
    pub fn record_agent_feedback(&mut self, input: NewFeedback) -> AppResult<AgentFeedback> {
        let source: Option<String> = self
            .connection
            .query_row(
                "SELECT source FROM agent_run WHERE id = ?1",
                params![input.run_id],
                |row| row.get(0),
            )
            .optional()?;
        match source.as_deref() {
            None => return Err(AppError::NotFound),
            Some("assistant") => {}
            Some(_) => return Err(invalid("Feedback is only for the assistant's own replies.")),
        }
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM agent_feedback WHERE run_id = ?1",
            params![input.run_id],
            |row| row.get(0),
        )?;
        if count >= MAX_FEEDBACK_PER_RUN {
            return Err(invalid(
                "This reply already has as much feedback as it can hold.",
            ));
        }
        if input.target == FeedbackTarget::Suggestion {
            return Err(invalid("Feedback on a heads-up isn't recorded here."));
        }
        let reason = optional_text(input.reason, MAX_FEEDBACK_REASON_CHARS, "The reason")?;
        let about = optional_text(input.about, MAX_FEEDBACK_ABOUT_CHARS, "What it is about")?;
        if input.target == FeedbackTarget::Change && about.is_none() {
            return Err(invalid("Say which change the feedback is about."));
        }
        let scope = if input.rating == FeedbackRating::NotHelpful && reason.is_some() {
            FeedbackScope::Pending
        } else {
            FeedbackScope::ThisTime
        };
        let id = Uuid::new_v4().to_string();
        let at = now();
        self.connection.execute(
            "INSERT INTO agent_feedback (id, run_id, target, about, rating, reason, scope,
                 created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                id,
                input.run_id,
                input.target.as_str(),
                about,
                input.rating.as_str(),
                reason,
                scope.as_str(),
                at,
            ],
        )?;
        self.agent_feedback(&id)
    }

    /// "Just this once": nothing from this feedback is remembered.
    pub fn keep_feedback_one_off(&mut self, id: &str) -> AppResult<AgentFeedback> {
        let feedback = self.agent_feedback(id)?;
        match feedback.scope {
            FeedbackScope::ThisTime => return Ok(feedback),
            FeedbackScope::Lasting => return Err(invalid(
                "This lesson is already remembered. Delete it in Settings › Memory to forget it.",
            )),
            FeedbackScope::Pending => {}
        }
        self.connection.execute(
            "UPDATE agent_feedback SET scope = 'this_time', updated_at = ?2
             WHERE id = ?1 AND scope = 'pending'",
            params![id, now()],
        )?;
        self.agent_feedback(id)
    }

    /// "Remember for next time": the person confirms the lesson (in words they may have edited)
    /// and it becomes a memory they stated, linked back to this feedback and its run.
    pub fn learn_from_feedback(
        &mut self,
        id: &str,
        kind: MemoryKind,
        content: &str,
    ) -> AppResult<AgentFeedback> {
        let feedback = self.agent_feedback(id)?;
        if feedback.rating != FeedbackRating::NotHelpful || feedback.reason.is_none() {
            return Err(invalid(
                "Only a correction with a reason can become a lesson.",
            ));
        }
        if feedback.scope == FeedbackScope::Lasting {
            return Err(invalid("This lesson is already remembered."));
        }
        // Claim it first, so a second press can't store the lesson twice.
        let claimed = self.connection.execute(
            "UPDATE agent_feedback SET scope = 'lasting', updated_at = ?2
             WHERE id = ?1 AND scope != 'lasting'",
            params![id, now()],
        )?;
        if claimed == 0 {
            return Err(invalid("This lesson is already remembered."));
        }
        let saved = self.add_memory(NewMemory {
            kind,
            content: content.to_string(),
            source: MemorySource::UserStated,
            source_ref: Some(match &feedback.run_id {
                Some(run) => format!("feedback {} on assistant run {run}", feedback.id),
                None => format!("feedback {}", feedback.id),
            }),
            plan_id: None,
        });
        match saved {
            Ok(memory) => {
                self.connection.execute(
                    "UPDATE agent_feedback SET memory_id = ?2 WHERE id = ?1",
                    params![id, memory.id],
                )?;
                self.agent_feedback(id)
            }
            Err(error) => {
                // Not remembered (memory paused, full, or the text was refused): the person can
                // fix it and try again.
                self.connection.execute(
                    "UPDATE agent_feedback SET scope = ?2 WHERE id = ?1",
                    params![id, feedback.scope.as_str()],
                )?;
                Err(error)
            }
        }
    }

    // -- reads ----------------------------------------------------------------------------------

    pub fn agent_feedback(&self, id: &str) -> AppResult<AgentFeedback> {
        self.connection
            .query_row(
                &format!("{FEEDBACK_SELECT} WHERE id = ?1"),
                params![id],
                feedback_from_row,
            )
            .optional()?
            .ok_or(AppError::NotFound)
    }

    /// One run's feedback, oldest first.
    pub fn run_feedback(&self, run_id: &str) -> AppResult<Vec<AgentFeedback>> {
        let mut statement = self.connection.prepare(&format!(
            "{FEEDBACK_SELECT} WHERE run_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = statement
            .query_map(params![run_id], feedback_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// What a conversation tells the assistant about the feedback on one of its replies, or `None`
/// when there was none. Only the same conversation ever carries this.
pub(super) fn feedback_note(feedback: &[AgentFeedback]) -> Option<String> {
    let notes: Vec<String> = feedback
        .iter()
        .map(|item| {
            let about = item
                .about
                .as_deref()
                .map(|about| format!(" about \"{}\"", clip(about, NOTE_ABOUT_CHARS)))
                .unwrap_or_default();
            match (item.rating, item.reason.as_deref()) {
                (FeedbackRating::Helpful, _) => {
                    format!("The person found this reply{about} helpful.")
                }
                (FeedbackRating::NotHelpful, None) => {
                    format!("The person said this reply{about} was not helpful.")
                }
                (FeedbackRating::NotHelpful, Some(reason)) => {
                    let kept = match item.scope {
                        FeedbackScope::Lasting => "They asked you to remember this.",
                        _ => "This applies to this conversation only.",
                    };
                    format!(
                        "The person said this reply{about} was not helpful because: \"{}\". {kept}",
                        clip(reason, NOTE_REASON_CHARS)
                    )
                }
            }
        })
        .collect();
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// The lesson to offer for a reason: the person's own words, with a leading "because" and the
/// like dropped, capitalised and ending in a full stop. Always a preference; the person can pick
/// another kind and edit the words before anything is stored.
pub fn propose_lesson(reason: &str) -> Option<LessonProposal> {
    let mut text = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        let lower = text.to_lowercase();
        let Some(prefix) = LEAD_INS.iter().find(|lead| lower.starts_with(*lead)) else {
            break;
        };
        text = text[prefix.len()..].trim_start().to_string();
    }
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == ',' || c == ':');
    let mut chars = text.chars();
    let first = chars.next()?;
    let mut content: String = first.to_uppercase().chain(chars).collect();
    if !content.ends_with(['.', '!', '?']) {
        content.push('.');
    }
    if content.chars().count() > MAX_MEMORY_CHARS {
        content = content.chars().take(MAX_MEMORY_CHARS).collect();
    }
    Some(LessonProposal {
        kind: MemoryKind::Preference,
        content,
    })
}

/// Openings that say why something wasn't helpful, not what to remember. Lower case.
const LEAD_INS: &[&str] = &[
    "this is not helpful because",
    "this isn't helpful because",
    "it's not helpful because",
    "it is not helpful because",
    "not helpful because",
    "not helpful:",
    "not helpful,",
    "because",
    "since",
    "bc ",
];

const FEEDBACK_SELECT: &str = "SELECT id, run_id, target, about, rating, reason, scope, memory_id,
        created_at, updated_at
     FROM agent_feedback";

fn feedback_from_row(row: &Row<'_>) -> rusqlite::Result<AgentFeedback> {
    fn text_enum<T>(
        row: &Row<'_>,
        index: usize,
        parse: fn(&str) -> Option<T>,
    ) -> rusqlite::Result<T> {
        let value: String = row.get(index)?;
        parse(&value).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                format!("unexpected value '{value}'").into(),
            )
        })
    }
    Ok(AgentFeedback {
        id: row.get(0)?,
        run_id: row.get(1)?,
        target: text_enum(row, 2, FeedbackTarget::parse)?,
        about: row.get(3)?,
        rating: text_enum(row, 4, FeedbackRating::parse)?,
        reason: row.get(5)?,
        scope: text_enum(row, 6, FeedbackScope::parse)?,
        memory_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn optional_text(value: Option<String>, max: usize, what: &str) -> AppResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(invalid(&format!("{what} can be at most {max} characters.")));
    }
    Ok(Some(value.to_string()))
}

fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut clipped: String = flat.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

fn invalid(message: &str) -> AppError {
    AppError::Validation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        Confirmation, MessageRole, MessageRunStatus, NewAgentMessage, NewAgentRun, RunSource,
        UpdateMemorySettings,
    };
    use crate::tools::registry::ToolName;
    use crate::tools::{NoCalendars, SessionConfig, ToolSession, ToolSources};
    use chrono::Utc;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    const REASON: &str = "because I don't do deep work after 4";

    fn database() -> (TempDir, PlannerDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let database = PlannerDatabase::open(&dir.path().join("feedback.sqlite3")).unwrap();
        (dir, database)
    }

    fn run(database: &mut PlannerDatabase, source: RunSource) -> String {
        let experiment = (source != RunSource::Assistant).then(|| {
            database
                .start_experiment_run(crate::db::NewExperimentRun {
                    kind: RunSource::Benchmark,
                    benchmark_version: "v1".into(),
                    config_hash: "hash".into(),
                    architecture_id: "agent".into(),
                    model_name: "qwen3:8b".into(),
                    model_digest: None,
                    runtime_version: None,
                    hardware: None,
                    split: None,
                    result_path: None,
                    notes: None,
                    started_at: None,
                })
                .unwrap()
                .id
        });
        database
            .start_agent_run(NewAgentRun {
                source,
                experiment_run_id: experiment,
                case_id: None,
                case_family: None,
                goal: "Plan two hours of focus tomorrow.".into(),
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

    /// A conversation with one request and the drafted reply that `run_id` produced.
    fn conversation(database: &mut PlannerDatabase, run_id: &str) -> String {
        let thread = database.create_agent_thread(None).unwrap();
        for message in [
            NewAgentMessage {
                role: MessageRole::User,
                text: "Plan two hours of focus tomorrow.".into(),
                run_id: None,
                run_status: None,
                created_at: None,
            },
            NewAgentMessage {
                role: MessageRole::Assistant,
                text: "Drafted: Review lease 4:30 to 6:00 pm.".into(),
                run_id: Some(run_id.into()),
                run_status: Some(MessageRunStatus::Answered),
                created_at: None,
            },
        ] {
            database.append_agent_message(&thread.id, message).unwrap();
        }
        thread.id
    }

    fn correction(run_id: &str) -> NewFeedback {
        NewFeedback {
            run_id: run_id.into(),
            target: FeedbackTarget::Change,
            about: Some("Review lease, Thu 4:30 pm".into()),
            rating: FeedbackRating::NotHelpful,
            reason: Some(REASON.into()),
        }
    }

    fn preferences(database: &PlannerDatabase) -> Vec<Value> {
        let mut config = SessionConfig::new(Utc::now(), "America/Chicago".parse().unwrap());
        config.permitted_tools = Some([ToolName::GetPreferences].into());
        let mut session = ToolSession::new(config);
        let sources = ToolSources {
            database,
            calendars: &NoCalendars,
        };
        let outcome =
            serde_json::to_value(session.call(&sources, "get_preferences", json!({}))).unwrap();
        assert_eq!(outcome["ok"], true, "{outcome}");
        outcome["result"]["rememberedPreferences"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn last_assistant_turn(database: &PlannerDatabase, thread: &str) -> String {
        database
            .conversation_context(thread, 6)
            .unwrap()
            .turns
            .last()
            .unwrap()
            .text
            .clone()
    }

    #[test]
    fn a_lesson_the_person_keeps_steers_later_plans() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let feedback = database.record_agent_feedback(correction(&run_id)).unwrap();
        assert_eq!(feedback.scope, FeedbackScope::Pending);
        assert!(preferences(&database).is_empty(), "nothing is learned yet");

        let lesson = propose_lesson(feedback.reason.as_deref().unwrap()).unwrap();
        assert_eq!(lesson.content, "I don't do deep work after 4.");
        let learned = database
            .learn_from_feedback(&feedback.id, lesson.kind, "No deep work after 4 pm.")
            .unwrap();
        assert_eq!(learned.scope, FeedbackScope::Lasting);
        let memory = database
            .memory_record(learned.memory_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(memory.kind, MemoryKind::Preference);
        assert_eq!(memory.source, MemorySource::UserStated);
        assert_eq!(memory.confirmation, Confirmation::Confirmed);
        assert_eq!(
            memory.source_ref.as_deref(),
            Some(format!("feedback {} on assistant run {run_id}", feedback.id).as_str())
        );

        let listed = preferences(&database);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["content"], "No deep work after 4 pm.");
        assert_eq!(listed[0]["trust"], "user_confirmed");

        // Pressing Remember twice stores it once.
        assert!(database
            .learn_from_feedback(&feedback.id, MemoryKind::Preference, "Again.")
            .is_err());
        assert_eq!(preferences(&database).len(), 1);
        // And it can no longer be turned into "just this once".
        assert!(database.keep_feedback_one_off(&feedback.id).is_err());
    }

    #[test]
    fn just_this_once_stays_in_its_conversation() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let thread = conversation(&mut database, &run_id);
        let feedback = database.record_agent_feedback(correction(&run_id)).unwrap();
        let settled = database.keep_feedback_one_off(&feedback.id).unwrap();
        assert_eq!(settled.scope, FeedbackScope::ThisTime);
        assert!(settled.memory_id.is_none());

        // Nothing is remembered, so later runs aren't steered by it…
        assert!(preferences(&database).is_empty());
        assert!(database.export_memory().unwrap().records.is_empty());
        // …but a retry in the same conversation sees what was wrong.
        let turn = last_assistant_turn(&database, &thread);
        assert!(turn.contains("I don't do deep work after 4"), "{turn}");
        assert!(turn.contains("this conversation only"), "{turn}");
        assert!(turn.contains("Review lease, Thu 4:30 pm"), "{turn}");
        // A new conversation carries nothing.
        let other = database.create_agent_thread(None).unwrap();
        assert!(database
            .conversation_context(&other.id, 6)
            .unwrap()
            .turns
            .is_empty());
    }

    #[test]
    fn a_remembered_lesson_is_named_as_such_in_its_conversation() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let thread = conversation(&mut database, &run_id);
        let feedback = database.record_agent_feedback(correction(&run_id)).unwrap();
        database
            .learn_from_feedback(
                &feedback.id,
                MemoryKind::Preference,
                "No deep work after 4.",
            )
            .unwrap();
        let turn = last_assistant_turn(&database, &thread);
        assert!(turn.contains("asked you to remember"), "{turn}");
    }

    #[test]
    fn deleting_the_lesson_forgets_the_words_too() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let feedback = database.record_agent_feedback(correction(&run_id)).unwrap();
        let learned = database
            .learn_from_feedback(
                &feedback.id,
                MemoryKind::Preference,
                "No deep work after 4.",
            )
            .unwrap();
        database
            .delete_memory(learned.memory_id.as_deref().unwrap())
            .unwrap();
        let after = database.agent_feedback(&feedback.id).unwrap();
        assert_eq!(after.reason, None);
        assert_eq!(after.memory_id, None);
        assert_eq!(after.scope, FeedbackScope::Lasting);
        assert!(preferences(&database).is_empty());
    }

    #[test]
    fn a_lesson_that_cant_be_stored_can_be_tried_again() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let feedback = database.record_agent_feedback(correction(&run_id)).unwrap();
        database
            .update_memory_settings(UpdateMemorySettings {
                enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert!(database
            .learn_from_feedback(
                &feedback.id,
                MemoryKind::Preference,
                "No deep work after 4."
            )
            .is_err());
        assert_eq!(
            database.agent_feedback(&feedback.id).unwrap().scope,
            FeedbackScope::Pending
        );
        database
            .update_memory_settings(UpdateMemorySettings {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        // Too long is refused too, and still leaves it open.
        assert!(database
            .learn_from_feedback(&feedback.id, MemoryKind::Preference, &"x".repeat(501))
            .is_err());
        let learned = database
            .learn_from_feedback(
                &feedback.id,
                MemoryKind::Preference,
                "No deep work after 4.",
            )
            .unwrap();
        assert_eq!(learned.scope, FeedbackScope::Lasting);
    }

    #[test]
    fn helpful_and_reasonless_feedback_settle_at_once() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let helpful = database
            .record_agent_feedback(NewFeedback {
                run_id: run_id.clone(),
                target: FeedbackTarget::Answer,
                about: None,
                rating: FeedbackRating::Helpful,
                reason: Some("Great".into()),
            })
            .unwrap();
        assert_eq!(helpful.scope, FeedbackScope::ThisTime);
        let bare = database
            .record_agent_feedback(NewFeedback {
                run_id: run_id.clone(),
                target: FeedbackTarget::Answer,
                about: None,
                rating: FeedbackRating::NotHelpful,
                reason: Some("   ".into()),
            })
            .unwrap();
        assert_eq!(bare.scope, FeedbackScope::ThisTime);
        assert_eq!(bare.reason, None);
        for id in [&helpful.id, &bare.id] {
            assert!(database
                .learn_from_feedback(id, MemoryKind::Preference, "Something.")
                .is_err());
        }
        assert_eq!(database.run_feedback(&run_id).unwrap().len(), 2);
    }

    #[test]
    fn feedback_is_checked() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let benchmark = run(&mut database, RunSource::Benchmark);
        assert!(matches!(
            database.record_agent_feedback(correction("no-such-run")),
            Err(AppError::NotFound)
        ));
        assert!(database
            .record_agent_feedback(correction(&benchmark))
            .is_err());
        let mut no_about = correction(&run_id);
        no_about.about = None;
        assert!(database.record_agent_feedback(no_about).is_err());
        let mut long = correction(&run_id);
        long.reason = Some("x".repeat(MAX_FEEDBACK_REASON_CHARS + 1));
        assert!(database.record_agent_feedback(long).is_err());
        let mut heads_up = correction(&run_id);
        heads_up.target = FeedbackTarget::Suggestion;
        assert!(database.record_agent_feedback(heads_up).is_err());
        for _ in 0..MAX_FEEDBACK_PER_RUN {
            database.record_agent_feedback(correction(&run_id)).unwrap();
        }
        assert!(database.record_agent_feedback(correction(&run_id)).is_err());
    }

    #[test]
    fn deleting_the_run_deletes_its_feedback() {
        let (_dir, mut database) = database();
        let run_id = run(&mut database, RunSource::Assistant);
        let thread = conversation(&mut database, &run_id);
        let feedback = database.record_agent_feedback(correction(&run_id)).unwrap();
        database.delete_agent_thread(&thread).unwrap();
        assert!(matches!(
            database.agent_feedback(&feedback.id),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn schema_twelve_gains_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dayplan.sqlite3");
        drop(PlannerDatabase::open(&path).unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER agent_feedback_forget_lesson; DROP TABLE agent_feedback;
                 PRAGMA user_version = 12;",
            )
            .unwrap();
        drop(connection);

        let mut migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(
            migrated.schema_version().unwrap(),
            super::super::CURRENT_SCHEMA_VERSION
        );
        let run_id = run(&mut migrated, RunSource::Assistant);
        let feedback = migrated.record_agent_feedback(correction(&run_id)).unwrap();
        let learned = migrated
            .learn_from_feedback(
                &feedback.id,
                MemoryKind::Preference,
                "No deep work after 4.",
            )
            .unwrap();
        migrated
            .delete_memory(learned.memory_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(migrated.agent_feedback(&feedback.id).unwrap().reason, None);
    }

    #[test]
    fn lessons_are_offered_in_the_person_s_own_words() {
        let lesson = |text: &str| propose_lesson(text).map(|l| l.content);
        assert_eq!(
            lesson("Not helpful because   I don't do deep work after 4").as_deref(),
            Some("I don't do deep work after 4.")
        );
        assert_eq!(
            lesson("because, sponsors come before venue!").as_deref(),
            Some("Sponsors come before venue!")
        );
        assert_eq!(
            lesson("no meetings on fridays").as_deref(),
            Some("No meetings on fridays.")
        );
        assert_eq!(lesson("because"), None);
        assert_eq!(lesson("   "), None);
        assert_eq!(propose_lesson("x").unwrap().kind, MemoryKind::Preference);
    }
}
