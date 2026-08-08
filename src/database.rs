use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rig_core::completion::Usage;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params, types::Type};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{IntakeItem, IntakeItemKind};

pub const DATABASE_QUEUE_CAPACITY: usize = 64;
pub const DATABASE_READER_COUNT: usize = 2;
pub const SCHEMA_VERSION: usize = MIGRATIONS.len();

pub const MIGRATIONS: [&str; 8] = [
    include_str!("migrations/001-initial.sql"),
    include_str!("migrations/002-global-entity-identity.sql"),
    include_str!("migrations/003-triage-runs.sql"),
    include_str!("migrations/004-detailed-telemetry.sql"),
    include_str!("migrations/005-redact-legacy-command-events.sql"),
    include_str!("migrations/006-step-summaries.sql"),
    include_str!("migrations/007-run-prompts.sql"),
    include_str!("migrations/008-triage-conclusions.sql"),
];

static MEMORY_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

type Reply<T> = oneshot::Sender<Result<T, DatabaseError>>;

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

#[derive(Debug)]
pub struct QueueOwnerLock {
    file: File,
    database: PathBuf,
}

impl QueueOwnerLock {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let database = canonical_database_identity(path.as_ref())?;
        let file_name = database
            .file_name()
            .ok_or_else(|| DatabaseError::InvalidValue("database path has no file name".into()))?;
        let mut lock_name = file_name.to_os_string();
        lock_name.push(".queue-owner.lock");
        let lock_path = database.with_file_name(lock_name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(lock_path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK)
                || error.raw_os_error() == Some(libc::EAGAIN)
            {
                return Err(DatabaseError::QueueOwnerBusy { database });
            }
            return Err(DatabaseError::Io(error));
        }
        Ok(Self { file, database })
    }

    pub fn database(&self) -> &Path {
        &self.database
    }
}

impl Drop for QueueOwnerLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn canonical_database_identity(path: &Path) -> Result<PathBuf, DatabaseError> {
    if path == Path::new(":memory:") {
        return Err(DatabaseError::InvalidValue(
            "queue ownership requires a file-backed database".into(),
        ));
    }
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }
    let parent = path
        .parent()
        .ok_or_else(|| DatabaseError::InvalidValue("database path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let parent = std::fs::canonicalize(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| DatabaseError::InvalidValue("database path has no file name".into()))?;
    Ok(parent.join(name))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    fn parse(value: String) -> Result<Self, DatabaseError> {
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
    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConclusionSource {
    Model,
    Derived,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    pub dispatch_reason: Option<String>,
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

#[derive(Clone, Debug)]
struct OpenTarget {
    value: String,
    flags: OpenFlags,
    directory: Option<PathBuf>,
}

impl OpenTarget {
    fn new(path: &Path) -> Self {
        if path == Path::new(":memory:") {
            let id = MEMORY_DATABASE_ID.fetch_add(1, Ordering::Relaxed);
            return Self {
                value: format!("file:intake-memory-{id}?mode=memory&cache=shared"),
                flags: OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                directory: None,
            };
        }
        Self {
            value: path.to_string_lossy().into_owned(),
            flags: OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            directory: path.parent().map(Path::to_path_buf),
        }
    }

    fn read_flags(&self) -> OpenFlags {
        if self.flags.contains(OpenFlags::SQLITE_OPEN_URI) {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
        }
    }
}

#[derive(Clone)]
pub struct IntakeDatabase {
    sender: mpsc::Sender<WriteOperation>,
    readers: DatabaseReaders,
}

impl IntakeDatabase {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let target = OpenTarget::new(path.as_ref());
        if let Some(directory) = &target.directory {
            std::fs::create_dir_all(directory)?;
        }
        let (sender, receiver) = mpsc::channel(DATABASE_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let actor_target = target.clone();
        thread::Builder::new()
            .name("intake-database".into())
            .spawn(move || write_actor(actor_target, receiver, ready_tx))?;
        ready_rx.await.map_err(|_| DatabaseError::ActorStopped)??;
        let readers = DatabaseReaders::open(target).await?;
        Ok(Self { sender, readers })
    }

    pub fn readers(&self) -> DatabaseReaders {
        self.readers.clone()
    }

    pub async fn source_succeeded(
        &self,
        source: String,
        checkpoint: Value,
        items: Vec<IntakeItem>,
        observed_at: DateTime<Utc>,
    ) -> Result<usize, DatabaseError> {
        self.request(|reply| WriteOperation::SourceSucceeded {
            source,
            checkpoint,
            items,
            observed_at: timestamp(observed_at),
            reply,
        })
        .await
    }

    pub async fn source_failed(
        &self,
        source: String,
        error: String,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::SourceFailed {
            source,
            error,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn claim_next(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<EventRecord>, DatabaseError> {
        self.request(|reply| WriteOperation::ClaimNext {
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn recover_interrupted(
        &self,
        stale_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize, DatabaseError> {
        self.request(|reply| WriteOperation::RecoverInterrupted {
            stale_before: timestamp(stale_before),
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn start_triage_run(
        &self,
        event_id: i64,
        attempt: u32,
        now: DateTime<Utc>,
    ) -> Result<RunId, DatabaseError> {
        self.start_triage_run_with_dispatch_reason(event_id, attempt, None, now)
            .await
    }

    pub async fn start_triage_run_with_dispatch_reason(
        &self,
        event_id: i64,
        attempt: u32,
        dispatch_reason: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<RunId, DatabaseError> {
        self.request(|reply| WriteOperation::StartRun {
            event_id,
            attempt,
            dispatch_reason,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn set_triage_run_metadata(
        &self,
        run_id: RunId,
        metadata: RunMetadata,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::SetRunMetadata {
            run_id,
            metadata,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn record_triage_run_prompt(
        &self,
        run_id: RunId,
        role: String,
        content: String,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::RecordPrompt {
            run_id,
            role,
            content,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn start_turn(
        &self,
        run_id: RunId,
        now: DateTime<Utc>,
    ) -> Result<TurnId, DatabaseError> {
        self.request(|reply| WriteOperation::StartTurn {
            run_id,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn finish_turn(
        &self,
        run_id: RunId,
        turn_id: TurnId,
        finish: TurnFinish,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::FinishTurn {
            run_id,
            turn_id,
            finish,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn start_tool(
        &self,
        run_id: RunId,
        turn_id: Option<TurnId>,
        name: String,
        summary: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<ToolId, DatabaseError> {
        self.request(|reply| WriteOperation::StartTool {
            run_id,
            turn_id,
            name,
            summary,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn finish_tool(
        &self,
        run_id: RunId,
        tool_id: ToolId,
        outcome: SpanOutcome,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::FinishTool {
            run_id,
            tool_id,
            outcome,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn start_retry(
        &self,
        run_id: RunId,
        turn_id: Option<TurnId>,
        retry: RetryStart,
        now: DateTime<Utc>,
    ) -> Result<RetryId, DatabaseError> {
        self.request(|reply| WriteOperation::StartRetry {
            run_id,
            turn_id,
            retry,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn finish_retry(
        &self,
        run_id: RunId,
        retry_id: RetryId,
        outcome: SpanOutcome,
        error_category: Option<ErrorCategory>,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::FinishRetry {
            run_id,
            retry_id,
            outcome,
            error_category,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn start_compaction(
        &self,
        run_id: RunId,
        turn_id: Option<TurnId>,
        reason: String,
        now: DateTime<Utc>,
    ) -> Result<CompactionId, DatabaseError> {
        self.request(|reply| WriteOperation::StartCompaction {
            run_id,
            turn_id,
            reason,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn finish_compaction(
        &self,
        run_id: RunId,
        compaction_id: CompactionId,
        finish: CompactionFinish,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::FinishCompaction {
            run_id,
            compaction_id,
            finish,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn record_assistant_text(
        &self,
        run_id: RunId,
        turn_id: Option<TurnId>,
        text: String,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::RecordAssistantText {
            run_id,
            turn_id,
            text,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn record_reasoning(
        &self,
        run_id: RunId,
        turn_id: Option<TurnId>,
        summary: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::RecordReasoning {
            run_id,
            turn_id,
            summary,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn finish_triage_run(
        &self,
        run_id: RunId,
        finish: RunFinish,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.finish_triage_run_with_conclusion(run_id, finish, None, now)
            .await
    }

    pub async fn finish_triage_run_with_conclusion(
        &self,
        run_id: RunId,
        finish: RunFinish,
        conclusion: Option<TriageConclusion>,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::FinishRun {
            run_id,
            finish,
            conclusion,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn succeed(&self, event_id: i64, now: DateTime<Utc>) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::Succeed {
            event_id,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn fail(
        &self,
        event_id: i64,
        error: String,
        max_attempts: u32,
        retry_base_seconds: u64,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::Fail {
            event_id,
            error,
            max_attempts,
            retry_base_seconds,
            now,
            reply,
        })
        .await
    }

    pub async fn retry(&self, event_id: i64, now: DateTime<Utc>) -> Result<bool, DatabaseError> {
        self.request(|reply| WriteOperation::Retry {
            event_id,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn ignore(&self, event_id: i64, now: DateTime<Utc>) -> Result<bool, DatabaseError> {
        self.request(|reply| WriteOperation::Ignore {
            event_id,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn record_command(
        &self,
        event_id: i64,
        executable: String,
        exit_code: i32,
        output: String,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        self.request(|reply| WriteOperation::RecordCommand {
            event_id,
            executable,
            exit_code,
            output,
            now: timestamp(now),
            reply,
        })
        .await
    }

    pub async fn flush(&self) -> Result<(), DatabaseError> {
        self.request(WriteOperation::Flush).await
    }

    pub async fn shutdown(&self) -> Result<(), DatabaseError> {
        self.request(WriteOperation::Shutdown).await
    }

    async fn request<T>(
        &self,
        operation: impl FnOnce(Reply<T>) -> WriteOperation,
    ) -> Result<T, DatabaseError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(operation(reply))
            .await
            .map_err(|_| DatabaseError::ActorClosed)?;
        response.await.map_err(|_| DatabaseError::ActorStopped)?
    }
}

#[derive(Clone)]
pub struct DatabaseReaders {
    senders: Vec<mpsc::Sender<ReadOperation>>,
    next: std::sync::Arc<AtomicU64>,
}

impl DatabaseReaders {
    async fn open(target: OpenTarget) -> Result<Self, DatabaseError> {
        let mut senders = Vec::with_capacity(DATABASE_READER_COUNT);
        for index in 0..DATABASE_READER_COUNT {
            let (sender, receiver) = mpsc::channel(DATABASE_QUEUE_CAPACITY);
            let (ready_tx, ready_rx) = oneshot::channel();
            let actor_target = target.clone();
            thread::Builder::new()
                .name(format!("intake-database-reader-{index}"))
                .spawn(move || read_actor(actor_target, receiver, ready_tx))?;
            ready_rx.await.map_err(|_| DatabaseError::ActorStopped)??;
            senders.push(sender);
        }
        Ok(Self {
            senders,
            next: std::sync::Arc::new(AtomicU64::new(0)),
        })
    }

    pub async fn source_checkpoint(&self, source: String) -> Result<Value, DatabaseError> {
        self.request(|reply| ReadOperation::SourceCheckpoint { source, reply })
            .await
    }

    pub async fn event(&self, id: i64) -> Result<Option<EventRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::Event { id, reply })
            .await
    }

    pub async fn list_events(&self, limit: usize) -> Result<Vec<EventRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::ListEvents { limit, reply })
            .await
    }

    pub async fn oldest_open_event_at(&self) -> Result<Option<String>, DatabaseError> {
        self.request(ReadOperation::OldestOpenEventAt).await
    }

    pub async fn status(&self) -> Result<HashMap<String, usize>, DatabaseError> {
        self.request(ReadOperation::Status).await
    }

    pub async fn source_statuses(&self) -> Result<Vec<SourceStatus>, DatabaseError> {
        self.request(ReadOperation::SourceStatuses).await
    }

    pub async fn triage_run(&self, id: RunId) -> Result<Option<TriageRunRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRun { id, reply })
            .await
    }

    pub async fn triage_run_steps(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageStepRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunSteps { id, reply })
            .await
    }

    pub async fn list_triage_run_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<TriageRunSummary>, DatabaseError> {
        self.request(|reply| ReadOperation::ListTriageRunSummaries { limit, reply })
            .await
    }

    pub async fn recent_triage_run_steps(
        &self,
        id: RunId,
        limit: usize,
    ) -> Result<Vec<TriageStepRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::RecentTriageRunSteps { id, limit, reply })
            .await
    }

    pub async fn triage_runs_for_event(
        &self,
        event_id: i64,
    ) -> Result<Vec<TriageRunRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunsForEvent { event_id, reply })
            .await
    }

    pub async fn triage_run_turns(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageTurnRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunTurns { id, reply })
            .await
    }

    pub async fn triage_run_retries(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageRetryRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunRetries { id, reply })
            .await
    }

    pub async fn triage_run_compactions(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageCompactionRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunCompactions { id, reply })
            .await
    }

    pub async fn triage_run_prompts(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageRunPromptRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunPrompts { id, reply })
            .await
    }

    pub async fn triage_run_effects(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageEffectRecord>, DatabaseError> {
        self.request(|reply| ReadOperation::TriageRunEffects { id, reply })
            .await
    }

    pub async fn integrity_check(&self) -> Result<String, DatabaseError> {
        self.request(ReadOperation::IntegrityCheck).await
    }

    async fn request<T>(
        &self,
        operation: impl FnOnce(Reply<T>) -> ReadOperation,
    ) -> Result<T, DatabaseError> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) as usize % self.senders.len();
        let (reply, response) = oneshot::channel();
        self.senders[index]
            .send(operation(reply))
            .await
            .map_err(|_| DatabaseError::ActorClosed)?;
        response.await.map_err(|_| DatabaseError::ActorStopped)?
    }
}

enum WriteOperation {
    SourceSucceeded {
        source: String,
        checkpoint: Value,
        items: Vec<IntakeItem>,
        observed_at: String,
        reply: Reply<usize>,
    },
    SourceFailed {
        source: String,
        error: String,
        now: String,
        reply: Reply<()>,
    },
    ClaimNext {
        now: String,
        reply: Reply<Option<EventRecord>>,
    },
    RecoverInterrupted {
        stale_before: String,
        now: String,
        reply: Reply<usize>,
    },
    StartRun {
        event_id: i64,
        attempt: u32,
        dispatch_reason: Option<String>,
        now: String,
        reply: Reply<RunId>,
    },
    SetRunMetadata {
        run_id: RunId,
        metadata: RunMetadata,
        now: String,
        reply: Reply<()>,
    },
    RecordPrompt {
        run_id: RunId,
        role: String,
        content: String,
        now: String,
        reply: Reply<()>,
    },
    StartTurn {
        run_id: RunId,
        now: String,
        reply: Reply<TurnId>,
    },
    FinishTurn {
        run_id: RunId,
        turn_id: TurnId,
        finish: TurnFinish,
        now: String,
        reply: Reply<()>,
    },
    StartTool {
        run_id: RunId,
        turn_id: Option<TurnId>,
        name: String,
        summary: Option<String>,
        now: String,
        reply: Reply<ToolId>,
    },
    FinishTool {
        run_id: RunId,
        tool_id: ToolId,
        outcome: SpanOutcome,
        now: String,
        reply: Reply<()>,
    },
    StartRetry {
        run_id: RunId,
        turn_id: Option<TurnId>,
        retry: RetryStart,
        now: String,
        reply: Reply<RetryId>,
    },
    FinishRetry {
        run_id: RunId,
        retry_id: RetryId,
        outcome: SpanOutcome,
        error_category: Option<ErrorCategory>,
        now: String,
        reply: Reply<()>,
    },
    StartCompaction {
        run_id: RunId,
        turn_id: Option<TurnId>,
        reason: String,
        now: String,
        reply: Reply<CompactionId>,
    },
    FinishCompaction {
        run_id: RunId,
        compaction_id: CompactionId,
        finish: CompactionFinish,
        now: String,
        reply: Reply<()>,
    },
    RecordAssistantText {
        run_id: RunId,
        turn_id: Option<TurnId>,
        text: String,
        now: String,
        reply: Reply<()>,
    },
    RecordReasoning {
        run_id: RunId,
        turn_id: Option<TurnId>,
        summary: Option<String>,
        now: String,
        reply: Reply<()>,
    },
    FinishRun {
        run_id: RunId,
        finish: RunFinish,
        conclusion: Option<TriageConclusion>,
        now: String,
        reply: Reply<()>,
    },
    Succeed {
        event_id: i64,
        now: String,
        reply: Reply<()>,
    },
    Fail {
        event_id: i64,
        error: String,
        max_attempts: u32,
        retry_base_seconds: u64,
        now: DateTime<Utc>,
        reply: Reply<()>,
    },
    Retry {
        event_id: i64,
        now: String,
        reply: Reply<bool>,
    },
    Ignore {
        event_id: i64,
        now: String,
        reply: Reply<bool>,
    },
    RecordCommand {
        event_id: i64,
        executable: String,
        exit_code: i32,
        output: String,
        now: String,
        reply: Reply<()>,
    },
    Flush(Reply<()>),
    Shutdown(Reply<()>),
}

enum ReadOperation {
    SourceCheckpoint {
        source: String,
        reply: Reply<Value>,
    },
    Event {
        id: i64,
        reply: Reply<Option<EventRecord>>,
    },
    ListEvents {
        limit: usize,
        reply: Reply<Vec<EventRecord>>,
    },
    OldestOpenEventAt(Reply<Option<String>>),
    Status(Reply<HashMap<String, usize>>),
    SourceStatuses(Reply<Vec<SourceStatus>>),
    TriageRun {
        id: RunId,
        reply: Reply<Option<TriageRunRecord>>,
    },
    TriageRunSteps {
        id: RunId,
        reply: Reply<Vec<TriageStepRecord>>,
    },
    ListTriageRunSummaries {
        limit: usize,
        reply: Reply<Vec<TriageRunSummary>>,
    },
    RecentTriageRunSteps {
        id: RunId,
        limit: usize,
        reply: Reply<Vec<TriageStepRecord>>,
    },
    TriageRunsForEvent {
        event_id: i64,
        reply: Reply<Vec<TriageRunRecord>>,
    },
    TriageRunTurns {
        id: RunId,
        reply: Reply<Vec<TriageTurnRecord>>,
    },
    TriageRunRetries {
        id: RunId,
        reply: Reply<Vec<TriageRetryRecord>>,
    },
    TriageRunCompactions {
        id: RunId,
        reply: Reply<Vec<TriageCompactionRecord>>,
    },
    TriageRunPrompts {
        id: RunId,
        reply: Reply<Vec<TriageRunPromptRecord>>,
    },
    TriageRunEffects {
        id: RunId,
        reply: Reply<Vec<TriageEffectRecord>>,
    },
    IntegrityCheck(Reply<String>),
}

fn write_actor(
    target: OpenTarget,
    mut receiver: mpsc::Receiver<WriteOperation>,
    ready: oneshot::Sender<Result<(), DatabaseError>>,
) {
    let mut connection = match open_connection(&target, false).and_then(|connection| {
        migrate(&connection)?;
        Ok(connection)
    }) {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    while let Some(operation) = receiver.blocking_recv() {
        let shutdown = matches!(operation, WriteOperation::Shutdown(_));
        dispatch_write(&mut connection, operation);
        if shutdown {
            break;
        }
    }
}

fn read_actor(
    target: OpenTarget,
    mut receiver: mpsc::Receiver<ReadOperation>,
    ready: oneshot::Sender<Result<(), DatabaseError>>,
) {
    let connection = match open_connection(&target, true) {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    while let Some(operation) = receiver.blocking_recv() {
        dispatch_read(&connection, operation);
    }
}

fn open_connection(target: &OpenTarget, read_only: bool) -> Result<Connection, DatabaseError> {
    let flags = if read_only {
        target.read_flags()
    } else {
        target.flags
    };
    let connection = Connection::open_with_flags(&target.value, flags)?;
    connection.busy_timeout(Duration::from_millis(5_000))?;
    if read_only {
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA query_only = ON;")?;
    } else {
        connection.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
    }
    Ok(connection)
}

fn migrate(connection: &Connection) -> Result<(), DatabaseError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    let mut statement =
        connection.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let applied = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(found) = applied.last().copied()
        && found > SCHEMA_VERSION as i64
    {
        return Err(DatabaseError::FutureSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, found) in applied.iter().copied().enumerate() {
        if found != index as i64 + 1 {
            return Err(DatabaseError::MigrationGap {
                found,
                position: index + 1,
            });
        }
    }
    drop(statement);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied.len()) {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            params![index + 1, timestamp(Utc::now())],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

fn dispatch_write(connection: &mut Connection, operation: WriteOperation) {
    macro_rules! reply {
        ($reply:expr, $result:expr) => {{
            let _ = $reply.send($result);
        }};
    }
    match operation {
        WriteOperation::SourceSucceeded {
            source,
            checkpoint,
            items,
            observed_at,
            reply: sender,
        } => reply!(
            sender,
            source_succeeded(connection, &source, checkpoint, &items, &observed_at)
        ),
        WriteOperation::SourceFailed {
            source,
            error,
            now,
            reply: sender,
        } => reply!(sender, source_failed(connection, &source, &error, &now)),
        WriteOperation::ClaimNext { now, reply: sender } => {
            reply!(sender, claim_next(connection, &now))
        }
        WriteOperation::RecoverInterrupted {
            stale_before,
            now,
            reply: sender,
        } => reply!(sender, recover_interrupted(connection, &stale_before, &now)),
        WriteOperation::StartRun {
            event_id,
            attempt,
            dispatch_reason,
            now,
            reply: sender,
        } => reply!(
            sender,
            start_run(
                connection,
                event_id,
                attempt,
                dispatch_reason.as_deref(),
                &now
            )
        ),
        WriteOperation::SetRunMetadata {
            run_id,
            metadata,
            now,
            reply: sender,
        } => reply!(sender, set_run_metadata(connection, run_id, metadata, &now)),
        WriteOperation::RecordPrompt {
            run_id,
            role,
            content,
            now,
            reply: sender,
        } => reply!(
            sender,
            record_prompt(connection, run_id, &role, &content, &now)
        ),
        WriteOperation::StartTurn {
            run_id,
            now,
            reply: sender,
        } => reply!(sender, start_turn(connection, run_id, &now)),
        WriteOperation::FinishTurn {
            run_id,
            turn_id,
            finish,
            now,
            reply: sender,
        } => reply!(
            sender,
            finish_turn(connection, run_id, turn_id, finish, &now)
        ),
        WriteOperation::StartTool {
            run_id,
            turn_id,
            name,
            summary,
            now,
            reply: sender,
        } => reply!(
            sender,
            start_tool(connection, run_id, turn_id, &name, summary.as_deref(), &now)
        ),
        WriteOperation::FinishTool {
            run_id,
            tool_id,
            outcome,
            now,
            reply: sender,
        } => reply!(
            sender,
            finish_span(
                connection,
                "triage_run_steps",
                run_id,
                tool_id.0,
                outcome,
                &now
            )
        ),
        WriteOperation::StartRetry {
            run_id,
            turn_id,
            retry,
            now,
            reply: sender,
        } => reply!(
            sender,
            start_retry(connection, run_id, turn_id, retry, &now)
        ),
        WriteOperation::FinishRetry {
            run_id,
            retry_id,
            outcome,
            error_category,
            now,
            reply: sender,
        } => reply!(
            sender,
            finish_retry(connection, run_id, retry_id, outcome, error_category, &now)
        ),
        WriteOperation::StartCompaction {
            run_id,
            turn_id,
            reason,
            now,
            reply: sender,
        } => reply!(
            sender,
            start_compaction(connection, run_id, turn_id, &reason, &now)
        ),
        WriteOperation::FinishCompaction {
            run_id,
            compaction_id,
            finish,
            now,
            reply: sender,
        } => reply!(
            sender,
            finish_compaction(connection, run_id, compaction_id, finish, &now)
        ),
        WriteOperation::RecordAssistantText {
            run_id,
            turn_id,
            text,
            now,
            reply: sender,
        } => reply!(
            sender,
            record_assistant_text(connection, run_id, turn_id, &text, &now)
        ),
        WriteOperation::RecordReasoning {
            run_id,
            turn_id,
            summary,
            now,
            reply: sender,
        } => reply!(
            sender,
            record_reasoning(connection, run_id, turn_id, summary.as_deref(), &now)
        ),
        WriteOperation::FinishRun {
            run_id,
            finish,
            conclusion,
            now,
            reply: sender,
        } => reply!(
            sender,
            finish_run(connection, run_id, finish, conclusion, &now)
        ),
        WriteOperation::Succeed {
            event_id,
            now,
            reply: sender,
        } => reply!(sender, succeed(connection, event_id, &now)),
        WriteOperation::Fail {
            event_id,
            error,
            max_attempts,
            retry_base_seconds,
            now,
            reply: sender,
        } => reply!(
            sender,
            fail(
                connection,
                event_id,
                &error,
                max_attempts,
                retry_base_seconds,
                now
            )
        ),
        WriteOperation::Retry {
            event_id,
            now,
            reply: sender,
        } => reply!(sender, retry_event(connection, event_id, &now)),
        WriteOperation::Ignore {
            event_id,
            now,
            reply: sender,
        } => reply!(sender, ignore_event(connection, event_id, &now)),
        WriteOperation::RecordCommand {
            event_id,
            executable,
            exit_code,
            output,
            now,
            reply: sender,
        } => reply!(
            sender,
            record_command(connection, event_id, &executable, exit_code, &output, &now)
        ),
        WriteOperation::Flush(sender) => reply!(sender, flush_connection(connection)),
        WriteOperation::Shutdown(sender) => reply!(sender, flush_connection(connection)),
    }
}

fn dispatch_read(connection: &Connection, operation: ReadOperation) {
    macro_rules! reply {
        ($reply:expr, $result:expr) => {{
            let _ = $reply.send($result);
        }};
    }
    match operation {
        ReadOperation::SourceCheckpoint {
            source,
            reply: sender,
        } => {
            reply!(sender, source_checkpoint(connection, &source))
        }
        ReadOperation::Event { id, reply: sender } => reply!(sender, event(connection, id)),
        ReadOperation::ListEvents {
            limit,
            reply: sender,
        } => {
            reply!(sender, list_events(connection, limit))
        }
        ReadOperation::OldestOpenEventAt(sender) => {
            reply!(sender, oldest_open_event_at(connection))
        }
        ReadOperation::Status(sender) => reply!(sender, status(connection)),
        ReadOperation::SourceStatuses(sender) => reply!(sender, source_statuses(connection)),
        ReadOperation::TriageRun { id, reply: sender } => {
            reply!(sender, triage_run(connection, id))
        }
        ReadOperation::TriageRunSteps { id, reply: sender } => {
            reply!(sender, triage_run_steps(connection, id))
        }
        ReadOperation::ListTriageRunSummaries {
            limit,
            reply: sender,
        } => reply!(sender, list_triage_run_summaries(connection, limit)),
        ReadOperation::RecentTriageRunSteps {
            id,
            limit,
            reply: sender,
        } => reply!(sender, recent_triage_run_steps(connection, id, limit)),
        ReadOperation::TriageRunsForEvent {
            event_id,
            reply: sender,
        } => reply!(sender, triage_runs_for_event(connection, event_id)),
        ReadOperation::TriageRunTurns { id, reply: sender } => {
            reply!(sender, triage_run_turns(connection, id))
        }
        ReadOperation::TriageRunRetries { id, reply: sender } => {
            reply!(sender, triage_run_retries(connection, id))
        }
        ReadOperation::TriageRunCompactions { id, reply: sender } => {
            reply!(sender, triage_run_compactions(connection, id))
        }
        ReadOperation::TriageRunPrompts { id, reply: sender } => {
            reply!(sender, triage_run_prompts(connection, id))
        }
        ReadOperation::TriageRunEffects { id, reply: sender } => {
            reply!(sender, triage_run_effects(connection, id))
        }
        ReadOperation::IntegrityCheck(sender) => reply!(sender, integrity_check(connection)),
    }
}

fn source_succeeded(
    connection: &mut Connection,
    source: &str,
    checkpoint: Value,
    items: &[IntakeItem],
    observed_at: &str,
) -> Result<usize, DatabaseError> {
    let transaction = connection.transaction()?;
    let mut inserted = 0;
    for item in items {
        let operational_metadata = serde_json::json!({
            "url": item.url,
            "kind": item.kind,
        });
        transaction.execute(
            "INSERT INTO entities(source, external_id, kind, title, last_event_at, operational_metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(external_id) DO UPDATE SET
               kind = excluded.kind,
               title = excluded.title,
               last_event_at = excluded.last_event_at,
               operational_metadata = excluded.operational_metadata",
            params![
                source,
                item.entity_id,
                intake_item_kind(item.kind),
                item.title,
                item.occurred_at,
                serde_json::to_string(&operational_metadata)?,
            ],
        )?;
        let entity_id: i64 = transaction.query_row(
            "SELECT id FROM entities WHERE external_id = ?1",
            [&item.entity_id],
            |row| row.get(0),
        )?;
        inserted += transaction.execute(
            "INSERT OR IGNORE INTO events(entity_id, source, revision_id, payload, occurred_at, observed_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entity_id,
                source,
                item.revision_id,
                serde_json::to_string(item)?,
                item.occurred_at,
                observed_at,
                observed_at,
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO source_state(source, checkpoint, last_success_at, last_error, updated_at)
         VALUES (?1, ?2, ?3, NULL, ?3)
         ON CONFLICT(source) DO UPDATE SET checkpoint = excluded.checkpoint,
           last_success_at = excluded.last_success_at, last_error = NULL, updated_at = excluded.updated_at",
        params![source, serde_json::to_string(&checkpoint)?, observed_at],
    )?;
    transaction.commit()?;
    Ok(inserted)
}

fn source_failed(
    connection: &Connection,
    source: &str,
    error: &str,
    now: &str,
) -> Result<(), DatabaseError> {
    connection.execute(
        "INSERT INTO source_state(source, last_error, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(source) DO UPDATE SET last_error = excluded.last_error, updated_at = excluded.updated_at",
        params![source, bounded(error, 4096), now],
    )?;
    Ok(())
}

fn claim_next(
    connection: &mut Connection,
    now: &str,
) -> Result<Option<EventRecord>, DatabaseError> {
    let transaction = connection.transaction()?;
    let id = transaction
        .query_row(
            "SELECT ev.id FROM events ev
             WHERE ev.status IN ('pending', 'retryable')
               AND (ev.next_attempt_at IS NULL OR ev.next_attempt_at <= ?1)
               AND NOT EXISTS (
                 SELECT 1 FROM events prior
                 WHERE prior.entity_id = ev.entity_id
                   AND prior.status IN ('pending', 'retryable', 'processing')
                   AND (prior.observed_at < ev.observed_at OR
                     (prior.observed_at = ev.observed_at AND prior.id < ev.id))
               )
             ORDER BY ev.observed_at, ev.id LIMIT 1",
            [now],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(id) = id else {
        transaction.commit()?;
        return Ok(None);
    };
    let run_ids = {
        let mut statement = transaction
            .prepare("SELECT id FROM triage_runs WHERE event_id = ?1 AND ended_at IS NULL")?;
        statement
            .query_map([id], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    interrupt_runs(&transaction, &run_ids, now, "superseded_attempt")?;
    transaction.execute(
        "UPDATE events SET status = 'processing', attempt_count = attempt_count + 1, updated_at = ?1 WHERE id = ?2",
        params![now, id],
    )?;
    let result = event(&transaction, id)?;
    transaction.commit()?;
    Ok(result)
}

fn recover_interrupted(
    connection: &mut Connection,
    stale_before: &str,
    now: &str,
) -> Result<usize, DatabaseError> {
    let transaction = connection.transaction()?;
    let run_ids = {
        let mut statement = transaction.prepare(
            "SELECT run.id FROM triage_runs run
             JOIN events event ON event.id = run.event_id
             WHERE run.ended_at IS NULL AND event.status = 'processing'
               AND event.updated_at <= ?1",
        )?;
        statement
            .query_map([stale_before], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    interrupt_runs(&transaction, &run_ids, now, "process_exit")?;
    let changed = transaction.execute(
        "UPDATE events SET status = 'retryable', next_attempt_at = ?1,
           last_error = 'triage interrupted by process exit', updated_at = ?1
         WHERE status = 'processing' AND updated_at <= ?2",
        params![now, stale_before],
    )?;
    transaction.commit()?;
    Ok(changed)
}

fn start_run(
    connection: &Connection,
    event_id: i64,
    attempt: u32,
    dispatch_reason: Option<&str>,
    now: &str,
) -> Result<RunId, DatabaseError> {
    connection.execute(
        "INSERT INTO triage_runs(event_id, attempt, started_at, last_activity_at,
           telemetry_version, telemetry_completeness, dispatch_reason)
         VALUES (?1, ?2, ?3, ?3, 2, 'partial', ?4)",
        params![
            event_id,
            attempt,
            now,
            dispatch_reason.map(|value| bounded_detail(value, 1000))
        ],
    )?;
    Ok(RunId(connection.last_insert_rowid()))
}

fn set_run_metadata(
    connection: &Connection,
    run_id: RunId,
    metadata: RunMetadata,
    now: &str,
) -> Result<(), DatabaseError> {
    connection.execute(
        "UPDATE triage_runs SET model_id = ?1, model_provider = ?2, thinking_level = ?3,
           context_window = ?4, max_tokens = ?5, last_activity_at = ?6 WHERE id = ?7",
        params![
            metadata.model_id,
            metadata.model_provider,
            metadata.thinking_level,
            metadata.context_window,
            metadata.max_tokens,
            now,
            run_id.0,
        ],
    )?;
    Ok(())
}

fn record_prompt(
    connection: &Connection,
    run_id: RunId,
    role: &str,
    content: &str,
    now: &str,
) -> Result<(), DatabaseError> {
    if !matches!(role, "system" | "user") {
        return Err(DatabaseError::InvalidValue(format!(
            "unknown prompt role {role}"
        )));
    }
    connection.execute(
        "INSERT INTO triage_run_prompts(run_id, role, content, recorded_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(run_id, role) DO UPDATE SET
           content = excluded.content, recorded_at = excluded.recorded_at",
        params![run_id.0, role, content, now],
    )?;
    touch_run(connection, run_id, now)
}

fn start_turn(connection: &Connection, run_id: RunId, now: &str) -> Result<TurnId, DatabaseError> {
    connection.execute(
        "INSERT INTO triage_run_turns(run_id, ordinal, started_at)
         SELECT ?1, COALESCE(MAX(ordinal), 0) + 1, ?2
         FROM triage_run_turns WHERE run_id = ?1",
        params![run_id.0, now],
    )?;
    let id = TurnId(connection.last_insert_rowid());
    refresh_counts(connection, run_id, now)?;
    Ok(id)
}

fn finish_turn(
    connection: &Connection,
    run_id: RunId,
    turn_id: TurnId,
    finish: TurnFinish,
    now: &str,
) -> Result<(), DatabaseError> {
    let usage = finish.usage.unwrap_or_default();
    let changed = connection.execute(
        "UPDATE triage_run_turns SET ended_at = ?1, stop_reason = ?2,
           input_tokens = ?3, output_tokens = ?4, cache_read_tokens = ?5,
           cache_write_tokens = ?6, reasoning_tokens = ?7, total_tokens = ?8,
           context_tokens = ?9, context_window = ?10
         WHERE id = ?11 AND run_id = ?12 AND ended_at IS NULL",
        params![
            now,
            safe_stop_reason(finish.stop_reason.as_deref()),
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.reasoning_tokens,
            usage.total_tokens,
            finish.context_tokens,
            finish.context_window,
            turn_id.0,
            run_id.0,
        ],
    )?;
    ensure_span_changed(changed)?;
    touch_run(connection, run_id, now)
}

fn start_tool(
    connection: &Connection,
    run_id: RunId,
    turn_id: Option<TurnId>,
    name: &str,
    summary: Option<&str>,
    now: &str,
) -> Result<ToolId, DatabaseError> {
    connection.execute(
        "INSERT INTO triage_run_steps(run_id, step_key, turn_id, kind, label, summary, started_at)
         VALUES (?1, lower(hex(randomblob(16))), ?2, 'tool', ?3, ?4, ?5)",
        params![
            run_id.0,
            turn_id.map(|id| id.0),
            safe_tool_name(name),
            summary.map(|value| bounded_detail(value, 4096)),
            now,
        ],
    )?;
    touch_run(connection, run_id, now)?;
    Ok(ToolId(connection.last_insert_rowid()))
}

fn finish_span(
    connection: &Connection,
    table: &str,
    run_id: RunId,
    id: i64,
    outcome: SpanOutcome,
    now: &str,
) -> Result<(), DatabaseError> {
    debug_assert_eq!(table, "triage_run_steps");
    let changed = connection.execute(
        "UPDATE triage_run_steps SET ended_at = ?1, outcome = ?2
         WHERE id = ?3 AND run_id = ?4 AND ended_at IS NULL",
        params![now, outcome.as_str(), id, run_id.0],
    )?;
    ensure_span_changed(changed)?;
    touch_run(connection, run_id, now)
}

fn start_retry(
    connection: &Connection,
    run_id: RunId,
    turn_id: Option<TurnId>,
    retry: RetryStart,
    now: &str,
) -> Result<RetryId, DatabaseError> {
    let wait_ended_at = DateTime::parse_from_rfc3339(now)
        .map_err(|error| DatabaseError::InvalidValue(error.to_string()))?
        .with_timezone(&Utc)
        + chrono::Duration::milliseconds(saturating_i64(retry.delay_ms));
    connection.execute(
        "INSERT INTO triage_run_retries(run_id, turn_id, attempt, max_attempts,
           delay_ms, started_at, wait_ended_at, error_category)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            run_id.0,
            turn_id.map(|id| id.0),
            retry.attempt,
            retry.max_attempts,
            saturating_i64(retry.delay_ms),
            now,
            timestamp(wait_ended_at),
            retry.error_category.map(ErrorCategory::as_str),
        ],
    )?;
    let id = RetryId(connection.last_insert_rowid());
    refresh_counts(connection, run_id, now)?;
    Ok(id)
}

fn finish_retry(
    connection: &Connection,
    run_id: RunId,
    retry_id: RetryId,
    outcome: SpanOutcome,
    error_category: Option<ErrorCategory>,
    now: &str,
) -> Result<(), DatabaseError> {
    let changed = connection.execute(
        "UPDATE triage_run_retries SET ended_at = ?1, outcome = ?2,
           error_category = COALESCE(?3, error_category)
         WHERE id = ?4 AND run_id = ?5 AND ended_at IS NULL",
        params![
            now,
            outcome.as_str(),
            error_category.map(ErrorCategory::as_str),
            retry_id.0,
            run_id.0,
        ],
    )?;
    ensure_span_changed(changed)?;
    touch_run(connection, run_id, now)
}

fn start_compaction(
    connection: &mut Connection,
    run_id: RunId,
    turn_id: Option<TurnId>,
    reason: &str,
    now: &str,
) -> Result<CompactionId, DatabaseError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO triage_run_compactions(run_id, turn_id, reason, started_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![run_id.0, turn_id.map(|id| id.0), reason, now],
    )?;
    let id = CompactionId(transaction.last_insert_rowid());
    transaction.execute(
        "INSERT INTO triage_run_steps(run_id, step_key, turn_id, kind, label, started_at)
         VALUES (?1, lower(hex(randomblob(16))), ?2, 'compaction', 'compaction', ?3)",
        params![run_id.0, turn_id.map(|id| id.0), now],
    )?;
    refresh_counts(&transaction, run_id, now)?;
    transaction.commit()?;
    Ok(id)
}

fn finish_compaction(
    connection: &mut Connection,
    run_id: RunId,
    compaction_id: CompactionId,
    finish: CompactionFinish,
    now: &str,
) -> Result<(), DatabaseError> {
    let transaction = connection.transaction()?;
    let usage = finish.usage.unwrap_or_default();
    let changed = transaction.execute(
        "UPDATE triage_run_compactions SET ended_at = ?1, outcome = ?2,
           aborted = ?3, will_retry = ?4, tokens_before = ?5,
           estimated_tokens_after = ?6, input_tokens = ?7, output_tokens = ?8,
           cache_read_tokens = ?9, cache_write_tokens = ?10,
           reasoning_tokens = ?11, total_tokens = ?12
         WHERE id = ?13 AND run_id = ?14 AND ended_at IS NULL",
        params![
            now,
            finish.outcome.as_str(),
            finish.aborted,
            finish.will_retry,
            finish.tokens_before,
            finish.estimated_tokens_after,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.reasoning_tokens,
            usage.total_tokens,
            compaction_id.0,
            run_id.0,
        ],
    )?;
    ensure_span_changed(changed)?;
    transaction.execute(
        "UPDATE triage_run_steps SET ended_at = ?1, outcome = ?2
         WHERE id = (SELECT id FROM triage_run_steps
           WHERE run_id = ?3 AND kind = 'compaction' AND ended_at IS NULL
           ORDER BY id DESC LIMIT 1)",
        params![
            now,
            if finish.outcome == SpanOutcome::Succeeded {
                "succeeded"
            } else {
                "failed"
            },
            run_id.0,
        ],
    )?;
    touch_run(&transaction, run_id, now)?;
    transaction.commit()?;
    Ok(())
}

fn record_assistant_text(
    connection: &Connection,
    run_id: RunId,
    turn_id: Option<TurnId>,
    text: &str,
    now: &str,
) -> Result<(), DatabaseError> {
    connection.execute(
        "INSERT INTO triage_run_steps(run_id, step_key, turn_id, kind, label,
           summary, started_at, ended_at, outcome)
         VALUES (?1, lower(hex(randomblob(16))), ?2, 'message', 'assistant',
           ?3, ?4, ?4, 'succeeded')",
        params![
            run_id.0,
            turn_id.map(|id| id.0),
            bounded_detail(text, 1000),
            now,
        ],
    )?;
    touch_run(connection, run_id, now)
}

fn record_reasoning(
    connection: &Connection,
    run_id: RunId,
    turn_id: Option<TurnId>,
    summary: Option<&str>,
    now: &str,
) -> Result<(), DatabaseError> {
    connection.execute(
        "INSERT INTO triage_run_steps(run_id, step_key, turn_id, kind, label,
           summary, started_at, ended_at, outcome)
         VALUES (?1, lower(hex(randomblob(16))), ?2, 'thinking', 'thinking',
           ?3, ?4, ?4, 'succeeded')",
        params![
            run_id.0,
            turn_id.map(|id| id.0),
            summary.map(|value| bounded_detail(value, 1000)),
            now,
        ],
    )?;
    touch_run(connection, run_id, now)
}

fn finish_run(
    connection: &mut Connection,
    run_id: RunId,
    finish: RunFinish,
    conclusion: Option<TriageConclusion>,
    now: &str,
) -> Result<(), DatabaseError> {
    flush_connection(connection)?;
    let transaction = connection.transaction()?;
    let conclusion = conclusion.unwrap_or_else(|| fallback_conclusion(&finish));
    let conclusion_json = serde_json::to_string(&conclusion)?;
    let open_count: i64 = transaction.query_row(
        "SELECT
           (SELECT COUNT(*) FROM triage_run_steps WHERE run_id = ?1 AND ended_at IS NULL) +
           (SELECT COUNT(*) FROM triage_run_turns WHERE run_id = ?1 AND ended_at IS NULL) +
           (SELECT COUNT(*) FROM triage_run_retries WHERE run_id = ?1 AND ended_at IS NULL) +
           (SELECT COUNT(*) FROM triage_run_compactions WHERE run_id = ?1 AND ended_at IS NULL)",
        [run_id.0],
        |row| row.get(0),
    )?;
    close_open_telemetry(&transaction, run_id, now)?;
    refresh_counts(&transaction, run_id, now)?;
    let complete =
        open_count == 0 && finish.outcome != RunOutcome::Interrupted && finish.recording_complete;
    transaction.execute(
        "UPDATE triage_runs SET ended_at = ?1, last_activity_at = ?1,
           outcome = ?2, termination_reason = ?3, failure_category = ?4,
           telemetry_completeness = ?5, conclusion_json = ?6
         WHERE id = ?7 AND ended_at IS NULL",
        params![
            now,
            finish.outcome.as_str(),
            safe_termination_reason(&finish.termination_reason),
            finish.failure_category.map(ErrorCategory::as_str),
            if complete { "complete" } else { "partial" },
            conclusion_json,
            run_id.0,
        ],
    )?;
    transaction.commit()?;
    flush_connection(connection)
}

fn fallback_conclusion(finish: &RunFinish) -> TriageConclusion {
    let (decision, summary, outcome, follow_up) = match (finish.outcome, finish.failure_category) {
        (RunOutcome::Succeeded, _) => (
            TriageDecision::NoAction,
            "Triage completed without a model-authored conclusion.",
            "The attempt completed successfully.",
            None,
        ),
        (RunOutcome::Interrupted, _) => (
            TriageDecision::Canceled,
            "Triage ended before the agent supplied a conclusion.",
            "The attempt was interrupted.",
            Some("Retry triage if this event still needs review."),
        ),
        (RunOutcome::Failed, Some(ErrorCategory::Timeout)) => (
            TriageDecision::TimedOut,
            "Triage reached its time limit before the agent supplied a conclusion.",
            "The attempt timed out.",
            Some("Retry triage or review the recorded activity."),
        ),
        (RunOutcome::Failed, Some(ErrorCategory::TurnLimit)) => (
            TriageDecision::TurnLimit,
            "Triage reached its turn limit before the agent supplied a conclusion.",
            "The attempt stopped at the configured turn limit.",
            Some("Review the recorded activity before retrying."),
        ),
        (RunOutcome::Failed, _) => (
            TriageDecision::Failed,
            "Triage failed before the agent supplied a conclusion.",
            "The attempt failed.",
            Some("Review the failure category and recorded activity before retrying."),
        ),
    };
    TriageConclusion {
        decision,
        summary: summary.into(),
        evidence: Vec::new(),
        actions: Vec::new(),
        outcome: outcome.into(),
        follow_up: follow_up.map(str::to_string),
        source: ConclusionSource::Derived,
    }
}

fn succeed(connection: &mut Connection, event_id: i64, now: &str) -> Result<(), DatabaseError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE events SET status = 'succeeded', payload = NULL, last_error = NULL,
           next_attempt_at = NULL, updated_at = ?1 WHERE id = ?2",
        params![now, event_id],
    )?;
    transaction.execute(
        "UPDATE entities SET handling_status = 'succeeded'
         WHERE id = (SELECT entity_id FROM events WHERE id = ?1)",
        [event_id],
    )?;
    transaction.commit()?;
    Ok(())
}

fn fail(
    connection: &mut Connection,
    event_id: i64,
    error: &str,
    max_attempts: u32,
    retry_base_seconds: u64,
    now: DateTime<Utc>,
) -> Result<(), DatabaseError> {
    let record = event(connection, event_id)?.ok_or(DatabaseError::UnknownEvent(event_id))?;
    let retryable = record.attempt_count < max_attempts;
    let exponent = record.attempt_count.saturating_sub(1).min(63);
    let delay = retry_base_seconds.saturating_mul(1_u64 << exponent);
    let next_attempt =
        retryable.then(|| timestamp(now + chrono::Duration::seconds(saturating_i64(delay))));
    let status = if retryable { "retryable" } else { "failed" };
    let now = timestamp(now);
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE events SET status = ?1, next_attempt_at = ?2, last_error = ?3,
           updated_at = ?4 WHERE id = ?5 AND status = 'processing'",
        params![status, next_attempt, bounded(error, 4096), now, event_id],
    )?;
    transaction.execute(
        "UPDATE entities SET handling_status = ?1
         WHERE id = (SELECT entity_id FROM events WHERE id = ?2)",
        params![status, event_id],
    )?;
    transaction.commit()?;
    Ok(())
}

fn retry_event(
    connection: &mut Connection,
    event_id: i64,
    now: &str,
) -> Result<bool, DatabaseError> {
    update_event_status(connection, event_id, now, "retryable", false)
}

fn ignore_event(
    connection: &mut Connection,
    event_id: i64,
    now: &str,
) -> Result<bool, DatabaseError> {
    update_event_status(connection, event_id, now, "ignored", true)
}

fn update_event_status(
    connection: &mut Connection,
    event_id: i64,
    now: &str,
    status: &str,
    scrub_payload: bool,
) -> Result<bool, DatabaseError> {
    let transaction = connection.transaction()?;
    let changed = if scrub_payload {
        transaction.execute(
            "UPDATE events SET status = 'ignored', payload = NULL, next_attempt_at = NULL,
             updated_at = ?1 WHERE id = ?2 AND status != 'processing'",
            params![now, event_id],
        )?
    } else {
        transaction.execute(
            "UPDATE events SET status = 'retryable', attempt_count = 0,
             next_attempt_at = NULL, last_error = NULL, updated_at = ?1
             WHERE id = ?2 AND payload IS NOT NULL AND status != 'processing'",
            params![now, event_id],
        )?
    };
    if changed == 1 {
        transaction.execute(
            "UPDATE entities SET handling_status = ?1
             WHERE id = (SELECT entity_id FROM events WHERE id = ?2)",
            params![status, event_id],
        )?;
    }
    transaction.commit()?;
    Ok(changed == 1)
}

fn record_command(
    connection: &mut Connection,
    event_id: i64,
    executable: &str,
    exit_code: i32,
    output: &str,
    now: &str,
) -> Result<(), DatabaseError> {
    let transaction = connection.transaction()?;
    let executable = safe_tool_name(executable);
    transaction.execute(
        "INSERT INTO command_events(event_id, command, exit_code, output_summary, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event_id,
            format!("tool={executable}"),
            exit_code,
            format!("bytes={}", output.len()),
            now,
        ],
    )?;
    if exit_code == 0 {
        if executable == "aven" {
            if let Some(reference) = find_aven_reference(output) {
                update_entity_reference(&transaction, event_id, "aven_ref", reference, now)?;
            }
        } else if executable == "workmux"
            && let Some(handle) = find_workmux_handle(output)
        {
            update_entity_reference(&transaction, event_id, "investigation_handle", &handle, now)?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn update_entity_reference(
    transaction: &Transaction<'_>,
    event_id: i64,
    column: &str,
    value: &str,
    now: &str,
) -> Result<(), DatabaseError> {
    let statement = match column {
        "aven_ref" => {
            "UPDATE entities SET aven_ref = ?1 WHERE id = (SELECT entity_id FROM events WHERE id = ?2)"
        }
        "investigation_handle" => {
            "UPDATE entities SET investigation_handle = ?1 WHERE id = (SELECT entity_id FROM events WHERE id = ?2)"
        }
        _ => return Err(DatabaseError::InvalidValue("unknown effect type".into())),
    };
    transaction.execute(statement, params![value, event_id])?;
    transaction.execute(
        "INSERT OR IGNORE INTO triage_run_effects(run_id, type, value, recorded_at)
         SELECT run.id, ?1, ?2, ?3 FROM triage_runs run
         WHERE run.event_id = ?4 ORDER BY run.id DESC LIMIT 1",
        params![
            if column == "aven_ref" {
                "aven_reference"
            } else {
                "investigation_handle"
            },
            value,
            now,
            event_id,
        ],
    )?;
    Ok(())
}

fn source_checkpoint(connection: &Connection, source: &str) -> Result<Value, DatabaseError> {
    let checkpoint = connection
        .query_row(
            "SELECT checkpoint FROM source_state WHERE source = ?1",
            [source],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    checkpoint
        .map(|value| serde_json::from_str(&value).map_err(DatabaseError::from))
        .unwrap_or(Ok(Value::Null))
}

const EVENT_SELECT: &str =
    "SELECT ev.id, COALESCE(ev.source, en.source), en.external_id, ev.revision_id, en.kind, en.title,
       ev.payload, en.operational_metadata, ev.occurred_at, ev.observed_at, ev.status,
       ev.attempt_count, ev.next_attempt_at, ev.last_error, en.aven_ref,
       en.investigation_handle
     FROM events ev JOIN entities en ON en.id = ev.entity_id";

fn event(connection: &Connection, id: i64) -> Result<Option<EventRecord>, DatabaseError> {
    let sql = format!("{EVENT_SELECT} WHERE ev.id = ?1");
    connection
        .query_row(&sql, [id], event_from_row)
        .optional()?
        .transpose()
}

fn list_events(connection: &Connection, limit: usize) -> Result<Vec<EventRecord>, DatabaseError> {
    let sql = format!("{EVENT_SELECT} ORDER BY ev.observed_at DESC, ev.id DESC LIMIT ?1");
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([saturating_i64(limit)])?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        records.push(event_from_row(row)??);
    }
    Ok(records)
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<EventRecord, DatabaseError>> {
    let id = row.get(0)?;
    let source = row.get(1)?;
    let entity_id = row.get(2)?;
    let revision_id = row.get(3)?;
    let kind = row.get(4)?;
    let title = row.get(5)?;
    let payload = row.get(6)?;
    let operational_metadata = row.get(7)?;
    let occurred_at = row.get(8)?;
    let observed_at = row.get(9)?;
    let status = EventStatus::parse(row.get(10)?);
    let attempt_count = row.get(11)?;
    let next_attempt_at = row.get(12)?;
    let last_error = row.get(13)?;
    let aven_ref = row.get(14)?;
    let investigation_handle = row.get(15)?;
    Ok(status.map(|status| EventRecord {
        id,
        source,
        entity_id,
        revision_id,
        kind,
        title,
        payload,
        operational_metadata,
        occurred_at,
        observed_at,
        status,
        attempt_count,
        next_attempt_at,
        last_error,
        aven_ref,
        investigation_handle,
    }))
}

fn oldest_open_event_at(connection: &Connection) -> Result<Option<String>, DatabaseError> {
    Ok(connection.query_row(
        "SELECT MIN(observed_at) FROM events WHERE status IN ('pending', 'processing', 'retryable')",
        [],
        |row| row.get(0),
    )?)
}

fn status(connection: &Connection) -> Result<HashMap<String, usize>, DatabaseError> {
    let mut statement =
        connection.prepare("SELECT status, COUNT(*) FROM events GROUP BY status")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.map(|result| {
        let (status, count) = result?;
        Ok((status, usize::try_from(count).unwrap_or(usize::MAX)))
    })
    .collect()
}

fn source_statuses(connection: &Connection) -> Result<Vec<SourceStatus>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT source, last_success_at, last_error, updated_at FROM source_state ORDER BY source",
    )?;
    Ok(statement
        .query_map([], |row| {
            Ok(SourceStatus {
                source: row.get(0)?,
                last_success_at: row.get(1)?,
                last_error: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_run(
    connection: &Connection,
    id: RunId,
) -> Result<Option<TriageRunRecord>, DatabaseError> {
    connection
        .query_row(
            "SELECT id, event_id, attempt, started_at, ended_at, last_activity_at,
               outcome, termination_reason, failure_category, model_id, model_provider,
               thinking_level, context_window, max_tokens, telemetry_version,
               telemetry_completeness, dispatch_reason, conclusion_json,
               turn_count, retry_count, compaction_count
             FROM triage_runs WHERE id = ?1",
            [id.0],
            triage_run_from_row,
        )
        .optional()
        .map_err(DatabaseError::from)
}

fn triage_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriageRunRecord> {
    Ok(TriageRunRecord {
        id: row.get(0)?,
        event_id: row.get(1)?,
        attempt: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        last_activity_at: row.get(5)?,
        outcome: row.get(6)?,
        termination_reason: row.get(7)?,
        failure_category: row.get(8)?,
        model_id: row.get(9)?,
        model_provider: row.get(10)?,
        thinking_level: row.get(11)?,
        context_window: row.get(12)?,
        max_tokens: row.get(13)?,
        telemetry_version: row.get(14)?,
        telemetry_completeness: row.get(15)?,
        dispatch_reason: row.get(16)?,
        conclusion: row
            .get::<_, Option<String>>(17)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(17, Type::Text, Box::new(error))
            })?,
        turn_count: row.get(18)?,
        retry_count: row.get(19)?,
        compaction_count: row.get(20)?,
    })
}

fn triage_run_steps(
    connection: &Connection,
    id: RunId,
) -> Result<Vec<TriageStepRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT step.id, step.turn_id, turn.ordinal, step.kind, step.label,
           step.summary, step.started_at, step.ended_at, step.outcome
         FROM triage_run_steps step
         LEFT JOIN triage_run_turns turn ON turn.id = step.turn_id
         WHERE step.run_id = ?1 ORDER BY step.started_at, step.id",
    )?;
    Ok(statement
        .query_map([id.0], |row| {
            Ok(TriageStepRecord {
                id: row.get(0)?,
                turn_id: row.get(1)?,
                turn_ordinal: row.get(2)?,
                kind: row.get(3)?,
                label: row.get(4)?,
                summary: row.get(5)?,
                started_at: row.get(6)?,
                ended_at: row.get(7)?,
                outcome: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn list_triage_run_summaries(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<TriageRunSummary>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT run.id, run.event_id, run.attempt, run.started_at, run.ended_at,
           run.last_activity_at, run.outcome, run.termination_reason,
           run.failure_category, run.model_id, run.model_provider,
           run.thinking_level, run.context_window, run.max_tokens,
           run.telemetry_version, run.telemetry_completeness, run.dispatch_reason,
           run.conclusion_json, run.turn_count, run.retry_count, run.compaction_count,
           (SELECT COUNT(*) FROM triage_run_steps step WHERE step.run_id = run.id)
         FROM triage_runs run ORDER BY run.started_at DESC, run.id DESC LIMIT ?1",
    )?;
    Ok(statement
        .query_map([saturating_i64(limit)], |row| {
            Ok(TriageRunSummary {
                run: triage_run_from_row(row)?,
                step_count: usize::try_from(row.get::<_, i64>(21)?).unwrap_or(usize::MAX),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn recent_triage_run_steps(
    connection: &Connection,
    id: RunId,
    limit: usize,
) -> Result<Vec<TriageStepRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT recent.id, recent.turn_id, recent.turn_ordinal, recent.kind,
           recent.label, recent.summary, recent.started_at, recent.ended_at,
           recent.outcome
         FROM (
           SELECT step.id, step.turn_id, turn.ordinal AS turn_ordinal, step.kind,
             step.label, step.summary, step.started_at, step.ended_at, step.outcome
           FROM triage_run_steps step
           LEFT JOIN triage_run_turns turn ON turn.id = step.turn_id
           WHERE step.run_id = ?1
           ORDER BY step.started_at DESC, step.id DESC LIMIT ?2
         ) recent ORDER BY recent.started_at, recent.id",
    )?;
    Ok(statement
        .query_map(params![id.0, saturating_i64(limit)], triage_step_from_row)?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_runs_for_event(
    connection: &Connection,
    event_id: i64,
) -> Result<Vec<TriageRunRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT id, event_id, attempt, started_at, ended_at, last_activity_at,
           outcome, termination_reason, failure_category, model_id, model_provider,
           thinking_level, context_window, max_tokens, telemetry_version,
           telemetry_completeness, dispatch_reason, conclusion_json,
           turn_count, retry_count, compaction_count
         FROM triage_runs WHERE event_id = ?1 ORDER BY attempt, id",
    )?;
    Ok(statement
        .query_map([event_id], triage_run_from_row)?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_run_turns(
    connection: &Connection,
    id: RunId,
) -> Result<Vec<TriageTurnRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT id, ordinal, started_at, ended_at, stop_reason, input_tokens,
           output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
           total_tokens, total_cost, context_tokens, context_window
         FROM triage_run_turns WHERE run_id = ?1 ORDER BY ordinal",
    )?;
    Ok(statement
        .query_map([id.0], |row| {
            Ok(TriageTurnRecord {
                id: row.get(0)?,
                ordinal: row.get(1)?,
                started_at: row.get(2)?,
                ended_at: row.get(3)?,
                stop_reason: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_write_tokens: row.get(8)?,
                reasoning_tokens: row.get(9)?,
                total_tokens: row.get(10)?,
                total_cost: row.get(11)?,
                context_tokens: row.get(12)?,
                context_window: row.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_run_retries(
    connection: &Connection,
    id: RunId,
) -> Result<Vec<TriageRetryRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT id, turn_id, attempt, max_attempts, delay_ms, started_at,
           wait_ended_at, ended_at, outcome, error_category
         FROM triage_run_retries WHERE run_id = ?1 ORDER BY started_at, id",
    )?;
    Ok(statement
        .query_map([id.0], |row| {
            Ok(TriageRetryRecord {
                id: row.get(0)?,
                turn_id: row.get(1)?,
                attempt: row.get(2)?,
                max_attempts: row.get(3)?,
                delay_ms: row.get(4)?,
                started_at: row.get(5)?,
                wait_ended_at: row.get(6)?,
                ended_at: row.get(7)?,
                outcome: row.get(8)?,
                error_category: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_run_compactions(
    connection: &Connection,
    id: RunId,
) -> Result<Vec<TriageCompactionRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT id, turn_id, reason, started_at, ended_at, outcome, aborted,
           will_retry, tokens_before, estimated_tokens_after, input_tokens,
           output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
           total_tokens, total_cost
         FROM triage_run_compactions WHERE run_id = ?1 ORDER BY started_at, id",
    )?;
    Ok(statement
        .query_map([id.0], |row| {
            Ok(TriageCompactionRecord {
                id: row.get(0)?,
                turn_id: row.get(1)?,
                reason: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
                outcome: row.get(5)?,
                aborted: row.get(6)?,
                will_retry: row.get(7)?,
                tokens_before: row.get(8)?,
                estimated_tokens_after: row.get(9)?,
                input_tokens: row.get(10)?,
                output_tokens: row.get(11)?,
                cache_read_tokens: row.get(12)?,
                cache_write_tokens: row.get(13)?,
                reasoning_tokens: row.get(14)?,
                total_tokens: row.get(15)?,
                total_cost: row.get(16)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_run_prompts(
    connection: &Connection,
    id: RunId,
) -> Result<Vec<TriageRunPromptRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT role, content, recorded_at FROM triage_run_prompts
         WHERE run_id = ?1 ORDER BY id",
    )?;
    Ok(statement
        .query_map([id.0], |row| {
            Ok(TriageRunPromptRecord {
                role: row.get(0)?,
                content: row.get(1)?,
                recorded_at: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_run_effects(
    connection: &Connection,
    id: RunId,
) -> Result<Vec<TriageEffectRecord>, DatabaseError> {
    let mut statement = connection.prepare(
        "SELECT type, value, recorded_at FROM triage_run_effects
         WHERE run_id = ?1 ORDER BY recorded_at, id",
    )?;
    Ok(statement
        .query_map([id.0], |row| {
            Ok(TriageEffectRecord {
                effect_type: row.get(0)?,
                value: row.get(1)?,
                recorded_at: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn triage_step_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriageStepRecord> {
    Ok(TriageStepRecord {
        id: row.get(0)?,
        turn_id: row.get(1)?,
        turn_ordinal: row.get(2)?,
        kind: row.get(3)?,
        label: row.get(4)?,
        summary: row.get(5)?,
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
        outcome: row.get(8)?,
    })
}

fn integrity_check(connection: &Connection) -> Result<String, DatabaseError> {
    Ok(connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
}

fn interrupt_runs(
    transaction: &Transaction<'_>,
    run_ids: &[i64],
    ended_at: &str,
    termination_reason: &str,
) -> Result<(), DatabaseError> {
    for id in run_ids {
        let run_id = RunId(*id);
        close_open_telemetry(transaction, run_id, ended_at)?;
        refresh_counts(transaction, run_id, ended_at)?;
        let conclusion = TriageConclusion {
            decision: TriageDecision::Canceled,
            summary: "Triage ended before the agent supplied a conclusion.".into(),
            evidence: Vec::new(),
            actions: Vec::new(),
            outcome: "The attempt was interrupted.".into(),
            follow_up: Some("Retry triage if this event still needs review.".into()),
            source: ConclusionSource::Derived,
        };
        transaction.execute(
            "UPDATE triage_runs SET ended_at = ?1, last_activity_at = ?1,
               outcome = 'interrupted', termination_reason = ?2,
               failure_category = 'interrupted', telemetry_completeness = 'partial',
               conclusion_json = ?3
             WHERE id = ?4 AND ended_at IS NULL",
            params![
                ended_at,
                termination_reason,
                serde_json::to_string(&conclusion)?,
                id
            ],
        )?;
    }
    Ok(())
}

fn close_open_telemetry(
    connection: &Connection,
    run_id: RunId,
    ended_at: &str,
) -> Result<(), DatabaseError> {
    connection.execute(
        "UPDATE triage_run_steps SET ended_at = ?1, outcome = 'interrupted'
         WHERE run_id = ?2 AND ended_at IS NULL",
        params![ended_at, run_id.0],
    )?;
    connection.execute(
        "UPDATE triage_run_turns SET ended_at = ?1, stop_reason = 'aborted'
         WHERE run_id = ?2 AND ended_at IS NULL",
        params![ended_at, run_id.0],
    )?;
    connection.execute(
        "UPDATE triage_run_retries SET ended_at = ?1, outcome = 'interrupted'
         WHERE run_id = ?2 AND ended_at IS NULL",
        params![ended_at, run_id.0],
    )?;
    connection.execute(
        "UPDATE triage_run_compactions SET ended_at = ?1, outcome = 'interrupted'
         WHERE run_id = ?2 AND ended_at IS NULL",
        params![ended_at, run_id.0],
    )?;
    Ok(())
}

fn refresh_counts(connection: &Connection, run_id: RunId, now: &str) -> Result<(), DatabaseError> {
    connection.execute(
        "UPDATE triage_runs SET last_activity_at = ?1,
           turn_count = (SELECT COUNT(*) FROM triage_run_turns WHERE run_id = ?2),
           retry_count = (SELECT COUNT(*) FROM triage_run_retries WHERE run_id = ?2),
           compaction_count = (SELECT COUNT(*) FROM triage_run_compactions WHERE run_id = ?2)
         WHERE id = ?2",
        params![now, run_id.0],
    )?;
    Ok(())
}

fn touch_run(connection: &Connection, run_id: RunId, now: &str) -> Result<(), DatabaseError> {
    connection.execute(
        "UPDATE triage_runs SET last_activity_at = ?1 WHERE id = ?2",
        params![now, run_id.0],
    )?;
    Ok(())
}

fn ensure_span_changed(changed: usize) -> Result<(), DatabaseError> {
    if changed == 1 {
        Ok(())
    } else {
        Err(DatabaseError::UnknownSpan)
    }
}

fn flush_connection(connection: &Connection) -> Result<(), DatabaseError> {
    connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
    Ok(())
}

pub fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn intake_item_kind(kind: IntakeItemKind) -> &'static str {
    match kind {
        IntakeItemKind::Email => "email",
        IntakeItemKind::GithubIssue => "github-issue",
        IntakeItemKind::GithubPullRequest => "github-pull-request",
        IntakeItemKind::Generic => "generic",
    }
}

fn safe_stop_reason(value: Option<&str>) -> Option<&str> {
    value.filter(|value| matches!(*value, "stop" | "length" | "toolUse" | "error" | "aborted"))
}

fn safe_termination_reason(value: &str) -> &'static str {
    match value {
        "completed" => "completed",
        "failed" => "failed",
        "model_error" => "model_error",
        "wall_timeout" => "wall_timeout",
        "turn_limit" => "turn_limit",
        "aborted" => "aborted",
        "context_limit" => "context_limit",
        "process_exit" => "process_exit",
        "superseded_attempt" => "superseded_attempt",
        _ => "failed",
    }
}

fn safe_tool_name(value: &str) -> &str {
    if value.len() <= 80
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_.:-".contains(character))
    {
        value
    } else {
        "tool"
    }
}

fn bounded(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn bounded_detail(value: &str, limit: usize) -> String {
    let normalized: String = value
        .trim()
        .chars()
        .filter(|character| {
            matches!(*character, '\t' | '\n' | '\r')
                || (!character.is_control() && *character != '\u{7f}')
        })
        .collect();
    if normalized.chars().count() <= limit {
        return normalized;
    }
    normalized
        .chars()
        .take(limit)
        .chain(std::iter::once('…'))
        .collect()
}

fn find_aven_reference(output: &str) -> Option<&str> {
    output
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .find(|word| {
            let Some((prefix, suffix)) = word.split_once('-') else {
                return false;
            };
            !prefix.is_empty()
                && prefix
                    .chars()
                    .next()
                    .is_some_and(|value| value.is_ascii_uppercase())
                && prefix
                    .chars()
                    .all(|value| value.is_ascii_uppercase() || value.is_ascii_digit())
                && suffix.len() >= 3
                && suffix
                    .chars()
                    .all(|value| value.is_ascii_uppercase() || value.is_ascii_digit())
        })
}

fn find_workmux_handle(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Worktree:") {
            let path = Path::new(value.trim());
            let candidate = path.file_name()?.to_str()?;
            if valid_handle(candidate) {
                return Some(candidate.to_string());
            }
        }
        let lowercase = line.to_ascii_lowercase();
        if let Some(index) = lowercase
            .find("handle:")
            .or_else(|| lowercase.find("handle="))
        {
            let candidate = line[index + 7..].split_whitespace().next()?;
            if valid_handle(candidate) {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

fn valid_handle(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || "._-".contains(value))
}

fn saturating_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}
