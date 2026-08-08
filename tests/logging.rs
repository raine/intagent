use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use intagent::database::{EventRecord, EventStatus, RunMetadata, RunOutcome};
use intagent::logging::{
    DurableLogStore, LogWriteOutcome, MAX_LOG_RECORD_BYTES, MAX_LOG_STRING_BYTES,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn event(id: i64, attempt: u32) -> EventRecord {
    EventRecord {
        id,
        source: "fake source".into(),
        entity_id: format!("entity-{id}"),
        revision_id: "revision-1".into(),
        kind: "generic".into(),
        title: "A human title".into(),
        payload: Some("{}".into()),
        operational_metadata: "{}".into(),
        occurred_at: "2026-08-04T12:00:00.000Z".parse().unwrap(),
        observed_at: "2026-08-04T12:00:01.000Z".parse().unwrap(),
        status: EventStatus::Processing,
        attempt_count: attempt,
        next_attempt_at: None,
        last_error: None,
        aven_ref: None,
        investigation_handle: None,
    }
}

#[tokio::test]
async fn writes_bounded_redacted_triage_lifecycle() {
    let temporary = TempDir::new().expect("temporary directory");
    let logs = DurableLogStore::new(temporary.path(), |value| {
        value.replace("visible-secret", "[REDACTED]")
    });
    let mut run = logs.triage(&event(17, 1));
    run.start().await;
    run.metadata(
        &RunMetadata {
            model_id: Some("gpt-test".into()),
            model_provider: Some("chatgpt".into()),
            thinking_level: Some("max".into()),
            context_window: None,
            max_tokens: None,
        },
        &["bash".into(), "/private/tool".into()],
    )
    .await;
    run.prompt("inspect visible-secret").await;
    run.start_turn(1).await;
    run.start_tool("/private/tool", Some("visible-secret command"))
        .await;
    run.finish_tool(
        "/private/tool",
        true,
        Some("working directory: /project\nstderr: visible-secret failure"),
    )
    .await;
    run.reasoning(Some("visible-secret reasoning")).await;
    run.finish_turn(1, json!({"usage": null})).await;
    run.finish(RunOutcome::Succeeded, None, "completed").await;

    assert!(!run.recording_failed());
    let records = records(run.path());
    assert_eq!(
        records
            .iter()
            .map(|record| record["type"].as_str().expect("record type"))
            .collect::<Vec<_>>(),
        [
            "run_start",
            "session_metadata",
            "prompt_submitted",
            "turn_start",
            "tool_execution_start",
            "tool_execution_end",
            "reasoning",
            "turn_end",
            "run_end",
        ]
    );
    let serialized = serde_json::to_string(&records).expect("serialized records");
    assert!(!serialized.contains("visible-secret"));
    assert!(!serialized.contains("A human title"));
    assert!(!serialized.contains("/private"));
    assert!(!serialized.contains("inspect visible"));
    assert!(serialized.contains("[REDACTED]"));
    assert_eq!(records[5]["isError"], true);
    assert_eq!(
        records[5]["diagnostic"],
        "working directory: /project\nstderr: [REDACTED] failure"
    );
    assert_eq!(
        fs::metadata(temporary.path())
            .expect("log directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(run.path())
            .expect("log file")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn separates_attempts_and_appends_monitor_records_in_order() {
    let temporary = TempDir::new().expect("temporary directory");
    let logs = DurableLogStore::new(temporary.path(), str::to_owned);
    let first = logs.triage(&event(42, 1));
    let second = logs.triage(&event(42, 2));
    assert_ne!(first.path(), second.path());
    assert!(
        first
            .path()
            .to_string_lossy()
            .contains("triage-event-42-attempt-1-fake-source")
    );
    assert!(
        second
            .path()
            .to_string_lossy()
            .contains("triage-event-42-attempt-2-fake-source")
    );

    assert_eq!(
        logs.monitor("process_start", json!({"mode": "watch"}))
            .await,
        LogWriteOutcome::Written
    );
    logs.monitor("source_poll_succeeded", json!({"queued": 2}))
        .await;
    logs.monitor("process_stop", json!({"mode": "watch"})).await;
    let records = records(&temporary.path().join("monitor.jsonl"));
    assert_eq!(
        records
            .iter()
            .map(|record| record["type"].as_str().expect("record type"))
            .collect::<Vec<_>>(),
        ["process_start", "source_poll_succeeded", "process_stop"]
    );
}

#[tokio::test]
async fn truncates_large_strings_and_records() {
    let temporary = TempDir::new().expect("temporary directory");
    let logs = DurableLogStore::new(temporary.path(), str::to_owned);
    let path = temporary.path().join("bounded.jsonl");
    let huge = "x".repeat(MAX_LOG_RECORD_BYTES + MAX_LOG_STRING_BYTES);
    assert_eq!(
        logs.append(
            path.clone(),
            json!({
                "timestamp": "2026-08-04T12:00:00.000Z",
                "type": "large",
                "value": huge,
                "one": "y".repeat(MAX_LOG_RECORD_BYTES),
                "two": "y".repeat(MAX_LOG_RECORD_BYTES),
                "three": "y".repeat(MAX_LOG_RECORD_BYTES),
                "four": "y".repeat(MAX_LOG_RECORD_BYTES),
                "five": "y".repeat(MAX_LOG_RECORD_BYTES),
                "six": "y".repeat(MAX_LOG_RECORD_BYTES),
                "seven": "y".repeat(MAX_LOG_RECORD_BYTES),
                "eight": "y".repeat(MAX_LOG_RECORD_BYTES),
            }),
        )
        .await,
        LogWriteOutcome::Written
    );
    let bytes = fs::read(&path).expect("bounded log");
    assert!(bytes.len() <= MAX_LOG_RECORD_BYTES + 1);
    let record = &records(&path)[0];
    assert_eq!(record["recordTruncated"], true);
    assert!(
        record["value"]
            .as_str()
            .expect("bounded value")
            .contains("[TRUNCATED:")
    );
}

#[tokio::test]
async fn reports_logging_failures_without_rejecting_callers() {
    let temporary = TempDir::new().expect("temporary directory");
    let blocked = temporary.path().join("blocked");
    fs::write(&blocked, "not a directory").expect("blocked path");
    let warning = Arc::new(Mutex::new(String::new()));
    let captured = warning.clone();
    let logs = DurableLogStore::with_warning_sink(&blocked, str::to_owned, move |message| {
        captured.lock().expect("warning lock").push_str(message)
    });
    assert_eq!(
        logs.monitor("process_start", json!({})).await,
        LogWriteOutcome::Failed
    );
    assert_eq!(
        logs.monitor("process_stop", json!({})).await,
        LogWriteOutcome::Failed
    );
    let warning = warning.lock().expect("warning lock");
    assert!(warning.contains("warning: intagent logging failed"));
    assert!(warning.contains("monitor.jsonl"));
    assert_eq!(warning.matches("warning:").count(), 1);
}

fn records(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("log contents")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSONL record"))
        .collect()
}
