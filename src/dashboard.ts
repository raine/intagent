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
    lastError: event.lastError,
    avenRef: event.avenRef,
    investigationHandle: event.investigationHandle,
  }))
  const oldestOpenAt = database.oldestOpenEventAt()
  const sources = database.sourceStatuses().map((source) => ({
    source: String(source.source),
    lastSuccessAt:
      typeof source.lastSuccessAt === "string" ? source.lastSuccessAt : null,
    lastError: typeof source.lastError === "string" ? source.lastError : null,
    updatedAt: String(source.updatedAt),
  }))

  return {
    generatedAt: now.toISOString(),
    counts,
    total: Object.values(counts).reduce((sum, count) => sum + count, 0),
    open: counts.pending + counts.processing + counts.retryable,
    attention: counts.retryable + counts.failed,
    handled: counts.succeeded + counts.ignored,
    oldestOpenAt,
    sources,
    events,
  }
}

const page = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark">
  <title>Intake monitor</title>
  <style>${dashboardStyles}</style>
</head>
<body>
  <a class="skip-link" href="#activity">Skip to activity</a>
  <div class="shell">
    <header class="topbar">
      <div class="brand" aria-label="Intake monitor">
        <svg class="brand-mark" viewBox="0 0 32 32" aria-hidden="true">
          <path d="M5 7h22v18H5z"/><path d="M9 11h14M9 16h9M9 21h6"/>
          <path class="brand-pulse" d="M22 17v4h4"/>
        </svg>
        <span class="brand-name">intake</span><span class="brand-divider"></span><span class="brand-section">monitor</span>
      </div>
      <div class="connection" role="status"><span class="live-dot"></span><span id="connection-label">connecting</span></div>
      <time id="updated-at" class="updated-at"></time>
    </header>

    <main>
      <section class="hero" aria-labelledby="page-title">
        <div class="hero-copy">
          <p class="eyebrow">Triage control</p>
          <h1 id="page-title">Every signal,<br><span>accounted for.</span></h1>
          <p id="hero-summary" class="hero-summary">Reading intake state...</p>
        </div>
        <div class="hero-status" aria-live="polite">
          <div class="readout">
            <span class="readout-label">Open queue</span>
            <strong id="open-count">0</strong>
            <span id="queue-age" class="readout-note">No waiting items</span>
          </div>
          <div id="attention-card" class="attention-card">
            <span class="attention-icon" aria-hidden="true">!</span>
            <span><strong id="attention-count">0</strong><small>need attention</small></span>
          </div>
        </div>
      </section>

      <section class="flow-panel" aria-labelledby="flow-title">
        <header class="section-heading">
          <div><p class="eyebrow">Queue flow</p><h2 id="flow-title">From observed to handled</h2></div>
          <p id="flow-total" class="section-meta"></p>
        </header>
        <ol class="flowline">
          <li data-stage="pending"><span class="stage-index">01</span><span class="stage-mark"></span><span class="stage-label">Pending</span><strong data-count="pending">0</strong></li>
          <li data-stage="processing"><span class="stage-index">02</span><span class="stage-mark"></span><span class="stage-label">Processing</span><strong data-count="processing">0</strong></li>
          <li data-stage="retryable"><span class="stage-index">03</span><span class="stage-mark"></span><span class="stage-label">Retrying</span><strong data-count="retryable">0</strong></li>
          <li data-stage="handled"><span class="stage-index">04</span><span class="stage-mark"></span><span class="stage-label">Handled</span><strong data-count="handled">0</strong></li>
        </ol>
        <div class="outcome-key" aria-label="Handled outcome breakdown">
          <span><i class="key-mark succeeded"></i>Succeeded <strong data-count="succeeded">0</strong></span>
          <span><i class="key-mark ignored"></i>Ignored <strong data-count="ignored">0</strong></span>
          <span><i class="key-mark failed"></i>Failed <strong data-count="failed">0</strong></span>
        </div>
      </section>

      <div class="workspace">
        <section id="activity" class="activity-panel" aria-labelledby="activity-title">
          <header class="section-heading activity-heading">
            <div><p class="eyebrow">Activity ledger</p><h2 id="activity-title">Recent intake</h2></div>
            <div class="filter-wrap">
              <label for="status-filter">Show</label>
              <select id="status-filter">
                <option value="all">All statuses</option>
                <option value="open">Open queue</option>
                <option value="attention">Needs attention</option>
                <option value="succeeded">Succeeded</option>
                <option value="ignored">Ignored</option>
              </select>
            </div>
          </header>
          <div class="table-wrap">
            <table>
              <thead><tr><th>Status</th><th>Item</th><th>Source</th><th>Observed</th><th><span class="sr-only">Details</span></th></tr></thead>
              <tbody id="event-rows"></tbody>
            </table>
            <div id="activity-empty" class="empty-state" hidden><strong>No matching activity</strong><span>Choose another status to see recent intake.</span></div>
          </div>
        </section>

        <aside class="sources-panel" aria-labelledby="sources-title">
          <header class="section-heading">
            <div><p class="eyebrow">Connectors</p><h2 id="sources-title">Source pulse</h2></div>
            <span id="source-count" class="source-count">0</span>
          </header>
          <div id="source-list" class="source-list"></div>
        </aside>
      </div>
    </main>
  </div>
  <template id="event-detail-template"><tr class="detail-row"><td colspan="5"><div class="event-detail"></div></td></tr></template>
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
