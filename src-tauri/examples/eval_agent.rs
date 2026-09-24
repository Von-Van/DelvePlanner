use chrono::{Days, NaiveDate, SecondsFormat, Utc};
use delve_planner_desktop::agent::{asks_to_choose, PlannerAgent, PlannerRequest, MODEL_NAME};
use delve_planner_desktop::db::{CandidateRequest, PlannerDatabase};
use delve_planner_desktop::error::AppError;
use delve_planner_desktop::model::{
    CreateEventInput, CreateInboxItemInput, CreateMilestoneInput, CreatePlanInput, CreateTaskInput,
    CreateWorkstreamInput, ExportBundle, LinkChange, MilestoneStatus, PlanStatus, PlannerResponse,
    PlanningFacts, ProposalReference, RecordKind, ReminderChange, TaskPriority, TaskStatus,
    UpdateEventInput, UpdateMilestoneInput, UpdatePlanInput, UpdatePlanningProfileInput,
    UpdateTaskInput,
};
use delve_planner_desktop::runtime::OllamaRuntimeManager;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

/// A fixtures file is either a list of cases or `{ "sharedFixtures": {...}, "cases": [...] }`,
/// where a case can start from a named set of records with `"uses"`.
#[derive(Deserialize)]
#[serde(untagged)]
enum FixtureFile {
    Cases(Vec<EvalCase>),
    Shared {
        #[serde(rename = "sharedFixtures")]
        shared_fixtures: HashMap<String, FixtureRecords>,
        cases: Vec<EvalCase>,
    },
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureRecords {
    #[serde(default)]
    plans: Vec<FixturePlan>,
    #[serde(default)]
    milestones: Vec<FixtureMilestone>,
    #[serde(default)]
    tasks: Vec<FixtureTask>,
    #[serde(default)]
    events: Vec<FixtureEvent>,
    #[serde(default)]
    workstreams: Vec<FixtureWorkstream>,
    #[serde(default)]
    inbox: Vec<FixtureInboxItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvalCase {
    id: String,
    command: String,
    day: String,
    time_zone: String,
    #[serde(default)]
    uses: Option<String>,
    #[serde(default)]
    plans: Vec<FixturePlan>,
    #[serde(default)]
    milestones: Vec<FixtureMilestone>,
    #[serde(default)]
    tasks: Vec<FixtureTask>,
    #[serde(default)]
    events: Vec<FixtureEvent>,
    #[serde(default)]
    workstreams: Vec<FixtureWorkstream>,
    #[serde(default)]
    inbox: Vec<FixtureInboxItem>,
    /// ISO weekdays the planning profile marks as days off, Monday = 1.
    #[serde(default)]
    days_off: Vec<u8>,
    /// The plan the request is made from, by title.
    #[serde(default)]
    active_plan: Option<String>,
    #[serde(default)]
    history: Vec<HistoryTurn>,
    /// Changes the proposal's existing records before applying it, which must then fail.
    #[serde(default)]
    after_proposal: Option<AfterProposal>,
    expected: ExpectedResponse,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixturePlan {
    title: String,
    #[serde(default)]
    status: Option<PlanStatus>,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    target_date: Option<String>,
    /// Created and then deleted, so it exists only in the past.
    #[serde(default)]
    deleted: bool,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureMilestone {
    plan: String,
    title: String,
    #[serde(default)]
    target_date: Option<String>,
    #[serde(default)]
    status: Option<MilestoneStatus>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureTask {
    title: String,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    milestone: Option<String>,
    #[serde(default)]
    scheduled_day: Option<String>,
    #[serde(default)]
    due_date: Option<String>,
    #[serde(default)]
    status: Option<TaskStatus>,
    #[serde(default)]
    priority: Option<TaskPriority>,
    /// A workstream of the task's plan, by name.
    #[serde(default)]
    workstream: Option<String>,
    #[serde(default)]
    deleted: bool,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureWorkstream {
    plan: String,
    name: String,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureInboxItem {
    text: String,
    #[serde(default)]
    notes: String,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureEvent {
    title: String,
    #[serde(default)]
    notes: String,
    start_at_utc: String,
    time_zone: String,
    duration_minutes: i64,
    #[serde(default)]
    reminder_minutes_before: Option<i64>,
    #[serde(default)]
    plan: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HistoryTurn {
    Command(String),
    Detailed {
        command: String,
        #[serde(default)]
        apply: bool,
    },
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum AfterProposal {
    Edit,
    Delete,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ExpectedResponse {
    /// Each operation lists the fields to check. Records are named by title: `event`, `task`,
    /// `milestone`, and `plan` (also for plan and milestone references). `eventTitle` is the
    /// original name for `event`.
    Proposal {
        operations: Vec<Map<String, Value>>,
    },
    Clarification,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvalReport {
    generated_at: String,
    model_name: String,
    model_digest: Option<String>,
    ollama_version: Option<String>,
    fixture_count: usize,
    runs: Vec<RunReport>,
    passed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunReport {
    run: usize,
    schema_valid: usize,
    exact: usize,
    fields_correct: usize,
    fields_total: usize,
    safety_exact: usize,
    safety_total: usize,
    failures: Vec<String>,
    passed: bool,
}

struct EvalOptions {
    fixtures: Vec<PathBuf>,
    runs: usize,
    json_output: Option<PathBuf>,
    only: Vec<String>,
}

#[tokio::main]
async fn main() {
    let options = eval_options().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2)
    });
    let mut cases = Vec::new();
    for path in &options.fixtures {
        let contents = fs::read_to_string(path).unwrap_or_else(|error| {
            eprintln!("Could not read {}: {error}", path.display());
            std::process::exit(2)
        });
        let file: FixtureFile = serde_json::from_str(&contents).unwrap_or_else(|error| {
            eprintln!("Invalid eval fixture JSON in {}: {error}", path.display());
            std::process::exit(2)
        });
        match file {
            FixtureFile::Cases(file_cases) => cases.extend(file_cases),
            FixtureFile::Shared {
                shared_fixtures,
                cases: file_cases,
            } => {
                for mut case in file_cases {
                    if let Some(name) = &case.uses {
                        let shared = shared_fixtures.get(name).unwrap_or_else(|| {
                            eprintln!("{}: unknown shared fixture {name}", case.id);
                            std::process::exit(2)
                        });
                        prepend(&mut case.plans, &shared.plans);
                        prepend(&mut case.milestones, &shared.milestones);
                        prepend(&mut case.tasks, &shared.tasks);
                        prepend(&mut case.events, &shared.events);
                        prepend(&mut case.workstreams, &shared.workstreams);
                        prepend(&mut case.inbox, &shared.inbox);
                    }
                    cases.push(case);
                }
            }
        }
    }
    if !options.only.is_empty() {
        cases.retain(|case| options.only.iter().any(|id| id == &case.id));
    }
    if cases.is_empty() {
        eprintln!("No evaluation cases matched.");
        std::process::exit(2);
    }
    point_at_installed_models();
    let runtime = OllamaRuntimeManager::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        eval_data_directory(),
    )
    .unwrap_or_else(|error| {
        eprintln!("Live evaluation cannot initialize Delve Planner's bundled runtime: {error}");
        std::process::exit(2)
    });
    let agent = PlannerAgent::new(runtime.endpoint(), MODEL_NAME);
    // `status` deliberately starts nothing, so the evaluation asks for the runtime it needs.
    let status = runtime.started_status(&agent).await;
    if !status.running || !status.model_installed {
        eprintln!("Live evaluation cannot start: {}", status.detail);
        eprintln!(
            "The gate runs against {MODEL_NAME}; install it in Delve Planner or Ollama first."
        );
        std::process::exit(2);
    }
    let mut runs = Vec::new();
    for run in 1..=options.runs {
        let result = evaluate_run(run, &cases, runtime.endpoint()).await;
        print_run(&result, cases.len());
        runs.push(result);
    }
    let passed = runs.iter().all(|run| run.passed);
    let report = EvalReport {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        model_name: status.model_name,
        model_digest: status.model_digest,
        ollama_version: status.ollama_version,
        fixture_count: cases.len(),
        runs,
        passed,
    };
    if let Some(path) = options.json_output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|error| {
                eprintln!("Could not create {}: {error}", parent.display());
                std::process::exit(2)
            });
        }
        fs::write(
            &path,
            serde_json::to_vec_pretty(&report).expect("serialize report"),
        )
        .unwrap_or_else(|error| {
            eprintln!("Could not write {}: {error}", path.display());
            std::process::exit(2)
        });
        println!("Machine-readable report: {}", path.display());
    }
    if !passed {
        std::process::exit(1);
    }
}

fn prepend<T: Clone>(target: &mut Vec<T>, shared: &[T]) {
    let own = std::mem::take(target);
    target.extend(shared.iter().cloned());
    target.extend(own);
}

/// Where the evaluation keeps its own runtime state. Never the app's own directory: Delve Planner
/// records the process ID of the server it started there, and a second runtime reading that
/// record would stop the running app's model as a leftover.
fn eval_data_directory() -> PathBuf {
    if let Some(path) = env::var_os("DELVE_PLANNER_EVAL_DATA_DIR") {
        return PathBuf::from(path);
    }
    env::temp_dir().join("delve-planner-eval-data")
}

/// The evaluation reads models from wherever the machine keeps them, so it doesn't need its own
/// copy of a five-gigabyte download. Delve Planner's own folder is offered to the runtime as the
/// machine's model folder, which it only ever reads from.
fn point_at_installed_models() {
    if env::var_os("OLLAMA_MODELS").is_some() {
        return;
    }
    let app_models = app_data_directory().map(|directory| directory.join("ai-models"));
    if let Some(models) = app_models.filter(|path| path.is_dir()) {
        env::set_var("OLLAMA_MODELS", models);
    }
}

fn app_data_directory() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME").map(|home| {
            PathBuf::from(home).join("Library/Application Support/com.vonvan.dayplan.desktop")
        })
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(|app_data| PathBuf::from(app_data).join("com.vonvan.dayplan.desktop"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

fn eval_options() -> Result<EvalOptions, String> {
    let usage = "Usage: eval_agent --fixtures path [--fixtures path…] [--runs 3] [--json-output path] [--only id,id]";
    let mut args = env::args().skip(1);
    let mut fixtures = Vec::new();
    let mut runs = 1usize;
    let mut json_output = None;
    let mut only = Vec::new();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--fixtures" => fixtures.push(args.next().map(PathBuf::from).ok_or(usage)?),
            "--runs" => {
                runs = args
                    .next()
                    .ok_or("--runs requires a positive integer")?
                    .parse()
                    .map_err(|_| "--runs requires a positive integer")?;
                if runs == 0 {
                    return Err("--runs requires a positive integer".into());
                }
            }
            "--json-output" => json_output = args.next().map(PathBuf::from),
            "--only" => {
                only = args
                    .next()
                    .ok_or("--only requires case ids")?
                    .split(',')
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty())
                    .collect()
            }
            _ => return Err(format!("Unknown argument: {argument}\n{usage}")),
        }
    }
    if fixtures.is_empty() {
        return Err(usage.into());
    }
    Ok(EvalOptions {
        fixtures,
        runs,
        json_output,
        only,
    })
}

/// Creates a case's records and returns the plan IDs by title.
fn seed(database: &mut PlannerDatabase, case: &EvalCase) -> HashMap<String, String> {
    let mut plan_ids = HashMap::new();
    let mut deleted_plans = Vec::new();
    for fixture in &case.plans {
        let plan = database
            .create_plan(CreatePlanInput {
                title: fixture.title.clone(),
                description: String::new(),
                status: fixture.status.unwrap_or(PlanStatus::Active),
                start_date: fixture.start_date.clone(),
                target_date: fixture.target_date.clone(),
                color: None,
                links: Vec::new(),
            })
            .expect("valid fixture plan");
        if fixture.deleted {
            deleted_plans.push(plan.clone());
        }
        plan_ids.insert(plan.title.clone(), plan.id);
    }
    let plan_id = |title: &str| {
        plan_ids
            .get(title)
            .cloned()
            .unwrap_or_else(|| panic!("{}: unknown fixture plan {title}", case.id))
    };
    let mut milestone_ids = HashMap::new();
    for fixture in &case.milestones {
        let milestone = database
            .create_milestone(CreateMilestoneInput {
                plan_id: plan_id(&fixture.plan),
                title: fixture.title.clone(),
                description: String::new(),
                target_date: fixture.target_date.clone(),
                status: fixture.status.unwrap_or(MilestoneStatus::Pending),
                workstream_id: None,
            })
            .expect("valid fixture milestone");
        milestone_ids.insert((fixture.plan.clone(), fixture.title.clone()), milestone.id);
    }
    let mut workstream_ids = HashMap::new();
    for fixture in &case.workstreams {
        let workstream = database
            .create_workstream(CreateWorkstreamInput {
                plan_id: plan_id(&fixture.plan),
                name: fixture.name.clone(),
                description: String::new(),
            })
            .expect("valid fixture workstream");
        workstream_ids.insert((fixture.plan.clone(), fixture.name.clone()), workstream.id);
    }
    for fixture in &case.inbox {
        database
            .create_inbox_item(CreateInboxItemInput {
                text: fixture.text.clone(),
                notes: fixture.notes.clone(),
            })
            .expect("valid fixture inbox item");
    }
    if !case.days_off.is_empty() {
        let profile = database.planning_profile().expect("profile");
        database
            .update_planning_profile(UpdatePlanningProfileInput {
                revision: profile.revision,
                preferred_start_minute: None,
                preferred_end_minute: None,
                max_planned_minutes: None,
                focus_minutes: None,
                break_minutes: None,
                no_work_days: case.days_off.clone(),
                energy: None,
                muted_observations: Vec::new(),
            })
            .expect("fixture days off");
    }
    for fixture in &case.tasks {
        let task = database
            .create_task(CreateTaskInput {
                title: fixture.title.clone(),
                description: String::new(),
                plan_id: fixture.plan.as_deref().map(plan_id),
                milestone_id: fixture.milestone.as_ref().map(|title| {
                    let plan = fixture.plan.clone().expect("a milestone task has a plan");
                    milestone_ids
                        .get(&(plan, title.clone()))
                        .cloned()
                        .unwrap_or_else(|| panic!("{}: unknown fixture milestone {title}", case.id))
                }),
                workstream_id: fixture.workstream.as_ref().map(|name| {
                    let plan = fixture.plan.clone().expect("a workstream task has a plan");
                    workstream_ids
                        .get(&(plan, name.clone()))
                        .cloned()
                        .unwrap_or_else(|| panic!("{}: unknown fixture workstream {name}", case.id))
                }),
                owner_id: None,
                due_date: fixture.due_date.clone(),
                scheduled_day: fixture.scheduled_day.clone(),
                planned_week: None,
                estimated_minutes: None,
                status: fixture.status.unwrap_or(TaskStatus::Todo),
                priority: fixture.priority.unwrap_or(TaskPriority::Normal),
                checklist: Vec::new(),
                recurrence: None,
                waiting_on: Vec::new(),
            })
            .expect("valid fixture task");
        if fixture.deleted {
            database
                .delete_task(&task.id, task.revision)
                .expect("delete fixture task");
        }
    }
    for fixture in &case.events {
        database
            .create_event(CreateEventInput {
                title: fixture.title.clone(),
                notes: fixture.notes.clone(),
                start_at_utc: fixture.start_at_utc.clone(),
                time_zone: fixture.time_zone.clone(),
                duration_minutes: fixture.duration_minutes,
                reminder_minutes_before: fixture.reminder_minutes_before,
                plan_id: fixture.plan.as_deref().map(plan_id),
                location: String::new(),
                workstream_id: None,
                owner_id: None,
            })
            .expect("valid fixture event");
    }
    for plan in deleted_plans {
        delete_plan(database, &plan.id);
    }
    plan_ids
}

fn delete_plan(database: &mut PlannerDatabase, id: &str) {
    let plan = database
        .list_plans("2026-01-01")
        .expect("plans")
        .into_iter()
        .find(|summary| summary.plan.id == id)
        .expect("plan to delete")
        .plan;
    let archived = database
        .update_plan(UpdatePlanInput {
            id: plan.id.clone(),
            revision: plan.revision,
            title: plan.title,
            description: plan.description,
            status: plan.status,
            start_date: plan.start_date,
            target_date: plan.target_date,
            color: plan.color,
            archived: true,
            links: Vec::new(),
        })
        .expect("archive plan");
    database
        .delete_plan(&archived.id, archived.revision)
        .expect("delete plan");
}

/// Titles of every record currently in the database, by ID.
fn titles(database: &PlannerDatabase) -> HashMap<String, String> {
    let bundle = database.export_bundle().expect("export");
    let mut titles = HashMap::new();
    titles.extend(bundle.plans.into_iter().map(|plan| (plan.id, plan.title)));
    titles.extend(
        bundle
            .milestones
            .into_iter()
            .map(|milestone| (milestone.id, milestone.title)),
    );
    titles.extend(bundle.tasks.into_iter().map(|task| (task.id, task.title)));
    titles.extend(
        bundle
            .events
            .into_iter()
            .map(|event| (event.id, event.title)),
    );
    titles.extend(
        bundle
            .workstreams
            .into_iter()
            .map(|workstream| (workstream.id, workstream.name)),
    );
    titles.extend(
        bundle
            .inbox_items
            .into_iter()
            .map(|item| (item.id, item.text)),
    );
    titles
}

/// Spare time on the coming days, as the app gives it to requests that hand over the timing.
/// The evaluation has no calendars, so only Delve Planner's own events and planned work take time.
fn planning_facts(database: &PlannerDatabase, case: &EvalCase) -> PlanningFacts {
    const DAYS: u32 = 14;
    let first = NaiveDate::parse_from_str(&case.day, "%Y-%m-%d").expect("fixture day");
    let end = first
        .checked_add_days(Days::new(u64::from(DAYS)))
        .expect("day in range")
        .to_string();
    let facts = database
        .capacity_facts(&case.day, &end, &case.time_zone, None)
        .expect("capacity facts");
    let profile = database.planning_profile().expect("profile");
    let capacity = delve_planner_desktop::capacity::capacity(
        first,
        DAYS,
        case.time_zone.parse().expect("fixture time zone"),
        facts,
        &[],
        false,
        &profile,
    );
    PlanningFacts::from_capacity(&capacity)
}

async fn propose(
    agent: &PlannerAgent,
    database: &PlannerDatabase,
    case: &EvalCase,
    command: &str,
    active_plan_id: Option<&str>,
) -> Result<PlannerResponse, AppError> {
    let referenced_ids = agent.referenced_ids();
    let mut candidates = database.planner_candidates(&CandidateRequest {
        command,
        selected_day: &case.day,
        time_zone: &case.time_zone,
        referenced_ids: &referenced_ids,
        active_plan_id,
    })?;
    if asks_to_choose(command) {
        candidates.planning = Some(planning_facts(database, case));
    }
    agent
        .propose(PlannerRequest {
            command,
            selected_day: &case.day,
            time_zone: &case.time_zone,
            active_plan_id,
            candidates: &candidates,
            week_start: chrono::Weekday::Mon,
        })
        .await
}

fn apply(
    agent: &PlannerAgent,
    database: &mut PlannerDatabase,
    proposal_id: &str,
) -> Result<(), AppError> {
    let proposal = agent.claim_pending(proposal_id)?;
    let result = database.apply_proposal(&proposal);
    agent.finish_pending(proposal_id, result.as_ref().ok())?;
    result.map(|_| ())
}

/// Edits or deletes each existing record a proposal refers to, as if the user changed it while
/// the proposal waited for review.
fn disturb(database: &mut PlannerDatabase, references: &[ProposalReference], after: AfterProposal) {
    let bundle: ExportBundle = database.export_bundle().expect("export");
    for reference in references {
        match (reference.kind, after) {
            (RecordKind::Event, AfterProposal::Edit) => {
                let event = bundle.events.iter().find(|event| event.id == reference.id);
                let event = event.expect("referenced event");
                database
                    .update_event(UpdateEventInput {
                        id: event.id.clone(),
                        revision: event.revision,
                        title: Some(format!("{} (edited)", event.title)),
                        notes: None,
                        start_at_utc: None,
                        time_zone: None,
                        duration_minutes: None,
                        reminder_change: ReminderChange::Unchanged,
                        location: None,
                        plan_change: LinkChange::Unchanged,
                        workstream_change: LinkChange::Unchanged,
                        owner_change: LinkChange::Unchanged,
                    })
                    .expect("edit event");
            }
            (RecordKind::Event, AfterProposal::Delete) => {
                let event = bundle.events.iter().find(|event| event.id == reference.id);
                let event = event.expect("referenced event");
                database
                    .delete_event(&event.id, event.revision)
                    .expect("delete event");
            }
            (RecordKind::Plan, AfterProposal::Edit) => {
                let plan = bundle.plans.iter().find(|plan| plan.id == reference.id);
                let plan = plan.expect("referenced plan").clone();
                database
                    .update_plan(UpdatePlanInput {
                        id: plan.id,
                        revision: plan.revision,
                        title: format!("{} (edited)", plan.title),
                        description: plan.description,
                        status: plan.status,
                        start_date: plan.start_date,
                        target_date: plan.target_date,
                        color: plan.color,
                        archived: false,
                        links: Vec::new(),
                    })
                    .expect("edit plan");
            }
            (RecordKind::Plan, AfterProposal::Delete) => delete_plan(database, &reference.id),
            (RecordKind::Milestone, AfterProposal::Edit) => {
                let milestone = bundle
                    .milestones
                    .iter()
                    .find(|milestone| milestone.id == reference.id)
                    .expect("referenced milestone")
                    .clone();
                database
                    .update_milestone(UpdateMilestoneInput {
                        id: milestone.id,
                        revision: milestone.revision,
                        title: format!("{} (edited)", milestone.title),
                        description: milestone.description,
                        target_date: milestone.target_date,
                        status: milestone.status,
                        workstream_id: milestone.workstream_id,
                    })
                    .expect("edit milestone");
            }
            (RecordKind::Milestone, AfterProposal::Delete) => {
                let milestone = bundle
                    .milestones
                    .iter()
                    .find(|milestone| milestone.id == reference.id)
                    .expect("referenced milestone");
                database
                    .delete_milestone(&milestone.id, milestone.revision)
                    .expect("delete milestone");
            }
            (RecordKind::Task, AfterProposal::Edit) => {
                let task = bundle
                    .tasks
                    .iter()
                    .find(|task| task.id == reference.id)
                    .expect("referenced task")
                    .clone();
                database
                    .update_task(UpdateTaskInput {
                        id: task.id,
                        revision: task.revision,
                        title: format!("{} (edited)", task.title),
                        description: task.description,
                        plan_id: task.plan_id,
                        milestone_id: task.milestone_id,
                        workstream_id: task.workstream_id,
                        owner_id: task.owner_id,
                        due_date: task.due_date,
                        scheduled_day: task.scheduled_day,
                        planned_week: task.planned_week,
                        estimated_minutes: task.estimated_minutes,
                        status: task.status,
                        priority: task.priority,
                        checklist: Vec::new(),
                        recurrence: None,
                        waiting_on: Vec::new(),
                    })
                    .expect("edit task");
            }
            (RecordKind::Task, AfterProposal::Delete) => {
                let task = bundle
                    .tasks
                    .iter()
                    .find(|task| task.id == reference.id)
                    .expect("referenced task");
                database
                    .delete_task(&task.id, task.revision)
                    .expect("delete task");
            }
            // Only events, plans, milestones, and tasks are disturbed after a proposal.
            (RecordKind::Workstream | RecordKind::InboxItem, _) => {}
        }
    }
}

async fn evaluate_run(run: usize, cases: &[EvalCase], endpoint: &str) -> RunReport {
    let mut result = RunReport {
        run,
        schema_valid: 0,
        exact: 0,
        fields_correct: 0,
        fields_total: 0,
        safety_exact: 0,
        safety_total: cases
            .iter()
            .filter(|case| matches!(case.expected, ExpectedResponse::Clarification))
            .count(),
        failures: Vec::new(),
        passed: false,
    };
    for case in cases {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("eval.sqlite3");
        let mut database = PlannerDatabase::open(&path).expect("evaluation database");
        let plan_ids = seed(&mut database, case);
        let active_plan_id = case.active_plan.as_ref().map(|title| {
            plan_ids
                .get(title)
                .cloned()
                .unwrap_or_else(|| panic!("{}: unknown active plan {title}", case.id))
        });
        let agent = PlannerAgent::new(endpoint, MODEL_NAME);
        let mut history_result = Ok(());
        for turn in &case.history {
            let (command, apply_turn) = match turn {
                HistoryTurn::Command(command) => (command.as_str(), false),
                HistoryTurn::Detailed { command, apply } => (command.as_str(), *apply),
            };
            match propose(&agent, &database, case, command, active_plan_id.as_deref()).await {
                Ok(PlannerResponse::Proposal { proposal_id, .. }) if apply_turn => {
                    if let Err(error) = apply(&agent, &mut database, &proposal_id) {
                        history_result = Err(format!("history apply failed: {error}"));
                        break;
                    }
                }
                Ok(PlannerResponse::Clarification { .. }) if apply_turn => {
                    history_result = Err("history turn to apply was a clarification".to_string());
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    history_result = Err(format!("history error: {error}"));
                    break;
                }
            }
        }
        if let Err(error) = history_result {
            result.fields_total += expected_field_count(&case.expected);
            result.failures.push(format!("{}: {error}", case.id));
            continue;
        }
        let actual = propose(
            &agent,
            &database,
            case,
            &case.command,
            active_plan_id.as_deref(),
        )
        .await;
        match actual {
            Ok(response) => {
                result.schema_valid += 1;
                let names = titles(&database);
                let (mut is_exact, correct, total, detail) =
                    score(&case.expected, &response, &names);
                let mut failure = detail;
                if let (
                    ExpectedResponse::Proposal { .. },
                    PlannerResponse::Proposal {
                        proposal_id,
                        references,
                        ..
                    },
                ) = (&case.expected, &response)
                {
                    if let Some(after) = case.after_proposal {
                        disturb(&mut database, references, after);
                    }
                    let outcome = apply(&agent, &mut database, proposal_id);
                    let apply_ok = matches!(
                        (case.after_proposal, &outcome),
                        (None, Ok(()))
                            | (Some(AfterProposal::Edit), Err(AppError::Conflict))
                            | (
                                Some(AfterProposal::Delete),
                                Err(AppError::NotFound | AppError::Validation(_))
                            )
                    );
                    if !apply_ok {
                        is_exact = false;
                        failure = Some(format!(
                            "apply outcome {}",
                            match outcome {
                                Ok(()) => "applied".to_string(),
                                Err(error) => format!("failed: {error}"),
                            }
                        ));
                    }
                }
                if is_exact {
                    result.exact += 1;
                    if matches!(case.expected, ExpectedResponse::Clarification) {
                        result.safety_exact += 1;
                    }
                } else {
                    result.failures.push(format!(
                        "{}: expected {}, got {}{}",
                        case.id,
                        expected_description(&case.expected),
                        actual_description(&response),
                        failure
                            .map(|detail| format!(" — {detail}"))
                            .unwrap_or_default()
                    ));
                }
                result.fields_correct += correct;
                result.fields_total += total;
            }
            Err(error) => {
                result.fields_total += expected_field_count(&case.expected);
                result
                    .failures
                    .push(format!("{}: agent error: {error}", case.id));
            }
        }
    }
    result.passed = result.schema_valid == cases.len()
        && result.safety_exact == result.safety_total
        && percentage(result.exact, cases.len()) >= 85.0
        && percentage(result.fields_correct, result.fields_total) >= 95.0;
    result
}

fn print_run(result: &RunReport, case_count: usize) {
    println!("Delve Planner qwen3:8b evaluation — run {}", result.run);
    println!(
        "Schema valid: {}/{} ({:.1}%) | exact: {}/{} ({:.1}%) | fields: {}/{} ({:.1}%) | safety: {}/{}",
        result.schema_valid,
        case_count,
        percentage(result.schema_valid, case_count),
        result.exact,
        case_count,
        percentage(result.exact, case_count),
        result.fields_correct,
        result.fields_total,
        percentage(result.fields_correct, result.fields_total),
        result.safety_exact,
        result.safety_total
    );
    if !result.failures.is_empty() {
        println!("Failures:");
        for failure in &result.failures {
            println!("- {failure}");
        }
    }
}

/// Whether the response matches, the matching and total field counts, and a short reason.
fn score(
    expected: &ExpectedResponse,
    actual: &PlannerResponse,
    titles: &HashMap<String, String>,
) -> (bool, usize, usize, Option<String>) {
    match (expected, actual) {
        (ExpectedResponse::Clarification, PlannerResponse::Clarification { .. }) => {
            (true, 1, 1, None)
        }
        (
            ExpectedResponse::Proposal {
                operations: expected,
            },
            PlannerResponse::Proposal {
                operations: actual, ..
            },
        ) => {
            let mut expected_ordered = expected.iter().map(expected_operation).collect::<Vec<_>>();
            expected_ordered.sort_by_key(operation_key);
            let mut actual_ordered = actual
                .iter()
                .map(|operation| actual_operation(&operation.change, titles))
                .collect::<Vec<_>>();
            actual_ordered.sort_by_key(operation_key);
            let mut exact = expected.len() == actual.len();
            let mut correct = 0;
            let mut total = 0;
            let mut mismatches = Vec::new();
            for (expected, actual) in expected_ordered.iter().zip(&actual_ordered) {
                let (matching, all, wrong) = score_operation(expected, Some(actual));
                exact &= wrong.is_empty();
                correct += matching;
                total += all;
                mismatches.extend(wrong);
            }
            if expected.len() != actual.len() {
                total += expected.len().abs_diff(actual.len());
                mismatches.push(format!(
                    "{} operations instead of {}: {}",
                    actual.len(),
                    expected.len(),
                    actual_ordered
                        .iter()
                        .map(operation_key)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let detail = (!mismatches.is_empty()).then(|| mismatches.join("; "));
            (exact, correct, total, detail)
        }
        (_, PlannerResponse::Clarification { question }) => (
            false,
            0,
            expected_field_count(expected),
            Some(format!("asked “{question}”")),
        ),
        _ => (
            false,
            0,
            expected_field_count(expected),
            Some(format!(
                "proposed {}",
                match actual {
                    PlannerResponse::Proposal { summary, .. } => summary.as_str(),
                    PlannerResponse::Clarification { question } => question.as_str(),
                }
            )),
        ),
    }
}

fn expected_operation(operation: &Map<String, Value>) -> Map<String, Value> {
    let mut operation = operation.clone();
    if let Some(title) = operation.remove("eventTitle") {
        operation.insert("event".into(), title);
    }
    operation
}

/// A proposal operation in the fixtures' vocabulary: IDs become titles and revisions are dropped.
fn actual_operation(
    operation: &delve_planner_desktop::model::MutationOperation,
    titles: &HashMap<String, String>,
) -> Map<String, Value> {
    let Value::Object(mut value) = serde_json::to_value(operation).expect("operation serializes")
    else {
        unreachable!("operations serialize as objects");
    };
    value.remove("expectedRevision");
    let title = |id: &str| {
        titles
            .get(id)
            .cloned()
            .unwrap_or_else(|| "<unknown>".into())
    };
    for (id_key, name_key) in [
        ("eventId", "event"),
        ("taskId", "task"),
        ("milestoneId", "milestone"),
        ("planId", "plan"),
    ] {
        if let Some(Value::String(id)) = value.remove(id_key) {
            value.insert(name_key.into(), Value::String(title(&id)));
        }
    }
    if let Some(Value::Object(source)) = value.get("fromInbox") {
        let text = match source.get("itemId") {
            Some(Value::String(id)) => title(id),
            _ => "<invalid>".into(),
        };
        value.insert("fromInbox".into(), Value::String(text));
    }
    for key in ["plan", "milestone", "workstream"] {
        if let Some(Value::Object(reference)) = value.get(key) {
            let name = match (reference.get("id"), reference.get("newTitle")) {
                (Some(Value::String(id)), _) => title(id),
                (_, Some(Value::String(new_title))) => new_title.clone(),
                _ => "<invalid>".into(),
            };
            value.insert(key.into(), Value::String(name));
        }
    }
    value
}

fn operation_key(operation: &Map<String, Value>) -> String {
    let field = |key: &str| {
        operation
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let kind = field("type");
    if kind == "create_event" {
        format!("create_event:{}:{}", field("title"), field("startAtUtc"))
    } else if kind == "create_workstream" {
        format!("{kind}:{}", field("name"))
    } else if kind.starts_with("create_") {
        format!("{kind}:{}", field("title"))
    } else {
        let target = ["event", "task", "milestone", "plan"]
            .iter()
            .map(|key| field(key))
            .find(|value| !value.is_empty())
            .unwrap_or_default();
        format!("{kind}:{target}")
    }
}

/// Compares the listed fields. For `update_event` and `reschedule_event`, a `null` title,
/// notes, or duration means "not checked", as in the original schedule fixtures; everywhere else
/// `null` must match. A `create_event` without `reminderMinutesBefore` expects no reminder.
fn score_operation(
    expected: &Map<String, Value>,
    actual: Option<&Map<String, Value>>,
) -> (usize, usize, Vec<String>) {
    let kind = expected
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let actual_kind = actual
        .and_then(|actual| actual.get("type"))
        .and_then(Value::as_str);
    let mut checks = expected
        .iter()
        .filter(|(key, value)| {
            *key != "type"
                && !(matches!(kind, "update_event" | "reschedule_event")
                    && matches!(key.as_str(), "title" | "notes" | "durationMinutes")
                    && value.is_null())
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    if kind == "create_event" && !expected.contains_key("reminderMinutesBefore") {
        checks.push(("reminderMinutesBefore".into(), Value::Null));
    }
    let Some(actual) = actual.filter(|_| actual_kind == Some(kind)) else {
        return (
            0,
            checks.len(),
            vec![format!(
                "expected {kind}, got {}",
                actual_kind.unwrap_or("nothing")
            )],
        );
    };
    let mut correct = 0;
    let mut wrong = Vec::new();
    for (key, value) in &checks {
        let found = actual.get(key).unwrap_or(&Value::Null);
        if found == value {
            correct += 1;
        } else {
            wrong.push(format!("{kind}.{key} = {found} (expected {value})"));
        }
    }
    (correct, checks.len(), wrong)
}

fn expected_field_count(expected: &ExpectedResponse) -> usize {
    match expected {
        ExpectedResponse::Clarification => 1,
        ExpectedResponse::Proposal { operations } => operations
            .iter()
            .map(|operation| score_operation(&expected_operation(operation), None).1)
            .sum(),
    }
}

fn percentage(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        100.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn expected_description(value: &ExpectedResponse) -> &'static str {
    match value {
        ExpectedResponse::Proposal { .. } => "proposal",
        ExpectedResponse::Clarification => "clarification",
    }
}

fn actual_description(value: &PlannerResponse) -> &'static str {
    match value {
        PlannerResponse::Proposal { .. } => "proposal",
        PlannerResponse::Clarification { .. } => "clarification",
    }
}
