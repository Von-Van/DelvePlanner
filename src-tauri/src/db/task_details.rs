//! The parts of a task and a plan that aren't a single column: a task's checklist, its repeat
//! rule, the tasks it waits on, and a plan's links. Checklists, rules, and links are stored as
//! JSON beside the record they belong to, so they move, export, and revision-check with it.
//! "Waits on" is a link table, so deleting a task can't leave another one waiting on nothing.

use super::{now, validate_id, SqlConnection};
use crate::error::{AppError, AppResult};
use crate::model::{
    ChecklistItem, PlanLink, Recurrence, MAX_CHECKLIST_ITEMS, MAX_LINK_URL_LENGTH, MAX_PLAN_LINKS,
    MAX_TITLE_LENGTH, MAX_WAITING_ON,
};
use reqwest::Url;
use rusqlite::{params, types::Type, Connection};
use std::collections::{HashMap, HashSet};

const MAX_LINK_TITLE_LENGTH: usize = 80;

/// Schema 8: a repeat rule and a checklist on every task, the links between waiting tasks and
/// what they wait on, and a list of links on every plan. Existing records start with none.
pub(super) fn ensure_task_details_schema(connection: &Connection) -> AppResult<()> {
    for (table, column, definition) in [
        ("tasks", "recurrence", "TEXT"),
        ("tasks", "checklist", "TEXT NOT NULL DEFAULT '[]'"),
        ("plans", "links", "TEXT NOT NULL DEFAULT '[]'"),
    ] {
        if !super::column_exists(connection, table, column)? {
            connection.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_dependencies (
             task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
             depends_on_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
             position INTEGER NOT NULL DEFAULT 0,
             created_at TEXT NOT NULL,
             PRIMARY KEY (task_id, depends_on_task_id),
             CHECK(task_id <> depends_on_task_id)
         );
         CREATE INDEX IF NOT EXISTS task_dependencies_prerequisite_idx
             ON task_dependencies(depends_on_task_id);",
    )?;
    Ok(())
}

/// Trims each item and checks the list's length and text.
pub(super) fn validate_checklist(items: &[ChecklistItem]) -> AppResult<Vec<ChecklistItem>> {
    if items.len() > MAX_CHECKLIST_ITEMS {
        return Err(AppError::Validation(format!(
            "A checklist can hold up to {MAX_CHECKLIST_ITEMS} items."
        )));
    }
    items
        .iter()
        .map(|item| {
            let text = item.text.trim();
            if text.is_empty() {
                return Err(AppError::Validation(
                    "Checklist items need some text.".into(),
                ));
            }
            if text.chars().count() > MAX_TITLE_LENGTH {
                return Err(AppError::Validation(
                    "Checklist items must be 140 characters or fewer.".into(),
                ));
            }
            Ok(ChecklistItem {
                text: text.to_string(),
                done: item.done,
            })
        })
        .collect()
}

/// Checks the shape of a "waits on" list: valid IDs, no repeats, not too many. Whether they exist
/// and form no loop is checked against the database when the list is saved.
pub(super) fn validate_waiting_on(ids: &[String]) -> AppResult<Vec<String>> {
    if ids.len() > MAX_WAITING_ON {
        return Err(AppError::Validation(format!(
            "A task can wait on up to {MAX_WAITING_ON} others."
        )));
    }
    let mut seen = HashSet::new();
    for id in ids {
        validate_id(id)?;
        if !seen.insert(id.as_str()) {
            return Err(AppError::Validation(
                "A task can only wait on another task once.".into(),
            ));
        }
    }
    Ok(ids.to_vec())
}

/// Replaces the tasks `task_id` waits on. None may already wait on `task_id`, directly or through
/// others: waiting in a circle would never end. A task that has since been deleted is dropped from
/// the list rather than refused, since waiting on something that no longer exists is no wait at
/// all, and an editor opened before the deletion shouldn't be unable to save.
pub(super) fn replace_dependencies<C: SqlConnection>(
    connection: &C,
    task_id: &str,
    waiting_on: &[String],
) -> AppResult<()> {
    let connection = connection.connection();
    let mut kept = Vec::with_capacity(waiting_on.len());
    for prerequisite in waiting_on {
        if prerequisite == task_id {
            return Err(AppError::Validation("A task can't wait on itself.".into()));
        }
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            params![prerequisite],
            |row| row.get(0),
        )?;
        if !exists {
            continue;
        }
        let circular: bool = connection.query_row(
            "WITH RECURSIVE reachable(id) AS (
                 SELECT depends_on_task_id FROM task_dependencies WHERE task_id = ?1
                 UNION
                 SELECT dependency.depends_on_task_id
                 FROM task_dependencies AS dependency
                 JOIN reachable ON dependency.task_id = reachable.id
             )
             SELECT EXISTS(SELECT 1 FROM reachable WHERE id = ?2)",
            params![prerequisite, task_id],
            |row| row.get(0),
        )?;
        if circular {
            return Err(AppError::Validation(
                "That would make tasks wait on each other in a circle.".into(),
            ));
        }
        kept.push(prerequisite);
    }
    connection.execute(
        "DELETE FROM task_dependencies WHERE task_id = ?1",
        params![task_id],
    )?;
    let timestamp = now();
    for (position, prerequisite) in kept.into_iter().enumerate() {
        connection.execute(
            "INSERT INTO task_dependencies (task_id, depends_on_task_id, position, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, prerequisite, position as i64, timestamp],
        )?;
    }
    Ok(())
}

/// Whether any task in `waiting_on` (by task ID) waits in a circle, for checking an import before
/// anything is written.
pub(super) fn has_circular_waits(waiting_on: &HashMap<&str, &[String]>) -> bool {
    fn visits(
        task: &str,
        waiting_on: &HashMap<&str, &[String]>,
        visiting: &mut HashSet<String>,
        done: &mut HashSet<String>,
    ) -> bool {
        if done.contains(task) {
            return false;
        }
        if !visiting.insert(task.to_string()) {
            return true;
        }
        let circular = waiting_on
            .get(task)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .any(|next| visits(next, waiting_on, visiting, done));
        visiting.remove(task);
        done.insert(task.to_string());
        circular
    }
    let mut done = HashSet::new();
    waiting_on
        .keys()
        .any(|task| visits(task, waiting_on, &mut HashSet::new(), &mut done))
}

/// Checks a plan's links: web addresses only, with short titles.
pub(super) fn validate_plan_links(links: &[PlanLink]) -> AppResult<Vec<PlanLink>> {
    if links.len() > MAX_PLAN_LINKS {
        return Err(AppError::Validation(format!(
            "A plan can keep up to {MAX_PLAN_LINKS} links."
        )));
    }
    links
        .iter()
        .map(|link| {
            let title = link.title.trim();
            if title.chars().count() > MAX_LINK_TITLE_LENGTH {
                return Err(AppError::Validation(
                    "Link titles must be 80 characters or fewer.".into(),
                ));
            }
            Ok(PlanLink {
                title: title.to_string(),
                url: web_address(&link.url)?,
            })
        })
        .collect()
}

/// A link as Delve Planner stores and opens it: an http or https address, nothing else, so a saved
/// link can never run a program or open a local file.
pub(crate) fn web_address(value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_LINK_URL_LENGTH {
        return Err(AppError::Validation(
            "Links must be web addresses of 2,048 characters or fewer.".into(),
        ));
    }
    let url = Url::parse(trimmed).map_err(|_| {
        AppError::Validation("That isn't a web address. Links start with https://.".into())
    })?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none_or(str::is_empty) {
        return Err(AppError::Validation(
            "Only web links (https:// or http://) can be saved.".into(),
        ));
    }
    Ok(url.to_string())
}

pub(super) fn to_json<T: serde::Serialize>(value: &T) -> AppResult<String> {
    serde_json::to_string(value).map_err(AppError::from)
}

pub(super) fn recurrence_json(rule: Option<&Recurrence>) -> AppResult<Option<String>> {
    rule.map(to_json).transpose()
}

/// Reads a JSON column written by this module, reporting a stored value that doesn't parse as a
/// conversion failure of that column.
pub(super) fn from_json<T: serde::de::DeserializeOwned>(
    value: &str,
    index: usize,
) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checklists_are_trimmed_and_bounded() {
        let items = validate_checklist(&[ChecklistItem {
            text: "  Book the van ".into(),
            done: false,
        }])
        .unwrap();
        assert_eq!(items[0].text, "Book the van");
        assert!(validate_checklist(&[ChecklistItem {
            text: "   ".into(),
            done: false
        }])
        .is_err());
        let too_many = vec![
            ChecklistItem {
                text: "Step".into(),
                done: false
            };
            MAX_CHECKLIST_ITEMS + 1
        ];
        assert!(validate_checklist(&too_many).is_err());
    }

    #[test]
    fn only_web_links_are_kept() {
        assert_eq!(
            web_address(" https://example.com/booking ").unwrap(),
            "https://example.com/booking"
        );
        for rejected in [
            "",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "mailto:someone@example.com",
            "not a link",
            "https://",
        ] {
            assert!(web_address(rejected).is_err(), "{rejected} was accepted");
        }
    }

    #[test]
    fn circular_waits_are_found_before_import() {
        let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
        let waits_on_b = [b.clone()];
        let waits_on_c = [c.clone()];
        let waits_on_a = [a.clone()];
        let mut graph: HashMap<&str, &[String]> = HashMap::new();
        graph.insert("a", &waits_on_b);
        graph.insert("b", &waits_on_c);
        assert!(!has_circular_waits(&graph));
        graph.insert("c", &waits_on_a);
        assert!(has_circular_waits(&graph));
    }
}
