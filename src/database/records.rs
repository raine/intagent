use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use rig_core::completion::Usage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database actor is unavailable")]
    ActorClosed,
    #[error("database actor stopped before replying")]
    ActorStopped,
    #[error("database schema version {found} is newer than supported version {supported}")]
    FutureSchema { found: i64, supported: usize },
    #[error(
        "database schema migrations must be contiguous from version 1; found {found} at position {position}"
    )]
    MigrationGap { found: i64, position: usize },
    #[error("invalid database value: {0}")]
    InvalidValue(String),
    #[error("unknown event {0}")]
    UnknownEvent(i64),
    #[error("unknown or closed telemetry span")]
    UnknownSpan,
    #[error("database JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("another intake watch or check owns the queue for {database}")]
    QueueOwnerBusy { database: PathBuf },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Pending,
    Processing,
    Retryable,
    Succeeded,
    Failed,
    Ignored,
}

impl EventStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Retryable => "retryable",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Ignored => "ignored",
        }
    }

    pub(super) fn parse(value: String) -> Result<Self, DatabaseError> {
        match value.as_str() {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "retryable" => Ok(Self::Retryable),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "ignored" => Ok(Self::Ignored),
            _ => Err(DatabaseError::InvalidValue(format!(
                "unknown event status {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRecord {
    pub id: i64,
    pub source: String,
    pub entity_id: String,
    pub revision_id: String,
    pub kind: String,
    pub title: String,
    pub payload: Option<String>,
    pub operational_metadata: String,
    pub occurred_at: String,
    pub observed_at: String,
    pub status: EventStatus,
    pub attempt_count: u32,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub aven_ref: Option<String>,
    pub investigation_handle: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub source: String,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunId(pub i64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TurnId(pub i64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolId(pub i64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryId(pub i64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactionId(pub i64);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportedUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

pub fn reported_usage(usage: Option<&Usage>, provider_reported: bool) -> Option<ReportedUsage> {
    let usage = usage?;
    if !provider_reported && !usage.has_values() {
        return None;
    }
    Some(ReportedUsage {
        input_tokens: Some(saturating_i64(usage.input_tokens)),
        output_tokens: Some(saturating_i64(usage.output_tokens)),
        cache_read_tokens: Some(saturating_i64(usage.cached_input_tokens)),
        cache_write_tokens: Some(saturating_i64(usage.cache_creation_input_tokens)),
        reasoning_tokens: Some(saturating_i64(usage.reasoning_tokens)),
        total_tokens: Some(saturating_i64(usage.total_tokens)),
    })
}

#[derive(Clone, Debug, Default)]
pub struct RunMetadata {
    pub model_id: Option<String>,
    pub model_provider: Option<String>,
    pub thinking_level: Option<String>,
    pub context_window: Option<i64>,
    pub max_tokens: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Succeeded,
    Failed,
    Interrupted,
}

impl RunOutcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanOutcome {
    Succeeded,
    Failed,
    Aborted,
    Interrupted,
}

impl SpanOutcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    Authentication,
    RateLimit,
    Timeout,
    Connection,
    NotFound,
    ModelUnavailable,
    ContextLimit,
    TurnLimit,
    Aborted,
    Interrupted,
    ToolFailure,
    Unknown,
}

impl ErrorCategory {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::NotFound => "not_found",
            Self::ModelUnavailable => "model_unavailable",
            Self::ContextLimit => "context_limit",
            Self::TurnLimit => "turn_limit",
            Self::Aborted => "aborted",
            Self::Interrupted => "interrupted",
            Self::ToolFailure => "tool_failure",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TurnFinish {
    pub stop_reason: Option<String>,
    pub usage: Option<ReportedUsage>,
    pub context_tokens: Option<i64>,
    pub context_window: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct RetryStart {
    pub attempt: u32,
    pub max_attempts: u32,
    pub delay_ms: u64,
    pub error_category: Option<ErrorCategory>,
}

#[derive(Clone, Debug)]
pub struct CompactionFinish {
    pub outcome: SpanOutcome,
    pub aborted: bool,
    pub will_retry: bool,
    pub tokens_before: Option<i64>,
    pub estimated_tokens_after: Option<i64>,
    pub usage: Option<ReportedUsage>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageDecision {
    ActionTaken,
    NoAction,
    NeedsFollowUp,
    Blocked,
    Failed,
    Canceled,
    TimedOut,
    TurnLimit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConclusionSource {
    Model,
    Derived,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchTrigger {
    Initial,
    Revision,
    BackoffRetry,
    RecoveryRetry,
    OperatorRetry,
    ManualInjection,
    SupersedingClaim,
    Unknown,
}

impl DispatchTrigger {
    pub(super) fn parse(value: &str) -> Result<Self, DatabaseError> {
        match value {
            "initial" => Ok(Self::Initial),
            "revision" => Ok(Self::Revision),
            "backoff_retry" => Ok(Self::BackoffRetry),
            "recovery_retry" => Ok(Self::RecoveryRetry),
            "operator_retry" => Ok(Self::OperatorRetry),
            "manual_injection" => Ok(Self::ManualInjection),
            "superseding_claim" => Ok(Self::SupersedingClaim),
            "unknown" => Ok(Self::Unknown),
            _ => Err(DatabaseError::InvalidValue(format!(
                "unknown dispatch trigger {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageConclusion {
    pub decision: TriageDecision,
    pub summary: String,
    pub evidence: Vec<String>,
    pub actions: Vec<String>,
    pub outcome: String,
    pub follow_up: Option<String>,
    pub source: ConclusionSource,
}

#[derive(Clone, Debug)]
pub struct RunFinish {
    pub outcome: RunOutcome,
    pub termination_reason: String,
    pub failure_category: Option<ErrorCategory>,
    pub recording_complete: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageRunRecord {
    pub id: i64,
    pub event_id: i64,
    pub attempt: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_activity_at: String,
    pub outcome: Option<String>,
    pub termination_reason: Option<String>,
    pub failure_category: Option<String>,
    pub model_id: Option<String>,
    pub model_provider: Option<String>,
    pub thinking_level: Option<String>,
    pub context_window: Option<i64>,
    pub max_tokens: Option<i64>,
    pub telemetry_version: Option<i64>,
    pub telemetry_completeness: String,
    pub dispatch_sequence: Option<u32>,
    pub dispatch_trigger: Option<DispatchTrigger>,
    pub dispatch_prior_run_id: Option<i64>,
    pub dispatch_scheduled_for: Option<String>,
    pub conclusion: Option<TriageConclusion>,
    pub turn_count: u32,
    pub retry_count: u32,
    pub compaction_count: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageStepRecord {
    pub id: i64,
    pub turn_id: Option<i64>,
    pub turn_ordinal: Option<i64>,
    pub kind: String,
    pub label: String,
    pub summary: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriageRunSummary {
    pub run: TriageRunRecord,
    pub step_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriageTurnRecord {
    pub id: i64,
    pub ordinal: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost: Option<f64>,
    pub context_tokens: Option<i64>,
    pub context_window: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriageRetryRecord {
    pub id: i64,
    pub turn_id: Option<i64>,
    pub attempt: i64,
    pub max_attempts: i64,
    pub delay_ms: i64,
    pub started_at: String,
    pub wait_ended_at: String,
    pub ended_at: Option<String>,
    pub outcome: Option<String>,
    pub error_category: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriageCompactionRecord {
    pub id: i64,
    pub turn_id: Option<i64>,
    pub reason: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: Option<String>,
    pub aborted: Option<bool>,
    pub will_retry: Option<bool>,
    pub tokens_before: Option<i64>,
    pub estimated_tokens_after: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriageRunPromptRecord {
    pub role: String,
    pub content: String,
    pub recorded_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriageEffectRecord {
    pub effect_type: String,
    pub value: String,
    pub recorded_at: String,
}

pub fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(super) fn saturating_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}
