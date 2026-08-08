use chrono::{TimeZone, Utc};
use intake::database::{
    CompactionFinish, DATABASE_QUEUE_CAPACITY, DatabaseError, DispatchTrigger, ErrorCategory,
    EventStatus, IntakeDatabase, MIGRATIONS, ReportedUsage, RetryStart, RunFinish, RunOutcome,
    SpanOutcome, TurnFinish, reported_usage, timestamp,
};
use intake::protocol::{IntakeItem, IntakeItemKind};
use rig_core::completion::Usage;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

const SCHEMA_FIXTURES: [&str; 10] = [
    include_str!("fixtures/database/schema-v0.sql"),
    include_str!("fixtures/database/schema-v1.sql"),
    include_str!("fixtures/database/schema-v2.sql"),
    include_str!("fixtures/database/schema-v3.sql"),
    include_str!("fixtures/database/schema-v4.sql"),
    include_str!("fixtures/database/schema-v5.sql"),
    include_str!("fixtures/database/schema-v6.sql"),
    include_str!("fixtures/database/schema-v7.sql"),
    include_str!("fixtures/database/schema-v8.sql"),
    include_str!("fixtures/database/schema-v9.sql"),
];

fn at(value: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn item(revision_id: &str) -> IntakeItem {
    IntakeItem {
        entity_id: "mail:thread-1".into(),
        revision_id: revision_id.into(),
        kind: IntakeItemKind::Email,
        title: "Needs attention".into(),
        body: "Complete content".into(),
        url: None,
        occurred_at: "2026-08-03T10:00:00.000Z".into(),
        metadata: Map::from_iter([("threadId".into(), json!("thread-1"))]),
    }
}

#[tokio::test]
async fn commits_source_checkpoint_and_serializes_entity_queue() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");
    assert_eq!(
        database
            .source_succeeded(
                "mail".into(),
                json!({"state": "a"}),
                vec![item("message-1"), item("message-2")],
                at("2026-08-03T10:01:00.000Z"),
            )
            .await
            .expect("source commit"),
        2
    );
    assert_eq!(
        database
            .source_succeeded(
                "mail-new".into(),
                json!({"state": "b"}),
                vec![item("message-1")],
                at("2026-08-03T10:02:00.000Z"),
            )
            .await
            .expect("duplicate source commit"),
        0
    );
    assert_eq!(
        database
            .readers()
            .source_checkpoint("mail-new".into())
            .await
            .expect("checkpoint"),
        json!({"state": "b"})
    );

    let first = database
        .claim_next(at("2026-08-03T10:03:00.000Z"))
        .await
        .expect("claim")
        .expect("event");
    assert_eq!(first.revision_id, "message-1");
    assert_eq!(first.status, EventStatus::Processing);
    assert!(
        database
            .claim_next(at("2026-08-03T10:03:00.000Z"))
            .await
            .expect("blocked claim")
            .is_none()
    );
    database
        .succeed(first.id, at("2026-08-03T10:04:00.000Z"))
        .await
        .expect("succeed");
    let second = database
        .claim_next(at("2026-08-03T10:05:00.000Z"))
        .await
        .expect("claim second")
        .expect("second event");
    assert_eq!(second.revision_id, "message-2");
    let revision = database
        .start_triage_run(
            second.id,
            second.attempt_count,
            at("2026-08-03T10:05:01.000Z"),
        )
        .await
        .expect("revision run");
    let revision = database
        .readers()
        .triage_run(revision)
        .await
        .expect("revision run read")
        .expect("revision run record");
    assert_eq!(revision.dispatch_trigger, Some(DispatchTrigger::Revision));
    assert_eq!(DATABASE_QUEUE_CAPACITY, 64);
}

#[tokio::test]
async fn retry_retains_payload_and_success_scrubs_it() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            vec![item("message-1")],
            at("2026-08-03T10:01:00.000Z"),
        )
        .await
        .expect("source commit");
    let event = database
        .claim_next(at("2026-08-03T10:02:00.000Z"))
        .await
        .expect("claim")
        .expect("event");
    database
        .fail(
            event.id,
            "model unavailable".into(),
            3,
            1,
            at("2026-08-03T10:03:00.000Z"),
        )
        .await
        .expect("fail");
    let failed = database
        .readers()
        .event(event.id)
        .await
        .expect("event read")
        .expect("failed event");
    assert_eq!(failed.status, EventStatus::Retryable);
    assert!(
        failed
            .payload
            .as_deref()
            .is_some_and(|value| value.contains("Complete content"))
    );
    assert_eq!(
        failed.next_attempt_at.as_deref(),
        Some("2026-08-03T10:03:01.000Z")
    );
    assert!(
        database
            .retry(event.id, at("2026-08-03T10:03:00.000Z"))
            .await
            .expect("manual retry")
    );
    let event = database
        .claim_next(at("2026-08-03T10:04:00.000Z"))
        .await
        .expect("claim retry")
        .expect("retried event");
    database
        .succeed(event.id, at("2026-08-03T10:05:00.000Z"))
        .await
        .expect("succeed");
    assert!(
        database
            .readers()
            .event(event.id)
            .await
            .expect("event read")
            .expect("event")
            .payload
            .is_none()
    );
}

#[tokio::test]
async fn records_dispatch_cause_at_queue_transitions() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            vec![item("message-1"), item("message-2")],
            at("2026-08-03T10:01:00.000Z"),
        )
        .await
        .expect("source commit");

    let event = database
        .claim_next(at("2026-08-03T10:02:00.000Z"))
        .await
        .expect("initial claim")
        .expect("initial event");
    let first = database
        .start_triage_run(
            event.id,
            event.attempt_count,
            at("2026-08-03T10:02:01.000Z"),
        )
        .await
        .expect("initial run");
    let first_record = database
        .readers()
        .triage_run(first)
        .await
        .expect("initial run read")
        .expect("initial run record");
    assert_eq!(first_record.dispatch_sequence, Some(1));
    assert_eq!(
        first_record.dispatch_trigger,
        Some(DispatchTrigger::Initial)
    );
    assert_eq!(first_record.dispatch_prior_run_id, None);
    database
        .finish_triage_run(
            first,
            RunFinish {
                outcome: RunOutcome::Failed,
                termination_reason: "model_error".into(),
                failure_category: Some(ErrorCategory::ModelUnavailable),
                recording_complete: true,
            },
            at("2026-08-03T10:02:10.000Z"),
        )
        .await
        .expect("finish initial run");
    database
        .fail(
            event.id,
            "model unavailable".into(),
            3,
            1,
            at("2026-08-03T10:02:11.000Z"),
        )
        .await
        .expect("schedule retry");

    let event = database
        .claim_next(at("2026-08-03T10:02:12.000Z"))
        .await
        .expect("retry claim")
        .expect("retry event");
    let retry = database
        .start_triage_run(
            event.id,
            event.attempt_count,
            at("2026-08-03T10:02:13.000Z"),
        )
        .await
        .expect("retry run");
    let retry_record = database
        .readers()
        .triage_run(retry)
        .await
        .expect("retry run read")
        .expect("retry run record");
    assert_eq!(retry_record.dispatch_sequence, Some(2));
    assert_eq!(
        retry_record.dispatch_trigger,
        Some(DispatchTrigger::BackoffRetry)
    );
    assert_eq!(retry_record.dispatch_prior_run_id, Some(first.0));
    assert_eq!(
        retry_record.dispatch_scheduled_for.as_deref(),
        Some("2026-08-03T10:02:12.000Z")
    );
    database
        .finish_triage_run(
            retry,
            RunFinish {
                outcome: RunOutcome::Failed,
                termination_reason: "wall_timeout".into(),
                failure_category: Some(ErrorCategory::Timeout),
                recording_complete: true,
            },
            at("2026-08-03T10:02:20.000Z"),
        )
        .await
        .expect("finish retry run");
    database
        .fail(
            event.id,
            "triage timed out".into(),
            2,
            1,
            at("2026-08-03T10:02:21.000Z"),
        )
        .await
        .expect("exhaust retries");

    assert!(
        database
            .retry(event.id, at("2026-08-03T10:03:00.000Z"))
            .await
            .expect("operator retry")
    );
    let event = database
        .claim_next(at("2026-08-03T10:03:01.000Z"))
        .await
        .expect("operator claim")
        .expect("operator event");
    let operator = database
        .start_triage_run(
            event.id,
            event.attempt_count,
            at("2026-08-03T10:03:02.000Z"),
        )
        .await
        .expect("operator run");
    let operator_record = database
        .readers()
        .triage_run(operator)
        .await
        .expect("operator run read")
        .expect("operator run record");
    assert_eq!(operator_record.dispatch_sequence, Some(3));
    assert_eq!(
        operator_record.dispatch_trigger,
        Some(DispatchTrigger::OperatorRetry)
    );
    assert_eq!(operator_record.dispatch_prior_run_id, Some(retry.0));
}

#[tokio::test]
async fn records_recovery_dispatch_after_process_interruption() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            vec![item("message-1")],
            at("2026-08-03T10:01:00.000Z"),
        )
        .await
        .expect("source commit");
    let event = database
        .claim_next(at("2026-08-03T10:02:00.000Z"))
        .await
        .expect("claim")
        .expect("event");
    let interrupted = database
        .start_triage_run(
            event.id,
            event.attempt_count,
            at("2026-08-03T10:02:01.000Z"),
        )
        .await
        .expect("interrupted run");
    assert_eq!(
        database
            .recover_interrupted(
                at("2026-08-03T10:02:30.000Z"),
                at("2026-08-03T10:03:00.000Z")
            )
            .await
            .expect("recover"),
        1
    );
    let event = database
        .claim_next(at("2026-08-03T10:03:00.000Z"))
        .await
        .expect("recovery claim")
        .expect("recovery event");
    let recovered = database
        .start_triage_run(
            event.id,
            event.attempt_count,
            at("2026-08-03T10:03:01.000Z"),
        )
        .await
        .expect("recovery run");
    let record = database
        .readers()
        .triage_run(recovered)
        .await
        .expect("recovery run read")
        .expect("recovery run record");
    assert_eq!(record.dispatch_sequence, Some(2));
    assert_eq!(
        record.dispatch_trigger,
        Some(DispatchTrigger::RecoveryRetry)
    );
    assert_eq!(record.dispatch_prior_run_id, Some(interrupted.0));
}

#[tokio::test]
async fn typed_telemetry_version_two_closes_before_complete() {
    let temporary = TempDir::new().expect("temporary directory");
    let path = temporary.path().join("intake.sqlite");
    let database = IntakeDatabase::open(&path).await.expect("database");
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            vec![item("message-1")],
            at("2026-08-03T10:01:00.000Z"),
        )
        .await
        .expect("source commit");
    let event = database
        .claim_next(at("2026-08-03T10:02:00.000Z"))
        .await
        .expect("claim")
        .expect("event");
    let run = database
        .start_triage_run(event.id, 1, at("2026-08-03T10:02:01.000Z"))
        .await
        .expect("run");
    let turn = database
        .start_turn(run, at("2026-08-03T10:02:02.000Z"))
        .await
        .expect("turn");
    let tool = database
        .start_tool(
            run,
            Some(turn),
            "read".into(),
            Some("/tmp/project".into()),
            at("2026-08-03T10:02:03.000Z"),
        )
        .await
        .expect("tool");
    database
        .finish_tool(
            run,
            tool,
            SpanOutcome::Succeeded,
            at("2026-08-03T10:02:04.000Z"),
        )
        .await
        .expect("finish tool");
    let retry = database
        .start_retry(
            run,
            Some(turn),
            RetryStart {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 1000,
                error_category: Some(ErrorCategory::RateLimit),
            },
            at("2026-08-03T10:02:04.000Z"),
        )
        .await
        .expect("retry");
    database
        .finish_retry(
            run,
            retry,
            SpanOutcome::Succeeded,
            None,
            at("2026-08-03T10:02:05.000Z"),
        )
        .await
        .expect("finish retry");
    let compaction = database
        .start_compaction(
            run,
            Some(turn),
            "threshold".into(),
            at("2026-08-03T10:02:05.000Z"),
        )
        .await
        .expect("compaction");
    database
        .finish_compaction(
            run,
            compaction,
            CompactionFinish {
                outcome: SpanOutcome::Succeeded,
                aborted: false,
                will_retry: false,
                tokens_before: Some(120_000),
                estimated_tokens_after: None,
                usage: Some(ReportedUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    ..ReportedUsage::default()
                }),
            },
            at("2026-08-03T10:02:06.000Z"),
        )
        .await
        .expect("finish compaction");
    database
        .record_reasoning(
            run,
            Some(turn),
            Some("bounded reasoning".into()),
            at("2026-08-03T10:02:06.000Z"),
        )
        .await
        .expect("reasoning");
    database
        .finish_turn(
            run,
            turn,
            TurnFinish {
                stop_reason: Some("stop".into()),
                usage: Some(ReportedUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    total_tokens: Some(12),
                    ..ReportedUsage::default()
                }),
                context_tokens: None,
                context_window: None,
            },
            at("2026-08-03T10:02:07.000Z"),
        )
        .await
        .expect("finish turn");
    database.flush().await.expect("flush");
    database
        .finish_triage_run(
            run,
            RunFinish {
                outcome: RunOutcome::Succeeded,
                termination_reason: "completed".into(),
                failure_category: None,
                recording_complete: true,
            },
            at("2026-08-03T10:02:08.000Z"),
        )
        .await
        .expect("finish run");

    let record = database
        .readers()
        .triage_run(run)
        .await
        .expect("run read")
        .expect("run record");
    assert_eq!(record.telemetry_version, Some(2));
    assert_eq!(record.telemetry_completeness, "complete");
    assert_eq!(
        (
            record.turn_count,
            record.retry_count,
            record.compaction_count
        ),
        (1, 1, 1)
    );
    assert_eq!(
        database
            .readers()
            .triage_run_steps(run)
            .await
            .expect("steps")
            .len(),
        3
    );
    database.shutdown().await.expect("shutdown");

    let raw = Connection::open(&path).expect("raw database");
    let costs: (Option<f64>, Option<f64>) = raw
        .query_row(
            "SELECT input_cost, total_cost FROM triage_run_turns WHERE run_id = ?1",
            [run.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("cost columns");
    assert_eq!(costs, (None, None));
}

#[tokio::test]
async fn open_typed_span_makes_run_partial_and_interrupted() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            vec![item("message-1")],
            at("2026-08-03T10:01:00.000Z"),
        )
        .await
        .expect("source commit");
    let event = database
        .claim_next(at("2026-08-03T10:02:00.000Z"))
        .await
        .expect("claim")
        .expect("event");
    let run = database
        .start_triage_run(event.id, 1, at("2026-08-03T10:02:01.000Z"))
        .await
        .expect("run");
    database
        .start_turn(run, at("2026-08-03T10:02:02.000Z"))
        .await
        .expect("turn");
    database
        .finish_triage_run(
            run,
            RunFinish {
                outcome: RunOutcome::Failed,
                termination_reason: "model_error".into(),
                failure_category: Some(ErrorCategory::ModelUnavailable),
                recording_complete: true,
            },
            at("2026-08-03T10:02:03.000Z"),
        )
        .await
        .expect("finish run");
    let run = database
        .readers()
        .triage_run(run)
        .await
        .expect("run read")
        .expect("run record");
    assert_eq!(run.telemetry_completeness, "partial");
}

#[tokio::test]
async fn batch_run_summaries_include_associated_events() {
    let temporary = TempDir::new().expect("temporary directory");
    let path = temporary.path().join("run-summaries.sqlite");
    let raw = Connection::open(&path).expect("fixture database");
    raw.execute_batch(SCHEMA_FIXTURES[7]).expect("load fixture");
    drop(raw);

    let database = IntakeDatabase::open(&path).await.expect("database");
    let summaries = database
        .readers()
        .list_triage_run_summaries(2)
        .await
        .expect("run summaries");

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].run.id, 3);
    assert_eq!(summaries[0].event.id, summaries[0].run.event_id);
    assert_eq!(
        summaries[0].event.title,
        "Investigate delayed notifications"
    );
    assert_eq!(summaries[0].step_count, 0);
    assert_eq!(summaries[1].run.id, 2);
    assert_eq!(summaries[1].event.id, summaries[1].run.event_id);
    assert_eq!(
        summaries[1].event.title,
        "Investigate delayed notifications"
    );
    assert_eq!(summaries[1].step_count, 1);
}

#[tokio::test]
async fn every_phase_zero_schema_fixture_migrates_with_integrity() {
    for (version, fixture) in SCHEMA_FIXTURES.iter().enumerate() {
        let temporary = TempDir::new().expect("temporary directory");
        let path = temporary.path().join(format!("schema-v{version}.sqlite"));
        let raw = Connection::open(&path).expect("fixture database");
        raw.execute_batch(fixture).expect("load fixture");
        drop(raw);

        let database = IntakeDatabase::open(&path).await.expect("migrate fixture");
        assert_eq!(
            database
                .readers()
                .integrity_check()
                .await
                .expect("integrity check"),
            "ok",
            "schema-v{version}"
        );
        if version == 7 {
            let events = database
                .readers()
                .list_events(10)
                .await
                .expect("version 7 events");
            assert_eq!(events.len(), 3);
            assert_eq!(events[0].revision_id, "issue-update-2");
            let run = database
                .readers()
                .triage_run(intake::database::RunId(1))
                .await
                .expect("version 7 run")
                .expect("fixture run");
            assert_eq!(run.model_id.as_deref(), Some("gpt-5.6-luna"));
            assert_eq!(run.telemetry_completeness, "complete");
        }
        database.shutdown().await.expect("shutdown");
        let raw = Connection::open(&path).expect("migrated database");
        let versions = raw
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .expect("version statement")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("version rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("versions");
        assert_eq!(versions, (1..=9).collect::<Vec<_>>(), "schema-v{version}");
    }
}

#[tokio::test]
async fn migration_schema_matches_every_phase_zero_version() {
    for (version, fixture_sql) in SCHEMA_FIXTURES.iter().enumerate() {
        let temporary = TempDir::new().expect("temporary directory");
        let rust_path = temporary.path().join("rust.sqlite");
        let fixture_path = temporary.path().join("fixture.sqlite");
        let rust = Connection::open(&rust_path).expect("Rust schema database");
        rust.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
        )
        .expect("migration table");
        for (index, migration) in MIGRATIONS.iter().take(version).enumerate() {
            let transaction = rust.unchecked_transaction().expect("migration transaction");
            transaction.execute_batch(migration).expect("migration SQL");
            transaction
                .execute(
                    "INSERT INTO schema_migrations VALUES (?1, '2026-08-07T00:00:00.000Z')",
                    [index + 1],
                )
                .expect("migration row");
            transaction.commit().expect("migration commit");
        }
        drop(rust);
        let fixture = Connection::open(&fixture_path).expect("fixture database");
        fixture
            .execute_batch(fixture_sql)
            .expect("load schema fixture");
        drop(fixture);
        assert_eq!(
            schema_rows(&rust_path),
            schema_rows(&fixture_path),
            "schema-v{version}"
        );
    }
}

#[tokio::test]
async fn migration_schema_matches_current_version() {
    let temporary = TempDir::new().expect("temporary directory");
    let rust_path = temporary.path().join("rust.sqlite");
    let fixture_path = temporary.path().join("fixture.sqlite");
    let rust = IntakeDatabase::open(&rust_path)
        .await
        .expect("Rust database");
    rust.shutdown().await.expect("shutdown");
    let fixture = Connection::open(&fixture_path).expect("fixture database");
    fixture
        .execute_batch(SCHEMA_FIXTURES[9])
        .expect("load schema fixture");
    drop(fixture);

    assert_eq!(schema_rows(&rust_path), schema_rows(&fixture_path));
    assert_eq!(MIGRATIONS.len(), 9);
}

#[tokio::test]
async fn rejects_migration_gaps_and_future_versions() {
    for (versions, expected) in [(&[2_i64][..], "contiguous"), (&[1_i64, 10][..], "newer")] {
        let temporary = TempDir::new().expect("temporary directory");
        let path = temporary.path().join("invalid.sqlite");
        let raw = Connection::open(&path).expect("database");
        raw.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
        )
        .expect("migration table");
        for version in versions {
            raw.execute(
                "INSERT INTO schema_migrations VALUES (?1, '2026-08-03T00:00:00.000Z')",
                [version],
            )
            .expect("migration row");
        }
        drop(raw);
        let error = match IntakeDatabase::open(&path).await {
            Ok(_) => panic!("invalid schema should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(expected));
        let raw = Connection::open(&path).expect("database after rejection");
        let count: i64 = raw
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))
            .expect("schema count");
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn shutdown_closes_writer_and_keeps_readers_available() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");
    let writer = database.clone();
    let readers = database.readers();

    database.shutdown().await.expect("shutdown");

    assert!(matches!(
        writer.flush().await,
        Err(DatabaseError::ActorClosed)
    ));
    assert!(readers.status().await.expect("reader request").is_empty());
}

#[tokio::test]
async fn request_errors_do_not_stop_the_writer() {
    let database = IntakeDatabase::open(":memory:").await.expect("database");

    assert!(matches!(
        database
            .fail(404, "missing".into(), 1, 1, at("2026-08-03T10:01:00.000Z"),)
            .await,
        Err(DatabaseError::UnknownEvent(404))
    ));
    database.flush().await.expect("writer remains available");
}

#[tokio::test]
async fn orders_stored_timestamps_chronologically_and_preserves_wire_text() {
    let temporary = TempDir::new().expect("temporary directory");
    let path = temporary.path().join("timestamps.sqlite");
    let database = IntakeDatabase::open(&path).await.expect("database");
    let items = ["whole", "offset", "hundred-ms", "ten-ms"]
        .into_iter()
        .map(|revision| {
            let mut value = item(revision);
            value.entity_id = format!("mail:{revision}");
            value
        })
        .collect();
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            items,
            at("2026-08-03T12:00:00.000Z"),
        )
        .await
        .expect("source commit");

    let raw = Connection::open(&path).expect("raw database");
    for (revision, observed_at) in [
        ("whole", "2026-08-03T10:00:00Z"),
        ("offset", "2026-08-03T11:00:00+02:00"),
        ("hundred-ms", "2026-08-03T10:00:00.1Z"),
        ("ten-ms", "2026-08-03T10:00:00.010Z"),
    ] {
        raw.execute(
            "UPDATE events SET occurred_at = ?1, observed_at = ?1 WHERE revision_id = ?2",
            [observed_at, revision],
        )
        .expect("replace timestamp");
    }
    drop(raw);

    let events = database
        .readers()
        .list_events(10)
        .await
        .expect("ordered events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.revision_id.as_str())
            .collect::<Vec<_>>(),
        ["hundred-ms", "ten-ms", "whole", "offset"]
    );
    let offset = events
        .iter()
        .find(|event| event.revision_id == "offset")
        .expect("offset event");
    assert_eq!(
        serde_json::to_value(offset).expect("event JSON")["observedAt"],
        "2026-08-03T11:00:00+02:00"
    );
    assert_eq!(
        database
            .readers()
            .oldest_open_event_at()
            .await
            .expect("oldest open")
            .expect("open event")
            .as_str(),
        "2026-08-03T11:00:00+02:00"
    );
    assert_eq!(
        database
            .claim_next(at("2026-08-03T12:00:00Z"))
            .await
            .expect("claim")
            .expect("event")
            .revision_id,
        "offset"
    );
}

#[tokio::test]
async fn rejects_invalid_stored_timestamps_at_the_read_boundary() {
    let temporary = TempDir::new().expect("temporary directory");
    let path = temporary.path().join("invalid-timestamp.sqlite");
    let database = IntakeDatabase::open(&path).await.expect("database");
    database
        .source_succeeded(
            "mail".into(),
            Value::Null,
            vec![item("invalid")],
            at("2026-08-03T10:01:00Z"),
        )
        .await
        .expect("source commit");
    let event_id = database.readers().list_events(1).await.expect("event list")[0].id;
    let raw = Connection::open(&path).expect("raw database");
    raw.execute(
        "UPDATE events SET observed_at = 'not-a-timestamp' WHERE id = ?1",
        [event_id],
    )
    .expect("replace timestamp");
    drop(raw);

    let error = database
        .readers()
        .event(event_id)
        .await
        .expect_err("invalid timestamp");
    assert!(matches!(
        error,
        DatabaseError::Sqlite(rusqlite::Error::FromSqlConversionFailure(_, _, _))
    ));
}

#[test]
fn formats_millisecond_utc_timestamps_and_reports_real_usage_only() {
    assert_eq!(
        timestamp(Utc.with_ymd_and_hms(2026, 8, 3, 10, 2, 1).unwrap()),
        "2026-08-03T10:02:01.000Z"
    );
    let empty = Usage::new();
    assert_eq!(reported_usage(Some(&empty), false), None);
    assert_eq!(
        reported_usage(Some(&empty), true),
        Some(ReportedUsage {
            input_tokens: Some(0),
            output_tokens: Some(0),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            reasoning_tokens: Some(0),
            total_tokens: Some(0),
        })
    );
}

fn schema_rows(path: &std::path::Path) -> Vec<(String, String, String)> {
    let connection = Connection::open(path).expect("schema database");
    let mut statement = connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .expect("schema statement");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("schema rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema values")
}
