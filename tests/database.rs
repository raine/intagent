use std::process::Command;

use chrono::{TimeZone, Utc};
use intake::database::{
    CompactionFinish, DATABASE_QUEUE_CAPACITY, ErrorCategory, EventStatus, IntakeDatabase,
    MIGRATIONS, ReportedUsage, RetryStart, RunFinish, RunOutcome, SpanOutcome, TurnFinish,
    reported_usage, timestamp,
};
use intake::protocol::{IntakeItem, IntakeItemKind};
use rig_core::completion::Usage;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

const SCHEMA_FIXTURES: [&str; 8] = [
    include_str!("fixtures/database/schema-v0.sql"),
    include_str!("fixtures/database/schema-v1.sql"),
    include_str!("fixtures/database/schema-v2.sql"),
    include_str!("fixtures/database/schema-v3.sql"),
    include_str!("fixtures/database/schema-v4.sql"),
    include_str!("fixtures/database/schema-v5.sql"),
    include_str!("fixtures/database/schema-v6.sql"),
    include_str!("fixtures/database/schema-v7.sql"),
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
        assert_eq!(versions, (1..=7).collect::<Vec<_>>(), "schema-v{version}");
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
async fn migration_schema_matches_phase_zero_version_seven() {
    let temporary = TempDir::new().expect("temporary directory");
    let rust_path = temporary.path().join("rust.sqlite");
    let fixture_path = temporary.path().join("fixture.sqlite");
    let rust = IntakeDatabase::open(&rust_path)
        .await
        .expect("Rust database");
    rust.shutdown().await.expect("shutdown");
    let fixture = Connection::open(&fixture_path).expect("fixture database");
    fixture
        .execute_batch(SCHEMA_FIXTURES[7])
        .expect("load schema fixture");
    drop(fixture);

    assert_eq!(schema_rows(&rust_path), schema_rows(&fixture_path));
    assert_eq!(MIGRATIONS.len(), 7);
}

#[tokio::test]
async fn rejects_migration_gaps_and_future_versions() {
    for (versions, expected) in [(&[2_i64][..], "contiguous"), (&[1_i64, 8][..], "newer")] {
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
async fn rust_and_bun_apply_reciprocal_operations() {
    if Command::new("bun").arg("--version").output().is_err() {
        eprintln!("bun unavailable, reciprocal compatibility test skipped");
        return;
    }
    let temporary = TempDir::new().expect("temporary directory");
    let path = temporary.path().join("compat.sqlite");
    let database = IntakeDatabase::open(&path).await.expect("Rust database");
    database
        .source_succeeded(
            "rust".into(),
            json!({"cursor": 1}),
            vec![item("rust-1")],
            at("2026-08-03T10:01:00.000Z"),
        )
        .await
        .expect("Rust source commit");
    database.shutdown().await.expect("shutdown");
    drop(database);

    let repository = env!("CARGO_MANIFEST_DIR");
    let script = format!(
        r#"
        import {{ IntakeDatabase }} from {module:?};
        const database = new IntakeDatabase(process.argv[1]);
        if (database.listEvents().length !== 1) throw new Error("Rust event unavailable");
        database.sourceSucceeded("bun", {{ cursor: 2 }}, [{{
          entityId: "mail:thread-1",
          revisionId: "bun-2",
          kind: "email",
          title: "Needs attention",
          body: "Written by Bun",
          occurredAt: "2026-08-03T10:02:00.000Z",
          metadata: {{ threadId: "thread-1" }},
        }}], "2026-08-03T10:02:01.000Z");
        database.close();
        "#,
        module = format!("{repository}/src/database.ts"),
    );
    let result = Command::new("bun")
        .args(["-e", &script, path.to_str().expect("UTF-8 path")])
        .current_dir(repository)
        .output()
        .expect("run Bun compatibility operation");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let database = IntakeDatabase::open(&path).await.expect("reopen in Rust");
    let events = database
        .readers()
        .list_events(10)
        .await
        .expect("event list");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].revision_id, "bun-2");
    assert_eq!(
        database
            .readers()
            .integrity_check()
            .await
            .expect("integrity check"),
        "ok"
    );
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
