use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use intake::agent::rig_runner::{TriageError, TriageRunner};
use intake::config::{
    CommandRule, CommandsConfig, IntakeConfig, SkillsConfig, StateConfig, TriageConfig,
};
use intake::database::{EventRecord, EventStatus, IntakeDatabase};
use intake::logging::DurableLogStore;
use intake::monitor::IntakeMonitor;
use intake::protocol::{IntakeItem, IntakeItemKind};
use serde_json::Map;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct FakeRunner {
    active: Arc<AtomicUsize>,
    maximum_active: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<(i64, u32)>>>,
    failure: Option<String>,
}

impl FakeRunner {
    fn successful() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            maximum_active: Arc::new(AtomicUsize::new(0)),
            events: Arc::new(Mutex::new(Vec::new())),
            failure: None,
        }
    }
}

impl TriageRunner for FakeRunner {
    async fn run(
        &self,
        event: EventRecord,
        _cancellation: CancellationToken,
    ) -> Result<(), TriageError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .push((event.id, event.attempt_count));
        tokio::task::yield_now().await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        match &self.failure {
            Some(message) => Err(TriageError::Configuration(message.clone())),
            None => Ok(()),
        }
    }
}

fn config(root: &TempDir) -> IntakeConfig {
    IntakeConfig {
        version: 1,
        project_roots: vec![root.path().display().to_string()],
        state: StateConfig {
            database: root.path().join("intake.sqlite").display().to_string(),
            logs: root.path().join("logs").display().to_string(),
        },
        skills: SkillsConfig {
            directories: vec![root.path().display().to_string()],
            approved_roots: vec![root.path().display().to_string()],
        },
        sources: Vec::new(),
        triage: TriageConfig {
            retry_base_seconds: 1,
            ..TriageConfig::default()
        },
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

fn item(entity: &str, revision: &str) -> IntakeItem {
    IntakeItem {
        entity_id: entity.into(),
        revision_id: revision.into(),
        kind: IntakeItemKind::Generic,
        title: entity.into(),
        body: "untrusted content".into(),
        url: None,
        occurred_at: intake::database::timestamp(Utc::now()),
        metadata: Map::new(),
    }
}

#[tokio::test]
async fn drains_events_serially_with_one_fresh_run_per_event() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let database = IntakeDatabase::open(":memory:").await.unwrap();
    database
        .source_succeeded(
            "fixture".into(),
            serde_json::json!({}),
            vec![
                item("entity-1", "revision-1"),
                item("entity-2", "revision-2"),
            ],
            Utc::now(),
        )
        .await
        .unwrap();
    let runner = FakeRunner::successful();
    let observed_runner = runner.clone();
    let logs = DurableLogStore::new(root.path().join("logs"), str::to_owned);
    let monitor = IntakeMonitor::new(config, database.clone(), runner, logs);

    let result = monitor.check().await.unwrap();

    assert_eq!(result.observed, 0);
    assert_eq!(result.handled, 2);
    assert!(result.errors.is_empty());
    assert_eq!(observed_runner.maximum_active.load(Ordering::SeqCst), 1);
    assert_eq!(
        *observed_runner.events.lock().unwrap(),
        vec![(1, 1), (2, 1)]
    );
    assert_eq!(
        database.readers().status().await.unwrap().get("succeeded"),
        Some(&2)
    );
}

#[tokio::test]
async fn applies_event_retries_and_records_safe_failure_categories() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let database = IntakeDatabase::open(":memory:").await.unwrap();
    database
        .source_succeeded(
            "fixture".into(),
            serde_json::json!({}),
            vec![item("private-title", "revision-1")],
            Utc::now(),
        )
        .await
        .unwrap();
    let runner = FakeRunner {
        failure: Some("timeout reading /private/project/file".into()),
        ..FakeRunner::successful()
    };
    let logs = DurableLogStore::new(root.path().join("logs"), |value| {
        value.replace("/private/project/file", "[REDACTED]")
    });
    let monitor = IntakeMonitor::new(config, database.clone(), runner, logs);

    let result = monitor.check().await.unwrap();

    assert_eq!(result.errors.len(), 1);
    let event = database.readers().event(1).await.unwrap().unwrap();
    assert_eq!(event.status, EventStatus::Retryable);
    assert_eq!(event.attempt_count, 1);
    assert!(event.next_attempt_at.is_some());
    let log = std::fs::read_to_string(root.path().join("logs/monitor.jsonl")).unwrap();
    assert!(log.contains("\"failureCategory\":\"timeout\""));
    assert!(!log.contains("/private/project/file"));
    assert!(!log.contains("private-title"));
}

#[tokio::test]
async fn recovers_stale_processing_events_before_claiming() {
    let root = tempfile::tempdir().unwrap();
    let config = config(&root);
    let database = IntakeDatabase::open(":memory:").await.unwrap();
    let old = Utc::now() - Duration::minutes(40);
    database
        .source_succeeded(
            "fixture".into(),
            serde_json::json!({}),
            vec![item("entity-1", "revision-1")],
            old,
        )
        .await
        .unwrap();
    database.claim_next(old).await.unwrap().unwrap();
    let runner = FakeRunner::successful();
    let observed_runner = runner.clone();
    let logs = DurableLogStore::new(root.path().join("logs"), str::to_owned);
    let monitor = IntakeMonitor::new(config, database.clone(), runner, logs);

    let result = monitor.check().await.unwrap();

    assert_eq!(result.handled, 1);
    assert_eq!(*observed_runner.events.lock().unwrap(), vec![(1, 2)]);
    assert_eq!(
        database.readers().event(1).await.unwrap().unwrap().status,
        EventStatus::Succeeded
    );
}
