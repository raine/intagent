use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use intake::config::{
    CommandRule, CommandsConfig, IntakeConfig, SkillsConfig, SourceConfig, StateConfig,
    TriageConfig,
};
use intake::database::IntakeDatabase;
use intake::protocol::{PollRequest, PollResponse};
use intake::source_runner::{SOURCE_OUTPUT_LIMIT, poll_source};
use serde_json::{Map, json};
use tempfile::TempDir;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn config(root: &Path, source: SourceConfig) -> IntakeConfig {
    IntakeConfig {
        version: 1,
        project_roots: vec![root.display().to_string()],
        state: StateConfig::default(),
        skills: SkillsConfig {
            directories: vec![root.display().to_string()],
            approved_roots: vec![root.display().to_string()],
        },
        sources: vec![source],
        triage: TriageConfig::default(),
        commands: CommandsConfig {
            path: vec!["/usr/bin".into(), "/bin".into()],
            timeout_seconds: 5,
            max_output_bytes: 4096,
            sensitive_patterns: Vec::new(),
            rules: vec![CommandRule {
                executable: "true".into(),
            }],
        },
    }
}

fn source(command: PathBuf) -> SourceConfig {
    SourceConfig {
        name: "fixture".into(),
        command: command.display().to_string(),
        args: Vec::new(),
        interval_seconds: 60,
        timeout_seconds: 3,
        item_limit: 10,
        environment: Vec::new(),
        options: Map::new(),
    }
}

fn executable(root: &TempDir, body: &str) -> PathBuf {
    let path = root.path().join("source");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write source fixture");
    let mut permissions = fs::metadata(&path).expect("source metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make source executable");
    path
}

fn response(items: serde_json::Value) -> String {
    json!({
        "protocolVersion": 1,
        "checkpoint": {"cursor": "next"},
        "items": items,
    })
    .to_string()
}

fn item(entity: &str) -> serde_json::Value {
    json!({
        "entityId": entity,
        "revisionId": "revision:1",
        "kind": "generic",
        "title": "External item",
        "body": "Payload",
        "occurredAt": "2026-08-03T10:00:00.000Z",
        "metadata": {},
    })
}

async fn checkpoint(database: &IntakeDatabase) -> serde_json::Value {
    database
        .readers()
        .source_checkpoint("fixture".into())
        .await
        .expect("checkpoint")
}

async fn last_error(database: &IntakeDatabase) -> String {
    database
        .readers()
        .source_statuses()
        .await
        .expect("source statuses")
        .into_iter()
        .find(|status| status.source == "fixture")
        .and_then(|status| status.last_error)
        .expect("recorded source error")
}

#[tokio::test]
async fn queues_valid_response_with_versioned_bounded_request() {
    let root = tempfile::tempdir().expect("temporary directory");
    let request_path = root.path().join("request.json");
    let body = format!(
        "IFS= read -r request\nprintf '%s' \"$request\" > '{}'\nprintf '%s\\n' '{}'",
        request_path.display(),
        response(json!([item("external:1")]))
    );
    let command = executable(&root, &body);
    let mut source = source(command);
    source
        .options
        .insert("project_roots".into(), json!(["override"]));
    let config = config(root.path(), source.clone());
    let database = IntakeDatabase::open(":memory:").await.expect("database");

    let queued = poll_source(&source, &config, &database, at("2026-08-03T10:01:00.000Z"))
        .await
        .expect("valid source poll");

    assert_eq!(queued, 1);
    assert_eq!(checkpoint(&database).await, json!({"cursor": "next"}));
    let request_bytes = fs::read(request_path).expect("captured request");
    assert!(request_bytes.len() < intake::protocol::MAX_STANDARD_INPUT_BYTES as usize);
    let request: PollRequest = serde_json::from_slice(&request_bytes).expect("poll request");
    request.validate().expect("valid poll request");
    assert_eq!(request.protocol_version, 1);
    assert_eq!(request.checkpoint, serde_json::Value::Null);
    assert_eq!(request.options["project_roots"], json!(["override"]));
    let event = database
        .claim_next(at("2026-08-03T10:02:00.000Z"))
        .await
        .expect("claim event")
        .expect("queued event");
    assert_eq!(event.entity_id, "external:1");
}

#[tokio::test]
async fn records_startup_failure_without_checkpoint_advance() {
    let root = tempfile::tempdir().expect("temporary directory");
    let source = source(root.path().join("missing-source"));
    let config = config(root.path(), source.clone());
    let database = IntakeDatabase::open(":memory:").await.expect("database");

    let error = poll_source(&source, &config, &database, Utc::now())
        .await
        .expect_err("startup failure")
        .to_string();

    assert!(error.contains("failed to start"), "{error}");
    assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
    assert!(last_error(&database).await.contains("failed to start"));
}

#[tokio::test]
async fn records_exit_utf8_json_schema_and_item_limit_failures() {
    let cases = vec![
        (
            "printf 'token=hidden-value' >&2\nexit 7".to_string(),
            "source exited 7",
        ),
        ("printf '\\377'".to_string(), "not valid UTF-8"),
        ("printf 'not json\\n'".to_string(), "not one JSON response"),
        (
            "printf '%s\\n' '{\"protocolVersion\":2,\"checkpoint\":null,\"items\":[]}'".to_string(),
            "failed validation",
        ),
        (
            format!(
                "printf '%s\\n' '{}'",
                response(json!([item("one"), item("two")]))
            ),
            "2 items for a limit of 1",
        ),
    ];

    for (body, expected) in cases {
        let root = tempfile::tempdir().expect("temporary directory");
        let command = executable(&root, &body);
        let mut source = source(command);
        source.item_limit = 1;
        let config = config(root.path(), source.clone());
        let database = IntakeDatabase::open(":memory:").await.expect("database");
        let error = poll_source(&source, &config, &database, Utc::now())
            .await
            .expect_err("invalid source output")
            .to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
        assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
        assert!(last_error(&database).await.contains(expected));
    }
}

#[tokio::test]
async fn bounds_stdout_and_stderr_without_pipe_deadlock() {
    let cases = [
        (
            "/usr/bin/yes output",
            SOURCE_OUTPUT_LIMIT,
            "output exceeded",
        ),
        (
            "/usr/bin/yes diagnostics >&2",
            65_536,
            "diagnostics exceeded",
        ),
    ];
    for (body, limit, expected) in cases {
        let root = tempfile::tempdir().expect("temporary directory");
        let command = executable(&root, body);
        let source = source(command);
        let config = config(root.path(), source.clone());
        let database = IntakeDatabase::open(":memory:").await.expect("database");
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            poll_source(&source, &config, &database, Utc::now()),
        )
        .await
        .expect("output bound completes")
        .expect_err("oversized source output")
        .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains(&limit.to_string()), "{error}");
        assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
    }
}

#[tokio::test]
async fn clears_environment_and_redacts_every_allowlisted_secret_value() {
    const SECRET_NAME: &str = "INTAKE_SOURCE_TEST_SECRET";
    const BLOCKED_NAME: &str = "INTAKE_SOURCE_TEST_BLOCKED";
    let secret = "tiny";
    unsafe {
        std::env::set_var(SECRET_NAME, secret);
        std::env::set_var(BLOCKED_NAME, "must-not-reach-source");
    }
    let root = tempfile::tempdir().expect("temporary directory");
    let body = format!(
        "if [ -n \"${{{BLOCKED_NAME}+present}}\" ]; then exit 9; fi\nif [ \"$PATH\" != '/usr/bin:/bin' ] || [ \"$LANG\" != 'C.UTF-8' ] || [ \"$LC_ALL\" != 'C.UTF-8' ] || [ \"$NO_COLOR\" != '1' ]; then exit 10; fi\nprintf 'Bearer %s token=%s password:%s secret=%s' \"${SECRET_NAME}\" \"${SECRET_NAME}\" \"${SECRET_NAME}\" \"${SECRET_NAME}\" >&2\nexit 8"
    );
    let command = executable(&root, &body);
    let mut source = source(command);
    source.environment.push(SECRET_NAME.into());
    let config = config(root.path(), source.clone());
    let database = IntakeDatabase::open(":memory:").await.expect("database");

    let error = poll_source(&source, &config, &database, Utc::now())
        .await
        .expect_err("source diagnostic failure")
        .to_string();
    unsafe {
        std::env::remove_var(SECRET_NAME);
        std::env::remove_var(BLOCKED_NAME);
    }

    assert!(error.contains("source exited 8"), "{error}");
    assert!(!error.contains(secret), "{error}");
    assert_eq!(error.matches("[REDACTED]").count(), 4, "{error}");
    let stored = last_error(&database).await;
    assert!(!stored.contains(secret), "{stored}");
    assert_eq!(stored.matches("[REDACTED]").count(), 4, "{stored}");
    assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
}

#[tokio::test]
async fn times_out_and_kills_descendants() {
    let root = tempfile::tempdir().expect("temporary directory");
    let marker = root.path().join("descendant-ran");
    let body = format!(
        "(/bin/sleep 2; /usr/bin/touch '{}') &\n/bin/sleep 5",
        marker.display()
    );
    let command = executable(&root, &body);
    let mut source = source(command);
    source.timeout_seconds = 1;
    let config = config(root.path(), source.clone());
    let database = IntakeDatabase::open(":memory:").await.expect("database");

    let error = poll_source(&source, &config, &database, Utc::now())
        .await
        .expect_err("source timeout")
        .to_string();

    assert!(error.contains("timed out"), "{error}");
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        !marker.exists(),
        "source descendant survived process-group kill"
    );
    assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
    assert!(last_error(&database).await.contains("timed out"));
}

#[tokio::test]
async fn rejects_oversized_request_before_starting_source() {
    let root = tempfile::tempdir().expect("temporary directory");
    let marker = root.path().join("source-started");
    let command = executable(&root, &format!("/usr/bin/touch '{}'", marker.display()));
    let mut source = source(command);
    source.options.insert(
        "oversized".into(),
        json!("x".repeat(intake::protocol::MAX_STANDARD_INPUT_BYTES as usize)),
    );
    let config = config(root.path(), source.clone());
    let database = IntakeDatabase::open(":memory:").await.expect("database");

    let error = poll_source(&source, &config, &database, Utc::now())
        .await
        .expect_err("oversized request")
        .to_string();

    assert!(error.contains("request exceeded"), "{error}");
    assert!(!marker.exists(), "source started for oversized request");
    assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
    assert!(last_error(&database).await.contains("request exceeded"));
}

#[tokio::test]
async fn rolls_back_events_and_checkpoint_when_commit_fails() {
    let root = tempfile::tempdir().expect("temporary directory");
    let command = executable(
        &root,
        &format!(
            "IFS= read -r request\nprintf '%s\\n' '{}'",
            response(json!([item("rollback:item")]))
        ),
    );
    let source = source(command);
    let config = config(root.path(), source.clone());
    let database_path = root.path().join("intake.sqlite");
    let database = IntakeDatabase::open(&database_path)
        .await
        .expect("database");
    rusqlite::Connection::open(&database_path)
        .expect("fixture connection")
        .execute_batch(
            "CREATE TRIGGER reject_fixture_event BEFORE INSERT ON events
             BEGIN SELECT RAISE(FAIL, 'fixture event rejection'); END;",
        )
        .expect("install rejecting trigger");

    let error = poll_source(&source, &config, &database, Utc::now())
        .await
        .expect_err("database commit failure")
        .to_string();

    assert!(error.contains("database operation failed"), "{error}");
    assert_eq!(checkpoint(&database).await, serde_json::Value::Null);
    assert!(
        database
            .readers()
            .list_events(10)
            .await
            .expect("events")
            .is_empty()
    );
    assert!(
        last_error(&database)
            .await
            .contains("fixture event rejection")
    );
}

#[test]
fn valid_response_fixture_remains_protocol_valid() {
    let value = response(json!([item("fixture:item")]));
    let response: PollResponse = serde_json::from_str(&value).expect("response fixture");
    response.validate().expect("valid response fixture");
}
