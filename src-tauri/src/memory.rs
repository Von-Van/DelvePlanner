//! Settings commands for the assistant's memory: see what is remembered, confirm, correct,
//! delete, export, pause it, and switch it off per project. The assistant itself never calls
//! these; it only reads memory through the `get_memory` tool.

use super::{with_database, AppState, FileActionResult};
use crate::db::{
    MemoryExport, MemoryFilter, MemoryKind, MemoryRecord, MemorySettings, MemorySource, NewMemory,
    UpdateMemorySettings,
};
use crate::error::{AppError, CommandError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

/// Everything the Memory settings panel shows.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryOverview {
    pub settings: MemorySettings,
    /// Current and historical (superseded) records, newest first. Expired inferences are
    /// forgotten before the list is read.
    pub records: Vec<MemoryRecord>,
}

/// A memory the person types in Settings. It is always recorded as something they stated.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMemoryInput {
    pub kind: MemoryKind,
    pub content: String,
    pub plan_id: Option<String>,
}

#[tauri::command]
pub(crate) fn get_memory_overview(
    state: State<'_, AppState>,
) -> Result<MemoryOverview, CommandError> {
    with_database(&state, |database| {
        database.prune_expired_memory()?;
        Ok(MemoryOverview {
            settings: database.memory_settings()?,
            records: database.list_memory(MemoryFilter {
                plan_id: None,
                include_history: true,
            })?,
        })
    })
}

#[tauri::command]
pub(crate) fn update_memory_settings(
    state: State<'_, AppState>,
    input: UpdateMemorySettings,
) -> Result<MemorySettings, CommandError> {
    with_database(&state, |database| database.update_memory_settings(input))
}

#[tauri::command]
pub(crate) fn set_project_memory(
    state: State<'_, AppState>,
    plan_id: String,
    enabled: bool,
) -> Result<MemorySettings, CommandError> {
    with_database(&state, |database| {
        database.set_project_memory(&plan_id, enabled)
    })
}

#[tauri::command]
pub(crate) fn add_memory(
    state: State<'_, AppState>,
    input: AddMemoryInput,
) -> Result<MemoryRecord, CommandError> {
    with_database(&state, |database| {
        database.add_memory(NewMemory {
            kind: input.kind,
            content: input.content,
            source: MemorySource::UserStated,
            source_ref: None,
            plan_id: input.plan_id,
        })
    })
}

#[tauri::command]
pub(crate) fn confirm_memory(
    state: State<'_, AppState>,
    id: String,
) -> Result<MemoryRecord, CommandError> {
    with_database(&state, |database| database.confirm_memory(&id))
}

#[tauri::command]
pub(crate) fn correct_memory(
    state: State<'_, AppState>,
    id: String,
    content: String,
) -> Result<MemoryRecord, CommandError> {
    with_database(&state, |database| database.correct_memory(&id, &content))
}

#[tauri::command]
pub(crate) fn delete_memory(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_memory(&id))
}

#[tauri::command]
pub(crate) fn forget_project_memory(
    state: State<'_, AppState>,
    plan_id: String,
) -> Result<usize, CommandError> {
    with_database(&state, |database| database.forget_project_memory(&plan_id))
}

#[tauri::command]
pub(crate) fn clear_memory(state: State<'_, AppState>) -> Result<usize, CommandError> {
    with_database(&state, |database| database.clear_memory())
}

#[tauri::command]
pub(crate) async fn export_memory_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<FileActionResult, CommandError> {
    let export: MemoryExport = with_database(&state, |database| database.export_memory())?;
    let path = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Export assistant memory")
            .set_file_name("delve-memory-export.json")
            .add_filter("JSON", &["json"])
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
    let bytes = serde_json::to_vec_pretty(&export)
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
