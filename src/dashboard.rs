use std::net::IpAddr;

use axum::body::Body;
use axum::extract::{Path, RawQuery, Request as AxumRequest, State};
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;

use crate::database::{DatabaseError, DatabaseReaders, EventRecord, EventStatus, RunId};
use crate::run_detail::{RunDetailOptions, run_detail, safe_event_url};

const DASHBOARD_SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/app.js"));
const DASHBOARD_STYLES: &str = include_str!(concat!(env!("OUT_DIR"), "/app.css"));

pub const DEFAULT_DASHBOARD_HOST: &str = "127.0.0.1";
pub const DEFAULT_DASHBOARD_PORT: u16 = 4545;
pub const NON_LOOPBACK_WARNING: &str =
    "Warning: the dashboard title and entity API has no authentication.";

const SECURITY_HEADERS: [(&str, &str); 7] = [
    ("cache-control", "no-store"),
    (
        "content-security-policy",
        "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ),
    ("cross-origin-resource-policy", "same-origin"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()",
    ),
    ("referrer-policy", "no-referrer"),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
];

#[derive(Clone, Copy, Debug, Default)]
pub struct DashboardRunLimits {
    pub max_turns: Option<u32>,
    pub wall_timeout_ms: Option<u64>,
}

#[derive(Clone)]
struct DashboardState {
    database: DatabaseReaders,
    limits: DashboardRunLimits,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub generated_at: String,
    pub counts: DashboardCounts,
    pub total: usize,
    pub open: usize,
    pub attention: usize,
    pub handled: usize,
    pub oldest_open_at: Option<String>,
    pub sources: Vec<DashboardSource>,
    pub runs: Vec<DashboardRun>,
    pub events: Vec<DashboardEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DashboardCounts {
    pub pending: usize,
    pub processing: usize,
    pub retryable: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub ignored: usize,
}

impl DashboardCounts {
    fn total(&self) -> usize {
        self.pending
            + self.processing
            + self.retryable
            + self.succeeded
            + self.failed
            + self.ignored
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSource {
    pub source: String,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardEvent {
    pub id: i64,
    pub source: String,
    pub entity_id: String,
    pub kind: String,
    pub title: String,
    pub url: Option<String>,
    pub occurred_at: String,
    pub observed_at: String,
    pub status: EventStatus,
    pub attempt_count: u32,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub aven_ref: Option<String>,
    pub investigation_handle: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardRun {
    pub id: i64,
    pub event_id: i64,
    pub event_title: String,
    pub source: String,
    pub event_kind: String,
    pub attempt: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_activity_at: String,
    pub state: String,
    pub model_id: Option<String>,
    pub model_provider: Option<String>,
    pub thinking_level: Option<String>,
    pub turn_count: u32,
    pub retry_count: u32,
    pub compaction_count: u32,
    pub telemetry_completeness: String,
    pub timeline_truncated: bool,
    pub investigation_handle: Option<String>,
    pub steps: Vec<DashboardStep>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardStep {
    pub id: i64,
    pub turn_ordinal: Option<i64>,
    pub kind: String,
    pub label: String,
    pub summary: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub state: String,
}

pub fn dashboard_router(database: DatabaseReaders, limits: DashboardRunLimits) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/api/snapshot", get(snapshot))
        .route("/api/runs/{id}", get(run))
        .fallback(fallback)
        .layer(middleware::from_fn(enforce_get))
        .layer(middleware::map_response(add_security_headers))
        .with_state(DashboardState { database, limits })
}

pub async fn dashboard_snapshot(
    database: &DatabaseReaders,
    now: DateTime<Utc>,
) -> Result<DashboardSnapshot, DatabaseError> {
    let stored_counts = database.status().await?;
    let counts = DashboardCounts {
        pending: stored_counts.get("pending").copied().unwrap_or(0),
        processing: stored_counts.get("processing").copied().unwrap_or(0),
        retryable: stored_counts.get("retryable").copied().unwrap_or(0),
        succeeded: stored_counts.get("succeeded").copied().unwrap_or(0),
        failed: stored_counts.get("failed").copied().unwrap_or(0),
        ignored: stored_counts.get("ignored").copied().unwrap_or(0),
    };
    let events = database
        .list_events(100)
        .await?
        .into_iter()
        .map(event_projection)
        .collect();
    let sources = database
        .source_statuses()
        .await?
        .into_iter()
        .filter(|source| source.source != "manual-injection")
        .map(|source| DashboardSource {
            source: source.source,
            last_success_at: source.last_success_at,
            last_error: public_error(source.last_error.as_deref()),
            updated_at: source.updated_at,
        })
        .collect();
    let mut runs = Vec::new();
    for summary in database.list_triage_run_summaries(50).await? {
        let Some(event) = database.event(summary.run.event_id).await? else {
            continue;
        };
        let state = summary.run.outcome.clone().unwrap_or_else(|| {
            if event.status == EventStatus::Processing {
                "active"
            } else {
                "interrupted"
            }
            .into()
        });
        let steps = if state == "active" {
            database
                .recent_triage_run_steps(RunId(summary.run.id), 12)
                .await?
        } else {
            Vec::new()
        };
        runs.push(DashboardRun {
            id: summary.run.id,
            event_id: summary.run.event_id,
            event_title: event.title,
            source: event.source,
            event_kind: event.kind,
            attempt: summary.run.attempt,
            started_at: summary.run.started_at,
            ended_at: summary
                .run
                .ended_at
                .or_else(|| (state == "interrupted").then(|| summary.run.last_activity_at.clone())),
            last_activity_at: summary.run.last_activity_at,
            state,
            model_id: summary.run.model_id,
            model_provider: summary.run.model_provider,
            thinking_level: summary.run.thinking_level,
            turn_count: summary.run.turn_count,
            retry_count: summary.run.retry_count,
            compaction_count: summary.run.compaction_count,
            telemetry_completeness: summary.run.telemetry_completeness,
            timeline_truncated: summary.step_count > steps.len(),
            investigation_handle: event.investigation_handle,
            steps: steps
                .into_iter()
                .map(|step| DashboardStep {
                    id: step.id,
                    turn_ordinal: step.turn_ordinal,
                    kind: step.kind,
                    label: step.label,
                    summary: step.summary,
                    started_at: step.started_at,
                    ended_at: step.ended_at,
                    state: step.outcome.unwrap_or_else(|| "active".into()),
                })
                .collect(),
        });
    }

    let total = counts.total();
    let open = counts.pending + counts.processing + counts.retryable;
    let attention = counts.retryable + counts.failed;
    let handled = counts.succeeded + counts.ignored;
    Ok(DashboardSnapshot {
        generated_at: crate::database::timestamp(now),
        counts,
        total,
        open,
        attention,
        handled,
        oldest_open_at: database.oldest_open_event_at().await?,
        sources,
        runs,
        events,
    })
}

pub fn public_error(error: Option<&str>) -> Option<String> {
    let error = error?;
    let value = error.to_lowercase();
    let category = if ["auth", "credential", "token", "unauthorized", "forbidden"]
        .iter()
        .any(|needle| value.contains(needle))
    {
        "Authentication failed"
    } else if value.contains("rate limit") || value.contains("too many requests") {
        "Rate limited"
    } else if value.contains("timeout") || value.contains("timed out") {
        "Request timed out"
    } else if value.contains("connection reset") {
        "Connection reset"
    } else if value.contains("not found") || value.contains("404") {
        "Resource not found (404)"
    } else if value.contains("model") && value.contains("unavailable") {
        "Model unavailable"
    } else if value.contains("interrupt") {
        "Triage interrupted"
    } else {
        "Operation failed"
    };
    Some(category.into())
}

fn event_projection(event: EventRecord) -> DashboardEvent {
    let url = safe_event_url(&event);
    DashboardEvent {
        id: event.id,
        source: event.source,
        entity_id: event.entity_id,
        kind: event.kind,
        title: event.title,
        url,
        occurred_at: event.occurred_at,
        observed_at: event.observed_at,
        status: event.status,
        attempt_count: event.attempt_count,
        next_attempt_at: event.next_attempt_at,
        last_error: public_error(event.last_error.as_deref()),
        aven_ref: event.aven_ref,
        investigation_handle: event.investigation_handle,
    }
}

async fn index() -> Response {
    response(StatusCode::OK, "text/html; charset=utf-8", dashboard_page())
}

async fn snapshot(State(state): State<DashboardState>) -> Response {
    match dashboard_snapshot(&state.database, Utc::now()).await {
        Ok(snapshot) => json_response(StatusCode::OK, &snapshot),
        Err(_) => response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "text/plain;charset=utf-8",
            "Operation failed",
        ),
    }
}

async fn run(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let Some(id) = positive_safe_integer(&id) else {
        return not_found();
    };
    let (offset, limit) = run_query(query.as_deref());
    let options = RunDetailOptions {
        offset,
        limit,
        max_turns: state.limits.max_turns,
        wall_timeout_ms: state.limits.wall_timeout_ms,
        now: Utc::now(),
    };
    match run_detail(&state.database, RunId(id), options).await {
        Ok(Some(detail)) => json_response(StatusCode::OK, &detail),
        Ok(None) => not_found(),
        Err(_) => response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "text/plain;charset=utf-8",
            "Operation failed",
        ),
    }
}

async fn fallback(request: Request<Body>) -> Response {
    if request.method() != Method::GET {
        let mut response = response(
            StatusCode::METHOD_NOT_ALLOWED,
            "text/plain;charset=utf-8",
            "Method not allowed",
        );
        response
            .headers_mut()
            .insert("allow", HeaderValue::from_static("GET"));
        response
    } else {
        not_found()
    }
}

async fn enforce_get(request: AxumRequest, next: Next) -> Response {
    if request.method() == Method::GET {
        next.run(request).await
    } else {
        let mut response = response(
            StatusCode::METHOD_NOT_ALLOWED,
            "text/plain;charset=utf-8",
            "Method not allowed",
        );
        response
            .headers_mut()
            .insert("allow", HeaderValue::from_static("GET"));
        response
    }
}

async fn add_security_headers(mut response: Response) -> Response {
    for (name, value) in SECURITY_HEADERS {
        response.headers_mut().insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

fn dashboard_page() -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>Intake Monitor</title>
  <script>
    try {{
      const stored = localStorage.getItem("im-theme")
      document.documentElement.dataset.theme = stored === "light" || stored === "dark"
        ? stored
        : matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark"
    }} catch {{
      document.documentElement.dataset.theme = "dark"
    }}
  </script>
  <style>{DASHBOARD_STYLES}</style>
</head>
<body>
  <div id="root"></div>
  <script type="module">{DASHBOARD_SCRIPT}</script>
</body>
</html>"##
    )
}

fn json_response(status: StatusCode, value: &impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/json;charset=utf-8"),
    );
    response
}

fn response(status: StatusCode, content_type: &'static str, body: impl Into<Body>) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(content_type));
    response
}

fn not_found() -> Response {
    response(
        StatusCode::NOT_FOUND,
        "text/plain;charset=utf-8",
        "Not found",
    )
}

fn run_query(query: Option<&str>) -> (usize, usize) {
    let mut offset = None;
    let mut limit = None;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        if key == "offset" {
            offset = query_integer(&value);
        } else if key == "limit" {
            limit = query_integer(&value);
        }
    }
    (offset.unwrap_or(0), limit.unwrap_or(200).clamp(1, 500))
}

fn query_integer(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = value.parse::<u64>().ok()?;
    (value <= 9_007_199_254_740_991).then_some(value as usize)
}

fn positive_safe_integer(value: &str) -> Option<i64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let value = value.parse::<u64>().ok()?;
    (value <= 9_007_199_254_740_991).then_some(value as i64)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardBind {
    host: String,
    port: u16,
    warning: Option<&'static str>,
}

impl DashboardBind {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn warning(&self) -> Option<&'static str> {
        self.warning
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DashboardBindError {
    #[error("dashboard port must be between 1 and 65535")]
    InvalidPort,
    #[error(
        "dashboard host {host} is not loopback; pass --allow-non-loopback to acknowledge that the title and entity API has no authentication"
    )]
    NonLoopbackRequiresAcknowledgement { host: String },
}

pub fn dashboard_bind(
    host: Option<&str>,
    port: Option<u32>,
    allow_non_loopback: bool,
) -> Result<DashboardBind, DashboardBindError> {
    let host = host.unwrap_or(DEFAULT_DASHBOARD_HOST);
    let port = port.unwrap_or(u32::from(DEFAULT_DASHBOARD_PORT));
    let port = u16::try_from(port)
        .ok()
        .filter(|port| *port != 0)
        .ok_or(DashboardBindError::InvalidPort)?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback && !allow_non_loopback {
        return Err(DashboardBindError::NonLoopbackRequiresAcknowledgement { host: host.into() });
    }
    Ok(DashboardBind {
        host: host.into(),
        port,
        warning: (!loopback).then_some(NON_LOOPBACK_WARNING),
    })
}

pub async fn serve_dashboard(
    database: DatabaseReaders,
    bind: &DashboardBind,
    limits: DashboardRunLimits,
) -> std::io::Result<()> {
    if let Some(warning) = bind.warning {
        eprintln!("{warning}");
    }
    let listener = tokio::net::TcpListener::bind((bind.host.as_str(), bind.port)).await?;
    axum::serve(listener, dashboard_router(database, limits)).await
}
