use std::path::Path;
use std::thread;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::errors::ErrorCategory;
use crate::protocol::{IntakeItem, IntakeItemKind};

use super::DATABASE_QUEUE_CAPACITY;
use super::reader::{DatabaseReaders, event};
use super::records::*;
use super::schema::{OpenTarget, migrate, open_connection};

type WriteAction = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

struct WriteRequest {
    action: WriteAction,
    shutdown: bool,
}

#[derive(Clone)]
pub struct IntagentDatabase {
    sender: mpsc::Sender<WriteRequest>,
    readers: DatabaseReaders,
}

impl IntagentDatabase {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let target = OpenTarget::new(path.as_ref());
        if let Some(directory) = &target.directory {
            std::fs::create_dir_all(directory)?;
        }
        let (sender, receiver) = mpsc::channel(DATABASE_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let actor_target = target.clone();
        thread::Builder::new()
            .name("intagent-database".into())
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
        let observed_at = timestamp(observed_at);
        self.request(move |connection| {
            source_succeeded(connection, &source, checkpoint, &items, &observed_at)
        })
        .await
    }

    pub async fn source_failed(
        &self,
        source: String,
        error: String,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| source_failed(connection, &source, &error, &now))
            .await
    }

    pub async fn claim_next(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<EventRecord>, DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| claim_next(connection, &now))
            .await
    }

    pub async fn recover_interrupted(
        &self,
        stale_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize, DatabaseError> {
        let stale_before = timestamp(stale_before);
        let now = timestamp(now);
        self.request(move |connection| recover_interrupted(connection, &stale_before, &now))
            .await
    }

    pub async fn start_triage_run(
        &self,
        event_id: i64,
        attempt: u32,
        now: DateTime<Utc>,
    ) -> Result<RunId, DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| start_run(connection, event_id, attempt, &now))
            .await
    }

    pub async fn set_triage_run_metadata(
        &self,
        run_id: RunId,
        metadata: RunMetadata,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| set_run_metadata(connection, run_id, metadata, &now))
            .await
    }

    pub async fn record_triage_run_prompt(
        &self,
        run_id: RunId,
        role: String,
        content: String,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| record_prompt(connection, run_id, &role, &content, &now))
            .await
    }

    pub async fn start_turn(
        &self,
        run_id: RunId,
        now: DateTime<Utc>,
    ) -> Result<TurnId, DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| start_turn(connection, run_id, &now))
            .await
    }

    pub async fn finish_turn(
        &self,
        run_id: RunId,
        turn_id: TurnId,
        finish: TurnFinish,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| finish_turn(connection, run_id, turn_id, finish, &now))
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
        let now = timestamp(now);
        self.request(move |connection| {
            start_tool(connection, run_id, turn_id, &name, summary.as_deref(), &now)
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
        let now = timestamp(now);
        self.request(move |connection| {
            finish_span(
                connection,
                "triage_run_steps",
                run_id,
                tool_id.0,
                outcome,
                &now,
            )
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
        let now = timestamp(now);
        self.request(move |connection| start_retry(connection, run_id, turn_id, retry, &now))
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
        let now = timestamp(now);
        self.request(move |connection| {
            finish_retry(connection, run_id, retry_id, outcome, error_category, &now)
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
        let now = timestamp(now);
        self.request(move |connection| start_compaction(connection, run_id, turn_id, &reason, &now))
            .await
    }

    pub async fn finish_compaction(
        &self,
        run_id: RunId,
        compaction_id: CompactionId,
        finish: CompactionFinish,
        now: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| {
            finish_compaction(connection, run_id, compaction_id, finish, &now)
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
        let now = timestamp(now);
        self.request(move |connection| {
            record_assistant_text(connection, run_id, turn_id, &text, &now)
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
        let now = timestamp(now);
        self.request(move |connection| {
            record_reasoning(connection, run_id, turn_id, summary.as_deref(), &now)
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
        let now = timestamp(now);
        self.request(move |connection| finish_run(connection, run_id, finish, conclusion, &now))
            .await
    }

    pub async fn succeed(&self, event_id: i64, now: DateTime<Utc>) -> Result<(), DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| succeed(connection, event_id, &now))
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
        self.request(move |connection| {
            fail(
                connection,
                event_id,
                &error,
                max_attempts,
                retry_base_seconds,
                now,
            )
        })
        .await
    }

    pub async fn retry(&self, event_id: i64, now: DateTime<Utc>) -> Result<bool, DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| retry_event(connection, event_id, &now))
            .await
    }

    pub async fn ignore(&self, event_id: i64, now: DateTime<Utc>) -> Result<bool, DatabaseError> {
        let now = timestamp(now);
        self.request(move |connection| ignore_event(connection, event_id, &now))
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
        let now = timestamp(now);
        self.request(move |connection| {
            record_command(connection, event_id, &executable, exit_code, &output, &now)
        })
        .await
    }

    pub async fn flush(&self) -> Result<(), DatabaseError> {
        self.request(|connection| flush_connection(connection))
            .await
    }

    pub async fn shutdown(&self) -> Result<(), DatabaseError> {
        self.request_with_shutdown(|connection| flush_connection(connection))
            .await
    }

    async fn request<T, F>(&self, action: F) -> Result<T, DatabaseError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DatabaseError> + Send + 'static,
    {
        self.send_request(action, false).await
    }

    async fn request_with_shutdown<T, F>(&self, action: F) -> Result<T, DatabaseError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DatabaseError> + Send + 'static,
    {
        self.send_request(action, true).await
    }

    async fn send_request<T, F>(&self, action: F, shutdown: bool) -> Result<T, DatabaseError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DatabaseError> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        let request = WriteRequest {
            action: Box::new(move |connection| {
                let _ = reply.send(action(connection));
            }),
            shutdown,
        };
        self.sender
            .send(request)
            .await
            .map_err(|_| DatabaseError::ActorClosed)?;
        response.await.map_err(|_| DatabaseError::ActorStopped)?
    }
}

fn write_actor(
    target: OpenTarget,
    mut receiver: mpsc::Receiver<WriteRequest>,
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
    while let Some(request) = receiver.blocking_recv() {
        if request.shutdown {
            receiver.close();
        }
        (request.action)(&mut connection);
        if request.shutdown {
            break;
        }
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
            "INSERT OR IGNORE INTO events(entity_id, source, revision_id, payload, occurred_at,
               observed_at, updated_at, next_dispatch_trigger)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
               CASE
                 WHEN ?2 = 'manual-injection' THEN 'manual_injection'
                 WHEN EXISTS (SELECT 1 FROM events WHERE entity_id = ?1) THEN 'revision'
                 ELSE 'initial'
               END)",
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
               AND (ev.next_attempt_at IS NULL OR julianday(ev.next_attempt_at) <= julianday(?1))
               AND NOT EXISTS (
                 SELECT 1 FROM events prior
                 WHERE prior.entity_id = ev.entity_id
                   AND prior.status IN ('pending', 'retryable', 'processing')
                   AND (julianday(prior.observed_at) < julianday(ev.observed_at) OR
                     (julianday(prior.observed_at) = julianday(ev.observed_at) AND prior.id < ev.id))
               )
             ORDER BY julianday(ev.observed_at), ev.id LIMIT 1",
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
        "UPDATE events SET status = 'processing', attempt_count = attempt_count + 1,
           next_dispatch_trigger = CASE
             WHEN ?3 THEN 'superseding_claim'
             WHEN next_dispatch_trigger != 'unknown' THEN next_dispatch_trigger
             WHEN source = 'manual-injection' THEN 'manual_injection'
             WHEN EXISTS (
               SELECT 1 FROM triage_runs WHERE event_id = events.id
             ) THEN 'backoff_retry'
             WHEN EXISTS (
               SELECT 1 FROM events prior
               WHERE prior.entity_id = events.entity_id AND prior.id != events.id
             ) THEN 'revision'
             ELSE 'initial'
           END,
           next_dispatch_prior_run_id = CASE
             WHEN ?3 THEN
               (SELECT id FROM triage_runs WHERE event_id = events.id ORDER BY id DESC LIMIT 1)
             ELSE COALESCE(
               next_dispatch_prior_run_id,
               (SELECT id FROM triage_runs WHERE event_id = events.id ORDER BY id DESC LIMIT 1)
             )
           END,
           updated_at = ?1 WHERE id = ?2",
        params![now, id, !run_ids.is_empty()],
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
               AND julianday(event.updated_at) <= julianday(?1)",
        )?;
        statement
            .query_map([stale_before], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    interrupt_runs(&transaction, &run_ids, now, "process_exit")?;
    let changed = transaction.execute(
        "UPDATE events SET status = 'retryable', next_attempt_at = ?1,
           last_error = 'triage interrupted by process exit',
           next_dispatch_trigger = 'recovery_retry',
           next_dispatch_prior_run_id = (
             SELECT id FROM triage_runs WHERE event_id = events.id ORDER BY id DESC LIMIT 1
           ),
           updated_at = ?1
         WHERE status = 'processing' AND julianday(updated_at) <= julianday(?2)",
        params![now, stale_before],
    )?;
    transaction.commit()?;
    Ok(changed)
}

fn start_run(
    connection: &mut Connection,
    event_id: i64,
    attempt: u32,
    now: &str,
) -> Result<RunId, DatabaseError> {
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "INSERT INTO triage_runs(event_id, attempt, started_at, last_activity_at,
           telemetry_version, telemetry_completeness, dispatch_sequence, dispatch_trigger,
           dispatch_prior_run_id, dispatch_scheduled_for)
         SELECT ?1, ?2, ?3, ?3, 2, 'partial',
           (SELECT COUNT(*) + 1 FROM triage_runs WHERE event_id = ?1),
           next_dispatch_trigger, next_dispatch_prior_run_id, next_attempt_at
         FROM events WHERE id = ?1",
        params![event_id, attempt, now],
    )?;
    if changed != 1 {
        return Err(DatabaseError::UnknownEvent(event_id));
    }
    let run_id = RunId(transaction.last_insert_rowid());
    transaction.execute(
        "UPDATE events SET next_dispatch_trigger = 'unknown',
           next_dispatch_prior_run_id = NULL WHERE id = ?1",
        [event_id],
    )?;
    transaction.commit()?;
    Ok(run_id)
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
           next_dispatch_trigger = CASE WHEN ?1 = 'retryable' THEN 'backoff_retry'
             ELSE next_dispatch_trigger END,
           next_dispatch_prior_run_id = CASE WHEN ?1 = 'retryable' THEN
             (SELECT id FROM triage_runs WHERE event_id = events.id ORDER BY id DESC LIMIT 1)
             ELSE next_dispatch_prior_run_id END,
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
             next_attempt_at = NULL, last_error = NULL,
             next_dispatch_trigger = 'operator_retry',
             next_dispatch_prior_run_id = (
               SELECT id FROM triage_runs WHERE event_id = events.id ORDER BY id DESC LIMIT 1
             ),
             updated_at = ?1
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
