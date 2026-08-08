use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::agent::rig_runner::{TriageError, TriageRunner};
use crate::config::{IntakeConfig, SourceConfig};
use crate::database::{ErrorCategory, EventRecord, IntakeDatabase};
use crate::logging::DurableLogStore;
use crate::source_runner::poll_source;
use crate::terminal::{stderr_line, stdout_line};

const TRIAGE_RECOVERY_GRACE_SECONDS: i64 = 60;
const RECOVERY_INTERVAL_MILLISECONDS: i64 = 60_000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheckResult {
    pub observed: usize,
    pub handled: usize,
    pub errors: Vec<String>,
}

pub struct IntakeMonitor<R> {
    inner: Arc<MonitorInner<R>>,
}

struct MonitorInner<R> {
    config: IntakeConfig,
    database: IntakeDatabase,
    runner: R,
    logs: DurableLogStore,
    stopping: AtomicBool,
    schedule_cancellation: CancellationToken,
    next_recovery_at: AtomicI64,
}

impl<R> Clone for IntakeMonitor<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R> IntakeMonitor<R>
where
    R: TriageRunner + Send + Sync + 'static,
{
    pub fn new(
        config: IntakeConfig,
        database: IntakeDatabase,
        runner: R,
        logs: DurableLogStore,
    ) -> Self {
        Self {
            inner: Arc::new(MonitorInner {
                config,
                database,
                runner,
                logs,
                stopping: AtomicBool::new(false),
                schedule_cancellation: CancellationToken::new(),
                next_recovery_at: AtomicI64::new(0),
            }),
        }
    }

    pub fn stop(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.schedule_cancellation.cancel();
        let logs = self.inner.logs.clone();
        tokio::spawn(async move {
            logs.monitor("stop_requested", json!({})).await;
        });
    }

    pub async fn check(&self) -> Result<CheckResult> {
        self.inner
            .logs
            .monitor(
                "process_start",
                json!({
                    "mode": "check",
                    "pid": std::process::id(),
                    "sources": self.inner.config.sources.iter().map(|source| &source.name).collect::<Vec<_>>(),
                    "queue": self.inner.database.readers().status().await?,
                }),
            )
            .await;

        let result = self.check_inner().await;
        if let Err(error) = &result {
            self.inner
                .logs
                .monitor(
                    "operational_error",
                    json!({ "operation": "check", "error": error.to_string() }),
                )
                .await;
        }
        self.inner
            .logs
            .monitor(
                "process_stop",
                json!({
                    "mode": "check",
                    "queue": self.inner.database.readers().status().await.unwrap_or_default(),
                }),
            )
            .await;
        result
    }

    async fn check_inner(&self) -> Result<CheckResult> {
        let mut polls = JoinSet::new();
        for source in self.inner.config.sources.clone() {
            let monitor = self.clone();
            polls.spawn(async move { monitor.poll(&source).await });
        }
        let mut observed = 0;
        let mut errors = Vec::new();
        while let Some(result) = polls.join_next().await {
            match result {
                Ok(Ok(count)) => observed += count,
                Ok(Err(error)) => errors.push(error.to_string()),
                Err(error) => errors.push(error.to_string()),
            }
        }
        self.inner
            .logs
            .monitor(
                "queue_state",
                json!({
                    "reason": "polls_complete",
                    "observed": observed,
                    "counts": self.inner.database.readers().status().await?,
                }),
            )
            .await;

        let mut handled = 0;
        while !self.is_stopping() {
            let Some(event) = self.claim_next().await? else {
                break;
            };
            match self.triage(event.clone()).await? {
                Some(error) => errors.push(format!("event {}: {error}", event.id)),
                None => handled += 1,
            }
        }
        Ok(CheckResult {
            observed,
            handled,
            errors,
        })
    }

    pub async fn watch(&self) -> Result<()> {
        let schedules = self
            .inner
            .config
            .sources
            .iter()
            .map(|source| {
                format!(
                    "{} every {} second{}",
                    source.name,
                    source.interval_seconds,
                    if source.interval_seconds == 1 {
                        ""
                    } else {
                        "s"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        stdout_line(&format!(
            "Watching {}. Press Ctrl-C to stop.",
            if schedules.is_empty() {
                "no configured sources"
            } else {
                &schedules
            }
        ));
        self.inner
            .logs
            .monitor(
                "process_start",
                json!({
                    "mode": "watch",
                    "pid": std::process::id(),
                    "schedules": schedules,
                    "sources": self.inner.config.sources.iter().map(|source| &source.name).collect::<Vec<_>>(),
                    "queue": self.inner.database.readers().status().await?,
                }),
            )
            .await;

        let result = self.watch_inner().await;
        if let Err(error) = &result {
            self.inner
                .logs
                .monitor(
                    "operational_error",
                    json!({ "operation": "watch", "error": error.to_string() }),
                )
                .await;
        }
        self.inner
            .logs
            .monitor(
                "process_stop",
                json!({
                    "mode": "watch",
                    "queue": self.inner.database.readers().status().await.unwrap_or_default(),
                }),
            )
            .await;
        result
    }

    async fn watch_inner(&self) -> Result<()> {
        let mut tasks = JoinSet::new();
        for source in self.inner.config.sources.clone() {
            let monitor = self.clone();
            tasks.spawn(async move {
                monitor.poll_loop(source).await;
                Ok::<(), anyhow::Error>(())
            });
        }
        let monitor = self.clone();
        tasks.spawn(async move { monitor.worker_loop().await });

        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tasks.abort_all();
                    return Err(error);
                }
                Err(error) => {
                    tasks.abort_all();
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    async fn poll(&self, source: &SourceConfig) -> Result<usize> {
        let started = std::time::Instant::now();
        self.inner
            .logs
            .monitor("source_poll_start", json!({ "source": source.name }))
            .await;
        let result =
            poll_source(source, &self.inner.config, &self.inner.database, Utc::now()).await;
        match result {
            Ok(observed) => {
                self.inner
                    .logs
                    .monitor(
                        "source_poll_succeeded",
                        json!({
                            "source": source.name,
                            "queued": observed,
                            "durationMs": duration_millis(started.elapsed()),
                            "queue": self.inner.database.readers().status().await.unwrap_or_default(),
                        }),
                    )
                    .await;
                Ok(observed)
            }
            Err(error) => {
                self.inner
                    .logs
                    .monitor(
                        "source_poll_failed",
                        json!({
                            "source": source.name,
                            "durationMs": duration_millis(started.elapsed()),
                            "error": error.to_string(),
                        }),
                    )
                    .await;
                Err(error.into())
            }
        }
    }

    async fn poll_loop(&self, source: SourceConfig) {
        let mut first = true;
        while !self.is_stopping() {
            if !first {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(source.interval_seconds)) => {}
                    _ = self.inner.schedule_cancellation.cancelled() => break,
                }
            }
            first = false;
            if self.is_stopping() {
                break;
            }
            match self.poll(&source).await {
                Ok(observed) if observed > 0 => stdout_line(&format!(
                    "{}: queued {} event{}",
                    source.name,
                    observed,
                    if observed == 1 { "" } else { "s" }
                )),
                Ok(_) => {}
                Err(error) => stderr_line(&format!("{}: {error}", source.name)),
            }
        }
    }

    async fn worker_loop(&self) -> Result<()> {
        while !self.is_stopping() {
            let Some(event) = self.claim_next().await? else {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    _ = self.inner.schedule_cancellation.cancelled() => break,
                }
                continue;
            };
            match self.triage(event.clone()).await? {
                Some(error) => stderr_line(&format!("event {}: {error}", event.id)),
                None => stdout_line(&format!("event {}: handled {}", event.id, event.title)),
            }
        }
        Ok(())
    }

    async fn claim_next(&self) -> Result<Option<EventRecord>> {
        let now = Utc::now();
        let now_millis = now.timestamp_millis();
        let next = self.inner.next_recovery_at.load(Ordering::Acquire);
        if now_millis >= next
            && self
                .inner
                .next_recovery_at
                .compare_exchange(
                    next,
                    now_millis + RECOVERY_INTERVAL_MILLISECONDS,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            let stale_before = now
                - ChronoDuration::minutes(self.inner.config.triage.timeout_minutes as i64)
                - ChronoDuration::seconds(TRIAGE_RECOVERY_GRACE_SECONDS);
            self.inner
                .database
                .recover_interrupted(stale_before, now)
                .await?;
        }
        Ok(self.inner.database.claim_next(now).await?)
    }

    async fn triage(&self, event: EventRecord) -> Result<Option<String>> {
        let started = std::time::Instant::now();
        self.inner
            .logs
            .monitor(
                "triage_start",
                json!({
                    "eventId": event.id,
                    "attempt": event.attempt_count,
                    "source": event.source,
                    "queue": self.inner.database.readers().status().await?,
                }),
            )
            .await;
        let result = self
            .inner
            .runner
            .run(event.clone(), CancellationToken::new())
            .await;
        match result {
            Ok(()) => {
                self.inner.database.succeed(event.id, Utc::now()).await?;
                self.inner
                    .logs
                    .monitor(
                        "triage_succeeded",
                        json!({
                            "eventId": event.id,
                            "attempt": event.attempt_count,
                            "durationMs": duration_millis(started.elapsed()),
                            "queue": self.inner.database.readers().status().await?,
                        }),
                    )
                    .await;
                Ok(None)
            }
            Err(error) => {
                let message = error.to_string();
                self.inner
                    .database
                    .fail(
                        event.id,
                        message.clone(),
                        self.inner.config.triage.max_attempts as u32,
                        self.inner.config.triage.retry_base_seconds,
                        Utc::now(),
                    )
                    .await?;
                let failed = self.inner.database.readers().event(event.id).await?;
                self.inner
                    .logs
                    .monitor(
                        "triage_failed",
                        json!({
                            "eventId": event.id,
                            "attempt": event.attempt_count,
                            "durationMs": duration_millis(started.elapsed()),
                            "failureCategory": safe_error_category(&error),
                            "outcome": failed.as_ref().map(|record| record.status),
                            "retry": failed.as_ref().is_some_and(|record| record.status == crate::database::EventStatus::Retryable),
                            "nextAttemptAt": failed.and_then(|record| record.next_attempt_at),
                            "queue": self.inner.database.readers().status().await?,
                        }),
                    )
                    .await;
                Ok(Some(message))
            }
        }
    }

    fn is_stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::Acquire)
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn category_name(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Authentication => "authentication",
        ErrorCategory::RateLimit => "rate_limit",
        ErrorCategory::Timeout => "timeout",
        ErrorCategory::Connection => "connection",
        ErrorCategory::NotFound => "not_found",
        ErrorCategory::ModelUnavailable => "model_unavailable",
        ErrorCategory::ContextLimit => "context_limit",
        ErrorCategory::TurnLimit => "turn_limit",
        ErrorCategory::Aborted => "aborted",
        ErrorCategory::Interrupted => "interrupted",
        ErrorCategory::ToolFailure => "tool_failure",
        ErrorCategory::Unknown => "unknown",
    }
}

pub fn safe_error_category(error: &TriageError) -> &'static str {
    let category = error.category();
    if category != ErrorCategory::Unknown {
        return category_name(category);
    }
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("auth") || message.contains("credential") {
        "authentication"
    } else if message.contains("rate limit") || message.contains("429") {
        "rate_limit"
    } else if message.contains("timeout") || message.contains("timed out") {
        "timeout"
    } else if message.contains("connection") || message.contains("socket") {
        "connection"
    } else {
        "unknown"
    }
}
