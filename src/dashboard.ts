import dashboardScript from "./dashboard/client.js" with { type: "text" }
import dashboardStyles from "./dashboard/styles.css" with { type: "text" }
import type { EventRecord, EventStatus, IntakeDatabase } from "./database.ts"

const allStatuses: EventStatus[] = [
  "pending",
  "processing",
  "retryable",
  "succeeded",
  "failed",
  "ignored",
]

export interface DashboardEvent {
  id: number
  source: string
  entityId: string
  kind: string
  title: string
  url: string | null
  occurredAt: string
  observedAt: string
  status: EventStatus
  attemptCount: number
  nextAttemptAt: string | null
  lastError: string | null
  avenRef: string | null
  investigationHandle: string | null
}

export interface DashboardRun {
  id: number
  eventId: number
  eventTitle: string
  source: string
  attempt: number
  startedAt: string
  endedAt: string | null
  lastActivityAt: string
  state: "active" | "succeeded" | "failed" | "interrupted"
  modelId: string | null
  modelProvider: string | null
  thinkingLevel: string | null
  turnCount: number
  retryCount: number
  compactionCount: number
  investigationHandle: string | null
  steps: Array<{
    id: number
    label: string
    startedAt: string
    endedAt: string | null
    state: "active" | "succeeded" | "failed" | "interrupted"
  }>
}

export interface DashboardSnapshot {
  generatedAt: string
  counts: Record<EventStatus, number>
  total: number
  open: number
  attention: number
  handled: number
  oldestOpenAt: string | null
  sources: Array<{
    source: string
    lastSuccessAt: string | null
    lastError: string | null
    updatedAt: string
  }>
  runs: DashboardRun[]
  events: DashboardEvent[]
}

function eventUrl(event: EventRecord): string | null {
  try {
    const metadata = JSON.parse(event.operationalMetadata) as { url?: unknown }
    return typeof metadata.url === "string" ? metadata.url : null
  } catch {
    return null
  }
}

function publicError(error: string | null): string | null {
  if (!error) return null
  const value = error.toLowerCase()
  if (
    value.includes("auth") ||
    value.includes("credential") ||
    value.includes("token") ||
    value.includes("unauthorized") ||
    value.includes("forbidden")
  )
    return "Authentication failed"
  if (value.includes("rate limit") || value.includes("too many requests"))
    return "Rate limited"
  if (value.includes("timeout") || value.includes("timed out"))
    return "Request timed out"
  if (value.includes("connection reset")) return "Connection reset"
  if (value.includes("not found") || value.includes("404"))
    return "Resource not found (404)"
  if (value.includes("model") && value.includes("unavailable"))
    return "Model unavailable"
  if (value.includes("interrupt")) return "Triage interrupted"
  return "Operation failed"
}

export function dashboardSnapshot(
  database: IntakeDatabase,
  now = new Date(),
): DashboardSnapshot {
  const storedCounts = database.status()
  const counts = Object.fromEntries(
    allStatuses.map((status) => [status, storedCounts[status] ?? 0]),
  ) as Record<EventStatus, number>
  const events = database.listEvents(100).map((event) => ({
    id: event.id,
    source: event.source,
    entityId: event.entityId,
    kind: event.kind,
    title: event.title,
    url: eventUrl(event),
    occurredAt: event.occurredAt,
    observedAt: event.observedAt,
    status: event.status,
    attemptCount: event.attemptCount,
    nextAttemptAt: event.nextAttemptAt,
    lastError: publicError(event.lastError),
    avenRef: event.avenRef,
    investigationHandle: event.investigationHandle,
  }))
  const oldestOpenAt = database.oldestOpenEventAt()
  const sources = database.sourceStatuses().map((source) => ({
    source: String(source.source),
    lastSuccessAt:
      typeof source.lastSuccessAt === "string" ? source.lastSuccessAt : null,
    lastError:
      typeof source.lastError === "string"
        ? publicError(source.lastError)
        : null,
    updatedAt: String(source.updatedAt),
  }))
  const runs = database.listTriageRuns(50).flatMap((run): DashboardRun[] => {
    const event = database.event(run.eventId)
    if (!event) return []
    const state = run.outcome
      ? run.outcome
      : event.status === "processing"
        ? "active"
        : "interrupted"
    return [
      {
        id: run.id,
        eventId: run.eventId,
        eventTitle: event.title,
        source: event.source,
        attempt: run.attempt,
        startedAt: run.startedAt,
        endedAt: run.endedAt,
        lastActivityAt: run.lastActivityAt,
        state,
        modelId: run.modelId,
        modelProvider: run.modelProvider,
        thinkingLevel: run.thinkingLevel,
        turnCount: run.turnCount,
        retryCount: run.retryCount,
        compactionCount: run.compactionCount,
        investigationHandle: event.investigationHandle,
        steps: run.steps.map((step) => ({
          id: step.id,
          label: step.label,
          startedAt: step.startedAt,
          endedAt: step.endedAt,
          state: step.outcome
            ? step.outcome
            : state === "active"
              ? "active"
              : "interrupted",
        })),
      },
    ]
  })

  return {
    generatedAt: now.toISOString(),
    counts,
    total: Object.values(counts).reduce((sum, count) => sum + count, 0),
    open: counts.pending + counts.processing + counts.retryable,
    attention: counts.retryable + counts.failed,
    handled: counts.succeeded + counts.ignored,
    oldestOpenAt,
    sources,
    runs,
    events,
  }
}

const page = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>Intake Monitor</title>
  <script>
    (() => {
      try {
        const choice = localStorage.getItem("im-theme") || "system"
        const resolved = choice === "system"
          ? (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")
          : choice
        document.documentElement.dataset.theme = resolved
        document.documentElement.dataset.themeChoice = choice
      } catch {}
    })()
  </script>
  <style>${dashboardStyles}</style>
</head>
<body>
  <a class="skip-link" href="#dashboard-content">Skip to dashboard</a>
  <header class="topbar">
    <div class="brand">
      <span class="brand-mark" aria-hidden="true">▤</span>
      <span class="brand-copy">
        <strong>Intake Monitor</strong>
        <span id="daemon-label">local intake daemon</span>
      </span>
    </div>
    <span class="topbar-spacer"></span>
    <div id="connection" class="connection connection-connecting" role="status" aria-live="polite">
      <span class="connection-dot" aria-hidden="true"></span>
      <strong id="connection-label">Connecting</strong>
      <span id="connection-note">waiting for data</span>
    </div>
    <div class="theme-control" role="group" aria-label="Theme">
      <button type="button" data-theme-choice="system" title="Follow system theme">auto</button>
      <button type="button" data-theme-choice="light" title="Light theme">light</button>
      <button type="button" data-theme-choice="dark" title="Dark theme">dark</button>
    </div>
  </header>

  <div id="connection-banner" class="connection-banner" role="alert" hidden>
    <strong id="banner-heading"></strong>
    <span id="banner-copy"></span>
  </div>

  <main id="dashboard-content" class="page-main">
    <section id="loading-view" class="loading-view" aria-label="Loading">
      <div class="skeleton-stats"><i></i><i></i><i></i><i></i><i></i></div>
      <i class="skeleton-block skeleton-runs"></i>
      <i class="skeleton-block skeleton-events"></i>
      <p>Connecting to intake daemon...</p>
    </section>

    <div id="dashboard-view" class="dashboard-view" hidden>
      <section class="overview" aria-label="Overview">
        <article class="stat stat-info"><span>Open</span><strong id="stat-open">0</strong><small id="stat-open-note">queue is clear</small></article>
        <article id="attention-stat" class="stat"><span>Needs attention</span><strong id="stat-attention">0</strong><small id="stat-attention-note">nothing to review</small></article>
        <article class="stat stat-ok"><span>Active runs</span><strong id="stat-active">0</strong><small id="stat-active-note">idle</small></article>
        <article class="stat"><span>Handled</span><strong id="stat-handled">0</strong><small>succeeded + ignored</small></article>
        <article class="stat"><span>Oldest open</span><strong id="stat-oldest">-</strong><small id="stat-oldest-note"></small></article>
      </section>

      <div class="dashboard-columns">
        <div class="primary-column">
          <section aria-labelledby="active-runs-title">
            <header class="section-title">
              <h2 id="active-runs-title">Active runs</h2>
              <span>refreshes every 1.5s</span>
            </header>
            <div id="active-runs" class="active-runs"></div>
          </section>

          <section aria-labelledby="events-title">
            <header class="section-title events-heading">
              <h2 id="events-title">Intake events</h2>
              <div id="event-filters" class="filter-tabs" role="group" aria-label="Filter events">
                <button type="button" data-filter="all" aria-pressed="true">Recent <b>0</b></button>
                <button type="button" data-filter="open" aria-pressed="false">Open <b>0</b></button>
                <button type="button" data-filter="attention" aria-pressed="false">Attention <b>0</b></button>
                <button type="button" data-filter="handled" aria-pressed="false">Handled <b>0</b></button>
              </div>
            </header>
            <div class="event-panel">
              <div id="event-list"></div>
              <p id="event-list-note" class="list-note"></p>
            </div>
          </section>
        </div>

        <aside class="rail">
          <section aria-labelledby="sources-title">
            <header class="section-title"><h2 id="sources-title">Sources</h2></header>
            <div id="source-list" class="rail-panel"></div>
          </section>
          <section aria-labelledby="history-title">
            <header class="section-title"><h2 id="history-title">Run history</h2></header>
            <div id="run-history" class="rail-panel"></div>
          </section>
        </aside>
      </div>
      <footer class="page-footer">
        <span>events + sources refresh every 5s - active runs every 1.5s</span>
        <span id="refresh-note"></span>
      </footer>
    </div>

    <section id="run-detail-view" class="run-detail-view" aria-label="Run detail" hidden>
      <button id="back-to-dashboard" class="back-button" type="button"><span aria-hidden="true">←</span> Back to dashboard</button>
      <div id="run-detail"></div>
      <footer class="page-footer">
        <span>events + sources refresh every 5s - active runs every 1.5s</span>
        <span id="detail-refresh-note"></span>
      </footer>
    </section>
  </main>
  <script>${dashboardScript}</script>
</body>
</html>`

const securityHeaders = {
  "Cache-Control": "no-store",
  "Content-Security-Policy":
    "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  "Referrer-Policy": "no-referrer",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
}

export function createDashboardHandler(
  database: IntakeDatabase,
): (request: Request) => Response {
  return (request) => {
    const url = new URL(request.url)
    if (request.method !== "GET")
      return new Response("Method not allowed", {
        status: 405,
        headers: { Allow: "GET", ...securityHeaders },
      })
    if (url.pathname === "/api/snapshot")
      return Response.json(dashboardSnapshot(database), {
        headers: securityHeaders,
      })
    if (url.pathname === "/" || url.pathname === "/index.html")
      return new Response(page, {
        headers: {
          "Content-Type": "text/html; charset=utf-8",
          ...securityHeaders,
        },
      })
    return new Response("Not found", { status: 404, headers: securityHeaders })
  }
}

export function startDashboard(
  database: IntakeDatabase,
  hostname: string,
  port: number,
): ReturnType<typeof Bun.serve> {
  return Bun.serve({ hostname, port, fetch: createDashboardHandler(database) })
}
