//! What the assistant may remember between conversations, kept on the device and under the
//! person's control.
//!
//! Memory is not planner data and never replaces it: plans, tasks and events stay the
//! authoritative record. A memory is a short, source-linked note such as a decision ("the weekly
//! sync moved to Thursdays"), a project fact, or a preference. Every record carries:
//!
//! - a **kind** (`decision`, `priority`, `project_fact`, `open_problem`, `preference`, `note`),
//! - a **source** (`user_stated`, `user_correction`, `approved_action`, `assistant_inferred`) and
//!   an optional source reference (a run, draft, or conversation id),
//! - a **confirmation** state. Only the person confirms. Something the assistant inferred starts
//!   unconfirmed, is labelled as such everywhere, and expires unless confirmed,
//! - timestamps, an optional linked project, and a **supersession** link: a correction never
//!   edits a record in place, it creates a new record and marks the old one superseded, so
//!   outdated memories stay visible for audit but are never offered to the agent.
//!
//! The model has no way to write here. Records are added by the person (Settings), or by later
//! pieces through this API with an honest source. Settings can pause memory, turn it off per
//! project, export it, and delete one record, one project's records, or everything. Memory is
//! left out of planner exports and imports.

use super::{now, utc_string, PlannerDatabase};
use crate::error::{AppError, AppResult};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_MEMORY_CHARS: usize = 500;
/// A hard cap on stored records (including superseded ones), so memory can't grow without limit.
pub const MAX_MEMORY_RECORDS: i64 = 2_000;
pub const MAX_CONVERSATION_TURNS: i64 = 20;
pub const MIN_INFERRED_TTL_DAYS: i64 = 1;
pub const MAX_INFERRED_TTL_DAYS: i64 = 90;
const MAX_SOURCE_REF_CHARS: usize = 200;

pub(super) fn ensure_memory_schema(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_memory (
             id TEXT PRIMARY KEY NOT NULL,
             kind TEXT NOT NULL
                 CHECK(kind IN ('decision', 'priority', 'project_fact', 'open_problem',
                               'preference', 'note')),
             content TEXT NOT NULL CHECK(length(content) > 0),
             source TEXT NOT NULL
                 CHECK(source IN ('user_stated', 'user_correction', 'approved_action',
                                  'assistant_inferred')),
             source_ref TEXT,
             plan_id TEXT,
             confirmation TEXT NOT NULL CHECK(confirmation IN ('confirmed', 'unconfirmed')),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             confirmed_at TEXT,
             expires_at TEXT,
             superseded_by TEXT REFERENCES agent_memory(id) ON DELETE SET NULL,
             superseded_at TEXT,
             CHECK((confirmation = 'confirmed') = (confirmed_at IS NOT NULL)),
             CHECK(confirmation = 'confirmed' OR expires_at IS NOT NULL)
         );
         CREATE INDEX IF NOT EXISTS agent_memory_plan_idx ON agent_memory(plan_id);
         CREATE INDEX IF NOT EXISTS agent_memory_updated_idx ON agent_memory(updated_at);

         CREATE TABLE IF NOT EXISTS memory_settings (
             id INTEGER PRIMARY KEY NOT NULL CHECK(id = 1),
             enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
             conversation_turns INTEGER NOT NULL DEFAULT 6
                 CHECK(conversation_turns BETWEEN 0 AND 20),
             inferred_ttl_days INTEGER NOT NULL DEFAULT 14
                 CHECK(inferred_ttl_days BETWEEN 1 AND 90)
         );
         INSERT OR IGNORE INTO memory_settings (id) VALUES (1);

         CREATE TABLE IF NOT EXISTS memory_project_setting (
             plan_id TEXT PRIMARY KEY NOT NULL,
             enabled INTEGER NOT NULL CHECK(enabled IN (0, 1))
         );",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Decision,
    /// What matters most right now ("sponsors come before venue").
    Priority,
    ProjectFact,
    /// Something unresolved the person wants kept in view.
    OpenProblem,
    Preference,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    /// The person said it or typed it in Settings.
    UserStated,
    /// The person corrected an earlier memory; the old one is superseded.
    UserCorrection,
    /// Recorded from a change the person approved.
    ApprovedAction,
    /// The assistant's interpretation. Unconfirmed until the person confirms it, and it expires.
    AssistantInferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    Confirmed,
    Unconfirmed,
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

text_enum!(MemoryKind {
    Decision => "decision",
    Priority => "priority",
    ProjectFact => "project_fact",
    OpenProblem => "open_problem",
    Preference => "preference",
    Note => "note",
});
text_enum!(MemorySource {
    UserStated => "user_stated",
    UserCorrection => "user_correction",
    ApprovedAction => "approved_action",
    AssistantInferred => "assistant_inferred",
});
text_enum!(Confirmation { Confirmed => "confirmed", Unconfirmed => "unconfirmed" });

impl MemoryKind {
    pub const ALL: [MemoryKind; 6] = [
        Self::Decision,
        Self::Priority,
        Self::ProjectFact,
        Self::OpenProblem,
        Self::Preference,
        Self::Note,
    ];
}

/// One remembered note with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub source: MemorySource,
    pub source_ref: Option<String>,
    pub plan_id: Option<String>,
    pub confirmation: Confirmation,
    pub created_at: String,
    pub updated_at: String,
    pub confirmed_at: Option<String>,
    pub expires_at: Option<String>,
    pub superseded_by: Option<String>,
    pub superseded_at: Option<String>,
}

impl MemoryRecord {
    /// Whether the record may still be offered to the agent at `now`.
    pub fn is_current(&self, now: &str) -> bool {
        self.superseded_by.is_none()
            && self.superseded_at.is_none()
            && self.expires_at.as_deref().is_none_or(|at| at > now)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewMemory {
    pub kind: MemoryKind,
    pub content: String,
    pub source: MemorySource,
    pub source_ref: Option<String>,
    pub plan_id: Option<String>,
}

/// The person's memory controls.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySettings {
    /// When off, nothing new is remembered and the agent is offered no memory.
    pub enabled: bool,
    /// How many recent conversation turns the assistant may carry into a request (0 = none).
    pub conversation_turns: i64,
    /// How long an unconfirmed, assistant-inferred memory lives before it is forgotten.
    pub inferred_ttl_days: i64,
    /// Projects whose memory is turned off.
    pub disabled_plan_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMemorySettings {
    pub enabled: Option<bool>,
    pub conversation_turns: Option<i64>,
    pub inferred_ttl_days: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryFilter {
    pub plan_id: Option<String>,
    /// Also list superseded and expired records (Settings shows them as history).
    #[serde(default)]
    pub include_history: bool,
}

/// Everything memory holds, for the person to keep or inspect.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryExport {
    pub format: &'static str,
    pub exported_at: String,
    pub settings: MemorySettings,
    pub records: Vec<MemoryRecord>,
}

pub const MEMORY_EXPORT_FORMAT: &str = "delve.memory.v1";

/// A memory export read back in: the same shape `export_memory` writes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryImport {
    pub format: String,
    pub settings: ImportedMemorySettings,
    pub records: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedMemorySettings {
    pub enabled: bool,
    pub conversation_turns: i64,
    pub inferred_ttl_days: i64,
    #[serde(default)]
    pub disabled_plan_ids: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Writes

impl PlannerDatabase {
    pub fn memory_settings(&self) -> AppResult<MemorySettings> {
        read_settings(&self.connection)
    }

    /// Whether `id` names a project, as opposed to a task, event or any other record.
    pub fn is_plan(&self, id: &str) -> AppResult<bool> {
        Ok(super::planning::ensure_plan_exists(&self.connection, id).is_ok())
    }

    pub fn update_memory_settings(
        &mut self,
        update: UpdateMemorySettings,
    ) -> AppResult<MemorySettings> {
        if let Some(turns) = update.conversation_turns {
            if !(0..=MAX_CONVERSATION_TURNS).contains(&turns) {
                return Err(invalid(&format!(
                    "Recent conversation must be between 0 and {MAX_CONVERSATION_TURNS} turns."
                )));
            }
        }
        if let Some(days) = update.inferred_ttl_days {
            if !(MIN_INFERRED_TTL_DAYS..=MAX_INFERRED_TTL_DAYS).contains(&days) {
                return Err(invalid(&format!(
                    "Unconfirmed memories must expire after {MIN_INFERRED_TTL_DAYS} to \
                     {MAX_INFERRED_TTL_DAYS} days."
                )));
            }
        }
        self.connection.execute(
            "UPDATE memory_settings SET
                 enabled = COALESCE(?1, enabled),
                 conversation_turns = COALESCE(?2, conversation_turns),
                 inferred_ttl_days = COALESCE(?3, inferred_ttl_days)
             WHERE id = 1",
            params![
                update.enabled,
                update.conversation_turns,
                update.inferred_ttl_days
            ],
        )?;
        self.memory_settings()
    }

    /// Turns memory on or off for one project. Turning it off does not delete what is stored;
    /// `forget_project_memory` does.
    pub fn set_project_memory(
        &mut self,
        plan_id: &str,
        enabled: bool,
    ) -> AppResult<MemorySettings> {
        let plan_id = plan_id_text(plan_id)?;
        if enabled {
            self.connection.execute(
                "DELETE FROM memory_project_setting WHERE plan_id = ?1",
                params![plan_id],
            )?;
        } else {
            self.connection.execute(
                "INSERT INTO memory_project_setting (plan_id, enabled) VALUES (?1, 0)
                 ON CONFLICT(plan_id) DO UPDATE SET enabled = 0",
                params![plan_id],
            )?;
        }
        self.memory_settings()
    }

    /// Remembers one note. Assistant inferences start unconfirmed and expire; everything else is
    /// confirmed because the person stated or approved it.
    pub fn add_memory(&mut self, input: NewMemory) -> AppResult<MemoryRecord> {
        let at = now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let settings = read_settings(&transaction)?;
        if !settings.enabled {
            return Err(invalid("Memory is paused. Turn it on in Settings first."));
        }
        let plan_id = input.plan_id.as_deref().map(plan_id_text).transpose()?;
        if let Some(plan) = &plan_id {
            if settings.disabled_plan_ids.iter().any(|id| id == plan) {
                return Err(invalid("Memory is turned off for this project."));
            }
        }
        if input.source == MemorySource::UserCorrection {
            return Err(invalid(
                "A correction replaces an existing memory; use correct_memory.",
            ));
        }
        let count: i64 =
            transaction.query_row("SELECT COUNT(*) FROM agent_memory", [], |row| row.get(0))?;
        if count >= MAX_MEMORY_RECORDS {
            return Err(invalid(&format!(
                "Memory holds its limit of {MAX_MEMORY_RECORDS} records. Delete some first."
            )));
        }
        let (confirmation, confirmed_at, expires_at) =
            if input.source == MemorySource::AssistantInferred {
                let expires = utc_string(Utc::now() + Duration::days(settings.inferred_ttl_days));
                (Confirmation::Unconfirmed, None, Some(expires))
            } else {
                (Confirmation::Confirmed, Some(at.clone()), None)
            };
        let id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO agent_memory (id, kind, content, source, source_ref, plan_id,
                 confirmation, created_at, updated_at, confirmed_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)",
            params![
                id,
                input.kind.as_str(),
                content_text(&input.content)?,
                input.source.as_str(),
                source_ref_text(input.source_ref)?,
                plan_id,
                confirmation.as_str(),
                at,
                confirmed_at,
                expires_at,
            ],
        )?;
        transaction.commit()?;
        self.memory_record(&id)
    }

    /// The person confirms an assistant inference: it stops expiring and is no longer labelled
    /// unconfirmed. Its source stays `assistant_inferred`, so where it came from is never lost.
    pub fn confirm_memory(&mut self, id: &str) -> AppResult<MemoryRecord> {
        let record = self.memory_record(id)?;
        if record.superseded_by.is_some() || record.superseded_at.is_some() {
            return Err(invalid("A superseded memory can't be confirmed."));
        }
        if record.confirmation == Confirmation::Confirmed {
            return Ok(record);
        }
        let at = now();
        if record
            .expires_at
            .as_deref()
            .is_some_and(|expiry| expiry <= at.as_str())
        {
            return Err(invalid("This memory has expired."));
        }
        self.connection.execute(
            "UPDATE agent_memory SET confirmation = 'confirmed', confirmed_at = ?1,
                 expires_at = NULL, updated_at = ?1
             WHERE id = ?2",
            params![at, id],
        )?;
        self.memory_record(id)
    }

    /// The person corrects a memory. The old record is kept as history, marked superseded, and a
    /// new confirmed record with the corrected text takes its place.
    pub fn correct_memory(&mut self, id: &str, content: &str) -> AppResult<MemoryRecord> {
        let content = content_text(content)?.to_string();
        let at = now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = select_record(&transaction, id)?;
        if old.superseded_by.is_some() || old.superseded_at.is_some() {
            return Err(AppError::Conflict);
        }
        let new_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO agent_memory (id, kind, content, source, source_ref, plan_id,
                 confirmation, created_at, updated_at, confirmed_at)
             VALUES (?1, ?2, ?3, 'user_correction', ?4, ?5, 'confirmed', ?6, ?6, ?6)",
            params![new_id, old.kind.as_str(), content, old.id, old.plan_id, at],
        )?;
        transaction.execute(
            "UPDATE agent_memory SET superseded_by = ?1, superseded_at = ?2, updated_at = ?2
             WHERE id = ?3",
            params![new_id, at, old.id],
        )?;
        transaction.commit()?;
        self.memory_record(&new_id)
    }

    pub fn delete_memory(&mut self, id: &str) -> AppResult<()> {
        // Deleted means gone: the record goes together with every version it replaced and every
        // correction that replaced it, so no earlier wording survives as history.
        let changed = self.connection.execute(
            "WITH RECURSIVE chain(id) AS (
                 SELECT id FROM agent_memory WHERE id = ?1
                 UNION
                 SELECT m.id FROM agent_memory m, chain c
                 WHERE m.superseded_by = c.id
                    OR m.id = (SELECT superseded_by FROM agent_memory WHERE id = c.id)
             )
             DELETE FROM agent_memory WHERE id IN (SELECT id FROM chain)",
            params![id],
        )?;
        if changed == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// Deletes every memory linked to one project. Returns how many were deleted.
    pub fn forget_project_memory(&mut self, plan_id: &str) -> AppResult<usize> {
        Ok(self.connection.execute(
            "DELETE FROM agent_memory WHERE plan_id = ?1",
            params![plan_id_text(plan_id)?],
        )?)
    }

    /// Deletes every memory. Settings are kept.
    pub fn clear_memory(&mut self) -> AppResult<usize> {
        Ok(self.connection.execute("DELETE FROM agent_memory", [])?)
    }

    /// Forgets unconfirmed memories whose time ran out. Returns how many were deleted.
    pub fn prune_expired_memory(&mut self) -> AppResult<usize> {
        Ok(self.connection.execute(
            "DELETE FROM agent_memory
             WHERE confirmation = 'unconfirmed' AND expires_at IS NOT NULL AND expires_at <= ?1",
            params![now()],
        )?)
    }

    // -- reads ----------------------------------------------------------------------------------

    pub fn memory_record(&self, id: &str) -> AppResult<MemoryRecord> {
        select_record(&self.connection, id)
    }

    /// Records newest first. Without `include_history`, only records the agent could be offered
    /// right now (not superseded, not expired).
    pub fn list_memory(&self, filter: MemoryFilter) -> AppResult<Vec<MemoryRecord>> {
        let mut statement = self.connection.prepare(&format!(
            "{RECORD_SELECT} WHERE (?1 IS NULL OR plan_id = ?1)
             ORDER BY updated_at DESC, id DESC"
        ))?;
        let rows = statement
            .query_map(params![filter.plan_id], record_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if filter.include_history {
            return Ok(rows);
        }
        let at = now();
        Ok(rows.into_iter().filter(|r| r.is_current(&at)).collect())
    }

    /// Every record (including history) with the settings, for export.
    pub fn export_memory(&self) -> AppResult<MemoryExport> {
        Ok(MemoryExport {
            format: MEMORY_EXPORT_FORMAT,
            exported_at: now(),
            settings: self.memory_settings()?,
            records: self.list_memory(MemoryFilter {
                plan_id: None,
                include_history: true,
            })?,
        })
    }

    /// Replaces everything memory holds (records and settings) with an export, keeping every
    /// record's provenance, confirmation and supersession exactly as exported. Used to restore a
    /// memory export and to seed benchmark fixtures. Returns how many records were imported.
    pub fn import_memory(&mut self, import: &MemoryImport) -> AppResult<usize> {
        if import.format != MEMORY_EXPORT_FORMAT {
            return Err(invalid(&format!(
                "This isn't a {MEMORY_EXPORT_FORMAT} memory export."
            )));
        }
        if import.records.len() as i64 > MAX_MEMORY_RECORDS {
            return Err(invalid(&format!(
                "A memory export may hold at most {MAX_MEMORY_RECORDS} records."
            )));
        }
        let settings = &import.settings;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM agent_memory", [])?;
        transaction.execute("DELETE FROM memory_project_setting", [])?;
        transaction
            .execute(
                "UPDATE memory_settings
                 SET enabled = ?1, conversation_turns = ?2, inferred_ttl_days = ?3 WHERE id = 1",
                params![
                    settings.enabled,
                    settings.conversation_turns,
                    settings.inferred_ttl_days
                ],
            )
            .map_err(|_| invalid("The export's memory settings are out of range."))?;
        for plan_id in &settings.disabled_plan_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO memory_project_setting (plan_id, enabled) VALUES (?1, 0)",
                params![plan_id_text(plan_id)?],
            )?;
        }
        // Supersession links point at other imported records, so they are set once all exist.
        for record in &import.records {
            transaction
                .execute(
                    "INSERT INTO agent_memory (id, kind, content, source, source_ref, plan_id,
                         confirmation, created_at, updated_at, confirmed_at, expires_at,
                         superseded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        record.id,
                        record.kind.as_str(),
                        content_text(&record.content)?,
                        record.source.as_str(),
                        source_ref_text(record.source_ref.clone())?,
                        record.plan_id.as_deref().map(plan_id_text).transpose()?,
                        record.confirmation.as_str(),
                        record.created_at,
                        record.updated_at,
                        record.confirmed_at,
                        record.expires_at,
                        record.superseded_at,
                    ],
                )
                .map_err(|_| {
                    invalid(&format!(
                        "Memory '{}' is a duplicate or contradicts itself.",
                        record.id
                    ))
                })?;
        }
        for record in &import.records {
            if let Some(newer) = &record.superseded_by {
                transaction
                    .execute(
                        "UPDATE agent_memory SET superseded_by = ?2 WHERE id = ?1",
                        params![record.id, newer],
                    )
                    .map_err(|_| {
                        invalid(&format!(
                            "Memory '{}' is replaced by '{newer}', which isn't in the export.",
                            record.id
                        ))
                    })?;
            }
        }
        transaction.commit()?;
        Ok(import.records.len())
    }

    /// What the agent may be offered at `now`: memory is on, the record is current, and its
    /// project (if any) has memory on. Selection by relevance happens in the `get_memory` tool.
    pub fn memory_candidates(&self, now: DateTime<Utc>) -> AppResult<(bool, Vec<MemoryRecord>)> {
        let settings = self.memory_settings()?;
        if !settings.enabled {
            return Ok((false, Vec::new()));
        }
        let at = utc_string(now);
        let records = self
            .list_memory(MemoryFilter {
                plan_id: None,
                include_history: true,
            })?
            .into_iter()
            .filter(|record| record.is_current(&at))
            .filter(|record| {
                record
                    .plan_id
                    .as_ref()
                    .is_none_or(|plan| !settings.disabled_plan_ids.contains(plan))
            })
            .collect();
        Ok((true, records))
    }
}

const RECORD_SELECT: &str = "SELECT id, kind, content, source, source_ref, plan_id, confirmation,
        created_at, updated_at, confirmed_at, expires_at, superseded_by, superseded_at
     FROM agent_memory";

fn select_record(connection: &Connection, id: &str) -> AppResult<MemoryRecord> {
    connection
        .query_row(
            &format!("{RECORD_SELECT} WHERE id = ?1"),
            params![id],
            record_from_row,
        )
        .optional()?
        .ok_or(AppError::NotFound)
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
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
    Ok(MemoryRecord {
        id: row.get(0)?,
        kind: text_enum(row, 1, MemoryKind::parse)?,
        content: row.get(2)?,
        source: text_enum(row, 3, MemorySource::parse)?,
        source_ref: row.get(4)?,
        plan_id: row.get(5)?,
        confirmation: text_enum(row, 6, Confirmation::parse)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        confirmed_at: row.get(9)?,
        expires_at: row.get(10)?,
        superseded_by: row.get(11)?,
        superseded_at: row.get(12)?,
    })
}

fn read_settings(connection: &Connection) -> AppResult<MemorySettings> {
    let (enabled, conversation_turns, inferred_ttl_days) = connection.query_row(
        "SELECT enabled, conversation_turns, inferred_ttl_days FROM memory_settings WHERE id = 1",
        [],
        |row| {
            Ok((
                row.get::<_, bool>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    let mut statement = connection
        .prepare("SELECT plan_id FROM memory_project_setting WHERE enabled = 0 ORDER BY plan_id")?;
    let disabled_plan_ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MemorySettings {
        enabled,
        conversation_turns,
        inferred_ttl_days,
        disabled_plan_ids,
    })
}

fn invalid(message: &str) -> AppError {
    AppError::Validation(message.to_string())
}

fn content_text(value: &str) -> AppResult<&str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid("A memory needs some text."));
    }
    if value.chars().count() > MAX_MEMORY_CHARS {
        return Err(invalid(&format!(
            "A memory is limited to {MAX_MEMORY_CHARS} characters."
        )));
    }
    Ok(value)
}

fn source_ref_text(value: Option<String>) -> AppResult<Option<String>> {
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) if text.chars().count() <= MAX_SOURCE_REF_CHARS => Ok(Some(text.to_string())),
        Some(_) => Err(invalid("A memory's source reference is too long.")),
    }
}

fn plan_id_text(value: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_SOURCE_REF_CHARS {
        return Err(invalid("A project id is required."));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn database() -> (TempDir, PlannerDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let database = PlannerDatabase::open(&dir.path().join("memory.sqlite3")).unwrap();
        (dir, database)
    }

    fn note(content: &str, source: MemorySource, plan: Option<&str>) -> NewMemory {
        NewMemory {
            kind: MemoryKind::Decision,
            content: content.into(),
            source,
            source_ref: None,
            plan_id: plan.map(str::to_string),
        }
    }

    #[test]
    fn deleting_a_corrected_memory_deletes_its_history_too() {
        let (_dir, mut db) = database();
        let first = db
            .add_memory(note("Sync on Mondays.", MemorySource::UserStated, None))
            .unwrap();
        let second = db.correct_memory(&first.id, "Sync on Tuesdays.").unwrap();
        let third = db.correct_memory(&second.id, "Sync on Thursdays.").unwrap();
        let other = db
            .add_memory(note(
                "Blog moves to phase 2.",
                MemorySource::UserStated,
                None,
            ))
            .unwrap();
        db.delete_memory(&third.id).unwrap();
        let left = db
            .list_memory(MemoryFilter {
                plan_id: None,
                include_history: true,
            })
            .unwrap();
        assert_eq!(
            left.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            [other.id.as_str()]
        );

        // Deleting from the middle of a chain removes the whole chain as well.
        let a = db
            .add_memory(note("A.", MemorySource::UserStated, None))
            .unwrap();
        let b = db.correct_memory(&a.id, "B.").unwrap();
        db.correct_memory(&b.id, "C.").unwrap();
        db.delete_memory(&b.id).unwrap();
        assert_eq!(db.export_memory().unwrap().records.len(), 1);
        assert!(matches!(db.delete_memory(&a.id), Err(AppError::NotFound)));
    }

    #[test]
    fn an_export_imports_back_unchanged() {
        let (_dir, mut db) = database();
        let plan = "00000000-0000-4000-8000-000000000101";
        let old = db
            .add_memory(note(
                "Sync on Tuesdays.",
                MemorySource::UserStated,
                Some(plan),
            ))
            .unwrap();
        db.correct_memory(&old.id, "Sync on Thursdays.").unwrap();
        db.add_memory(note(
            "Maybe mornings.",
            MemorySource::AssistantInferred,
            None,
        ))
        .unwrap();
        db.set_project_memory(plan, false).unwrap();
        let export = db.export_memory().unwrap();
        let json = serde_json::to_string(&export).unwrap();

        let (_other_dir, mut other) = database();
        let import: MemoryImport = serde_json::from_str(&json).unwrap();
        assert_eq!(other.import_memory(&import).unwrap(), 3);
        let again = other.export_memory().unwrap();
        assert_eq!(again.records, export.records);
        assert_eq!(again.settings, export.settings);

        let mut broken = import.clone();
        broken.records[0].superseded_by = Some("missing".into());
        assert!(other.import_memory(&broken).is_err());
        assert_eq!(other.export_memory().unwrap().records, export.records);
        let mut wrong = import;
        wrong.format = "something.else".into();
        assert!(other.import_memory(&wrong).is_err());
    }

    #[test]
    fn stated_memories_are_confirmed_and_inferences_are_not() {
        let (_dir, mut db) = database();
        let stated = db
            .add_memory(note(
                "Weekly sync is on Thursdays.",
                MemorySource::UserStated,
                None,
            ))
            .unwrap();
        assert_eq!(stated.confirmation, Confirmation::Confirmed);
        assert!(stated.confirmed_at.is_some());
        assert!(stated.expires_at.is_none());

        let inferred = db
            .add_memory(note(
                "Prefers mornings for deep work.",
                MemorySource::AssistantInferred,
                None,
            ))
            .unwrap();
        assert_eq!(inferred.confirmation, Confirmation::Unconfirmed);
        assert!(inferred.confirmed_at.is_none());
        assert!(inferred.expires_at.is_some());

        let confirmed = db.confirm_memory(&inferred.id).unwrap();
        assert_eq!(confirmed.confirmation, Confirmation::Confirmed);
        assert_eq!(confirmed.source, MemorySource::AssistantInferred);
        assert!(confirmed.expires_at.is_none());
    }

    #[test]
    fn a_correction_supersedes_instead_of_editing() {
        let (_dir, mut db) = database();
        let old = db
            .add_memory(note(
                "Sync on Tuesdays.",
                MemorySource::UserStated,
                Some("p1"),
            ))
            .unwrap();
        let new = db
            .correct_memory(&old.id, "Sync moved to Thursdays.")
            .unwrap();
        assert_eq!(new.source, MemorySource::UserCorrection);
        assert_eq!(new.source_ref.as_deref(), Some(old.id.as_str()));
        assert_eq!(new.plan_id.as_deref(), Some("p1"));

        let old = db.memory_record(&old.id).unwrap();
        assert_eq!(old.superseded_by.as_deref(), Some(new.id.as_str()));
        assert_eq!(old.content, "Sync on Tuesdays.");

        let current = db.list_memory(MemoryFilter::default()).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, new.id);
        let history = db
            .list_memory(MemoryFilter {
                plan_id: None,
                include_history: true,
            })
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(matches!(
            db.correct_memory(&old.id, "again"),
            Err(AppError::Conflict)
        ));
        assert!(db.confirm_memory(&old.id).is_err());
    }

    #[test]
    fn pausing_and_project_switches_withhold_memory() {
        let (_dir, mut db) = database();
        db.add_memory(note("General.", MemorySource::UserStated, None))
            .unwrap();
        db.add_memory(note(
            "Launch fact.",
            MemorySource::UserStated,
            Some("launch"),
        ))
        .unwrap();
        let (enabled, records) = db.memory_candidates(Utc::now()).unwrap();
        assert!(enabled);
        assert_eq!(records.len(), 2);

        db.set_project_memory("launch", false).unwrap();
        let (_, records) = db.memory_candidates(Utc::now()).unwrap();
        assert_eq!(records.len(), 1);
        assert!(db
            .add_memory(note("More.", MemorySource::UserStated, Some("launch")))
            .is_err());
        assert_eq!(
            db.memory_settings().unwrap().disabled_plan_ids,
            vec!["launch".to_string()]
        );

        db.set_project_memory("launch", true).unwrap();
        db.update_memory_settings(UpdateMemorySettings {
            enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
        let (enabled, records) = db.memory_candidates(Utc::now()).unwrap();
        assert!(!enabled);
        assert!(records.is_empty());
        assert!(db
            .add_memory(note("Paused.", MemorySource::UserStated, None))
            .is_err());
    }

    #[test]
    fn expired_inferences_are_withheld_and_pruned() {
        let (_dir, mut db) = database();
        let inferred = db
            .add_memory(note("Maybe.", MemorySource::AssistantInferred, None))
            .unwrap();
        let later = Utc::now() + Duration::days(15);
        let (_, records) = db.memory_candidates(later).unwrap();
        assert!(records.is_empty(), "the default 14-day expiry has passed");
        let (_, records) = db.memory_candidates(Utc::now()).unwrap();
        assert_eq!(records.len(), 1);

        db.connection
            .execute(
                "UPDATE agent_memory SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                params![inferred.id],
            )
            .unwrap();
        assert!(db.confirm_memory(&inferred.id).is_err());
        assert_eq!(db.prune_expired_memory().unwrap(), 1);
        assert!(matches!(
            db.memory_record(&inferred.id),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn delete_paths_and_export() {
        let (_dir, mut db) = database();
        let one = db
            .add_memory(note("One.", MemorySource::UserStated, Some("a")))
            .unwrap();
        db.add_memory(note("Two.", MemorySource::UserStated, Some("a")))
            .unwrap();
        db.add_memory(note("Three.", MemorySource::ApprovedAction, None))
            .unwrap();
        db.delete_memory(&one.id).unwrap();
        assert!(matches!(db.delete_memory(&one.id), Err(AppError::NotFound)));
        assert_eq!(db.forget_project_memory("a").unwrap(), 1);
        let export = db.export_memory().unwrap();
        assert_eq!(export.format, MEMORY_EXPORT_FORMAT);
        assert_eq!(export.records.len(), 1);
        assert_eq!(db.clear_memory().unwrap(), 1);
        assert!(db.list_memory(MemoryFilter::default()).unwrap().is_empty());
    }

    #[test]
    fn inputs_are_bounded() {
        let (_dir, mut db) = database();
        assert!(db
            .add_memory(note("   ", MemorySource::UserStated, None))
            .is_err());
        assert!(db
            .add_memory(note(&"x".repeat(501), MemorySource::UserStated, None))
            .is_err());
        assert!(db
            .add_memory(note("Fix.", MemorySource::UserCorrection, None))
            .is_err());
        assert!(db
            .update_memory_settings(UpdateMemorySettings {
                conversation_turns: Some(21),
                ..Default::default()
            })
            .is_err());
        assert!(db
            .update_memory_settings(UpdateMemorySettings {
                inferred_ttl_days: Some(0),
                ..Default::default()
            })
            .is_err());
        let settings = db
            .update_memory_settings(UpdateMemorySettings {
                conversation_turns: Some(0),
                inferred_ttl_days: Some(30),
                enabled: None,
            })
            .unwrap();
        assert_eq!(settings.conversation_turns, 0);
        assert_eq!(settings.inferred_ttl_days, 30);
        assert!(settings.enabled);
    }

    #[test]
    fn schema_nine_gains_memory_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dayplan.sqlite3");
        drop(PlannerDatabase::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "DROP TABLE agent_memory; DROP TABLE memory_settings;
                 DROP TABLE memory_project_setting; PRAGMA user_version = 9;",
            )
            .unwrap();
        drop(connection);

        let mut migrated = PlannerDatabase::open(&path).unwrap();
        assert_eq!(
            migrated.schema_version().unwrap(),
            super::super::CURRENT_SCHEMA_VERSION
        );
        let settings = migrated.memory_settings().unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.conversation_turns, 6);
        assert_eq!(settings.inferred_ttl_days, 14);
        assert!(settings.disabled_plan_ids.is_empty());
        migrated
            .add_memory(note(
                "Works after the upgrade.",
                MemorySource::UserStated,
                None,
            ))
            .unwrap();
    }

    #[test]
    fn memory_is_not_part_of_planner_exports() {
        let (_dir, mut db) = database();
        db.add_memory(note("Kept apart.", MemorySource::UserStated, None))
            .unwrap();
        let bundle = serde_json::to_string(&db.export_bundle().unwrap()).unwrap();
        assert!(!bundle.contains("Kept apart."));
    }
}
