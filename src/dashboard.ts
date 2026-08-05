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

export const dashboardDesigns = [
  "ledger",
  "console",
  "pipeline",
  "briefing",
  "wall",
] as const

export type DashboardDesign = (typeof dashboardDesigns)[number]

export function dashboardDesign(value: string | null): DashboardDesign | null {
  return dashboardDesigns.find((design) => design === value) ?? null
}

function dashboardPage(queryDesign: DashboardDesign | null): string {
  const requestedDesign = queryDesign ? JSON.stringify(queryDesign) : "null"
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>Intake Monitor</title>
  <script>
    (() => {
      const designs = ["ledger", "console", "pipeline", "briefing", "wall"]
      try {
        const themeChoice = localStorage.getItem("im-theme") || "system"
        const theme = themeChoice === "system"
          ? (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")
          : themeChoice
        const requestedDesign = ${requestedDesign}
        const storedDesign = localStorage.getItem("im-design")
        const design = requestedDesign || (designs.includes(storedDesign) ? storedDesign : "ledger")
        document.documentElement.dataset.theme = theme
        document.documentElement.dataset.themeChoice = themeChoice
        document.documentElement.dataset.design = design
      } catch {
        document.documentElement.dataset.theme = "dark"
        document.documentElement.dataset.themeChoice = "system"
        document.documentElement.dataset.design = ${requestedDesign} || "ledger"
      }
    })()
  </script>
  <style>${dashboardStyles}</style>
</head>
<body>
  <a class="skip-link" href="#dashboard-content">Skip to dashboard</a>
  <header class="topbar">
    <a class="brand" href="#/" aria-label="Intake Monitor dashboard">
      <span class="brand-mark" aria-hidden="true">IM</span>
      <span class="brand-copy"><strong>Intake Monitor</strong><span id="daemon-label">local daemon</span></span>
    </a>
    <div class="design-control">
      <label for="design-select">Design</label>
      <select id="design-select" aria-label="Dashboard design">
        <option value="ledger">Ledger</option>
        <option value="console">Console</option>
        <option value="pipeline">Pipeline</option>
        <option value="briefing">Briefing</option>
        <option value="wall">Wall</option>
      </select>
    </div>
    <span class="topbar-spacer"></span>
    <div id="connection" class="connection connection-connecting" role="status" aria-live="polite">
      <span class="connection-dot" aria-hidden="true"></span>
      <strong id="connection-label">Connecting</strong>
      <span id="connection-note">waiting for data</span>
    </div>
    <div class="theme-control" role="group" aria-label="Theme">
      <button type="button" data-theme-choice="system" aria-pressed="true">auto</button>
      <button type="button" data-theme-choice="light" aria-pressed="false">light</button>
      <button type="button" data-theme-choice="dark" aria-pressed="false">dark</button>
    </div>
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
    <button id="route-backdrop" class="route-backdrop" type="button" aria-label="Close detail"></button>
    <section id="route-panel" class="route-panel" role="dialog" aria-modal="true" aria-labelledby="route-title">
      <header class="route-header">
        <button id="route-back" class="back-button" type="button"><span aria-hidden="true">←</span> Back</button>
        <span id="route-kind"></span>
        <button id="route-close" class="icon-button" type="button" aria-label="Close detail">×</button>
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
      return new Response(
        dashboardPage(dashboardDesign(url.searchParams.get("design"))),
        {
          headers: {
            "Content-Type": "text/html; charset=utf-8",
            ...securityHeaders,
          },
        },
      )
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
