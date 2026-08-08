use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::agent::rig_runner::{TriageError, TriageRunner};
use crate::config::{IntakeConfig, SourceConfig};
use crate::database::{ErrorCategory, EventRecord, IntakeDatabase};
use crate::errors::{classify_message, public_error};
use crate::source_runner::{SourceRunnerError, poll_source};

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
    pub fn new(config: IntakeConfig, database: IntakeDatabase, runner: R) -> Self {
        Self {
            inner: Arc::new(MonitorInner {
                config,
                database,
                runner,
                stopping: AtomicBool::new(false),
                schedule_cancellation: CancellationToken::new(),
                next_recovery_at: AtomicI64::new(0),
            }),
        }
    }

    pub fn stop(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.schedule_cancellation.cancel();
        tracing::info!(target: "intake::monitor", "stop requested");
    }

    pub async fn check(&self) -> Result<CheckResult> {
        tracing::info!(
            target: "intake::monitor",
            mode = "check",
            source_count = self.inner.config.sources.len(),
            "monitor started"
        );

        let result = self.check_inner().await;
        if result.is_err() {
            tracing::error!(target: "intake::monitor", mode = "check", "monitor failed");
        }
        tracing::info!(target: "intake::monitor", mode = "check", "monitor stopped");
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
        tracing::debug!(
            target: "intake::monitor",
            observed,
            "source polls completed"
        );

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
        tracing::info!(
            target: "intake::terminal",
            "Watching {}. Press Ctrl-C to stop.",
            if schedules.is_empty() {
                "no configured sources"
            } else {
                &schedules
            }
        );
        tracing::info!(
            target: "intake::monitor",
            mode = "watch",
            source_count = self.inner.config.sources.len(),
            "monitor started"
        );

        let result = self.watch_inner().await;
        if result.is_err() {
            tracing::error!(target: "intake::monitor", mode = "watch", "monitor failed");
        }
        tracing::info!(target: "intake::monitor", mode = "watch", "monitor stopped");
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
        tracing::debug!(
            target: "intake::monitor",
            source = source.name,
            "source poll started"
        );
        let result =
            poll_source(source, &self.inner.config, &self.inner.database, Utc::now()).await;
        match result {
            Ok(observed) => {
                tracing::info!(
                    target: "intake::monitor",
                    source = source.name,
                    queued = observed,
                    duration_ms = duration_millis(started.elapsed()),
                    "source poll succeeded"
                );
                Ok(observed)
            }
            Err(error) => {
                tracing::error!(
                    target: "intake::monitor",
                    source = source.name,
                    duration_ms = duration_millis(started.elapsed()),
                    failure_category = source_failure_category(&error),
                    "source poll failed"
                );
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
                Ok(observed) if observed > 0 => tracing::info!(
                    target: "intake::terminal",
                    "{}: queued {} event{}",
                    source.name,
                    observed,
                    if observed == 1 { "" } else { "s" }
                ),
                Ok(_) => {}
                Err(error) => {
                    let message = public_error(Some(&error.to_string()))
                        .unwrap_or_else(|| "Operation failed".into());
                    tracing::error!(
                        target: "intake::terminal::error",
                        "{}: {}",
                        source.name,
                        message
                    );
                }
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
                Some(_) => tracing::error!(
                    target: "intake::terminal::error",
                    "event {}: triage failed",
                    event.id
                ),
                None => tracing::info!(
                    target: "intake::terminal",
                    "event {}: handled {}",
                    event.id,
                    event.title
                ),
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
        tracing::info!(
            target: "intake::monitor",
            event_id = event.id,
            attempt = event.attempt_count,
            source = event.source,
            "triage started"
        );
        let result = self
            .inner
            .runner
            .run(event.clone(), CancellationToken::new())
            .await;
        match result {
            Ok(()) => {
                self.inner.database.succeed(event.id, Utc::now()).await?;
                tracing::info!(
                    target: "intake::monitor",
                    event_id = event.id,
                    attempt = event.attempt_count,
                    duration_ms = duration_millis(started.elapsed()),
                    "triage succeeded"
                );
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
                tracing::error!(
                    target: "intake::monitor",
                    event_id = event.id,
                    attempt = event.attempt_count,
                    duration_ms = duration_millis(started.elapsed()),
                    failure_category = safe_error_category(&error),
                    retry = failed.as_ref().is_some_and(|record| record.status == crate::database::EventStatus::Retryable),
                    "triage failed"
                );
                Ok(Some(message))
            }
        }
    }

    fn is_stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::Acquire)
    }
}

fn source_failure_category(error: &SourceRunnerError) -> &'static str {
    match error {
        SourceRunnerError::RequestTooLarge | SourceRunnerError::OutputTooLarge { .. } => "limit",
        SourceRunnerError::Spawn(_) => "spawn",
        SourceRunnerError::Timeout => "timeout",
        SourceRunnerError::Exit { .. } => "exit",
        SourceRunnerError::Stream(_) | SourceRunnerError::Utf8(_) => "stream",
        SourceRunnerError::Json(_)
        | SourceRunnerError::Schema(_)
        | SourceRunnerError::ItemLimit { .. } => "protocol",
        SourceRunnerError::Database(_) | SourceRunnerError::FailureRecording { .. } => "database",
        SourceRunnerError::Poll(message) => safe_message_category(message),
    }
}

fn safe_message_category(message: &str) -> &'static str {
    match classify_message(message) {
        category @ (ErrorCategory::Authentication
        | ErrorCategory::RateLimit
        | ErrorCategory::Timeout
        | ErrorCategory::Connection) => category.as_str(),
        _ => ErrorCategory::Unknown.as_str(),
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

pub fn safe_error_category(error: &TriageError) -> &'static str {
    let category = error.category();
    if category != ErrorCategory::Unknown {
        return category.as_str();
    }
    match classify_message(&error.to_string()) {
        category @ (ErrorCategory::Authentication
        | ErrorCategory::RateLimit
        | ErrorCategory::Timeout
        | ErrorCategory::Connection) => category.as_str(),
        _ => ErrorCategory::Unknown.as_str(),
    }
}
