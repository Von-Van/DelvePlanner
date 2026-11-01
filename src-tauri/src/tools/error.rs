//! The error taxonomy every read tool reports. An agent sees a code it can branch on, a message
//! safe to show, and whether trying the same call again could help.

use crate::error::AppError;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    /// The arguments don't match the tool's input schema or name an impossible value.
    InvalidArguments,
    /// A record the call names doesn't exist.
    NotFound,
    /// The call matches more than one record and the tool won't guess.
    Ambiguous,
    /// The tool exists but this run isn't allowed to use it.
    PermissionDenied,
    /// The call took longer than its time budget; its result was discarded.
    Timeout,
    /// A source the tool reads is unavailable, or the tool is reserved for later.
    Unavailable,
    /// The run used up its tool budget, the result is too large, or the same failing call
    /// was repeated too often.
    BudgetExhausted,
    /// Something went wrong inside the broker.
    Internal,
}

impl ToolErrorCode {
    pub const ALL: [ToolErrorCode; 8] = [
        Self::InvalidArguments,
        Self::NotFound,
        Self::Ambiguous,
        Self::PermissionDenied,
        Self::Timeout,
        Self::Unavailable,
        Self::BudgetExhausted,
        Self::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::PermissionDenied => "permission_denied",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Internal => "internal",
        }
    }

    /// Whether repeating the identical call could succeed.
    pub fn retryable(self) -> bool {
        matches!(self, Self::Timeout | Self::Unavailable | Self::Internal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ToolError {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::InvalidArguments, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::NotFound, message)
    }

    pub fn budget(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::BudgetExhausted, message)
    }
}

impl From<AppError> for ToolError {
    /// Planner errors become tool errors without leaking storage details to the model.
    fn from(error: AppError) -> Self {
        match error {
            AppError::Validation(message) => Self::invalid(message),
            AppError::NotFound => Self::not_found("The requested record does not exist."),
            AppError::Calendar(_) | AppError::Keychain | AppError::Network(_) => Self::new(
                ToolErrorCode::Unavailable,
                "Calendar data is unavailable right now.",
            ),
            AppError::CorruptDatabase
            | AppError::UnsupportedDatabaseVersion
            | AppError::Database(_)
            | AppError::Io(_) => Self::new(
                ToolErrorCode::Unavailable,
                "Planner data is unavailable right now.",
            ),
            _ => Self::new(ToolErrorCode::Internal, "The tool failed unexpectedly."),
        }
    }
}
