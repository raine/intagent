use rig_agent::completion::PromptError;
use rig_core::completion::CompletionError;

use crate::database::ErrorCategory;
use crate::errors::classify_message;

use super::context::ProjectionError;
use super::driver::{EngineError, is_context_limit};
use super::telemetry::PrototypeObserverError;

#[derive(Debug, thiserror::Error)]
pub enum TriageError {
    #[error("event content is unavailable for triage")]
    UnavailableEvent,
    #[error("skill validation failed: {0}")]
    SkillValidation(String),
    #[error("triage configuration failed: {0}")]
    Configuration(String),
    #[error("triage exceeded its wall-clock limit")]
    WallTimeout,
    #[error("triage was canceled")]
    Cancelled,
    #[error("triage telemetry recording failed")]
    RecordingFailure,
    #[error("context limit: {0}")]
    ContextLimit(String),
    #[error("history has no safe compaction boundary")]
    NoSafeCompactionBoundary,
    #[error(transparent)]
    Driver(DriverError),
    #[error("provider completion failed: {0}")]
    Completion(CompletionError),
    #[error(transparent)]
    Prompt(#[from] PromptError),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error(transparent)]
    Database(#[from] crate::database::DatabaseError),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl TriageError {
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::WallTimeout => ErrorCategory::Timeout,
            Self::Cancelled => ErrorCategory::Aborted,
            Self::ContextLimit(_) => ErrorCategory::ContextLimit,
            Self::Completion(error) => completion_category(error),
            Self::Prompt(PromptError::MaxTurnsError { .. }) => ErrorCategory::TurnLimit,
            Self::Other(error) => match classify_message(&error.to_string()) {
                category @ (ErrorCategory::Authentication | ErrorCategory::NotFound) => category,
                _ => ErrorCategory::Unknown,
            },
            _ => ErrorCategory::Unknown,
        }
    }

    pub(crate) fn termination_reason(&self) -> &'static str {
        match self.category() {
            ErrorCategory::Timeout => "wall_timeout",
            ErrorCategory::TurnLimit => "turn_limit",
            ErrorCategory::Aborted => "aborted",
            ErrorCategory::ContextLimit => "context_limit",
            ErrorCategory::ModelUnavailable
            | ErrorCategory::Authentication
            | ErrorCategory::RateLimit
            | ErrorCategory::Connection
            | ErrorCategory::NotFound => "model_error",
            _ => "failed",
        }
    }
}

pub(crate) fn completion_category(error: &CompletionError) -> ErrorCategory {
    match error
        .provider_response_status()
        .map(|status| status.as_u16())
    {
        Some(401 | 403) => ErrorCategory::Authentication,
        Some(404) => ErrorCategory::NotFound,
        Some(408 | 504) => ErrorCategory::Timeout,
        Some(429) => ErrorCategory::RateLimit,
        _ if is_context_limit(error) => ErrorCategory::ContextLimit,
        _ => classify_message(&error.to_string()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("agent run failed: {0}")]
    Prompt(#[from] PromptError),
    #[error("provider completion failed: {0}")]
    Completion(CompletionError),
    #[error("context limit: {0}")]
    ContextLimit(String),
    #[error("agent run was canceled")]
    Cancelled,
    #[error("history projection failed: {0}")]
    Projection(#[from] ProjectionError),
    #[error("telemetry database failed: {0}")]
    Telemetry(#[from] rusqlite::Error),
    #[error("agent state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("history has no safe compaction boundary")]
    NoSafeCompactionBoundary,
    #[error("compaction response contained no summary text")]
    EmptyCompactionSummary,
    #[error("tool telemetry recording failed")]
    RecordingFailure,
}

impl From<EngineError<PrototypeObserverError>> for DriverError {
    fn from(error: EngineError<PrototypeObserverError>) -> Self {
        match error {
            EngineError::Prompt(error) => Self::Prompt(error),
            EngineError::Completion(error) => Self::Completion(error),
            EngineError::ContextLimit(error) => Self::ContextLimit(error),
            EngineError::Cancelled => Self::Cancelled,
            EngineError::Projection(error) => Self::Projection(error),
            EngineError::Serialization(error) => Self::Serialization(error),
            EngineError::Observer(PrototypeObserverError::Telemetry(error)) => {
                Self::Telemetry(error)
            }
            EngineError::Observer(PrototypeObserverError::Serialization(error)) => {
                Self::Serialization(error)
            }
            EngineError::RecordingFailure => Self::RecordingFailure,
            EngineError::NoSafeCompactionBoundary => Self::NoSafeCompactionBoundary,
            EngineError::EmptyCompactionSummary => Self::EmptyCompactionSummary,
        }
    }
}
