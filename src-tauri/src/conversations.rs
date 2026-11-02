//! Commands for the Assistant's saved conversations: list them, open one, rename it, and delete
//! it. New conversations start with their first request (`run_assistant` without a thread id),
//! and messages are added by `run_assistant` itself, never by the screen.

use super::{with_database, AppState};
use crate::db::{AgentThread, AgentThreadDetail};
use crate::error::CommandError;
use tauri::State;

#[tauri::command]
pub(crate) fn list_assistant_threads(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<AgentThread>, CommandError> {
    with_database(&state, |database| database.agent_threads(limit))
}

#[tauri::command]
pub(crate) fn get_assistant_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<AgentThreadDetail, CommandError> {
    with_database(&state, |database| database.agent_thread(&thread_id))
}

#[tauri::command]
pub(crate) fn rename_assistant_thread(
    state: State<'_, AppState>,
    thread_id: String,
    title: String,
) -> Result<AgentThread, CommandError> {
    with_database(&state, |database| {
        database.rename_agent_thread(&thread_id, &title)
    })
}

/// Deletes a conversation with its messages and the runs it started, and forgets those runs on
/// screen too.
#[tauri::command]
pub(crate) fn delete_assistant_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), CommandError> {
    with_database(&state, |database| database.delete_agent_thread(&thread_id))?;
    state.assistant.forget_thread(&thread_id);
    Ok(())
}
