import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { RunRoute as RunDetailRoute } from "./run-inspector.tsx"

type EventStatus =
  | "pending"
  | "processing"
  | "retryable"
  | "succeeded"
  | "failed"
  | "ignored"
type RunStatus = "active" | "succeeded" | "failed" | "interrupted"
type StepKind = "tool" | "thinking" | "compaction"

interface DashboardEvent {
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

interface DashboardRun {
  id: number
  eventId: number
  eventTitle: string
  source: string
  eventKind: string
  attempt: number
  startedAt: string
  endedAt: string | null
  lastActivityAt: string
  state: RunStatus
  modelId: string | null
  modelProvider: string | null
  thinkingLevel: string | null
  turnCount: number
  retryCount: number
  compactionCount: number
  telemetryCompleteness: "complete" | "partial" | "legacy"
  timelineTruncated: boolean
  investigationHandle: string | null
  steps: Array<{
    id: number
    turnOrdinal: number | null
    kind: StepKind
    label: string
    summary: string | null
    startedAt: string
    endedAt: string | null
    state: RunStatus
  }>
}

interface DashboardSnapshot {
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

type Filter = "all" | "open" | "attention" | "handled"
type Route = { kind: "run"; id: number } | null

const eventStates: Record<
  EventStatus,
  { glyph: string; short: string; label: string }
> = {
  pending: { glyph: "○", short: "PEND", label: "Pending" },
  processing: { glyph: "◐", short: "PROC", label: "Processing" },
  retryable: { glyph: "↻", short: "RTRY", label: "Retryable" },
  succeeded: { glyph: "✓", short: "OK", label: "Succeeded" },
  failed: { glyph: "✕", short: "FAIL", label: "Failed" },
  ignored: { glyph: "−", short: "IGN", label: "Ignored" },
}

const runStates: Record<
  RunStatus,
  { glyph: string; short: string; label: string }
> = {
  active: { glyph: "◐", short: "RUN", label: "Running" },
  succeeded: { glyph: "✓", short: "OK", label: "Succeeded" },
  failed: { glyph: "✕", short: "FAIL", label: "Failed" },
  interrupted: { glyph: "◌", short: "STOP", label: "Interrupted" },
}

const privacyNote =
  "Event identity, title, source link, typed effects, Bash commands, and read targets are shown. Prompt text, intake bodies, thinking text, other tool arguments, output, raw errors, call identifiers, and cwd are excluded."

function positiveSafeInteger(value: string | null): number | null {
  if (value === null || !/^[1-9]\d*$/.test(value)) return null
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) ? parsed : null
}

function parseTime(value: string | null): number {
  return value ? Date.parse(value) : Date.now()
}

function elapsed(start: string, end: string | null = null): number {
  return Math.max(0, parseTime(end) - parseTime(start))
}

function formatDuration(value: number): string {
  if (value < 1000) return `${Math.round(value)}ms`
  const seconds = Math.round(value / 100) / 10
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  return `${minutes}m ${Math.round(seconds % 60)}s`
}

function compactDuration(value: number): string {
  if (value < 1000) return `${Math.round(value)}ms`
  if (value < 60_000) return `${Math.round(value / 100) / 10}s`
  return `${Math.floor(value / 60_000)}m${Math.round((value % 60_000) / 1000)}s`
}

function relativeTime(value: string, now: number): string {
  const difference = parseTime(value) - now
  const absolute = Math.abs(difference)
  const suffix = difference > 0 ? "from now" : "ago"
  if (absolute < 10_000) return "now"
  if (absolute < 60_000) return `${Math.round(absolute / 1000)}s ${suffix}`
  if (absolute < 3_600_000) return `${Math.round(absolute / 60_000)}m ${suffix}`
  if (absolute < 86_400_000)
    return `${Math.round(absolute / 3_600_000)}h ${suffix}`
  return `${Math.round(absolute / 86_400_000)}d ${suffix}`
}

function clockTime(value: string): string {
  return new Date(value).toLocaleTimeString([], { hour12: false })
}

function useClock(): number {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [])
  return now
}

function useSnapshot(): {
  snapshot: DashboardSnapshot | null
  connection: "connecting" | "live" | "stale" | "offline"
  lastSuccess: number | null
} {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null)
  const [connection, setConnection] = useState<
    "connecting" | "live" | "stale" | "offline"
  >("connecting")
  const [lastSuccess, setLastSuccess] = useState<number | null>(null)

  useEffect(() => {
    let stopped = false
    let timer = 0
    let failures = 0

    const refresh = async (): Promise<void> => {
      try {
        const response = await fetch("/api/snapshot", { cache: "no-store" })
        if (!response.ok) throw new Error(`HTTP ${response.status}`)
        const next = (await response.json()) as DashboardSnapshot
        if (stopped) return
        setSnapshot(next)
        setLastSuccess(Date.now())
        setConnection("live")
        failures = 0
        timer = window.setTimeout(
          refresh,
          next.runs.some((run) => run.state === "active") ? 1500 : 5000,
        )
      } catch {
        if (stopped) return
        failures += 1
        setConnection(failures > 2 ? "offline" : "stale")
        timer = window.setTimeout(
          refresh,
          Math.min(5000 * 2 ** (failures - 1), 30_000),
        )
      }
    }

    void refresh()
    const visible = (): void => {
      if (document.visibilityState === "visible") {
        window.clearTimeout(timer)
        void refresh()
      }
    }
    document.addEventListener("visibilitychange", visible)
    return () => {
      stopped = true
      window.clearTimeout(timer)
      document.removeEventListener("visibilitychange", visible)
    }
  }, [])

  return { snapshot, connection, lastSuccess }
}

function useRoute(): [Route, (route: Route) => void] {
  const parse = (): Route => {
    const match = location.hash.match(/^#\/run\/(\d+)$/)
    const id = match ? positiveSafeInteger(match[1] ?? null) : null
    return id === null ? null : { kind: "run", id }
  }
  const [route, setRoute] = useState<Route>(parse)
  useEffect(() => {
    const update = (): void => setRoute(parse())
    window.addEventListener("hashchange", update)
    return () => window.removeEventListener("hashchange", update)
  }, [])
  const navigate = useCallback((next: Route): void => {
    location.hash = next ? `#/${next.kind}/${next.id}` : "#/"
  }, [])
  return [route, navigate]
}

function Status({
  status,
  run = false,
}: {
  status: EventStatus | RunStatus
  run?: boolean
}): JSX.Element {
  const definition = run
    ? runStates[status as RunStatus]
    : eventStates[status as EventStatus]
  return (
    <span className={`status status-${status}`} aria-label={definition.label}>
      <span aria-hidden="true">{definition.glyph}</span>
      <b aria-hidden="true">{definition.short}</b>
    </span>
  )
}

function ThemeToggle(): JSX.Element {
  const [theme, setTheme] = useState<"light" | "dark">(
    document.documentElement.dataset.theme === "light" ? "light" : "dark",
  )
  const toggle = (): void => {
    const next = theme === "dark" ? "light" : "dark"
    setTheme(next)
    document.documentElement.dataset.theme = next
    localStorage.setItem("im-theme", next)
  }
  return (
    <button
      className="theme-toggle"
      type="button"
      onClick={toggle}
      aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
    >
      {theme === "dark" ? "☾ dark" : "☀ light"}
    </button>
  )
}

function ActivityList({
  run,
  limit,
  className = "activity-list",
}: {
  run: DashboardRun
  limit?: number
  className?: string
}): JSX.Element {
  const now = useClock()
  const steps = limit ? run.steps.slice(-limit) : run.steps
  const maxDuration = Math.max(
    ...run.steps.map((step) => elapsed(step.startedAt, step.endedAt)),
    1,
  )
  if (!steps.length)
    return <p className="empty-state">Waiting for recorded activity</p>
  return (
    <div className={className}>
      {steps.map((step) => {
        const duration = elapsed(step.startedAt, step.endedAt)
        const kind = step.kind || "tool"
        const definition = runStates[step.state]
        const label =
          kind === "thinking"
            ? "∴ thinking"
            : kind === "compaction"
              ? "⇲ compaction"
              : step.summary
                ? `${step.label} · ${step.summary}`
                : step.label
        const glyph =
          step.state === "active"
            ? "◐"
            : kind === "thinking"
              ? "∴"
              : kind === "compaction"
                ? "⇲"
                : definition.glyph
        const liveDuration = step.endedAt
          ? duration
          : now - parseTime(step.startedAt)
        const description = `${label}, ${definition.label}, ${formatDuration(liveDuration)}`
        return (
          <div
            className={`activity-row activity-${kind} activity-${step.state}`}
            key={step.id}
            tabIndex={0}
            aria-label={description}
          >
            <time className="activity-clock">{clockTime(step.startedAt)}</time>
            <span className="activity-turn">{kind}</span>
            <strong className="activity-label">{label}</strong>
            <span className="activity-state">
              {glyph} {compactDuration(liveDuration)}
              {step.state === "active" ? "..." : ""}
            </span>
            <span className="activity-track" aria-hidden="true">
              <i
                style={{
                  width: `${Math.max(2, Math.round((duration / maxDuration) * 100))}%`,
                }}
              />
            </span>
          </div>
        )
      })}
    </div>
  )
}

export function ActiveRunCard({ run }: { run: DashboardRun }): JSX.Element {
  const [expanded, setExpanded] = useState(true)
  const now = useClock()
  const stalled = now - parseTime(run.lastActivityAt) > 120_000
  return (
    <article className={`active-run${stalled ? " is-stalled" : ""}`}>
      <button
        className="active-run-summary"
        type="button"
        onClick={() => setExpanded((value) => !value)}
        aria-expanded={expanded}
      >
        <span className="disclosure" aria-hidden="true">
          {expanded ? "▼" : "▶"}
        </span>
        <strong>{run.eventTitle}</strong>
        <small>{run.source}</small>
        {stalled ? (
          <span className="slow-badge">
            No telemetry for{" "}
            {compactDuration(now - parseTime(run.lastActivityAt))}
          </span>
        ) : null}
        <time>{compactDuration(now - parseTime(run.startedAt))}</time>
      </button>
      <div className="run-metadata">
        <span>
          model <b>{run.modelId ?? "-"}</b>
        </span>
        <span>
          attempt <b>{run.attempt}</b>
        </span>
        <span>
          turns <b>{run.turnCount}</b>
        </span>
        <span>
          retries <b>{run.retryCount}</b>
        </span>
        <span>
          last activity <b>{relativeTime(run.lastActivityAt, now)}</b>
        </span>
      </div>
      <div
        className={`active-run-activity${expanded ? " is-expanded" : ""}`}
        aria-label={
          expanded ? "All recorded activity" : "Latest recorded activity"
        }
      >
        <ActivityList run={run} limit={expanded ? run.steps.length : 4} />
      </div>
      {expanded ? (
        <div className="activity-footer">
          <span>{run.steps.length} entries</span>
          <span className="privacy-inline">{privacyNote}</span>
        </div>
      ) : null}
    </article>
  )
}

export function EventRow({
  event,
  now,
  run,
  openRun,
}: {
  event: DashboardEvent
  now: number
  run: DashboardRun | null
  openRun: () => void
}): JSX.Element {
  const summary = (
    <>
      <Status status={event.status} />
      <strong>{event.title}</strong>
      <small>
        {event.source}/{event.kind}
      </small>
      <span className="event-attempt">
        {event.attemptCount ? `att ${event.attemptCount}` : "-"}
      </span>
      <time>
        {event.status === "retryable" && event.nextAttemptAt
          ? `retry ${relativeTime(event.nextAttemptAt, now)}`
          : relativeTime(event.observedAt, now)}
      </time>
      <span className="event-disclosure" aria-hidden="true">
        {run ? "→" : ""}
      </span>
    </>
  )
  return (
    <article className={`event-row event-${event.status}`}>
      {run ? (
        <button
          className="event-summary"
          type="button"
          onClick={openRun}
          aria-label={`Open run inspector for ${event.title}`}
        >
          {summary}
        </button>
      ) : (
        <div className="event-summary">{summary}</div>
      )}
    </article>
  )
}

export function SourceList({
  sources,
  now,
}: {
  sources: DashboardSnapshot["sources"]
  now: number
}): JSX.Element {
  if (!sources.length)
    return <p className="empty-state">No polling sources configured</p>
  return (
    <div className="sources-list">
      {sources.map((source) => (
        <article
          className={`source-card ${source.lastError ? "is-failing" : "is-healthy"}`}
          key={source.source}
        >
          <div className="source-heading">
            <span aria-hidden="true">{source.lastError ? "✕" : "✓"}</span>
            <strong>{source.source}</strong>
            <b>{source.lastError ? "FAILING" : "HEALTHY"}</b>
          </div>
          <p className="source-poll">
            last poll {relativeTime(source.updatedAt, now)}
          </p>
          {source.lastError ? (
            <p className="source-error">{source.lastError}</p>
          ) : null}
        </article>
      ))}
    </div>
  )
}

function RecentRuns({
  runs,
  now,
  open,
}: {
  runs: DashboardRun[]
  now: number
  open: (run: DashboardRun) => void
}): JSX.Element {
  if (!runs.length)
    return (
      <p className="empty-state">
        Run timelines appear after the watcher records triage activity.
      </p>
    )
  return (
    <div className="recent-runs">
      {runs.map((run) => (
        <button
          className={`recent-run recent-run-${run.state}`}
          type="button"
          key={run.id}
          onClick={() => open(run)}
        >
          <span className="recent-run-state" aria-hidden="true">
            {runStates[run.state].glyph}
          </span>
          <strong>{run.eventTitle}</strong>
          <time>{run.endedAt ? relativeTime(run.endedAt, now) : "active"}</time>
        </button>
      ))}
    </div>
  )
}

function RouteLayer({
  route,
  close,
  navigate,
}: {
  route: Route
  close: () => void
  navigate: (runId: number) => void
}): JSX.Element | null {
  const panel = useRef<HTMLElement>(null)
  const opener = useRef<HTMLElement | null>(null)
  const isOpen = route !== null
  useEffect(() => {
    if (!isOpen) return
    opener.current = document.activeElement as HTMLElement | null
    const background = document.querySelectorAll<HTMLElement>(
      ".skip-link, .topbar, .connection-banner, #dashboard-content",
    )
    document.body.classList.add("route-open")
    for (const element of background) {
      element.setAttribute("inert", "")
      element.setAttribute("aria-hidden", "true")
    }
    requestAnimationFrame(() => {
      const target =
        panel.current?.querySelector<HTMLButtonElement>(".back-button") ??
        panel.current
      target?.focus()
    })
    return () => {
      document.body.classList.remove("route-open")
      for (const element of background) {
        element.removeAttribute("inert")
        element.removeAttribute("aria-hidden")
      }
      if (opener.current?.isConnected) opener.current.focus()
      opener.current = null
    }
  }, [isOpen])
  useEffect(() => {
    if (!route) return
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        close()
        return
      }
      if (event.key !== "Tab" || !panel.current) return
      const focusable = [
        ...panel.current.querySelectorAll<HTMLElement>(
          "button:not(:disabled), select:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])",
        ),
      ].filter((element) => element.offsetParent !== null)
      const first = focusable[0]
      const last = focusable.at(-1)
      if (!first || !last) return
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault()
        last.focus()
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault()
        first.focus()
      }
    }
    document.addEventListener("keydown", onKey)
    return () => document.removeEventListener("keydown", onKey)
  }, [route, close])
  if (!route) return null
  return (
    <div id="route-layer">
      <section
        ref={panel}
        id="route-panel"
        className="route-panel"
        role="dialog"
        aria-modal="true"
        aria-label={`Run ${route.id} inspector`}
        tabIndex={-1}
      >
        <RunDetailRoute runId={route.id} close={close} navigate={navigate} />
      </section>
    </div>
  )
}

function Loading(): JSX.Element {
  return (
    <section className="loading-view" aria-label="Loading dashboard">
      <div className="skeleton-line skeleton-short" />
      <div className="skeleton-line" />
      <div className="skeleton-grid">
        <i />
        <i />
        <i />
        <i />
      </div>
      <p>Connecting to the intake daemon...</p>
    </section>
  )
}

export function App(): JSX.Element {
  const now = useClock()
  const { snapshot, connection, lastSuccess } = useSnapshot()
  const [filter, setFilter] = useState<Filter>("all")
  const [route, navigate] = useRoute()
  const completed = useMemo(
    () => snapshot?.runs.filter((run) => run.state !== "active") ?? [],
    [snapshot],
  )
  const filtered = useMemo(() => {
    if (!snapshot || filter === "all") return snapshot?.events ?? []
    if (filter === "open")
      return snapshot.events.filter((event) =>
        ["pending", "processing", "retryable"].includes(event.status),
      )
    if (filter === "attention")
      return snapshot.events.filter((event) =>
        ["retryable", "failed"].includes(event.status),
      )
    return snapshot.events.filter((event) =>
      ["succeeded", "ignored"].includes(event.status),
    )
  }, [filter, snapshot])
  const counts = useMemo(
    () => ({
      all: snapshot?.events.length ?? 0,
      open:
        snapshot?.events.filter((event) =>
          ["pending", "processing", "retryable"].includes(event.status),
        ).length ?? 0,
      attention:
        snapshot?.events.filter((event) =>
          ["retryable", "failed"].includes(event.status),
        ).length ?? 0,
      handled:
        snapshot?.events.filter((event) =>
          ["succeeded", "ignored"].includes(event.status),
        ).length ?? 0,
    }),
    [snapshot],
  )

  return (
    <>
      <a className="skip-link" href="#dashboard-content">
        Skip to dashboard
      </a>
      <header className="topbar">
        <a className="brand" href="#/" aria-label="Intake Monitor dashboard">
          intake-monitor
        </a>
        <span className="topbar-separator" aria-hidden="true">
          ·
        </span>
        <div className={`connection connection-${connection}`}>
          <span className="connection-dot" aria-hidden="true" />
          <strong role="status" aria-live="polite">
            {connection === "live" ? "LIVE" : connection.toUpperCase()}
          </strong>
          <span className="connection-note">
            {lastSuccess
              ? `refreshed ${relativeTime(new Date(lastSuccess).toISOString(), now)}`
              : "waiting for data"}
          </span>
        </div>
        <span className="topbar-spacer" />
        <time className="dashboard-clock">
          {new Date(now).toLocaleString()}
        </time>
        <ThemeToggle />
      </header>
      {connection === "stale" || connection === "offline" ? (
        <div className="connection-banner" role="alert">
          <strong>
            {connection === "offline"
              ? "Connection lost"
              : "Connection unstable"}
          </strong>
          <span>
            {lastSuccess
              ? `Last successful refresh ${relativeTime(new Date(lastSuccess).toISOString(), now)}`
              : "The dashboard has not received data."}
          </span>
        </div>
      ) : null}
      <main id="dashboard-content" tabIndex={-1}>
        {!snapshot ? (
          <Loading />
        ) : (
          <div id="dashboard-root">
            <section className="stat-strip" aria-label="Queue status">
              <article>
                <span>OPEN</span>
                <strong>{snapshot.open}</strong>
                <small>
                  {snapshot.oldestOpenAt
                    ? `oldest ${relativeTime(snapshot.oldestOpenAt, now)}`
                    : "queue clear"}
                </small>
              </article>
              <article className={snapshot.attention ? "stat-attention" : ""}>
                <span>⚠ NEEDS ATTENTION</span>
                <strong>{snapshot.attention}</strong>
                <small>
                  {snapshot.counts.failed} failed · {snapshot.counts.retryable}{" "}
                  retrying
                </small>
              </article>
              <article className="stat-active">
                <span>▶ ACTIVE RUNS</span>
                <strong>
                  {snapshot.runs.filter((run) => run.state === "active").length}
                </strong>
                <small>live triage</small>
              </article>
              <article>
                <span>HANDLED</span>
                <strong>{snapshot.handled}</strong>
                <small>succeeded + ignored</small>
              </article>
              <article>
                <span>OLDEST OPEN</span>
                <strong>
                  {snapshot.oldestOpenAt
                    ? compactDuration(now - parseTime(snapshot.oldestOpenAt))
                    : "-"}
                </strong>
                <small>{snapshot.total} events retained</small>
              </article>
            </section>
            <div className="dashboard-grid">
              <div className="primary-column">
                <section
                  className="active-section"
                  aria-labelledby="active-title"
                >
                  <h1 id="active-title" className="section-label">
                    ACTIVE RUNS <span>· refresh 1.5s</span>
                  </h1>
                  <div>
                    {snapshot.runs
                      .filter((run) => run.state === "active")
                      .map((run) => (
                        <ActiveRunCard run={run} key={run.id} />
                      ))}
                    {snapshot.runs.every((run) => run.state !== "active") ? (
                      <p className="empty-state active-empty">
                        No active runs. The queue is idle.
                      </p>
                    ) : null}
                  </div>
                </section>
                <section
                  className="events-section"
                  aria-labelledby="events-title"
                >
                  <header className="events-header">
                    <h2 id="events-title" className="section-label">
                      RECENT EVENTS
                    </h2>
                    <div
                      className="filters"
                      role="group"
                      aria-label="Filter recent events"
                    >
                      {(
                        ["all", "open", "attention", "handled"] as Filter[]
                      ).map((value) => (
                        <button
                          type="button"
                          key={value}
                          onClick={() => setFilter(value)}
                          aria-pressed={filter === value}
                        >
                          {value} <b>{counts[value]}</b>
                        </button>
                      ))}
                    </div>
                  </header>
                  <div className="events-list">
                    {filtered.map((event) => {
                      const run =
                        snapshot.runs.find(
                          (candidate) => candidate.eventId === event.id,
                        ) ?? null
                      return (
                        <EventRow
                          event={event}
                          now={now}
                          run={run}
                          key={event.id}
                          openRun={() => {
                            if (run) navigate({ kind: "run", id: run.id })
                          }}
                        />
                      )
                    })}
                    {!filtered.length ? (
                      <p className="empty-state">No events in this view</p>
                    ) : null}
                  </div>
                  <p className="window-note">
                    Showing {filtered.length} from the {snapshot.events.length}
                    -event recent window
                  </p>
                </section>
              </div>
              <aside
                className="side-column"
                aria-label="Source health and completed runs"
              >
                <section aria-labelledby="sources-title">
                  <h2 id="sources-title" className="section-label">
                    SOURCES
                  </h2>
                  <SourceList sources={snapshot.sources} now={now} />
                </section>
                <section
                  className="recent-runs-section"
                  aria-labelledby="recent-runs-title"
                >
                  <h2 id="recent-runs-title" className="section-label">
                    RECENT RUNS
                  </h2>
                  <RecentRuns
                    runs={completed}
                    now={now}
                    open={(run) => navigate({ kind: "run", id: run.id })}
                  />
                </section>
              </aside>
            </div>
          </div>
        )}
      </main>
      <RouteLayer
        route={route}
        close={() => navigate(null)}
        navigate={(runId) => navigate({ kind: "run", id: runId })}
      />
    </>
  )
}
