use crate::model::CalendarProblem;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Internal(String),
    #[error("{0}")]
    Validation(String),
    #[error("The requested record no longer exists.")]
    NotFound,
    #[error("This item changed after it was loaded or proposed. Review the latest version and try again.")]
    Conflict,
    #[error("DayPlan's local AI runtime is unavailable. Restart it from Settings and try again.")]
    OllamaUnavailable,
    #[error("DayPlan's bundled AI runtime is missing or incompatible: {0}")]
    OllamaRuntime(String),
    #[error("The local model download was cancelled.")]
    ModelDownloadCancelled,
    #[error("Ollama returned an invalid planner response: {0}")]
    InvalidModelResponse(String),
    #[error("The local DayPlan database did not pass its integrity check. Restore a backup from Settings.")]
    CorruptDatabase,
    #[error("This DayPlan database was created by a newer app version.")]
    UnsupportedDatabaseVersion,
    #[error("The selected backup is not available.")]
    BackupNotFound,
    #[error("That schedule proposal is unavailable or has already been used.")]
    ProposalUnavailable,
    #[error("That schedule proposal expired. Ask DayPlan to prepare it again.")]
    ProposalExpired,
    #[error("The planner request was cancelled.")]
    RequestCancelled,
    #[error("{}", .0.message())]
    Calendar(CalendarProblem),
    #[error("DayPlan couldn't use the system keychain for this calendar's link. Unlock the keychain or allow access, then try again.")]
    Keychain,
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl CommandError {
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "internal",
            message: message.into(),
            retryable: true,
            details: None,
        }
    }
}

impl From<AppError> for CommandError {
    fn from(error: AppError) -> Self {
        let (code, retryable) = match &error {
            AppError::Internal(_) => ("internal", true),
            AppError::Validation(_) => ("validation", false),
            AppError::NotFound => ("not_found", false),
            AppError::Conflict => ("conflict", true),
            AppError::OllamaUnavailable => ("ollama_unavailable", true),
            AppError::OllamaRuntime(_) => ("ollama_runtime", true),
            AppError::ModelDownloadCancelled => ("model_download_cancelled", true),
            AppError::InvalidModelResponse(_) => ("invalid_model_response", true),
            AppError::CorruptDatabase => ("corrupt_database", false),
            AppError::UnsupportedDatabaseVersion => ("unsupported_database_version", false),
            AppError::BackupNotFound => ("backup_not_found", false),
            AppError::ProposalUnavailable => ("proposal_unavailable", false),
            AppError::ProposalExpired => ("proposal_expired", true),
            AppError::RequestCancelled => ("request_cancelled", true),
            AppError::Calendar(problem) => ("calendar", problem.retryable()),
            AppError::Keychain => ("keychain", true),
            AppError::Database(_) | AppError::Json(_) | AppError::Io(_) => ("storage", true),
            AppError::Network(_) => ("network", true),
        };
        Self {
            code,
            message: error.to_string(),
            retryable,
            details: None,
        }
    }
}
