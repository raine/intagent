use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use rusqlite::{Connection, OptionalExtension, params, types::Type};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::records::*;
use super::schema::{OpenTarget, open_connection};
use super::{DATABASE_QUEUE_CAPACITY, DATABASE_READER_COUNT};

type ReadAction = Box<dyn FnOnce(&Connection) + Send + 'static>;

#[derive(Clone)]
pub struct DatabaseReaders {
    senders: Vec<mpsc::Sender<ReadAction>>,
    next: Arc<AtomicU64>,
}

impl DatabaseReaders {
    pub(super) async fn open(target: OpenTarget) -> Result<Self, DatabaseError> {
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
            next: Arc::new(AtomicU64::new(0)),
        })
    }

    pub async fn source_checkpoint(&self, source: String) -> Result<Value, DatabaseError> {
        self.request(move |connection| source_checkpoint(connection, &source))
            .await
    }

    pub async fn event(&self, id: i64) -> Result<Option<EventRecord>, DatabaseError> {
        self.request(move |connection| event(connection, id)).await
    }

    pub async fn list_events(&self, limit: usize) -> Result<Vec<EventRecord>, DatabaseError> {
        self.request(move |connection| list_events(connection, limit))
            .await
    }

    pub async fn oldest_open_event_at(&self) -> Result<Option<String>, DatabaseError> {
        self.request(oldest_open_event_at).await
    }

    pub async fn status(&self) -> Result<HashMap<String, usize>, DatabaseError> {
        self.request(status).await
    }

    pub async fn source_statuses(&self) -> Result<Vec<SourceStatus>, DatabaseError> {
        self.request(source_statuses).await
    }

    pub async fn triage_run(&self, id: RunId) -> Result<Option<TriageRunRecord>, DatabaseError> {
        self.request(move |connection| triage_run(connection, id))
            .await
    }

    pub async fn triage_run_steps(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageStepRecord>, DatabaseError> {
        self.request(move |connection| triage_run_steps(connection, id))
            .await
    }

    pub async fn list_triage_run_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<TriageRunSummary>, DatabaseError> {
        self.request(move |connection| list_triage_run_summaries(connection, limit))
            .await
    }

    pub async fn recent_triage_run_steps(
        &self,
        id: RunId,
        limit: usize,
    ) -> Result<Vec<TriageStepRecord>, DatabaseError> {
        self.request(move |connection| recent_triage_run_steps(connection, id, limit))
            .await
    }

    pub async fn triage_runs_for_event(
        &self,
        event_id: i64,
    ) -> Result<Vec<TriageRunRecord>, DatabaseError> {
        self.request(move |connection| triage_runs_for_event(connection, event_id))
            .await
    }

    pub async fn triage_run_turns(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageTurnRecord>, DatabaseError> {
        self.request(move |connection| triage_run_turns(connection, id))
            .await
    }

    pub async fn triage_run_retries(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageRetryRecord>, DatabaseError> {
        self.request(move |connection| triage_run_retries(connection, id))
            .await
    }

    pub async fn triage_run_compactions(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageCompactionRecord>, DatabaseError> {
        self.request(move |connection| triage_run_compactions(connection, id))
            .await
    }

    pub async fn triage_run_prompts(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageRunPromptRecord>, DatabaseError> {
        self.request(move |connection| triage_run_prompts(connection, id))
            .await
    }

    pub async fn triage_run_effects(
        &self,
        id: RunId,
    ) -> Result<Vec<TriageEffectRecord>, DatabaseError> {
        self.request(move |connection| triage_run_effects(connection, id))
            .await
    }

    pub async fn integrity_check(&self) -> Result<String, DatabaseError> {
        self.request(integrity_check).await
    }

    async fn request<T, F>(&self, action: F) -> Result<T, DatabaseError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, DatabaseError> + Send + 'static,
    {
        let index = self.next.fetch_add(1, Ordering::Relaxed) as usize % self.senders.len();
        let (reply, response) = oneshot::channel();
        self.senders[index]
            .send(Box::new(move |connection| {
                let _ = reply.send(action(connection));
            }))
            .await
            .map_err(|_| DatabaseError::ActorClosed)?;
        response.await.map_err(|_| DatabaseError::ActorStopped)?
    }
}

fn read_actor(
    target: OpenTarget,
    mut receiver: mpsc::Receiver<ReadAction>,
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
    while let Some(action) = receiver.blocking_recv() {
        action(&connection);
    }
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

pub(super) fn event(
    connection: &Connection,
    id: i64,
) -> Result<Option<EventRecord>, DatabaseError> {
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
               telemetry_completeness, dispatch_sequence, dispatch_trigger,
               dispatch_prior_run_id, dispatch_scheduled_for, conclusion_json,
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
        dispatch_sequence: row.get(16)?,
        dispatch_trigger: row
            .get::<_, Option<String>>(17)?
            .map(|value| DispatchTrigger::parse(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(17, Type::Text, Box::new(error))
            })?,
        dispatch_prior_run_id: row.get(18)?,
        dispatch_scheduled_for: row.get(19)?,
        conclusion: row
            .get::<_, Option<String>>(20)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(20, Type::Text, Box::new(error))
            })?,
        turn_count: row.get(21)?,
        retry_count: row.get(22)?,
        compaction_count: row.get(23)?,
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
           run.telemetry_version, run.telemetry_completeness, run.dispatch_sequence,
           run.dispatch_trigger, run.dispatch_prior_run_id, run.dispatch_scheduled_for,
           run.conclusion_json, run.turn_count, run.retry_count, run.compaction_count,
           (SELECT COUNT(*) FROM triage_run_steps step WHERE step.run_id = run.id)
         FROM triage_runs run ORDER BY run.started_at DESC, run.id DESC LIMIT ?1",
    )?;
    Ok(statement
        .query_map([saturating_i64(limit)], |row| {
            Ok(TriageRunSummary {
                run: triage_run_from_row(row)?,
                step_count: usize::try_from(row.get::<_, i64>(24)?).unwrap_or(usize::MAX),
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
           telemetry_completeness, dispatch_sequence, dispatch_trigger,
           dispatch_prior_run_id, dispatch_scheduled_for, conclusion_json,
           turn_count, retry_count, compaction_count
         FROM triage_runs WHERE event_id = ?1 ORDER BY id",
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
