use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rig_agent::agent::run::AgentRun;
use rig_core::completion::{CompletionError, CompletionResponse, Usage};
use rig_core::message::ToolCall;
use rusqlite::{Connection, params};

use super::driver::{
    AgentObserver, CompletionScope, SpanFinish, ToolOutcome, duration_millis, retry_reason,
};
use super::tools::ToolCallResult;

#[derive(Clone, Debug, Default)]
pub struct CancellationTelemetry {
    state: Arc<Mutex<Option<String>>>,
}

impl CancellationTelemetry {
    pub fn checkpoint(&self, run: &AgentRun) -> serde_json::Result<()> {
        let serialized = serde_json::to_string(run)?;
        match self.state.lock() {
            Ok(mut state) => *state = Some(serialized),
            Err(poisoned) => *poisoned.into_inner() = Some(serialized),
        }
        Ok(())
    }

    pub fn serialized_state(&self) -> Option<String> {
        match self.state.lock() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

#[derive(Clone)]
pub struct PrototypeObserver {
    telemetry: PrototypeTelemetry,
    cancellation: CancellationTelemetry,
}

impl PrototypeObserver {
    pub fn new(telemetry: PrototypeTelemetry, cancellation: CancellationTelemetry) -> Self {
        Self {
            telemetry,
            cancellation,
        }
    }
}

impl AgentObserver for PrototypeObserver {
    type Error = PrototypeObserverError;
    type Retry = TelemetrySpan;
    type Compaction = TelemetrySpan;
    type Tool = ();

    async fn checkpoint(&mut self, run: &AgentRun) -> Result<(), Self::Error> {
        self.cancellation.checkpoint(run)?;
        Ok(())
    }

    async fn turn_started(&mut self, _ordinal: u32) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn turn_completed(
        &mut self,
        _ordinal: u32,
        _response: &CompletionResponse,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn turn_failed(&mut self, _ordinal: u32, _reason: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn retry_started(
        &mut self,
        scope: CompletionScope,
        attempt: usize,
        _max_retries: usize,
        error: &CompletionError,
        delay: Duration,
    ) -> Result<Self::Retry, Self::Error> {
        Ok(self.telemetry.start_retry(
            scope.as_str(),
            attempt,
            retry_reason(error),
            duration_millis(delay),
        )?)
    }

    async fn retry_finished(
        &mut self,
        retry: Self::Retry,
        finish: SpanFinish,
    ) -> Result<(), Self::Error> {
        match finish {
            SpanFinish::Completed => retry.complete(None),
            SpanFinish::Failed => retry.fail(None),
            SpanFinish::Interrupted => drop(retry),
        }
        Ok(())
    }

    async fn compaction_started(
        &mut self,
        reason: &str,
        source_message_count: usize,
    ) -> Result<Self::Compaction, Self::Error> {
        Ok(self
            .telemetry
            .start_compaction(reason, source_message_count)?)
    }

    async fn compaction_finished(
        &mut self,
        compaction: Self::Compaction,
        finish: SpanFinish,
        usage: Option<Usage>,
    ) -> Result<(), Self::Error> {
        match finish {
            SpanFinish::Completed => compaction.complete(usage),
            SpanFinish::Failed => compaction.fail(usage),
            SpanFinish::Interrupted => drop(compaction),
        }
        Ok(())
    }

    async fn tool_started(&mut self, _call: &ToolCall) -> Result<Self::Tool, Self::Error> {
        Ok(())
    }

    async fn tool_finished(
        &mut self,
        _tool: Self::Tool,
        _call: &ToolCall,
        _result: &ToolCallResult,
        _outcome: ToolOutcome,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrototypeObserverError {
    #[error("telemetry database failed: {0}")]
    Telemetry(#[from] rusqlite::Error),
    #[error("agent state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct PrototypeTelemetry {
    connection: Arc<Mutex<Connection>>,
}

impl PrototypeTelemetry {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS triage_run_retries (
               id INTEGER PRIMARY KEY,
               scope TEXT NOT NULL,
               attempt INTEGER NOT NULL,
               reason TEXT NOT NULL,
               delay_ms INTEGER NOT NULL,
               outcome TEXT NOT NULL DEFAULT 'open'
             );
             CREATE TABLE IF NOT EXISTS triage_run_compactions (
               id INTEGER PRIMARY KEY,
               reason TEXT NOT NULL,
               source_message_count INTEGER NOT NULL,
               input_tokens INTEGER,
               output_tokens INTEGER,
               outcome TEXT NOT NULL DEFAULT 'open'
             );",
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn start_retry(
        &self,
        scope: &str,
        attempt: usize,
        reason: &str,
        delay_ms: u64,
    ) -> rusqlite::Result<TelemetrySpan> {
        let connection = self.connection();
        connection.execute(
            "INSERT INTO triage_run_retries (scope, attempt, reason, delay_ms) VALUES (?1, ?2, ?3, ?4)",
            params![scope, to_i64(attempt), reason, to_i64(delay_ms)],
        )?;
        Ok(TelemetrySpan::new(
            self.clone(),
            SpanKind::Retry,
            connection.last_insert_rowid(),
        ))
    }

    pub fn start_compaction(
        &self,
        reason: &str,
        source_message_count: usize,
    ) -> rusqlite::Result<TelemetrySpan> {
        let connection = self.connection();
        connection.execute(
            "INSERT INTO triage_run_compactions (reason, source_message_count) VALUES (?1, ?2)",
            params![reason, to_i64(source_message_count)],
        )?;
        Ok(TelemetrySpan::new(
            self.clone(),
            SpanKind::Compaction,
            connection.last_insert_rowid(),
        ))
    }

    pub fn retry_rows(&self) -> rusqlite::Result<Vec<RetryRow>> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT scope, attempt, reason, delay_ms, outcome FROM triage_run_retries ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok(RetryRow {
                    scope: row.get(0)?,
                    attempt: row.get(1)?,
                    reason: row.get(2)?,
                    delay_ms: row.get(3)?,
                    outcome: row.get(4)?,
                })
            })?
            .collect()
    }

    pub fn compaction_rows(&self) -> rusqlite::Result<Vec<CompactionRow>> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT reason, source_message_count, input_tokens, output_tokens, outcome FROM triage_run_compactions ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok(CompactionRow {
                    reason: row.get(0)?,
                    source_message_count: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    outcome: row.get(4)?,
                })
            })?
            .collect()
    }

    fn connection(&self) -> MutexGuard<'_, Connection> {
        match self.connection.lock() {
            Ok(connection) => connection,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn finish(&self, kind: SpanKind, id: i64, outcome: &str, usage: Option<Usage>) {
        let connection = self.connection();
        match kind {
            SpanKind::Retry => {
                let _ = connection.execute(
                    "UPDATE triage_run_retries SET outcome = ?1 WHERE id = ?2 AND outcome = 'open'",
                    params![outcome, id],
                );
            }
            SpanKind::Compaction => {
                let (input_tokens, output_tokens) = usage
                    .filter(Usage::has_values)
                    .map(|usage| {
                        (
                            Some(to_i64(usage.input_tokens)),
                            Some(to_i64(usage.output_tokens)),
                        )
                    })
                    .unwrap_or((None, None));
                let _ = connection.execute(
                    "UPDATE triage_run_compactions
                     SET outcome = ?1, input_tokens = ?2, output_tokens = ?3
                     WHERE id = ?4 AND outcome = 'open'",
                    params![outcome, input_tokens, output_tokens, id],
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SpanKind {
    Retry,
    Compaction,
}

pub struct TelemetrySpan {
    telemetry: PrototypeTelemetry,
    kind: SpanKind,
    id: i64,
    finished: bool,
}

impl TelemetrySpan {
    fn new(telemetry: PrototypeTelemetry, kind: SpanKind, id: i64) -> Self {
        Self {
            telemetry,
            kind,
            id,
            finished: false,
        }
    }

    pub fn complete(mut self, usage: Option<Usage>) {
        self.telemetry
            .finish(self.kind, self.id, "completed", usage);
        self.finished = true;
    }

    pub fn fail(mut self, usage: Option<Usage>) {
        self.telemetry.finish(self.kind, self.id, "failed", usage);
        self.finished = true;
    }
}

impl Drop for TelemetrySpan {
    fn drop(&mut self) {
        if !self.finished {
            self.telemetry
                .finish(self.kind, self.id, "interrupted", None);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct RetryRow {
    pub scope: String,
    pub attempt: i64,
    pub reason: String,
    pub delay_ms: i64,
    pub outcome: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct CompactionRow {
    pub reason: String,
    pub source_message_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub outcome: String,
}

fn to_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}
