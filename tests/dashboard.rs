use axum::body::{Body, to_bytes};
use chrono::{TimeZone, Utc};
use http::{Method, Request, StatusCode};
use intagent::dashboard::{
    DEFAULT_DASHBOARD_HOST, DEFAULT_DASHBOARD_PORT, DashboardBindError, DashboardRunLimits,
    NON_LOOPBACK_WARNING, dashboard_bind, dashboard_router, dashboard_snapshot,
};
use intagent::database::{IntagentDatabase, RunId};
use intagent::run_detail::{RunDetailOptions, run_detail};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn matches_phase_zero_snapshot_fixture() {
    let (_directory, database) = fixture_database().await;
    let snapshot = dashboard_snapshot(
        &database.readers(),
        Utc.with_ymd_and_hms(2026, 8, 7, 10, 5, 0).unwrap(),
    )
    .await
    .unwrap();

    let actual = serde_json::to_value(snapshot).unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/dashboard/snapshot.json")).unwrap();
    assert_eq!(actual, expected);
    let serialized = actual.to_string();
    assert!(!serialized.contains("retained retry payload"));
    assert!(!serialized.contains("token=private"));
}

#[tokio::test]
async fn dashboard_and_run_detail_share_presentation_facts() {
    let (_directory, database) = fixture_database().await;
    let readers = database.readers();
    let snapshot = dashboard_snapshot(&readers, Utc::now()).await.unwrap();

    for summary in &snapshot.runs {
        let detail = run_detail(&readers, RunId(summary.id), RunDetailOptions::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.state, detail.run.state);
        assert_eq!(summary.ended_at, detail.run.ended_at);
        assert_eq!(summary.dispatch_sequence, detail.run.dispatch.sequence);
        assert_eq!(summary.dispatch_trigger, detail.run.dispatch.trigger);
        assert_eq!(summary.conclusion, detail.run.conclusion);
        assert_eq!(summary.source, detail.event.source);
        assert_eq!(summary.event_kind, detail.event.kind);
        assert_eq!(summary.event_title, detail.event.title);

        let event = snapshot
            .events
            .iter()
            .find(|event| event.id == summary.event_id)
            .unwrap();
        assert_eq!(event.source, detail.event.source);
        assert_eq!(event.entity_id, detail.event.entity_id);
        assert_eq!(event.kind, detail.event.kind);
        assert_eq!(event.title, detail.event.title);
        assert_eq!(event.url, detail.event.url);
        assert_eq!(event.occurred_at, detail.event.occurred_at);
        assert_eq!(event.observed_at, detail.event.observed_at);
        assert_eq!(event.status, detail.event.status);
        assert_eq!(event.aven_ref, detail.event.aven_ref);
        assert_eq!(
            event.investigation_handle,
            detail.event.investigation_handle
        );
    }
}

#[tokio::test]
async fn reads_legacy_events_with_null_source_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dashboard.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/database/schema-v7.sql"))
        .unwrap();
    let expected_source: String = connection
        .query_row(
            "SELECT en.source FROM events ev JOIN entities en ON en.id = ev.entity_id WHERE ev.id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute("UPDATE events SET source = NULL WHERE id = 1", [])
        .unwrap();
    drop(connection);

    let database = IntagentDatabase::open(&path).await.unwrap();
    let snapshot = dashboard_snapshot(&database.readers(), Utc::now())
        .await
        .unwrap();
    let event = snapshot.events.iter().find(|event| event.id == 1).unwrap();
    assert_eq!(event.source, expected_source);
}

#[tokio::test]
async fn serves_only_the_read_only_dashboard_surface_with_security_headers() {
    let (_directory, database) = fixture_database().await;
    let router = dashboard_router(
        database.readers(),
        DashboardRunLimits {
            max_turns: Some(50),
            max_attempts: Some(3),
            wall_timeout_ms: Some(1_800_000),
        },
    );

    for path in ["/", "/index.html", "/api/snapshot", "/api/runs/1"] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_security_headers(&response);
    }

    for path in [
        "/app.js",
        "/app.css",
        "/api",
        "/api/runs/0",
        "/api/runs/-1",
        "/api/runs/1.5",
        "/api/runs/9007199254740992",
        "/api/runs/1/extra",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_security_headers(&response);
    }

    for method in [Method::POST, Method::HEAD, Method::OPTIONS] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri("/api/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(response.headers().get("allow").unwrap(), "GET");
        assert_security_headers(&response);
    }
}

#[tokio::test]
async fn serves_inlined_assets_and_observable_content_types() {
    let (_directory, database) = fixture_database().await;
    let router = dashboard_router(database.readers(), DashboardRunLimits::default());
    let page = router
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        page.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    let html = String::from_utf8(
        to_bytes(page.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("<div id=\"root\"></div>"));
    assert!(html.contains("<script type=\"module\">"));
    assert!(html.contains("ACTIVE RUNS"));
    assert!(html.contains("RECENT EVENTS"));
    assert!(html.contains("SOURCES"));
    assert!(html.contains("localStorage.getItem(\"intagent-theme\")"));
    assert!(html.contains("stored === \"system\""));
    assert!(html.contains("dataset.themePreference"));
    assert!(html.contains("@media (width<=700px)"));

    let api = router
        .oneshot(
            Request::builder()
                .uri("/api/runs/1?offset=1&limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        api.headers().get("content-type").unwrap(),
        "application/json;charset=utf-8"
    );
    let detail: Value =
        serde_json::from_slice(&to_bytes(api.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(detail["timeline"]["page"]["offset"], 1);
    assert_eq!(detail["timeline"]["page"]["limit"], 2);
}

#[test]
fn maps_errors_to_public_categories() {
    use intagent::dashboard::public_error;

    for (raw, expected) in [
        ("credential rejected", "Authentication failed"),
        (
            "source exited 1: source polling failed: Fastmail email response is invalid",
            "Fastmail email response is invalid",
        ),
        ("too many requests", "Rate limited"),
        ("request timed out", "Request timed out"),
        ("connection reset by peer", "Connection reset"),
        ("resource returned 404", "Resource not found (404)"),
        ("configured model unavailable", "Model unavailable"),
        ("triage interrupted", "Triage interrupted"),
        ("secret internal detail", "Operation failed"),
    ] {
        assert_eq!(public_error(Some(raw)).as_deref(), Some(expected));
    }
    assert_eq!(public_error(None), None);
}

#[test]
fn requires_acknowledgement_for_non_loopback_hosts() {
    let defaults = dashboard_bind(None, None, false).unwrap();
    assert_eq!(defaults.host(), DEFAULT_DASHBOARD_HOST);
    assert_eq!(defaults.port(), DEFAULT_DASHBOARD_PORT);
    assert_eq!(defaults.warning(), None);

    assert_eq!(
        dashboard_bind(Some("0.0.0.0"), None, false),
        Err(DashboardBindError::NonLoopbackRequiresAcknowledgement {
            host: "0.0.0.0".into(),
        })
    );
    let acknowledged = dashboard_bind(Some("0.0.0.0"), Some(8080), true).unwrap();
    assert_eq!(acknowledged.warning(), Some(NON_LOOPBACK_WARNING));
    assert!(dashboard_bind(Some("::1"), None, false).is_ok());
    assert!(dashboard_bind(Some("localhost"), None, false).is_ok());
    assert_eq!(
        dashboard_bind(None, Some(0), false),
        Err(DashboardBindError::InvalidPort)
    );
}

fn assert_security_headers(response: &http::Response<Body>) {
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    assert!(
        response
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    assert_eq!(
        response
            .headers()
            .get("cross-origin-resource-policy")
            .unwrap(),
        "same-origin"
    );
    assert_eq!(response.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        response.headers().get("referrer-policy").unwrap(),
        "no-referrer"
    );
    assert_eq!(
        response.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(
        response.headers().get("permissions-policy").unwrap(),
        "camera=(), microphone=(), geolocation=()"
    );
}

async fn fixture_database() -> (TempDir, IntagentDatabase) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dashboard.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/database/schema-v7.sql"))
        .unwrap();
    drop(connection);
    let database = IntagentDatabase::open(&path).await.unwrap();
    (directory, database)
}
