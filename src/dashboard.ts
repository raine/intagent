import dashboardScript from "../web/generated/app.js" with { type: "text" }
import dashboardStyles from "../web/generated/app.css" with { type: "text" }
import type {
  EventRecord,
  EventStatus,
  IntakeDatabase,
  TelemetryCompleteness,
} from "./database.ts"
import { runDetail } from "./run-detail.ts"

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
  eventKind: string
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
  telemetryCompleteness: TelemetryCompleteness
  timelineTruncated: boolean
  investigationHandle: string | null
  steps: Array<{
    id: number
    turnOrdinal: number | null
    kind: "tool" | "thinking" | "compaction"
    label: string
    summary: string | null
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
    if (typeof metadata.url !== "string") return null
    const url = new URL(metadata.url)
    if (
      (url.protocol !== "http:" && url.protocol !== "https:") ||
      url.username ||
      url.password
    )
      return null
    url.search = ""
    url.hash = ""
    return url.href
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
  const sources = database
    .sourceStatuses()
    .filter((source) => source.source !== "manual-injection")
    .map((source) => ({
      source: String(source.source),
      lastSuccessAt:
        typeof source.lastSuccessAt === "string" ? source.lastSuccessAt : null,
      lastError:
        typeof source.lastError === "string"
          ? publicError(source.lastError)
          : null,
      updatedAt: String(source.updatedAt),
    }))
  const runs = database
    .listTriageRunSummaries(50)
    .flatMap((run): DashboardRun[] => {
      const event = database.event(run.eventId)
      if (!event) return []
      const state = run.outcome
        ? run.outcome
        : event.status === "processing"
          ? "active"
          : "interrupted"
      const activitySteps =
        state === "active" ? database.recentTriageRunSteps(run.id, 12) : []
      return [
        {
          id: run.id,
          eventId: run.eventId,
          eventTitle: event.title,
          source: event.source,
          eventKind: event.kind,
          attempt: run.attempt,
          startedAt: run.startedAt,
          endedAt:
            run.endedAt ??
            (state === "interrupted" ? run.lastActivityAt : null),
          lastActivityAt: run.lastActivityAt,
          state,
          modelId: run.modelId,
          modelProvider: run.modelProvider,
          thinkingLevel: run.thinkingLevel,
          turnCount: run.turnCount,
          retryCount: run.retryCount,
          compactionCount: run.compactionCount,
          telemetryCompleteness: run.telemetryCompleteness,
          timelineTruncated: run.stepCount > activitySteps.length,
          investigationHandle: event.investigationHandle,
          steps: activitySteps.map((step) => ({
            id: step.id,
            turnOrdinal: step.turnOrdinal,
            kind: step.kind,
            label: step.label,
            summary: step.summary,
            startedAt: step.startedAt,
            endedAt: step.endedAt,
            state: step.outcome ?? "active",
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
    try {
      const stored = localStorage.getItem("im-theme")
      document.documentElement.dataset.theme = stored === "light" || stored === "dark"
        ? stored
        : matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark"
    } catch {
      document.documentElement.dataset.theme = "dark"
    }
  </script>
  <style>${dashboardStyles}</style>
</head>
<body>
  <div id="root"></div>
  <script type="module">${dashboardScript}</script>
</body>
</html>`
}

const securityHeaders = {
  "Cache-Control": "no-store",
  "Content-Security-Policy":
    "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  "Cross-Origin-Resource-Policy": "same-origin",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
  "Referrer-Policy": "no-referrer",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
}

export interface DashboardRunLimits {
  maxTurns: number | null
  wallTimeoutMs: number | null
}

export function createDashboardHandler(
  database: IntakeDatabase,
  limits: DashboardRunLimits = { maxTurns: null, wallTimeoutMs: null },
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
    const runMatch = url.pathname.match(/^\/api\/runs\/(\d+)$/)
    const runId = runMatch ? positiveSafeInteger(runMatch[1] ?? null) : null
    if (runId !== null) {
      const detail = runDetail(database, runId, {
        offset: queryInteger(url.searchParams.get("offset")) ?? 0,
        limit: queryInteger(url.searchParams.get("limit")) ?? 200,
        maxTurns: limits.maxTurns,
        wallTimeoutMs: limits.wallTimeoutMs,
      })
      return detail
        ? Response.json(detail, { headers: securityHeaders })
        : new Response("Not found", { status: 404, headers: securityHeaders })
    }
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

function queryInteger(value: string | null): number | undefined {
  if (value === null || !/^\d+$/.test(value)) return undefined
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) ? parsed : undefined
}

function positiveSafeInteger(value: string | null): number | null {
  if (value === null || !/^[1-9]\d*$/.test(value)) return null
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) ? parsed : null
}

export function startDashboard(
  database: IntakeDatabase,
  hostname: string,
  port: number,
  limits: DashboardRunLimits = { maxTurns: null, wallTimeoutMs: null },
): ReturnType<typeof Bun.serve> {
  return Bun.serve({
    hostname,
    port,
    fetch: createDashboardHandler(database, limits),
  })
}
