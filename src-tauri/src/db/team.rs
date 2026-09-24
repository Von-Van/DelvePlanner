//! People and workstreams: team planning semantics without accounts, sync, or permissions.

use super::planning::{ensure_plan_exists, validate_description, TASK_SELECT};
use super::{
    collect, event_from_row, now, planning::task_from_row, validate_id, validate_revision,
    PlannerDatabase, SqlConnection, EVENT_SELECT,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    CreatePersonInput, CreateWorkstreamInput, Person, PersonSummary, UpdatePersonInput,
    UpdateWorkstreamInput, Workstream, MAX_EMAIL_LENGTH, MAX_NAME_LENGTH, MAX_NOTES_LENGTH,
};
use rusqlite::{params, OptionalExtension, Row, TransactionBehavior};
use uuid::Uuid;

const PERSON_SELECT: &str =
    "SELECT id, display_name, role, email, notes, revision, created_at, updated_at FROM people";
const WORKSTREAM_SELECT: &str = "SELECT id, plan_id, name, description, sort_order, revision,
        created_at, updated_at
     FROM workstreams";
const PERSON_TASK_PREVIEW: usize = 5;
const PERSON_EVENT_PREVIEW: usize = 3;

impl PlannerDatabase {
    /// People by name, each with a count of open owned tasks and a short preview of what is next.
    pub fn list_people(&self) -> AppResult<Vec<PersonSummary>> {
        let people = self.all_people()?;
        let timestamp = now();
        let mut summaries = Vec::with_capacity(people.len());
        for person in people {
            let open_task_count: i64 = self.connection.query_row(
                "SELECT COUNT(*) FROM tasks WHERE owner_id = ?1 AND status <> 'done'",
                params![person.id],
                |row| row.get(0),
            )?;
            let mut statement = self.connection.prepare(&format!(
                "{TASK_SELECT}
                 WHERE owner_id = ?1 AND status <> 'done'
                 ORDER BY due_date IS NULL, due_date ASC, scheduled_day IS NULL,
                          scheduled_day ASC, sort_order ASC
                 LIMIT ?2"
            ))?;
            let open_tasks = collect(statement.query_map(
                params![person.id, PERSON_TASK_PREVIEW as i64],
                task_from_row,
            )?)?;
            let mut statement = self.connection.prepare(&format!(
                "{EVENT_SELECT}
                 WHERE owner_id = ?1
                   AND julianday(start_at_utc, printf('+%d minutes', duration_minutes))
                       > julianday(?2)
                 ORDER BY start_at_utc ASC
                 LIMIT ?3"
            ))?;
            let upcoming_events = collect(statement.query_map(
                params![person.id, timestamp, PERSON_EVENT_PREVIEW as i64],
                event_from_row,
            )?)?;
            summaries.push(PersonSummary {
                person,
                open_task_count,
                open_tasks,
                upcoming_events,
            });
        }
        Ok(summaries)
    }

    pub fn create_person(&mut self, input: CreatePersonInput) -> AppResult<Person> {
        let email = validate_person_shape(
            &input.display_name,
            &input.role,
            input.email.as_deref(),
            &input.notes,
        )?;
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        self.connection.execute(
            "INSERT INTO people
             (id, display_name, role, email, notes, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            params![
                id,
                input.display_name.trim(),
                input.role.trim(),
                email,
                input.notes.trim(),
                timestamp
            ],
        )?;
        person_by_id(&self.connection, &id)?.ok_or(AppError::NotFound)
    }

    pub fn update_person(&mut self, input: UpdatePersonInput) -> AppResult<Person> {
        validate_id(&input.id)?;
        validate_revision(input.revision)?;
        let email = validate_person_shape(
            &input.display_name,
            &input.role,
            input.email.as_deref(),
            &input.notes,
        )?;
        let changed = self.connection.execute(
            "UPDATE people
             SET display_name = ?1, role = ?2, email = ?3, notes = ?4,
                 revision = revision + 1, updated_at = ?5
             WHERE id = ?6 AND revision = ?7",
            params![
                input.display_name.trim(),
                input.role.trim(),
                email,
                input.notes.trim(),
                now(),
                input.id,
                input.revision
            ],
        )?;
        if changed != 1 {
            return Err(stale_write_error(&self.connection, "people", &input.id)?);
        }
        person_by_id(&self.connection, &input.id)?.ok_or(AppError::NotFound)
    }

    /// Deletes a person and leaves the tasks and events they owned unassigned.
    pub fn delete_person(&mut self, id: &str, revision: i64) -> AppResult<()> {
        validate_id(id)?;
        validate_revision(revision)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let person = person_by_id(&transaction, id)?.ok_or(AppError::NotFound)?;
        if person.revision != revision {
            return Err(AppError::Conflict);
        }
        let timestamp = now();
        for table in ["tasks", "schedule_events"] {
            transaction.execute(
                &format!(
                    "UPDATE {table}
                     SET owner_id = NULL, revision = revision + 1, updated_at = ?1
                     WHERE owner_id = ?2"
                ),
                params![timestamp, id],
            )?;
        }
        transaction.execute("DELETE FROM people WHERE id = ?1", params![id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn create_workstream(&mut self, input: CreateWorkstreamInput) -> AppResult<Workstream> {
        insert_workstream(&self.connection, &input)
    }

    pub fn update_workstream(&mut self, input: UpdateWorkstreamInput) -> AppResult<Workstream> {
        validate_id(&input.id)?;
        validate_revision(input.revision)?;
        validate_workstream_shape(&input.name, &input.description)?;
        let existing = workstream_by_id(&self.connection, &input.id)?.ok_or(AppError::NotFound)?;
        if existing.revision != input.revision {
            return Err(AppError::Conflict);
        }
        ensure_unique_workstream_name(
            &self.connection,
            &existing.plan_id,
            &input.name,
            Some(&existing.id),
        )?;
        let changed = self.connection.execute(
            "UPDATE workstreams
             SET name = ?1, description = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?4 AND revision = ?5",
            params![
                input.name.trim(),
                input.description.trim(),
                now(),
                input.id,
                input.revision
            ],
        )?;
        if changed != 1 {
            return Err(stale_write_error(
                &self.connection,
                "workstreams",
                &input.id,
            )?);
        }
        workstream_by_id(&self.connection, &input.id)?.ok_or(AppError::NotFound)
    }

    /// Deletes a workstream; its tasks, milestones, and events stay in the plan without one.
    pub fn delete_workstream(&mut self, id: &str, revision: i64) -> AppResult<()> {
        validate_id(id)?;
        validate_revision(revision)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workstream = workstream_by_id(&transaction, id)?.ok_or(AppError::NotFound)?;
        if workstream.revision != revision {
            return Err(AppError::Conflict);
        }
        let timestamp = now();
        for table in ["tasks", "milestones", "schedule_events"] {
            transaction.execute(
                &format!(
                    "UPDATE {table}
                     SET workstream_id = NULL, revision = revision + 1, updated_at = ?1
                     WHERE workstream_id = ?2"
                ),
                params![timestamp, id],
            )?;
        }
        transaction.execute("DELETE FROM workstreams WHERE id = ?1", params![id])?;
        transaction.commit()?;
        Ok(())
    }

    pub(super) fn all_people(&self) -> AppResult<Vec<Person>> {
        let mut statement = self.connection.prepare(&format!(
            "{PERSON_SELECT} ORDER BY display_name COLLATE NOCASE ASC, created_at ASC"
        ))?;
        let rows = statement.query_map([], person_from_row)?;
        collect(rows)
    }

    pub(super) fn all_workstreams(&self) -> AppResult<Vec<Workstream>> {
        let mut statement = self.connection.prepare(&format!(
            "{WORKSTREAM_SELECT} ORDER BY plan_id ASC, sort_order ASC, created_at ASC"
        ))?;
        let rows = statement.query_map([], workstream_from_row)?;
        collect(rows)
    }

    pub(super) fn workstreams_for_plan(&self, plan_id: &str) -> AppResult<Vec<Workstream>> {
        workstreams_in_plan(&self.connection, plan_id)
    }
}

/// Adds a workstream to a plan, for callers that may be inside a transaction. Names are unique
/// within a plan, ignoring case.
pub(super) fn insert_workstream<C: SqlConnection>(
    connection: &C,
    input: &CreateWorkstreamInput,
) -> AppResult<Workstream> {
    validate_workstream_shape(&input.name, &input.description)?;
    ensure_plan_exists(connection, &input.plan_id)?;
    ensure_unique_workstream_name(connection, &input.plan_id, &input.name, None)?;
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    let next_order: i64 = connection.connection().query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM workstreams WHERE plan_id = ?1",
        params![input.plan_id],
        |row| row.get(0),
    )?;
    connection.connection().execute(
        "INSERT INTO workstreams
         (id, plan_id, name, description, sort_order, revision, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
        params![
            id,
            input.plan_id,
            input.name.trim(),
            input.description.trim(),
            next_order,
            timestamp
        ],
    )?;
    workstream_by_id(connection, &id)?.ok_or(AppError::NotFound)
}

pub(super) fn workstreams_in_plan<C: SqlConnection>(
    connection: &C,
    plan_id: &str,
) -> AppResult<Vec<Workstream>> {
    let mut statement = connection.connection().prepare(&format!(
        "{WORKSTREAM_SELECT} WHERE plan_id = ?1 ORDER BY sort_order ASC, created_at ASC"
    ))?;
    let rows = statement.query_map(params![plan_id], workstream_from_row)?;
    collect(rows)
}

/// Validates optional plan-scoped links shared by tasks, milestones, and events: a workstream must
/// belong to the item's plan and an owner must exist.
pub(super) fn validate_links<C: SqlConnection>(
    connection: &C,
    plan_id: Option<&str>,
    workstream_id: Option<&str>,
    owner_id: Option<&str>,
) -> AppResult<()> {
    if let Some(workstream_id) = workstream_id {
        validate_id(workstream_id)?;
        let workstream_plan: Option<String> = connection
            .connection()
            .query_row(
                "SELECT plan_id FROM workstreams WHERE id = ?1",
                params![workstream_id],
                |row| row.get(0),
            )
            .optional()?;
        match workstream_plan {
            None => {
                return Err(AppError::Validation(
                    "The selected workstream no longer exists.".into(),
                ))
            }
            Some(workstream_plan) if Some(workstream_plan.as_str()) != plan_id => {
                return Err(workstream_plan_mismatch())
            }
            Some(_) => {}
        }
    }
    if let Some(owner_id) = owner_id {
        validate_id(owner_id)?;
        let exists: bool = connection.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM people WHERE id = ?1)",
            params![owner_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::Validation(
                "The selected owner no longer exists.".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn workstream_plan_mismatch() -> AppError {
    AppError::Validation("A workstream must belong to the same plan as the item.".into())
}

/// Returns the trimmed email, or `None` when it was left blank.
pub(super) fn validate_person_shape(
    display_name: &str,
    role: &str,
    email: Option<&str>,
    notes: &str,
) -> AppResult<Option<String>> {
    validate_name(display_name)?;
    if role.trim().chars().count() > MAX_NAME_LENGTH {
        return Err(AppError::Validation(
            "Roles must be 80 characters or fewer.".into(),
        ));
    }
    if notes.trim().chars().count() > MAX_NOTES_LENGTH {
        return Err(AppError::Validation(
            "Notes must be 800 characters or fewer.".into(),
        ));
    }
    let email = email.map(str::trim).filter(|value| !value.is_empty());
    if let Some(email) = email {
        let well_formed = email.chars().count() <= MAX_EMAIL_LENGTH
            && !email.chars().any(char::is_whitespace)
            && email
                .split_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
        if !well_formed {
            return Err(AppError::Validation(
                "Enter a valid email address or leave it blank.".into(),
            ));
        }
    }
    Ok(email.map(str::to_string))
}

pub(super) fn validate_workstream_shape(name: &str, description: &str) -> AppResult<()> {
    validate_name(name)?;
    validate_description(description)
}

fn validate_name(value: &str) -> AppResult<()> {
    if !(1..=MAX_NAME_LENGTH).contains(&value.trim().chars().count()) {
        return Err(AppError::Validation(
            "Names must be 1–80 characters.".into(),
        ));
    }
    Ok(())
}

/// Workstream names are unique within a plan, ignoring ASCII case like SQLite's NOCASE.
fn ensure_unique_workstream_name<C: SqlConnection>(
    connection: &C,
    plan_id: &str,
    name: &str,
    except_id: Option<&str>,
) -> AppResult<()> {
    let taken: bool = connection.connection().query_row(
        "SELECT EXISTS(
             SELECT 1 FROM workstreams
             WHERE plan_id = ?1 AND name = ?2 COLLATE NOCASE AND id <> COALESCE(?3, '')
         )",
        params![plan_id, name.trim(), except_id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(AppError::Validation(
            "This plan already has a workstream with that name.".into(),
        ));
    }
    Ok(())
}

fn stale_write_error<C: SqlConnection>(
    connection: &C,
    table: &'static str,
    id: &str,
) -> AppResult<AppError> {
    let exists: bool = connection.connection().query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
        params![id],
        |row| row.get(0),
    )?;
    Ok(if exists {
        AppError::Conflict
    } else {
        AppError::NotFound
    })
}

fn person_by_id<C: SqlConnection>(connection: &C, id: &str) -> AppResult<Option<Person>> {
    connection
        .connection()
        .query_row(
            &format!("{PERSON_SELECT} WHERE id = ?1"),
            params![id],
            person_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn workstream_by_id<C: SqlConnection>(connection: &C, id: &str) -> AppResult<Option<Workstream>> {
    connection
        .connection()
        .query_row(
            &format!("{WORKSTREAM_SELECT} WHERE id = ?1"),
            params![id],
            workstream_from_row,
        )
        .optional()
        .map_err(AppError::from)
}

fn person_from_row(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.get(0)?,
        display_name: row.get(1)?,
        role: row.get(2)?,
        email: row.get(3)?,
        notes: row.get(4)?,
        revision: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn workstream_from_row(row: &Row<'_>) -> rusqlite::Result<Workstream> {
    Ok(Workstream {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        sort_order: row.get(4)?,
        revision: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CreateEventInput, CreatePlanInput, CreateTaskInput, LinkChange, PlanStatus, TaskPriority,
        TaskStatus, UpdateEventInput, UpdatePlanInput, UpdateTaskInput,
    };
    use tempfile::tempdir;

    fn database() -> PlannerDatabase {
        let path = tempdir().unwrap().keep().join("dayplan.sqlite3");
        PlannerDatabase::open(&path).unwrap()
    }

    fn plan(database: &mut PlannerDatabase, title: &str) -> crate::model::Plan {
        database
            .create_plan(CreatePlanInput {
                title: title.into(),
                description: String::new(),
                status: PlanStatus::Active,
                start_date: None,
                target_date: None,
                color: None,
                links: Vec::new(),
            })
            .unwrap()
    }

    fn person(database: &mut PlannerDatabase, name: &str) -> Person {
        database
            .create_person(CreatePersonInput {
                display_name: name.into(),
                role: String::new(),
                email: None,
                notes: String::new(),
            })
            .unwrap()
    }

    fn workstream(database: &mut PlannerDatabase, plan_id: &str, name: &str) -> Workstream {
        database
            .create_workstream(CreateWorkstreamInput {
                plan_id: plan_id.into(),
                name: name.into(),
                description: String::new(),
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
            checklist: Vec::new(),
            recurrence: None,
            waiting_on: Vec::new(),
        }
    }

    fn event(title: &str) -> CreateEventInput {
        CreateEventInput {
            title: title.into(),
            notes: String::new(),
            start_at_utc: "2030-11-09T18:00:00Z".into(),
            time_zone: "America/New_York".into(),
            duration_minutes: 30,
            reminder_minutes_before: None,
            plan_id: None,
            location: String::new(),
            workstream_id: None,
            owner_id: None,
        }
    }

    #[test]
    fn people_need_a_name_and_a_plausible_email() {
        let mut database = database();
        let maya = database
            .create_person(CreatePersonInput {
                display_name: "  Maya  ".into(),
                role: "Creator Relations".into(),
                email: Some("   ".into()),
                notes: String::new(),
            })
            .unwrap();
        assert_eq!((maya.display_name.as_str(), maya.email), ("Maya", None));
        for (name, email) in [
            ("", None),
            (&"x".repeat(81)[..], None),
            ("Alex", Some("alex at example")),
            ("Alex", Some("alex@localhost")),
        ] {
            let result = database.create_person(CreatePersonInput {
                display_name: name.into(),
                role: String::new(),
                email: email.map(Into::into),
                notes: String::new(),
            });
            assert!(
                matches!(result, Err(AppError::Validation(_))),
                "{name} {email:?}"
            );
        }
        let updated = database
            .update_person(UpdatePersonInput {
                id: maya.id.clone(),
                revision: maya.revision,
                display_name: "Maya Chen".into(),
                role: "Creator Relations".into(),
                email: Some("maya@example.com".into()),
                notes: String::new(),
            })
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert!(matches!(
            database.update_person(UpdatePersonInput {
                id: maya.id,
                revision: 1,
                display_name: "Stale".into(),
                role: String::new(),
                email: None,
                notes: String::new(),
            }),
            Err(AppError::Conflict)
        ));
    }

    #[test]
    fn deleting_a_person_unassigns_their_tasks_and_events() {
        let mut database = database();
        let alex = person(&mut database, "Alex");
        let owned_task = database
            .create_task(CreateTaskInput {
                scheduled_day: Some("2030-11-08".into()),
                owner_id: Some(alex.id.clone()),
                ..task("Finish overlays")
            })
            .unwrap();
        let owned_event = database
            .create_event(CreateEventInput {
                owner_id: Some(alex.id.clone()),
                ..event("Opening broadcast")
            })
            .unwrap();
        assert!(matches!(
            database.delete_person(&alex.id, alex.revision + 1),
            Err(AppError::Conflict)
        ));
        database.delete_person(&alex.id, alex.revision).unwrap();
        let task_after = &database.tasks_for_day("2030-11-08").unwrap()[0];
        assert_eq!(task_after.owner_id, None);
        assert_eq!(task_after.revision, owned_task.revision + 1);
        let event_after = &database
            .events_for_day("2030-11-09", "America/New_York")
            .unwrap()[0];
        assert_eq!(event_after.owner_id, None);
        assert_eq!(event_after.revision, owned_event.revision + 1);
        assert!(database.list_people().unwrap().is_empty());
    }

    #[test]
    fn people_summaries_preview_their_open_work() {
        let mut database = database();
        let jacob = person(&mut database, "jacob");
        let alex = person(&mut database, "Alex");
        for (title, due, status) in [
            ("Collect bios", "2030-11-02", TaskStatus::Todo),
            ("Confirm creators", "2030-11-01", TaskStatus::InProgress),
            ("Book rehearsal slot", "2030-10-30", TaskStatus::Done),
        ] {
            database
                .create_task(CreateTaskInput {
                    due_date: Some(due.into()),
                    owner_id: Some(jacob.id.clone()),
                    status,
                    ..task(title)
                })
                .unwrap();
        }
        database
            .create_event(CreateEventInput {
                owner_id: Some(jacob.id.clone()),
                ..event("Creator briefing")
            })
            .unwrap();
        let summaries = database.list_people().unwrap();
        let names = summaries
            .iter()
            .map(|summary| summary.person.display_name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Alex", "jacob"]);
        let jacob_summary = summaries
            .iter()
            .find(|summary| summary.person.id == jacob.id)
            .unwrap();
        assert_eq!(jacob_summary.open_task_count, 2);
        let previews = jacob_summary
            .open_tasks
            .iter()
            .map(|task| task.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(previews, ["Confirm creators", "Collect bios"]);
        assert_eq!(jacob_summary.upcoming_events.len(), 1);
        let alex_summary = summaries
            .iter()
            .find(|summary| summary.person.id == alex.id)
            .unwrap();
        assert_eq!(alex_summary.open_task_count, 0);
    }

    #[test]
    fn workstream_names_are_unique_within_a_plan_only() {
        let mut database = database();
        let charity = plan(&mut database, "Streamer Charity Week");
        let launch = plan(&mut database, "Product Launch");
        let production = workstream(&mut database, &charity.id, "Production");
        assert!(matches!(
            database.create_workstream(CreateWorkstreamInput {
                plan_id: charity.id.clone(),
                name: " production ".into(),
                description: String::new(),
            }),
            Err(AppError::Validation(_))
        ));
        workstream(&mut database, &launch.id, "Production");
        let marketing = workstream(&mut database, &charity.id, "Marketing");
        assert!(matches!(
            database.update_workstream(UpdateWorkstreamInput {
                id: marketing.id.clone(),
                revision: marketing.revision,
                name: "PRODUCTION".into(),
                description: String::new(),
            }),
            Err(AppError::Validation(_))
        ));
        let renamed = database
            .update_workstream(UpdateWorkstreamInput {
                id: production.id.clone(),
                revision: production.revision,
                name: "Broadcast production".into(),
                description: "Overlays, audio, and scene changes".into(),
            })
            .unwrap();
        assert_eq!((renamed.revision, renamed.sort_order), (2, 0));
        assert!(matches!(
            database.create_workstream(CreateWorkstreamInput {
                plan_id: uuid::Uuid::new_v4().to_string(),
                name: "Sponsors".into(),
                description: String::new(),
            }),
            Err(AppError::Validation(_))
        ));
        let names = database
            .plan_workspace(&charity.id)
            .unwrap()
            .workstreams
            .into_iter()
            .map(|item| item.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Broadcast production", "Marketing"]);
    }

    #[test]
    fn workstreams_only_link_items_in_their_own_plan() {
        let mut database = database();
        let charity = plan(&mut database, "Streamer Charity Week");
        let launch = plan(&mut database, "Product Launch");
        let sponsors = workstream(&mut database, &charity.id, "Sponsors");
        let rejected = [
            CreateTaskInput {
                plan_id: Some(launch.id.clone()),
                workstream_id: Some(sponsors.id.clone()),
                ..task("Wrong plan")
            },
            CreateTaskInput {
                scheduled_day: Some("2030-11-09".into()),
                workstream_id: Some(sponsors.id.clone()),
                ..task("No plan")
            },
            CreateTaskInput {
                plan_id: Some(charity.id.clone()),
                owner_id: Some(uuid::Uuid::new_v4().to_string()),
                ..task("Missing owner")
            },
        ];
        for input in rejected {
            assert!(matches!(
                database.create_task(input),
                Err(AppError::Validation(_))
            ));
        }
        let approved = database
            .create_task(CreateTaskInput {
                plan_id: Some(charity.id.clone()),
                workstream_id: Some(sponsors.id.clone()),
                ..task("Approve sponsor graphics")
            })
            .unwrap();
        let moved_without_clearing = database.update_task(UpdateTaskInput {
            id: approved.id.clone(),
            revision: approved.revision,
            title: approved.title.clone(),
            description: String::new(),
            plan_id: Some(launch.id.clone()),
            milestone_id: None,
            workstream_id: approved.workstream_id.clone(),
            owner_id: None,
            due_date: None,
            scheduled_day: None,
            planned_week: None,
            estimated_minutes: None,
            status: approved.status,
            priority: approved.priority,
            checklist: Vec::new(),
            recurrence: None,
            waiting_on: Vec::new(),
        });
        assert!(matches!(
            moved_without_clearing,
            Err(AppError::Validation(_))
        ));

        assert!(matches!(
            database.create_event(CreateEventInput {
                workstream_id: Some(sponsors.id.clone()),
                ..event("Sponsor review")
            }),
            Err(AppError::Validation(_))
        ));
        let review = database
            .create_event(CreateEventInput {
                plan_id: Some(charity.id.clone()),
                workstream_id: Some(sponsors.id.clone()),
                location: " Studio B ".into(),
                ..event("Sponsor review")
            })
            .unwrap();
        assert_eq!(review.location, "Studio B");
        let edit = |plan_change, workstream_change| UpdateEventInput {
            id: review.id.clone(),
            revision: review.revision,
            title: None,
            notes: None,
            start_at_utc: None,
            time_zone: None,
            duration_minutes: None,
            location: None,
            reminder_change: Default::default(),
            plan_change,
            workstream_change,
            owner_change: LinkChange::Unchanged,
        };
        assert!(matches!(
            database.update_event(edit(
                LinkChange::Set {
                    id: launch.id.clone()
                },
                LinkChange::Unchanged
            )),
            Err(AppError::Validation(_))
        ));
        let moved = database
            .update_event(edit(
                LinkChange::Set {
                    id: launch.id.clone(),
                },
                LinkChange::Clear,
            ))
            .unwrap();
        assert_eq!(
            (moved.plan_id.as_deref(), moved.workstream_id),
            (Some(launch.id.as_str()), None)
        );
    }

    #[test]
    fn deleting_a_workstream_keeps_its_work_in_the_plan() {
        let mut database = database();
        let charity = plan(&mut database, "Streamer Charity Week");
        let marketing = workstream(&mut database, &charity.id, "Marketing");
        let posts = database
            .create_task(CreateTaskInput {
                plan_id: Some(charity.id.clone()),
                workstream_id: Some(marketing.id.clone()),
                ..task("Prepare social posts")
            })
            .unwrap();
        let launch = database
            .create_milestone(crate::model::CreateMilestoneInput {
                plan_id: charity.id.clone(),
                title: "Campaign launch".into(),
                description: String::new(),
                target_date: Some("2030-11-02".into()),
                status: crate::model::MilestoneStatus::Pending,
                workstream_id: Some(marketing.id.clone()),
            })
            .unwrap();
        database
            .delete_workstream(&marketing.id, marketing.revision)
            .unwrap();
        let workspace = database.plan_workspace(&charity.id).unwrap();
        assert!(workspace.workstreams.is_empty());
        assert_eq!(workspace.tasks[0].workstream_id, None);
        assert_eq!(workspace.tasks[0].revision, posts.revision + 1);
        assert_eq!(workspace.milestones[0].workstream_id, None);
        assert_eq!(workspace.milestones[0].revision, launch.revision + 1);
    }

    #[test]
    fn deleting_a_plan_removes_its_workstreams_but_keeps_owners() {
        let mut database = database();
        let charity = plan(&mut database, "Streamer Charity Week");
        let maya = person(&mut database, "Maya");
        let relations = workstream(&mut database, &charity.id, "Creator Relations");
        database
            .create_task(CreateTaskInput {
                plan_id: Some(charity.id.clone()),
                workstream_id: Some(relations.id.clone()),
                owner_id: Some(maya.id.clone()),
                due_date: Some("2030-11-01".into()),
                ..task("Confirm creators")
            })
            .unwrap();
        database
            .create_event(CreateEventInput {
                plan_id: Some(charity.id.clone()),
                workstream_id: Some(relations.id.clone()),
                owner_id: Some(maya.id.clone()),
                ..event("Creator briefing")
            })
            .unwrap();
        let archived = database
            .update_plan(UpdatePlanInput {
                id: charity.id.clone(),
                revision: charity.revision,
                title: charity.title.clone(),
                description: String::new(),
                status: charity.status,
                start_date: None,
                target_date: None,
                color: None,
                archived: true,
                links: Vec::new(),
            })
            .unwrap();
        let deletion = database
            .delete_plan(&archived.id, archived.revision)
            .unwrap();
        assert_eq!(deletion.deleted_workstreams, 1);
        let kept_task = &database.tasks_due_on("2030-11-01").unwrap()[0];
        assert_eq!(
            (
                kept_task.workstream_id.as_deref(),
                kept_task.owner_id.as_deref()
            ),
            (None, Some(maya.id.as_str()))
        );
        let kept_event = &database
            .events_for_day("2030-11-09", "America/New_York")
            .unwrap()[0];
        assert_eq!(
            (
                kept_event.workstream_id.as_deref(),
                kept_event.owner_id.as_deref()
            ),
            (None, Some(maya.id.as_str()))
        );
    }
}
