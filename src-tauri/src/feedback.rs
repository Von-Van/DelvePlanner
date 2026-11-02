//! Commands for feedback on the assistant's replies (milestone M6): "helpful" or "not helpful
//! because …", then, for a correction, "just this once" or "remember for next time". The
//! assistant never calls these. A remembered lesson is an ordinary memory the person stated, so
//! Settings › Memory shows, corrects and deletes it like any other.

use super::{with_database, AppState};
use crate::db::{propose_lesson, AgentFeedback, LessonProposal, MemoryKind, NewFeedback};
use crate::error::CommandError;
use serde::Serialize;
use tauri::State;

/// Feedback as just recorded, with the lesson to offer when it is a correction with a reason.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackReceipt {
    pub feedback: AgentFeedback,
    pub lesson: Option<LessonProposal>,
}

/// One run's feedback, each with the lesson still on offer while the person hasn't chosen.
pub fn receipts(feedback: Vec<AgentFeedback>) -> Vec<FeedbackReceipt> {
    feedback
        .into_iter()
        .map(|feedback| FeedbackReceipt {
            lesson: match feedback.scope {
                crate::db::FeedbackScope::Pending => {
                    feedback.reason.as_deref().and_then(propose_lesson)
                }
                _ => None,
            },
            feedback,
        })
        .collect()
}

#[tauri::command]
pub(crate) fn record_assistant_feedback(
    state: State<'_, AppState>,
    input: NewFeedback,
) -> Result<FeedbackReceipt, CommandError> {
    let feedback = with_database(&state, |database| database.record_agent_feedback(input))?;
    Ok(receipts(vec![feedback]).remove(0))
}

#[tauri::command]
pub(crate) fn list_assistant_feedback(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<Vec<FeedbackReceipt>, CommandError> {
    let feedback = with_database(&state, |database| database.run_feedback(&run_id))?;
    Ok(receipts(feedback))
}

/// "Remember for next time", with the lesson in the words the person confirmed.
#[tauri::command]
pub(crate) fn learn_from_feedback(
    state: State<'_, AppState>,
    feedback_id: String,
    kind: MemoryKind,
    content: String,
) -> Result<FeedbackReceipt, CommandError> {
    let feedback = with_database(&state, |database| {
        database.learn_from_feedback(&feedback_id, kind, &content)
    })?;
    Ok(receipts(vec![feedback]).remove(0))
}

/// "Just this once": nothing is remembered.
#[tauri::command]
pub(crate) fn keep_feedback_one_off(
    state: State<'_, AppState>,
    feedback_id: String,
) -> Result<FeedbackReceipt, CommandError> {
    let feedback = with_database(&state, |database| {
        database.keep_feedback_one_off(&feedback_id)
    })?;
    Ok(receipts(vec![feedback]).remove(0))
}
