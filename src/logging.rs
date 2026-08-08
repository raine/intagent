use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::database::{ErrorCategory, EventRecord, RunMetadata, RunOutcome, timestamp};

pub const LOG_QUEUE_CAPACITY: usize = 64;
pub const MAX_LOG_STRING_BYTES: usize = 256 * 1024;
pub const MAX_LOG_RECORD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LOG_DEPTH: usize = 24;
pub const MAX_LOG_ARRAY_ITEMS: usize = 10_000;
pub const MAX_LOG_OBJECT_KEYS: usize = 2_000;

static LOG_FILE_ID: AtomicU64 = AtomicU64::new(1);

type Redactor = Arc<dyn Fn(&str) -> String + Send + Sync>;
type WarningSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogWriteOutcome {
    Written,
    Failed,
}

impl LogWriteOutcome {
    pub fn is_written(self) -> bool {
        self == Self::Written
    }
}

#[derive(Clone)]
pub struct DurableLogStore {
    directory: Arc<PathBuf>,
    sender: mpsc::Sender<LogOperation>,
    redact: Redactor,
    warnings: WarningSink,
    warned: Arc<Mutex<HashSet<PathBuf>>>,
}

impl DurableLogStore {
    pub fn new(
        directory: impl Into<PathBuf>,
        redact: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        Self::with_warning_sink(directory, redact, |_| {
            tracing::warn!(
                target: "intake::logging",
                log_kind = "attempt",
                "structured log write failed"
            );
            tracing::warn!(
                target: "intake::terminal::error",
                "Warning: structured attempt logging is unavailable."
            );
        })
    }

    pub fn with_warning_sink(
        directory: impl Into<PathBuf>,
        redact: impl Fn(&str) -> String + Send + Sync + 'static,
        warning: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        let directory = directory.into();
        let _ = fs::create_dir_all(&directory);
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
        let (sender, receiver) = mpsc::channel(LOG_QUEUE_CAPACITY);
        let _ = thread::Builder::new()
            .name("intake-jsonl-log".into())
            .spawn(move || log_actor(receiver));
        Self {
            directory: Arc::new(directory),
            sender,
            redact: Arc::new(redact),
            warnings: Arc::new(warning),
            warned: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub async fn monitor<T: Serialize>(&self, event_type: &str, details: T) -> LogWriteOutcome {
        let mut record = object_value(details);
        record.insert("timestamp".into(), Value::String(timestamp(Utc::now())));
        record.insert("type".into(), Value::String(event_type.to_string()));
        self.append(self.directory.join("monitor.jsonl"), Value::Object(record))
            .await
    }

    pub fn triage(&self, event: &EventRecord) -> TriageRunLog {
        let source = filename_part(&event.source);
        let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let sequence = LOG_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let filename = format!(
            "triage-event-{}-attempt-{}-{}-{timestamp}-{sequence:08x}.jsonl",
            event.id, event.attempt_count, source
        );
        TriageRunLog {
            path: self.directory.join("triage").join(filename),
            event_id: event.id,
            attempt: event.attempt_count,
            source: event.source.clone(),
            kind: event.kind.clone(),
            occurred_at: event.occurred_at.clone(),
            observed_at: event.observed_at.clone(),
            started_at: Instant::now(),
            store: self.clone(),
            recording_failed: false,
        }
    }

    pub async fn append(&self, path: PathBuf, record: Value) -> LogWriteOutcome {
        let normalized = normalize(record, &*self.redact, 0);
        let line = bounded_record(normalized, &*self.redact);
        let (reply, response) = oneshot::channel();
        if self
            .sender
            .send(LogOperation {
                path: path.clone(),
                line,
                reply,
            })
            .await
            .is_err()
        {
            self.warn(&path, "logging actor is unavailable");
            return LogWriteOutcome::Failed;
        }
        match response.await {
            Ok(Ok(())) => LogWriteOutcome::Written,
            Ok(Err(error)) => {
                self.warn(&path, &error.to_string());
                LogWriteOutcome::Failed
            }
            Err(_) => {
                self.warn(&path, "logging actor stopped before replying");
                LogWriteOutcome::Failed
            }
        }
    }

    fn warn(&self, path: &Path, error: &str) {
        let first = match self.warned.lock() {
            Ok(mut warned) => warned.insert(path.to_path_buf()),
            Err(poisoned) => poisoned.into_inner().insert(path.to_path_buf()),
        };
        if !first {
            return;
        }
        let message = (self.redact)(error);
        (self.warnings)(&format!(
            "warning: intake logging failed for {}: {message}\n",
            path.display()
        ));
    }
}

pub struct TriageRunLog {
    path: PathBuf,
    event_id: i64,
    attempt: u32,
    source: String,
    kind: String,
    occurred_at: String,
    observed_at: String,
    started_at: Instant,
    store: DurableLogStore,
    recording_failed: bool,
}

impl TriageRunLog {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn recording_failed(&self) -> bool {
        self.recording_failed
    }

    pub async fn start(&mut self) {
        self.started_at = Instant::now();
        self.record(
            "run_start",
            json!({
                "event": {
                    "source": self.source,
                    "kind": self.kind,
                    "occurredAt": self.occurred_at,
                    "observedAt": self.observed_at,
                }
            }),
        )
        .await;
    }

    pub async fn metadata(&mut self, metadata: &RunMetadata, tools: &[String]) {
        self.record(
            "session_metadata",
            json!({
                "model": {
                    "id": metadata.model_id,
                    "provider": metadata.model_provider,
                    "contextWindow": metadata.context_window,
                    "maxTokens": metadata.max_tokens,
                },
                "thinkingLevel": metadata.thinking_level,
                "tools": tools.iter().map(|tool| safe_tool_name(tool)).collect::<Vec<_>>(),
                "telemetryVersion": 2,
            }),
        )
        .await;
    }

    pub async fn prompt(&mut self, value: &str) {
        self.record("prompt_submitted", json!({ "byteLength": value.len() }))
            .await;
    }

    pub async fn start_turn(&mut self, ordinal: u32) {
        self.record("turn_start", json!({ "ordinal": ordinal }))
            .await;
    }

    pub async fn finish_turn<T: Serialize>(&mut self, ordinal: u32, details: T) {
        self.record(
            "turn_end",
            json!({ "ordinal": ordinal, "details": details }),
        )
        .await;
    }

    pub async fn start_tool(&mut self, name: &str, summary: Option<&str>) {
        self.record(
            "tool_execution_start",
            json!({ "toolName": safe_tool_name(name), "summary": summary }),
        )
        .await;
    }

    pub async fn finish_tool(&mut self, name: &str, failed: bool, diagnostic: Option<&str>) {
        self.record(
            "tool_execution_end",
            json!({
                "toolName": safe_tool_name(name),
                "isError": failed,
                "diagnostic": failed.then(|| bounded_string(
                    diagnostic.unwrap_or("tool failed without a diagnostic"),
                    16 * 1024,
                    &*self.store.redact,
                )),
            }),
        )
        .await;
    }

    pub async fn start_retry(
        &mut self,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        category: Option<ErrorCategory>,
    ) {
        self.record(
            "auto_retry_start",
            json!({
                "attempt": attempt,
                "maxAttempts": max_attempts,
                "delayMs": delay_ms,
                "errorCategory": category.map(ErrorCategory::as_str),
            }),
        )
        .await;
    }

    pub async fn finish_retry(&mut self, succeeded: bool) {
        self.record(
            "auto_retry_end",
            json!({ "outcome": if succeeded { "succeeded" } else { "failed" } }),
        )
        .await;
    }

    pub async fn start_compaction(&mut self, reason: &str) {
        self.record("compaction_start", json!({ "reason": reason }))
            .await;
    }

    pub async fn finish_compaction<T: Serialize>(&mut self, details: T) {
        self.record("compaction_end", details).await;
    }

    pub async fn assistant(&mut self, text: &str) {
        self.record("assistant_text", json!({ "text": text })).await;
    }

    pub async fn reasoning(&mut self, summary: Option<&str>) {
        self.record("reasoning", json!({ "summary": summary }))
            .await;
    }

    pub async fn finish(
        &mut self,
        outcome: RunOutcome,
        failure_category: Option<ErrorCategory>,
        termination_reason: &str,
    ) {
        self.record(
            "run_end",
            json!({
                "outcome": match outcome {
                    RunOutcome::Succeeded => "succeeded",
                    RunOutcome::Failed => "failed",
                    RunOutcome::Interrupted => "interrupted",
                },
                "durationMs": saturating_millis(self.started_at.elapsed()),
                "failureCategory": failure_category.map(ErrorCategory::as_str),
                "terminationReason": safe_termination_reason(termination_reason),
                "recordingFailed": self.recording_failed,
            }),
        )
        .await;
    }

    async fn record<T: Serialize>(&mut self, event_type: &str, details: T) {
        let mut record = object_value(details);
        record.insert("timestamp".into(), Value::String(timestamp(Utc::now())));
        record.insert("type".into(), Value::String(event_type.to_string()));
        record.insert("eventId".into(), Value::Number(self.event_id.into()));
        record.insert("attempt".into(), Value::Number(self.attempt.into()));
        if self
            .store
            .append(self.path.clone(), Value::Object(record))
            .await
            == LogWriteOutcome::Failed
        {
            self.recording_failed = true;
        }
    }
}

struct LogOperation {
    path: PathBuf,
    line: Vec<u8>,
    reply: oneshot::Sender<Result<(), std::io::Error>>,
}

fn log_actor(mut receiver: mpsc::Receiver<LogOperation>) {
    while let Some(operation) = receiver.blocking_recv() {
        let result = append_line(&operation.path, &operation.line);
        let _ = operation.reply.send(result);
    }
}

fn append_line(path: &Path, line: &[u8]) -> Result<(), std::io::Error> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("log path has no parent directory"))?;
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(line)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn bounded_record(value: Value, redact: &dyn Fn(&str) -> String) -> Vec<u8> {
    let line = serde_json::to_vec(&value).unwrap_or_else(|_| b"null".to_vec());
    if line.len() <= MAX_LOG_RECORD_BYTES {
        return line;
    }
    let timestamp = value.get("timestamp").cloned().unwrap_or(Value::Null);
    let event_type = value.get("type").cloned().unwrap_or(Value::Null);
    serde_json::to_vec(&json!({
        "timestamp": timestamp,
        "type": event_type,
        "value": bounded_string(&String::from_utf8_lossy(&line), MAX_LOG_STRING_BYTES, redact),
        "recordTruncated": true,
    }))
    .unwrap_or_else(|_| b"null".to_vec())
}

fn normalize(value: Value, redact: &dyn Fn(&str) -> String, depth: usize) -> Value {
    match value {
        Value::String(value) => Value::String(bounded_string(&value, MAX_LOG_STRING_BYTES, redact)),
        Value::Array(values) => {
            if depth >= MAX_LOG_DEPTH {
                return Value::String("[TRUNCATED: maximum depth reached]".into());
            }
            let original_len = values.len();
            let mut normalized = values
                .into_iter()
                .take(MAX_LOG_ARRAY_ITEMS)
                .map(|value| normalize(value, redact, depth + 1))
                .collect::<Vec<_>>();
            if original_len > MAX_LOG_ARRAY_ITEMS {
                normalized.push(Value::String(format!(
                    "[TRUNCATED: {} array items]",
                    original_len - MAX_LOG_ARRAY_ITEMS
                )));
            }
            Value::Array(normalized)
        }
        Value::Object(values) => {
            if depth >= MAX_LOG_DEPTH {
                return Value::String("[TRUNCATED: maximum depth reached]".into());
            }
            let original_len = values.len();
            let mut normalized = values
                .into_iter()
                .take(MAX_LOG_OBJECT_KEYS)
                .map(|(key, value)| (key, normalize(value, redact, depth + 1)))
                .collect::<Map<_, _>>();
            if original_len > MAX_LOG_OBJECT_KEYS {
                normalized.insert(
                    "valueTruncated".into(),
                    Value::String(format!(
                        "{} object keys",
                        original_len - MAX_LOG_OBJECT_KEYS
                    )),
                );
            }
            Value::Object(normalized)
        }
        value => value,
    }
}

fn bounded_string(value: &str, limit: usize, redact: &dyn Fn(&str) -> String) -> String {
    let filtered = redact(value);
    if filtered.len() <= limit {
        return filtered;
    }
    let mut end = limit;
    while !filtered.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[TRUNCATED: {} bytes total]",
        &filtered[..end],
        filtered.len()
    )
}

fn object_value<T: Serialize>(value: T) -> Map<String, Value> {
    match serde_json::to_value(value).unwrap_or(Value::Null) {
        Value::Object(value) => value,
        value => Map::from_iter([("details".into(), value)]),
    }
}

fn filename_part(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            result.push(character.to_ascii_lowercase());
            separator = false;
        } else if !result.is_empty() && !separator {
            result.push('-');
            separator = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    result
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

fn safe_termination_reason(value: &str) -> Option<&str> {
    matches!(
        value,
        "completed"
            | "failed"
            | "model_error"
            | "wall_timeout"
            | "turn_limit"
            | "aborted"
            | "context_limit"
            | "process_exit"
            | "superseded_attempt"
            | "legacy_interruption"
    )
    .then_some(value)
}

fn saturating_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
