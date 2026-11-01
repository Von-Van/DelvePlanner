//! `get_memory`: the agent's only way to read what the person asked it to remember.
//!
//! This is memory strategy M3 from the handoff: records are *selected*, not dumped. A record is
//! offered only when memory is on, the record is current (not superseded by a correction, not an
//! expired inference), and its project has memory on. Among those, records are ranked by how many
//! of the query's words they contain, then confirmed before unconfirmed, then newest first.
//! Every record says where it came from and whether the person confirmed it, so the agent can
//! tell a decision from a guess.

use super::error::ToolError;
use super::ToolSources;
use crate::db::{Confirmation, MemoryKind, MemoryRecord, MemorySource};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_MEMORY_LIMIT: u32 = 20;
pub const DEFAULT_MEMORY_LIMIT: u32 = 8;
pub const MAX_MEMORY_QUERY_LENGTH: usize = 200;

/// Words too common to say anything about relevance.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "was", "were", "are", "you", "your", "our", "has",
    "have", "had", "from", "into", "about", "what", "when", "which", "who", "how", "did", "does",
    "can", "could", "should", "would", "will", "just", "also", "then", "than", "there", "their",
    "them", "they", "its", "move", "make", "please", "again", "last", "time",
];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetMemoryArgs {
    pub query: Option<String>,
    pub plan_id: Option<String>,
    pub kinds: Option<Vec<MemoryKind>>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryView {
    pub id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub source: MemorySource,
    pub confirmation: Confirmation,
    pub plan_id: Option<String>,
    /// When the record was last written (created, confirmed, or superseded its predecessor).
    pub recorded_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Share of the query's words found in the record, from 0 to 1 (1 when there is no query).
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRetrieval {
    /// False when the person has paused memory; `records` is then empty.
    pub memory_enabled: bool,
    pub records: Vec<MemoryView>,
    pub total_matches: usize,
    pub truncated: bool,
}

pub fn get_memory(
    sources: &ToolSources<'_>,
    now: DateTime<Utc>,
    args: GetMemoryArgs,
) -> Result<MemoryRetrieval, ToolError> {
    let limit = args.limit.unwrap_or(DEFAULT_MEMORY_LIMIT);
    if !(1..=MAX_MEMORY_LIMIT).contains(&limit) {
        return Err(ToolError::invalid(format!(
            "limit must be between 1 and {MAX_MEMORY_LIMIT}."
        )));
    }
    if let Some(query) = &args.query {
        if query.chars().count() > MAX_MEMORY_QUERY_LENGTH {
            return Err(ToolError::invalid(format!(
                "query can be at most {MAX_MEMORY_QUERY_LENGTH} characters."
            )));
        }
    }
    if let Some(plan_id) = &args.plan_id {
        if uuid::Uuid::parse_str(plan_id).is_err() {
            return Err(ToolError::invalid("planId must be a plan id."));
        }
    }
    let (enabled, candidates) = sources.database.memory_candidates(now)?;
    let selected = select(candidates, &args);
    let total_matches = selected.len();
    let records: Vec<MemoryView> = selected.into_iter().take(limit as usize).collect();
    Ok(MemoryRetrieval {
        memory_enabled: enabled,
        truncated: total_matches > records.len(),
        total_matches,
        records,
    })
}

/// Filters and ranks current records for one request. Kept separate from the database so the
/// ranking can be tested on its own and mirrored by the benchmark's mock.
pub fn select(candidates: Vec<MemoryRecord>, args: &GetMemoryArgs) -> Vec<MemoryView> {
    let terms = query_terms(args.query.as_deref().unwrap_or(""));
    let mut views: Vec<MemoryView> = candidates
        .into_iter()
        .filter(|record| match (&args.plan_id, &record.plan_id) {
            (Some(wanted), Some(plan)) => wanted == plan,
            _ => true,
        })
        .filter(|record| {
            args.kinds
                .as_ref()
                .is_none_or(|kinds| kinds.is_empty() || kinds.contains(&record.kind))
        })
        .filter_map(|record| {
            let relevance = relevance(&terms, &record.content);
            (terms.is_empty() || relevance > 0.0).then(|| MemoryView {
                id: record.id,
                kind: record.kind,
                content: record.content,
                source: record.source,
                confirmation: record.confirmation,
                plan_id: record.plan_id,
                recorded_at: record.updated_at,
                expires_at: record.expires_at,
                relevance,
            })
        })
        .collect();
    views.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| confirmed_rank(a).cmp(&confirmed_rank(b)))
            .then_with(|| b.recorded_at.cmp(&a.recorded_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    views
}

fn confirmed_rank(view: &MemoryView) -> u8 {
    match view.confirmation {
        Confirmation::Confirmed => 0,
        Confirmation::Unconfirmed => 1,
    }
}

/// Lower-cased words of three or more letters, minus common words, with a trailing plural `s`
/// dropped so "reviews" finds "review".
pub fn query_terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = words(text)
        .filter(|word| word.chars().count() >= 3 && !STOPWORDS.contains(&word.as_str()))
        .map(|word| stem(&word))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

fn stem(word: &str) -> String {
    match word.strip_suffix('s') {
        Some(stem) if stem.chars().count() >= 3 && !stem.ends_with('s') => stem.to_string(),
        _ => word.to_string(),
    }
}

fn relevance(terms: &[String], content: &str) -> f64 {
    if terms.is_empty() {
        return 1.0;
    }
    let found: Vec<String> = words(content).map(|word| stem(&word)).collect();
    let hits = terms.iter().filter(|term| found.contains(term)).count();
    let score = hits as f64 / terms.len() as f64;
    (score * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{MemorySource, NewMemory, PlannerDatabase, UpdateMemorySettings};
    use crate::tools::registry::ToolName;
    use crate::tools::{NoCalendars, SessionConfig, ToolSession};
    use serde_json::{json, Value};

    const PLAN: &str = "5b0d3f0e-5f5c-4d47-9a1e-1f2d3c4b5a69";
    const OTHER_PLAN: &str = "0f4c2a51-8f0e-4a43-8b0e-7a3b2c1d0e9f";

    fn database() -> (tempfile::TempDir, PlannerDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let database = PlannerDatabase::open(&dir.path().join("memory-tool.sqlite3")).unwrap();
        (dir, database)
    }

    fn remember(
        db: &mut PlannerDatabase,
        content: &str,
        source: MemorySource,
        plan: Option<&str>,
    ) -> String {
        db.add_memory(NewMemory {
            kind: MemoryKind::Decision,
            content: content.into(),
            source,
            source_ref: None,
            plan_id: plan.map(str::to_string),
        })
        .unwrap()
        .id
    }

    fn call(db: &PlannerDatabase, arguments: Value) -> Value {
        let mut config = SessionConfig::new(Utc::now(), "America/Chicago".parse().unwrap());
        config.permitted_tools = Some([ToolName::GetMemory].into());
        let mut session = ToolSession::new(config);
        let sources = ToolSources {
            database: db,
            calendars: &NoCalendars,
        };
        let outcome = session.call(&sources, "get_memory", arguments);
        serde_json::to_value(outcome).unwrap()
    }

    #[test]
    fn returns_relevant_current_records_with_provenance() {
        let (_dir, mut db) = database();
        let old = remember(
            &mut db,
            "Weekly sync is on Tuesdays at 10.",
            MemorySource::UserStated,
            Some(PLAN),
        );
        let new = db
            .correct_memory(&old, "Weekly sync moved to Thursdays at 10.")
            .unwrap();
        remember(
            &mut db,
            "Prefers no meetings on Fridays.",
            MemorySource::UserStated,
            None,
        );
        remember(
            &mut db,
            "Launch copy is owned by Sam.",
            MemorySource::UserStated,
            Some(OTHER_PLAN),
        );

        let outcome = call(&db, json!({ "query": "when is the weekly sync?" }));
        assert_eq!(outcome["ok"], true);
        let result = &outcome["result"];
        assert_eq!(result["memoryEnabled"], true);
        let records = result["records"].as_array().unwrap();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["id"], new.id);
        assert_eq!(records[0]["source"], "user_correction");
        assert_eq!(records[0]["confirmation"], "confirmed");
        assert_eq!(records[0]["relevance"], 1.0);
    }

    #[test]
    fn project_filter_keeps_general_records_and_drops_other_projects() {
        let (_dir, mut db) = database();
        remember(&mut db, "General preference.", MemorySource::UserStated, None);
        remember(&mut db, "This project.", MemorySource::UserStated, Some(PLAN));
        remember(&mut db, "Other project.", MemorySource::UserStated, Some(OTHER_PLAN));
        let outcome = call(&db, json!({ "planId": PLAN }));
        let contents: Vec<&str> = outcome["result"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["content"].as_str().unwrap())
            .collect();
        assert_eq!(contents.len(), 2);
        assert!(!contents.contains(&"Other project."));
    }

    #[test]
    fn unconfirmed_inferences_rank_below_confirmed_and_are_labelled() {
        let (_dir, mut db) = database();
        remember(
            &mut db,
            "Design review probably moves to afternoons.",
            MemorySource::AssistantInferred,
            None,
        );
        remember(
            &mut db,
            "Design review is at 3 pm.",
            MemorySource::UserStated,
            None,
        );
        let outcome = call(&db, json!({ "query": "design review" }));
        let records = outcome["result"]["records"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["confirmation"], "confirmed");
        assert_eq!(records[1]["confirmation"], "unconfirmed");
        assert!(records[1]["expiresAt"].is_string());
    }

    #[test]
    fn paused_memory_answers_empty_not_an_error() {
        let (_dir, mut db) = database();
        remember(&mut db, "Something.", MemorySource::UserStated, None);
        db.update_memory_settings(UpdateMemorySettings {
            enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
        let outcome = call(&db, json!({}));
        assert_eq!(outcome["ok"], true);
        assert_eq!(outcome["result"]["memoryEnabled"], false);
        assert_eq!(outcome["result"]["records"], json!([]));
    }

    #[test]
    fn arguments_are_checked() {
        let (_dir, db) = database();
        for bad in [
            json!({ "limit": 0 }),
            json!({ "limit": 21 }),
            json!({ "planId": "not-a-plan" }),
            json!({ "kinds": ["gossip"] }),
            json!({ "query": "x".repeat(201) }),
            json!({ "unknown": true }),
        ] {
            let outcome = call(&db, bad.clone());
            assert_eq!(outcome["ok"], false, "{bad}");
            assert_eq!(outcome["error"]["code"], "invalid_arguments", "{bad}");
        }
    }

    #[test]
    fn limit_truncates_and_reports_total() {
        let (_dir, mut db) = database();
        for n in 0..5 {
            remember(&mut db, &format!("Note {n}."), MemorySource::UserStated, None);
        }
        let outcome = call(&db, json!({ "limit": 2 }));
        let result = &outcome["result"];
        assert_eq!(result["records"].as_array().unwrap().len(), 2);
        assert_eq!(result["totalMatches"], 5);
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn query_terms_drop_noise_and_plurals() {
        assert_eq!(
            query_terms("Move this and the reviews to Thursday, please"),
            vec!["review".to_string(), "thursday".to_string()]
        );
        assert_eq!(query_terms("it to a"), Vec::<String>::new());
        assert_eq!(query_terms("class"), vec!["class".to_string()]);
    }
}
