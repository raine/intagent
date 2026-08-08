use chrono::{TimeZone, Utc};
use intake::database::{EventRecord, EventStatus, IntakeDatabase, RunId};
use intake::run_detail::{RunDetailOptions, TimelineEntry, run_detail, safe_event_url};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

#[tokio::test]
async fn matches_phase_zero_run_detail_fixture() {
    let (_directory, database) = fixture_database().await;
    let detail = run_detail(
        &database.readers(),
        RunId(1),
        RunDetailOptions {
            max_turns: Some(50),
            wall_timeout_ms: Some(1_800_000),
            now: Utc.with_ymd_and_hms(2026, 8, 7, 10, 5, 0).unwrap(),
            ..RunDetailOptions::default()
        },
    )
    .await
    .unwrap()
    .unwrap();

    let actual = serde_json::to_value(detail).unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/dashboard/run-detail.json")).unwrap();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn omits_retained_payloads_and_raw_errors() {
    let (_directory, database) = fixture_database().await;
    let detail = run_detail(&database.readers(), RunId(3), RunDetailOptions::default())
        .await
        .unwrap()
        .unwrap();
    let serialized = serde_json::to_string(&detail).unwrap();

    assert!(!serialized.contains("retained retry payload"));
    assert!(!serialized.contains("token=private"));
}

#[tokio::test]
async fn clamps_pagination_and_resolves_legacy_siblings() {
    let (_directory, database) = fixture_database().await;
    let detail = run_detail(
        &database.readers(),
        RunId(1),
        RunDetailOptions {
            offset: usize::MAX,
            limit: usize::MAX,
            now: Utc.with_ymd_and_hms(2026, 8, 7, 10, 5, 0).unwrap(),
            ..RunDetailOptions::default()
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(detail.timeline.page.limit, 500);
    assert!(detail.timeline.entries.is_empty());
    assert!(!detail.timeline.page.has_more);

    let legacy = run_detail(
        &database.readers(),
        RunId(2),
        RunDetailOptions {
            now: Utc.with_ymd_and_hms(2026, 8, 7, 10, 5, 0).unwrap(),
            ..RunDetailOptions::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(legacy.run.telemetry.completeness, "legacy");
    assert_eq!(legacy.metrics.tool_call_count, None);
    assert_eq!(legacy.metrics.duration_ms.setup, None);
}

#[tokio::test]
async fn sorts_mixed_timestamp_forms_and_calculates_duration_chronologically() {
    let (directory, database) = fixture_database().await;
    let raw = Connection::open(directory.path().join("dashboard.sqlite")).unwrap();
    raw.execute(
        "UPDATE triage_runs SET started_at = '2026-08-07T12:00:00+02:00' WHERE id = 1",
        [],
    )
    .unwrap();
    raw.execute(
        "UPDATE triage_run_turns SET started_at = '2026-08-07T10:00:10.010Z' WHERE id = 1",
        [],
    )
    .unwrap();
    raw.execute(
        "UPDATE triage_run_steps SET started_at = CASE id
           WHEN 1 THEN '2026-08-07T11:00:00+02:00'
           WHEN 3 THEN '2026-08-07T10:00:10.1Z'
           ELSE started_at END
         WHERE id IN (1, 3)",
        [],
    )
    .unwrap();
    drop(raw);

    let detail = run_detail(
        &database.readers(),
        RunId(1),
        RunDetailOptions {
            now: Utc.with_ymd_and_hms(2026, 8, 7, 10, 5, 0).unwrap(),
            ..RunDetailOptions::default()
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(detail.metrics.duration_ms.wall, 180_000);
    assert_eq!(detail.run.started_at.as_str(), "2026-08-07T12:00:00+02:00");
    assert!(matches!(
        &detail.timeline.entries[0],
        TimelineEntry::Span { id: 1, started_at, .. }
            if started_at.as_str() == "2026-08-07T11:00:00+02:00"
    ));
    assert!(matches!(
        (&detail.timeline.entries[1], &detail.timeline.entries[2]),
        (
            TimelineEntry::Turn { id: 1, .. },
            TimelineEntry::Span { id: 3, .. }
        )
    ));
    assert_eq!(
        serde_json::to_value(&detail).unwrap()["run"]["startedAt"],
        "2026-08-07T12:00:00+02:00"
    );
}

#[test]
fn strips_url_secrets_and_rejects_embedded_credentials() {
    let mut event = event_with_metadata(
        r#"{"url":"https://example.test/issues/1?access_token=private#secret"}"#,
    );
    assert_eq!(
        safe_event_url(&event).as_deref(),
        Some("https://example.test/issues/1")
    );

    event.operational_metadata = r#"{"url":"https://user:password@example.test/issues/1"}"#.into();
    assert_eq!(safe_event_url(&event), None);
    event.operational_metadata = r#"{"url":"file:///private/path"}"#.into();
    assert_eq!(safe_event_url(&event), None);
}

fn event_with_metadata(operational_metadata: &str) -> EventRecord {
    EventRecord {
        id: 1,
        source: "github".into(),
        entity_id: "github:example/project#1".into(),
        revision_id: "revision".into(),
        kind: "github-issue".into(),
        title: "Fixture".into(),
        payload: Some("private".into()),
        operational_metadata: operational_metadata.into(),
        occurred_at: "2026-08-07T10:00:00.000Z".parse().unwrap(),
        observed_at: "2026-08-07T10:00:00.000Z".parse().unwrap(),
        status: EventStatus::Pending,
        attempt_count: 0,
        next_attempt_at: None,
        last_error: None,
        aven_ref: None,
        investigation_handle: None,
    }
}

async fn fixture_database() -> (TempDir, IntakeDatabase) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dashboard.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/database/schema-v7.sql"))
        .unwrap();
    drop(connection);
    let database = IntakeDatabase::open(&path).await.unwrap();
    (directory, database)
}
