//! The capture inbox: unorganized thoughts that later become tasks, plans, or events.

use super::planning::{insert_plan, insert_task};
use super::{
    collect, insert_event, now, validate_id, validate_revision, PlannerDatabase, PreparedEvent,
    SqlConnection,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    CreateInboxItemInput, InboxConversion, InboxItem, InboxTarget, ProcessInboxItemInput,
    RecordKind, UpdateInboxItemInput, MAX_NOTES_LENGTH, MAX_TITLE_LENGTH,
};
use rusqlite::{params, OptionalExtension, Row, TransactionBehavior};
use uuid::Uuid;

const INBOX_SELECT: &str =
    "SELECT id, text, notes, revision, created_at, updated_at FROM inbox_items";

impl PlannerDatabase {
    /// Everything captured and not yet converted, newest first.
    pub fn list_inbox_items(&self) -> AppResult<Vec<InboxItem>> {
        let mut statement = self.connection.prepare(&format!(
            "{INBOX_SELECT} ORDER BY created_at DESC, rowid DESC"
        ))?;
        let rows = statement.query_map([], inbox_item_from_row)?;
        collect(rows)
    }

    pub fn create_inbox_item(&mut self, input: CreateInboxItemInput) -> AppResult<InboxItem> {
        validate_inbox_text(&input.text, &input.notes)?;
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        self.connection.execute(
            "INSERT INTO inbox_items (id, text, notes, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            params![id, input.text.trim(), input.notes.trim(), timestamp],
        )?;
        inbox_item_by_id(&self.connection, &id)?.ok_or(AppError::NotFound)
    }

    pub fn update_inbox_item(&mut self, input: UpdateInboxItemInput) -> AppResult<InboxItem> {
        validate_id(&input.id)?;
        validate_revision(input.revision)?;
        validate_inbox_text(&input.text, &input.notes)?;
        let changed = self.connection.execute(
            "UPDATE inbox_items
             SET text = ?1, notes = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?4 AND revision = ?5",
            params![
                input.text.trim(),
                input.notes.trim(),
                now(),
                input.id,
                input.revision
            ],
        )?;
        if changed != 1 {
            return Err(stale_inbox_error(&self.connection, &input.id)?);
        }
        inbox_item_by_id(&self.connection, &input.id)?.ok_or(AppError::NotFound)
    }

    pub fn delete_inbox_item(&mut self, id: &str, revision: i64) -> AppResult<()> {
        validate_id(id)?;
        validate_revision(revision)?;
        let changed = self.connection.execute(
            "DELETE FROM inbox_items WHERE id = ?1 AND revision = ?2",
            params![id, revision],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(stale_inbox_error(&self.connection, id)?)
        }
    }

    /// Creates the target record and removes the inbox item in one transaction, so a rejected
    /// conversion keeps the item and a stale item creates nothing.
    pub fn process_inbox_item(
        &mut self,
        input: ProcessInboxItemInput,
    ) -> AppResult<InboxConversion> {
        validate_id(&input.id)?;
        validate_revision(input.revision)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let item = inbox_item_by_id(&transaction, &input.id)?.ok_or(AppError::NotFound)?;
        if item.revision != input.revision {
            return Err(AppError::Conflict);
        }
        let conversion = match input.target {
            InboxTarget::Task { task } => InboxConversion {
                kind: RecordKind::Task,
                id: insert_task(&transaction, &task)?.id,
            },
            InboxTarget::Plan { plan } => InboxConversion {
                kind: RecordKind::Plan,
                id: insert_plan(&transaction, &plan)?.id,
            },
            InboxTarget::Event { event } => InboxConversion {
                kind: RecordKind::Event,
                id: insert_event(&transaction, PreparedEvent::from_input(event)?)?.id,
            },
        };
        transaction.execute("DELETE FROM inbox_items WHERE id = ?1", params![item.id])?;
        transaction.commit()?;
        Ok(conversion)
    }

    pub(super) fn all_inbox_items(&self) -> AppResult<Vec<InboxItem>> {
        let mut statement = self.connection.prepare(&format!(
            "{INBOX_SELECT} ORDER BY created_at ASC, rowid ASC"
        ))?;
        let rows = statement.query_map([], inbox_item_from_row)?;
        collect(rows)
    }
}

/// Captured text follows task title limits so it can become a title unchanged.
pub(super) fn validate_inbox_text(text: &str, notes: &str) -> AppResult<()> {
    if !(1..=MAX_TITLE_LENGTH).contains(&text.trim().chars().count()) {
        return Err(AppError::Validation(
            "Captured text must be 1–140 characters.".into(),
        ));
    }
    if notes.trim().chars().count() > MAX_NOTES_LENGTH {
        return Err(AppError::Validation(
            "Inbox notes must be 800 characters or fewer.".into(),
        ));
    }
    Ok(())
}

fn stale_inbox_error<C: SqlConnection>(connection: &C, id: &str) -> AppResult<AppError> {
    let exists: bool = connection.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM inbox_items WHERE id = ?1)",
        params![id],
        |row| row.get(0),
    )?;
    Ok(if exists {
        AppError::Conflict
    } else {
        AppError::NotFound
    })
}

fn inbox_item_by_id<C: SqlConnection>(connection: &C, id: &str) -> AppResult<Option<InboxItem>> {
    connection
        .connection()
        .query_row(
            &format!("{INBOX_SELECT} WHERE id = ?1"),
            params![id],
            inbox_item_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn inbox_item_from_row(row: &Row<'_>) -> rusqlite::Result<InboxItem> {
    Ok(InboxItem {
        id: row.get(0)?,
        text: row.get(1)?,
        notes: row.get(2)?,
        revision: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CreateEventInput, CreatePlanInput, CreateTaskInput, PlanStatus, TaskPriority, TaskStatus,
    };
    use tempfile::tempdir;

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn capture(database: &mut PlannerDatabase, text: &str) -> InboxItem {
        database
            .create_inbox_item(CreateInboxItemInput {
                text: text.into(),
                notes: String::new(),
            })
            .unwrap()
    }

    fn task(title: &str) -> CreateTaskInput {
        CreateTaskInput {
            title: title.into(),
            description: String::new(),
            plan_id: None,
            milestone_id: None,
            workstream_id: None,
            owner_id: None,
            due_date: None,
            scheduled_day: None,
            planned_week: None,
            estimated_minutes: None,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
        }
    }

    fn inbox_count(database: &PlannerDatabase) -> usize {
        database.list_inbox_items().unwrap().len()
    }

    #[test]
    fn captures_need_only_text_and_edits_are_revision_checked() {
        let mut database = database();
        let item = database
            .create_inbox_item(CreateInboxItemInput {
                text: "  Call dentist ".into(),
                notes: " Before Friday ".into(),
            })
            .unwrap();
        assert_eq!(
            (item.text.as_str(), item.notes.as_str()),
            ("Call dentist", "Before Friday")
        );
        assert!(matches!(
            database.create_inbox_item(CreateInboxItemInput {
                text: "   ".into(),
                notes: String::new(),
            }),
            Err(AppError::Validation(_))
        ));
        let edit = |revision, text: &str| UpdateInboxItemInput {
            id: item.id.clone(),
            revision,
            text: text.into(),
            notes: String::new(),
        };
        let edited = database
            .update_inbox_item(edit(1, "Call the dentist"))
            .unwrap();
        assert_eq!(edited.revision, 2);
        assert!(matches!(
            database.update_inbox_item(edit(1, "Stale")),
            Err(AppError::Conflict)
        ));
        assert!(matches!(
            database.delete_inbox_item(&item.id, 1),
            Err(AppError::Conflict)
        ));
        database.delete_inbox_item(&item.id, 2).unwrap();
        assert!(matches!(
            database.delete_inbox_item(&item.id, 2),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn converting_an_item_creates_the_record_and_removes_the_item_together() {
        let mut database = database();
        let dentist = capture(&mut database, "Call dentist");
        let hotels = capture(&mut database, "Research hotels");
        let party = capture(&mut database, "Plan the housewarming");
        assert_eq!(
            database
                .list_inbox_items()
                .unwrap()
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            ["Plan the housewarming", "Research hotels", "Call dentist"]
        );

        let converted = database
            .process_inbox_item(ProcessInboxItemInput {
                id: dentist.id.clone(),
                revision: dentist.revision,
                target: InboxTarget::Task {
                    task: CreateTaskInput {
                        planned_week: Some("2026-09-13".into()),
                        estimated_minutes: Some(15),
                        ..task("Call dentist")
                    },
                },
            })
            .unwrap();
        assert_eq!(converted.kind, RecordKind::Task);
        let created = database.all_tasks().unwrap().remove(0);
        assert_eq!(
            (
                created.id.as_str(),
                created.planned_week.as_deref(),
                created.estimated_minutes
            ),
            (converted.id.as_str(), Some("2026-09-13"), Some(15))
        );

        let plan = database
            .process_inbox_item(ProcessInboxItemInput {
                id: party.id.clone(),
                revision: party.revision,
                target: InboxTarget::Plan {
                    plan: CreatePlanInput {
                        title: "Housewarming".into(),
                        description: String::new(),
                        status: PlanStatus::Planning,
                        start_date: None,
                        target_date: None,
                        color: None,
                    },
                },
            })
            .unwrap();
        assert_eq!(plan.kind, RecordKind::Plan);
        let event = database
            .process_inbox_item(ProcessInboxItemInput {
                id: hotels.id.clone(),
                revision: hotels.revision,
                target: InboxTarget::Event {
                    event: CreateEventInput {
                        title: "Research hotels".into(),
                        notes: String::new(),
                        start_at_utc: "2030-10-02T15:00:00Z".into(),
                        time_zone: "America/New_York".into(),
                        duration_minutes: 30,
                        reminder_minutes_before: None,
                        plan_id: Some(plan.id.clone()),
                        location: String::new(),
                        workstream_id: None,
                        owner_id: None,
                    },
                },
            })
            .unwrap();
        assert_eq!(event.kind, RecordKind::Event);
        assert_eq!(inbox_count(&database), 0);
    }

    #[test]
    fn a_rejected_or_stale_conversion_keeps_the_item_and_creates_nothing() {
        let mut database = database();
        let item = capture(&mut database, "Look into authentication bug");
        let homeless = database.process_inbox_item(ProcessInboxItemInput {
            id: item.id.clone(),
            revision: item.revision,
            target: InboxTarget::Task {
                task: task("Look into authentication bug"),
            },
        });
        assert!(matches!(homeless, Err(AppError::Validation(_))));
        let stale = database.process_inbox_item(ProcessInboxItemInput {
            id: item.id.clone(),
            revision: item.revision + 1,
            target: InboxTarget::Task {
                task: CreateTaskInput {
                    scheduled_day: Some("2026-09-15".into()),
                    ..task("Look into authentication bug")
                },
            },
        });
        assert!(matches!(stale, Err(AppError::Conflict)));
        assert_eq!(inbox_count(&database), 1);
        assert!(database.all_tasks().unwrap().is_empty());
    }
}
