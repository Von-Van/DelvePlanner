pub mod agent;
pub mod db;
pub mod error;
pub mod model;
pub mod runtime;

use agent::{OllamaStatus, PlannerAgent, PlannerRequest};
use db::{
    backups_for_path, parse_export_bundle, restore_backup, CandidateRequest, DueReminder,
    PlannerDatabase, CURRENT_SCHEMA_VERSION,
};
use error::{AppError, CommandError};
use model::{
    Agenda, AppliedProposal, CreateEventInput, CreateInboxItemInput, CreateMilestoneInput,
    CreatePersonInput, CreatePlanInput, CreateTaskInput, CreateWorkstreamInput, DatabaseStatus,
    ExportBundle, ImportPreview, InboxConversion, InboxItem, LocalDateTimeInput,
    LocalDateTimeResolution, Milestone, Person, PersonSummary, Plan, PlanDeletion, PlanSummary,
    PlanWorkspace, PlannerResponse, PlanningBoard, ProcessInboxItemInput, RescheduleEventInput,
    ScheduleEvent, Task, TaskMove, UpdateEventInput, UpdateInboxItemInput, UpdateMilestoneInput,
    UpdatePersonInput, UpdatePlanInput, UpdateTaskInput, UpdateWorkstreamInput, Workstream,
};
use runtime::OllamaRuntimeManager;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;
use zip::write::SimpleFileOptions;

const MAX_IMPORT_BYTES: u64 = 50 * 1024 * 1024;

struct AppState {
    database: Mutex<DatabaseRuntime>,
    agent: PlannerAgent,
    ollama: OllamaRuntimeManager,
    pending_import: Mutex<Option<PendingImport>>,
}

struct PendingImport {
    token: String,
    bundle: ExportBundle,
}

struct DatabaseRuntime {
    path: PathBuf,
    database: Option<PlannerDatabase>,
    startup_error: Option<CommandError>,
}

impl DatabaseRuntime {
    fn new(path: PathBuf) -> Self {
        match PlannerDatabase::open(&path) {
            Ok(database) => Self {
                path,
                database: Some(database),
                startup_error: None,
            },
            Err(error) => Self {
                path,
                database: None,
                startup_error: Some(error.into()),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportSelection {
    token: String,
    preview: ImportPreview,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileActionResult {
    completed: bool,
    file_name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticManifest {
    generated_at: String,
    app_version: String,
    operating_system: String,
    architecture: String,
    database_ready: bool,
    schema_version: u32,
    backup_count: usize,
    ollama_running: bool,
    model_installed: bool,
    model_name: String,
    model_digest: Option<String>,
    ollama_version: Option<String>,
    ollama_phase: runtime::RuntimePhase,
    model_license: Option<String>,
    model_storage_bytes: Option<u64>,
    privacy_note: &'static str,
}

fn with_database<T>(
    state: &State<'_, AppState>,
    operation: impl FnOnce(&mut PlannerDatabase) -> Result<T, AppError>,
) -> Result<T, CommandError> {
    let mut runtime = state
        .database
        .lock()
        .map_err(|_| CommandError::internal("The local database session is unavailable."))?;
    if runtime.database.is_none() {
        return Err(runtime
            .startup_error
            .clone()
            .unwrap_or_else(|| CommandError::internal("The local database could not be opened.")));
    }
    let database = runtime
        .database
        .as_mut()
        .expect("database presence checked");
    operation(database).map_err(CommandError::from)
}

#[tauri::command]
fn list_agenda(
    state: State<'_, AppState>,
    day: String,
    time_zone: String,
) -> Result<Agenda, CommandError> {
    with_database(&state, |database| database.agenda(&day, 1, &time_zone))
}

#[tauri::command]
fn list_week(
    state: State<'_, AppState>,
    start_day: String,
    time_zone: String,
) -> Result<Agenda, CommandError> {
    with_database(&state, |database| {
        database.agenda(&start_day, 7, &time_zone)
    })
}

#[tauri::command]
fn list_people(state: State<'_, AppState>) -> Result<Vec<PersonSummary>, CommandError> {
    with_database(&state, |database| database.list_people())
}

#[tauri::command]
fn create_person(
    state: State<'_, AppState>,
    input: CreatePersonInput,
) -> Result<Person, CommandError> {
    with_database(&state, |database| database.create_person(input))
}

#[tauri::command]
fn update_person(
    state: State<'_, AppState>,
    input: UpdatePersonInput,
) -> Result<Person, CommandError> {
    with_database(&state, |database| database.update_person(input))
}

#[tauri::command]
fn delete_person(
    state: State<'_, AppState>,
    id: String,
    revision: i64,
) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_person(&id, revision))
}

#[tauri::command]
fn create_workstream(
    state: State<'_, AppState>,
    input: CreateWorkstreamInput,
) -> Result<Workstream, CommandError> {
    with_database(&state, |database| database.create_workstream(input))
}

#[tauri::command]
fn update_workstream(
    state: State<'_, AppState>,
    input: UpdateWorkstreamInput,
) -> Result<Workstream, CommandError> {
    with_database(&state, |database| database.update_workstream(input))
}

#[tauri::command]
fn delete_workstream(
    state: State<'_, AppState>,
    id: String,
    revision: i64,
) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_workstream(&id, revision))
}

#[tauri::command]
fn list_plans(state: State<'_, AppState>, today: String) -> Result<Vec<PlanSummary>, CommandError> {
    with_database(&state, |database| database.list_plans(&today))
}

#[tauri::command]
fn get_plan_workspace(
    state: State<'_, AppState>,
    id: String,
) -> Result<PlanWorkspace, CommandError> {
    with_database(&state, |database| database.plan_workspace(&id))
}

#[tauri::command]
fn create_plan(state: State<'_, AppState>, input: CreatePlanInput) -> Result<Plan, CommandError> {
    with_database(&state, |database| database.create_plan(input))
}

#[tauri::command]
fn update_plan(state: State<'_, AppState>, input: UpdatePlanInput) -> Result<Plan, CommandError> {
    with_database(&state, |database| database.update_plan(input))
}

#[tauri::command]
fn delete_plan(
    state: State<'_, AppState>,
    id: String,
    revision: i64,
) -> Result<PlanDeletion, CommandError> {
    with_database(&state, |database| database.delete_plan(&id, revision))
}

#[tauri::command]
fn create_milestone(
    state: State<'_, AppState>,
    input: CreateMilestoneInput,
) -> Result<Milestone, CommandError> {
    with_database(&state, |database| database.create_milestone(input))
}

#[tauri::command]
fn update_milestone(
    state: State<'_, AppState>,
    input: UpdateMilestoneInput,
) -> Result<Milestone, CommandError> {
    with_database(&state, |database| database.update_milestone(input))
}

#[tauri::command]
fn delete_milestone(
    state: State<'_, AppState>,
    id: String,
    revision: i64,
) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_milestone(&id, revision))
}

#[tauri::command]
fn database_status(state: State<'_, AppState>) -> Result<DatabaseStatus, CommandError> {
    let runtime = state
        .database
        .lock()
        .map_err(|_| CommandError::internal("The local database session is unavailable."))?;
    let backups = backups_for_path(&runtime.path).unwrap_or_default();
    let schema_version = runtime
        .database
        .as_ref()
        .and_then(|database| database.schema_version().ok())
        .unwrap_or(CURRENT_SCHEMA_VERSION);
    Ok(DatabaseStatus {
        ready: runtime.database.is_some(),
        schema_version,
        error: runtime.startup_error.clone(),
        backups,
    })
}

#[tauri::command]
fn create_event(
    state: State<'_, AppState>,
    input: CreateEventInput,
) -> Result<ScheduleEvent, CommandError> {
    with_database(&state, |database| database.create_event(input))
}

#[tauri::command]
fn update_event(
    state: State<'_, AppState>,
    input: UpdateEventInput,
) -> Result<ScheduleEvent, CommandError> {
    with_database(&state, |database| database.update_event(input))
}

#[tauri::command]
fn delete_event(state: State<'_, AppState>, id: String, revision: i64) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_event(&id, revision))
}

#[tauri::command]
fn reschedule_event(
    state: State<'_, AppState>,
    input: RescheduleEventInput,
) -> Result<ScheduleEvent, CommandError> {
    with_database(&state, |database| database.reschedule_event(input))
}

#[tauri::command]
fn create_task(state: State<'_, AppState>, input: CreateTaskInput) -> Result<Task, CommandError> {
    with_database(&state, |database| database.create_task(input))
}

#[tauri::command]
fn update_task(state: State<'_, AppState>, input: UpdateTaskInput) -> Result<Task, CommandError> {
    with_database(&state, |database| database.update_task(input))
}

#[tauri::command]
fn delete_task(state: State<'_, AppState>, id: String, revision: i64) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_task(&id, revision))
}

#[tauri::command]
fn list_inbox_items(state: State<'_, AppState>) -> Result<Vec<InboxItem>, CommandError> {
    with_database(&state, |database| database.list_inbox_items())
}

#[tauri::command]
fn create_inbox_item(
    state: State<'_, AppState>,
    input: CreateInboxItemInput,
) -> Result<InboxItem, CommandError> {
    with_database(&state, |database| database.create_inbox_item(input))
}

#[tauri::command]
fn update_inbox_item(
    state: State<'_, AppState>,
    input: UpdateInboxItemInput,
) -> Result<InboxItem, CommandError> {
    with_database(&state, |database| database.update_inbox_item(input))
}

#[tauri::command]
fn delete_inbox_item(
    state: State<'_, AppState>,
    id: String,
    revision: i64,
) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_inbox_item(&id, revision))
}

#[tauri::command]
fn process_inbox_item(
    state: State<'_, AppState>,
    input: ProcessInboxItemInput,
) -> Result<InboxConversion, CommandError> {
    with_database(&state, |database| database.process_inbox_item(input))
}

#[tauri::command]
fn get_planning_board(
    state: State<'_, AppState>,
    start_day: String,
) -> Result<PlanningBoard, CommandError> {
    with_database(&state, |database| database.planning_board(&start_day))
}

#[tauri::command]
fn move_tasks(state: State<'_, AppState>, moves: Vec<TaskMove>) -> Result<Vec<Task>, CommandError> {
    with_database(&state, |database| database.move_tasks(moves))
}

#[tauri::command]
fn resolve_local_datetime(
    input: LocalDateTimeInput,
) -> Result<LocalDateTimeResolution, CommandError> {
    PlannerDatabase::resolve_local_datetime(&input).map_err(CommandError::from)
}

#[tauri::command]
async fn export_planner_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<FileActionResult, CommandError> {
    let bundle = with_database(&state, |database| database.export_bundle())?;
    let app_for_dialog = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        app_for_dialog
            .dialog()
            .file()
            .set_title("Export DayPlan data")
            .set_file_name("dayplan-export.json")
            .add_filter("DayPlan JSON", &["json"])
            .blocking_save_file()
            .and_then(|path| path.as_path().map(PathBuf::from))
    })
    .await
    .map_err(|_| CommandError::internal("The export dialog could not be opened."))?;
    let Some(path) = path else {
        return Ok(FileActionResult {
            completed: false,
            file_name: None,
        });
    };
    let bytes = serde_json::to_vec_pretty(&bundle)
        .map_err(AppError::from)
        .map_err(CommandError::from)?;
    fs::write(&path, bytes)
        .map_err(AppError::from)
        .map_err(CommandError::from)?;
    Ok(FileActionResult {
        completed: true,
        file_name: path.file_name().map(|name| name.to_string_lossy().into()),
    })
}

#[tauri::command]
async fn select_planner_import(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ImportSelection>, CommandError> {
    let app_for_dialog = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        app_for_dialog
            .dialog()
            .file()
            .set_title("Choose a DayPlan export")
            .add_filter("DayPlan JSON", &["json"])
            .blocking_pick_file()
            .and_then(|path| path.as_path().map(PathBuf::from))
    })
    .await
    .map_err(|_| CommandError::internal("The import dialog could not be opened."))?;
    let Some(path) = path else {
        return Ok(None);
    };
    if path
        .metadata()
        .map_err(AppError::from)
        .map_err(CommandError::from)?
        .len()
        > MAX_IMPORT_BYTES
    {
        return Err(CommandError::from(AppError::Validation(
            "That import is larger than the 50 MB limit.".into(),
        )));
    }
    let contents = fs::read_to_string(path)
        .map_err(AppError::from)
        .map_err(CommandError::from)?;
    let bundle = parse_export_bundle(&contents).map_err(CommandError::from)?;
    let preview = PlannerDatabase::preview_import(&bundle).map_err(CommandError::from)?;
    let token = Uuid::new_v4().to_string();
    state
        .pending_import
        .lock()
        .map_err(|_| CommandError::internal("The import preview is unavailable."))?
        .replace(PendingImport {
            token: token.clone(),
            bundle,
        });
    Ok(Some(ImportSelection { token, preview }))
}

#[tauri::command]
fn apply_selected_import(
    state: State<'_, AppState>,
    token: String,
) -> Result<ImportPreview, CommandError> {
    let bundle = {
        let mut pending = state
            .pending_import
            .lock()
            .map_err(|_| CommandError::internal("The import preview is unavailable."))?;
        let selected = pending.as_ref().ok_or_else(|| {
            CommandError::from(AppError::Validation(
                "Choose and preview an import file before replacing data.".into(),
            ))
        })?;
        if selected.token != token {
            return Err(CommandError::from(AppError::Validation(
                "That import preview is no longer current.".into(),
            )));
        }
        pending
            .take()
            .expect("pending import presence checked")
            .bundle
    };
    with_database(&state, |database| database.import_bundle(&bundle))
}

#[tauri::command]
fn discard_selected_import(state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .pending_import
        .lock()
        .map_err(|_| CommandError::internal("The import preview is unavailable."))?
        .take();
    Ok(())
}

#[tauri::command]
fn restore_database_backup(
    state: State<'_, AppState>,
    backup_name: String,
) -> Result<(), CommandError> {
    let mut runtime = state
        .database
        .lock()
        .map_err(|_| CommandError::internal("The local database session is unavailable."))?;
    if let Some(database) = runtime.database.as_ref() {
        database.backup("pre-restore").map_err(CommandError::from)?;
    }
    runtime.database.take();
    let result = restore_backup(&runtime.path, &backup_name)
        .and_then(|()| PlannerDatabase::open(&runtime.path));
    match result {
        Ok(database) => {
            runtime.database = Some(database);
            runtime.startup_error = None;
            Ok(())
        }
        Err(error) => {
            let command_error = CommandError::from(error);
            runtime.database = PlannerDatabase::open(&runtime.path).ok();
            runtime.startup_error = Some(command_error.clone());
            Err(command_error)
        }
    }
}

#[tauri::command]
async fn current_ollama_status(state: State<'_, AppState>) -> Result<OllamaStatus, CommandError> {
    Ok(state.ollama.status(&state.agent).await)
}

#[tauri::command]
async fn download_ollama_model(state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .ollama
        .pull_model(&state.agent)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
fn cancel_ollama_model_download(state: State<'_, AppState>) {
    state.ollama.cancel_download();
}

#[tauri::command]
async fn restart_ollama_runtime(state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .ollama
        .restart(&state.agent)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
async fn remove_ollama_model(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.agent.clear_context().map_err(CommandError::from)?;
    state
        .ollama
        .remove_model()
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
async fn export_diagnostic_bundle(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<FileActionResult, CommandError> {
    let (database_ready, schema_version, backup_count) = {
        let runtime = state
            .database
            .lock()
            .map_err(|_| CommandError::internal("Database diagnostics are unavailable."))?;
        (
            runtime.database.is_some(),
            runtime
                .database
                .as_ref()
                .and_then(|database| database.schema_version().ok())
                .unwrap_or(CURRENT_SCHEMA_VERSION),
            backups_for_path(&runtime.path)
                .map(|items| items.len())
                .unwrap_or(0),
        )
    };
    let model = state.ollama.status(&state.agent).await;
    let manifest = DiagnosticManifest {
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        app_version: app.package_info().version.to_string(),
        operating_system: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        database_ready,
        schema_version,
        backup_count,
        ollama_running: model.running,
        model_installed: model.model_installed,
        model_name: model.model_name,
        model_digest: model.model_digest,
        ollama_version: model.ollama_version,
        ollama_phase: model.phase,
        model_license: model.model_license,
        model_storage_bytes: model.storage_bytes,
        privacy_note: "Commands, plan/milestone/event/task titles, people, workstreams, notes, descriptions, locations, proposal contents, and database paths are excluded.",
    };
    let app_for_dialog = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        app_for_dialog
            .dialog()
            .file()
            .set_title("Export DayPlan diagnostics")
            .set_file_name("dayplan-diagnostics.zip")
            .add_filter("ZIP archive", &["zip"])
            .blocking_save_file()
            .and_then(|path| path.as_path().map(PathBuf::from))
    })
    .await
    .map_err(|_| CommandError::internal("The diagnostic export dialog could not be opened."))?;
    let Some(path) = path else {
        return Ok(FileActionResult {
            completed: false,
            file_name: None,
        });
    };
    let file_name = path.file_name().map(|name| name.to_string_lossy().into());
    let log_directory = app.path().app_log_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        write_diagnostic_zip(&path, &manifest, log_directory)
    })
    .await
    .map_err(|_| CommandError::internal("The diagnostic bundle could not be created."))??;
    Ok(FileActionResult {
        completed: true,
        file_name,
    })
}

fn write_diagnostic_zip(
    destination: &PathBuf,
    manifest: &DiagnosticManifest,
    log_directory: Option<PathBuf>,
) -> Result<(), CommandError> {
    let file = File::create(destination)
        .map_err(AppError::from)
        .map_err(CommandError::from)?;
    let mut archive = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    archive
        .start_file("diagnostics.json", options)
        .map_err(|_| CommandError::internal("The diagnostic archive could not be written."))?;
    archive
        .write_all(
            &serde_json::to_vec_pretty(manifest)
                .map_err(AppError::from)
                .map_err(CommandError::from)?,
        )
        .map_err(AppError::from)
        .map_err(CommandError::from)?;
    if let Some(directory) = log_directory {
        let mut logs = fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with("dayplan")
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "log")
            })
            .collect::<Vec<_>>();
        logs.sort_by_key(|entry| entry.file_name());
        for (index, entry) in logs.into_iter().rev().take(5).enumerate() {
            let mut bytes = Vec::new();
            if File::open(entry.path())
                .and_then(|file| file.take(512 * 1024).read_to_end(&mut bytes))
                .is_ok()
            {
                archive
                    .start_file(format!("logs/dayplan-{index}.log"), options)
                    .map_err(|_| {
                        CommandError::internal("The diagnostic archive could not be written.")
                    })?;
                archive
                    .write_all(&bytes)
                    .map_err(AppError::from)
                    .map_err(CommandError::from)?;
            }
        }
    }
    archive
        .finish()
        .map_err(|_| CommandError::internal("The diagnostic archive could not be finalized."))?;
    Ok(())
}

#[tauri::command]
async fn propose_schedule_changes(
    state: State<'_, AppState>,
    command: String,
    day: String,
    time_zone: String,
    active_plan_id: Option<String>,
) -> Result<PlannerResponse, CommandError> {
    state
        .ollama
        .ensure_started(&state.agent)
        .await
        .map_err(CommandError::from)?;
    let referenced_ids = state.agent.referenced_ids();
    let candidates = with_database(&state, |database| {
        database.planner_candidates(&CandidateRequest {
            command: &command,
            selected_day: &day,
            time_zone: &time_zone,
            referenced_ids: &referenced_ids,
            active_plan_id: active_plan_id.as_deref(),
        })
    })?;
    state
        .agent
        .propose(PlannerRequest {
            command: &command,
            selected_day: &day,
            time_zone: &time_zone,
            active_plan_id: active_plan_id.as_deref(),
            candidates: &candidates,
        })
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
fn apply_schedule_changes(
    state: State<'_, AppState>,
    proposal_id: String,
) -> Result<AppliedProposal, CommandError> {
    let proposal = state
        .agent
        .claim_pending(&proposal_id)
        .map_err(CommandError::from)?;
    let result = with_database(&state, |database| database.apply_proposal(&proposal));
    state
        .agent
        .finish_pending(&proposal_id, result.as_ref().ok())
        .map_err(CommandError::from)?;
    result
}

#[tauri::command]
fn clear_planner_context(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.agent.clear_context().map_err(CommandError::from)
}

#[tauri::command]
fn discard_schedule_proposal(
    state: State<'_, AppState>,
    proposal_id: String,
) -> Result<(), CommandError> {
    state
        .agent
        .discard_pending(&proposal_id)
        .map_err(CommandError::from)
}

#[tauri::command]
fn cancel_planner_request(state: State<'_, AppState>) {
    state.agent.cancel_current();
}

async fn reminder_worker(app: AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_secs(15));
    loop {
        interval.tick().await;
        let due = {
            let state = app.state::<AppState>();
            let Ok(mut runtime) = state.database.lock() else {
                continue;
            };
            let Some(database) = runtime.database.as_mut() else {
                continue;
            };
            if database.reconcile_reminders().is_err() {
                continue;
            }
            database.due_reminders(25).unwrap_or_default()
        };

        for reminder in due {
            let delivered = app
                .notification()
                .builder()
                .title(&reminder.title)
                .body(reminder_body(&reminder))
                .show()
                .is_ok();
            let state = app.state::<AppState>();
            let Ok(mut runtime) = state.database.lock() else {
                continue;
            };
            let Some(database) = runtime.database.as_mut() else {
                continue;
            };
            if delivered {
                let _ = database.mark_reminder_delivered(&reminder);
            } else {
                let _ = database.mark_reminder_error(&reminder, "notification_delivery_failed");
            }
        }
    }
}

fn reminder_body(reminder: &DueReminder) -> String {
    let formatted = chrono::DateTime::parse_from_rfc3339(&reminder.start_at_utc)
        .ok()
        .and_then(|start| {
            reminder
                .time_zone
                .parse::<chrono_tz::Tz>()
                .ok()
                .map(|zone| {
                    start
                        .with_timezone(&zone)
                        .format("%A at %-I:%M %p")
                        .to_string()
                })
        })
        .unwrap_or_else(|| reminder.start_at_utc.clone());
    format!("Starts {formatted}")
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show DayPlan", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit DayPlan", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("DayPlan — reminders stay active while this icon is running")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(tauri_plugin_log::log::LevelFilter::Info)
                .max_file_size(512 * 1024)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(5))
                .target(
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("dayplan".into()),
                    })
                    .filter(|metadata| metadata.target().starts_with("dayplan_desktop")),
                )
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(option_env!("DAYPLAN_UPDATER_PUBKEY").unwrap_or(""))
                .build(),
        )
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let directory = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let resource_directory = app
                .path()
                .resource_dir()
                .map_err(|error| error.to_string())?;
            let ollama = OllamaRuntimeManager::new(resource_directory, directory.clone())
                .map_err(|error| error.to_string())?;
            let agent = PlannerAgent::new(ollama.endpoint(), agent::MODEL_NAME);
            app.manage(AppState {
                database: Mutex::new(DatabaseRuntime::new(directory.join("dayplan.sqlite3"))),
                agent,
                ollama,
                pending_import: Mutex::new(None),
            });
            tauri_plugin_log::log::info!("app_started");
            install_tray(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(reminder_worker(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_agenda,
            list_week,
            database_status,
            list_people,
            create_person,
            update_person,
            delete_person,
            create_workstream,
            update_workstream,
            delete_workstream,
            list_plans,
            get_plan_workspace,
            create_plan,
            update_plan,
            delete_plan,
            create_milestone,
            update_milestone,
            delete_milestone,
            create_event,
            update_event,
            delete_event,
            reschedule_event,
            create_task,
            update_task,
            delete_task,
            list_inbox_items,
            create_inbox_item,
            update_inbox_item,
            delete_inbox_item,
            process_inbox_item,
            get_planning_board,
            move_tasks,
            resolve_local_datetime,
            export_planner_file,
            select_planner_import,
            apply_selected_import,
            discard_selected_import,
            restore_database_backup,
            current_ollama_status,
            download_ollama_model,
            cancel_ollama_model_download,
            restart_ollama_runtime,
            remove_ollama_model,
            export_diagnostic_bundle,
            propose_schedule_changes,
            apply_schedule_changes,
            discard_schedule_proposal,
            cancel_planner_request,
            clear_planner_context,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DayPlan desktop");
}
