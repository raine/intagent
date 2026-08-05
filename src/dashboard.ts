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
    kind: "tool" | "thinking" | "compaction"
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
          kind: step.kind,
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

function dashboardPage(): string {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>Intake Monitor</title>
  <script>
    (() => {
      try {
        const storedTheme = localStorage.getItem("im-theme")
        const theme = storedTheme === "light" || storedTheme === "dark"
          ? storedTheme
          : (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")
        document.documentElement.dataset.theme = theme
      } catch {
        document.documentElement.dataset.theme = "dark"
      }
    })()
  </script>
  <style>${dashboardStyles}</style>
</head>
<body>
  <a class="skip-link" href="#dashboard-content">Skip to dashboard</a>
  <header class="topbar">
    <a class="brand" href="#/" aria-label="Intake Monitor dashboard">intake-monitor</a>
    <span class="topbar-separator" aria-hidden="true">·</span>
    <div id="connection" class="connection connection-connecting" role="status" aria-live="polite">
      <span class="connection-dot" aria-hidden="true"></span>
      <strong id="connection-label">Connecting</strong>
      <span id="connection-note">waiting for data</span>
    </div>
    <span class="topbar-spacer"></span>
    <time id="dashboard-clock" class="dashboard-clock"></time>
    <button id="theme-toggle" class="theme-toggle" type="button" aria-label="Switch color theme"></button>
  </header>

  <div id="connection-banner" class="connection-banner" role="alert" hidden>
    <strong id="banner-heading"></strong>
    <span id="banner-copy"></span>
  </div>

  <main id="dashboard-content" tabindex="-1">
    <section id="loading-view" class="loading-view" aria-label="Loading dashboard">
      <div class="skeleton-line skeleton-short"></div>
      <div class="skeleton-line"></div>
      <div class="skeleton-grid"><i></i><i></i><i></i><i></i></div>
      <p>Connecting to the intake daemon...</p>
    </section>
    <div id="dashboard-root" hidden></div>
  </main>

  <div id="route-layer" hidden>
    <section id="route-panel" class="route-panel" role="dialog" aria-modal="true" aria-labelledby="route-title">
      <header class="route-header">
        <button id="route-back" class="back-button" type="button"><span aria-hidden="true">←</span> dashboard</button>
        <span aria-hidden="true">/</span>
        <strong id="route-title"></strong>
        <span id="route-status"></span>
        <span class="route-spacer"></span>
        <time id="route-finished"></time>
      </header>
      <div id="route-content"></div>
    </section>
  </div>

  <p id="announcer" class="visually-hidden" aria-live="polite"></p>
  <script>${dashboardScript}</script>
</body>
</html>`
}

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
      return new Response(dashboardPage(), {
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
