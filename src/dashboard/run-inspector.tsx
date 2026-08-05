import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import type { RunDetail, RunTimelineEntry } from "../run-detail.ts"
import {
  entryEnd,
  entryPosition,
  eventRunDisagree,
  groupTimeline,
  matchesFilter,
  runEnd,
  runSummaryCounts,
  timeBudget,
  type CompactionEntry,
  type GroupedSpan,
  type PhaseEntry,
  type RetryEntry,
  type SummaryCount,
  type TimelineFilter,
  type TurnGroup,
} from "./run-inspector-data.ts"

const privacyNote =
  "Run telemetry and durable triage logs retain safe timing, state, counts, tool names, and categorized failures. They exclude prompt text, intake bodies, thinking text, arguments, commands, output, raw errors, session and tool call identifiers, cwd, and file paths."
const staleThreshold = 120_000
const pageSize = 200

const filterLabels: Record<TimelineFilter, string> = {
  all: "All",
  attention: "Attention",
  tools: "Tools",
  thinking: "Thinking",
  retries: "Retries",
  compactions: "Compactions",
  gaps: "Gaps",
}

function safeExternalUrl(value: string | null): string | null {
  if (!value) return null
  try {
    const url = new URL(value)
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

function parseTime(value: string): number {
  return Date.parse(value)
}

function timelineDuration(
  detail: RunDetail,
  entry: RunTimelineEntry,
  now: number,
): number {
  return Math.max(0, entryEnd(detail, entry, now) - parseTime(entry.startedAt))
}

function formatDuration(value: number): string {
  if (value < 1000) return `${Math.round(value)}ms`
  const seconds = Math.round(value / 100) / 10
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${Math.round(seconds % 60)}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat(undefined, { notation: "compact" }).format(value)
}

function formatMoney(value: number): string {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 4,
  }).format(value)
}

function exactTime(value: string): string {
  return new Date(value).toLocaleString([], {
    dateStyle: "medium",
    timeStyle: "medium",
  })
}

function offsetTime(detail: RunDetail, value: string): string {
  return `+${formatDuration(Math.max(0, parseTime(value) - parseTime(detail.run.startedAt)))}`
}

function relativeAge(value: string, now: number): string {
  const age = Math.max(0, now - parseTime(value))
  return `${formatDuration(age)} ago`
}

function stateLabel(value: string): string {
  return value.replaceAll("_", " ")
}

function useInspectorClock(active: boolean): number {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (!active) return
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [active])
  return now
}

interface RunDetailState {
  detail: RunDetail | null
  state: "loading" | "ready" | "not-found" | "failed" | "stale"
  error: string | null
  refreshing: boolean
}

function useRunDetail(runId: number): {
  value: RunDetailState
  refresh: () => Promise<void>
  loadMore: () => Promise<void>
} {
  const [value, setValue] = useState<RunDetailState>({
    detail: null,
    state: "loading",
    error: null,
    refreshing: false,
  })
  const stopped = useRef(false)
  const requestSequence = useRef(0)

  const fetchPage = useCallback(
    async (offset: number): Promise<RunDetail> => {
      const response = await fetch(
        `/api/runs/${runId}?offset=${offset}&limit=${pageSize}`,
        { cache: "no-store" },
      )
      if (response.status === 404) throw new Error("not-found")
      if (!response.ok) throw new Error(`HTTP ${response.status}`)
      return (await response.json()) as RunDetail
    },
    [runId],
  )

  const refresh = useCallback(async (): Promise<void> => {
    const sequence = ++requestSequence.current
    setValue((current) => ({
      ...current,
      state: current.detail ? current.state : "loading",
      refreshing: Boolean(current.detail),
    }))
    try {
      let next = await fetchPage(0)
      const retainedCount = value.detail?.timeline.entries.length ?? pageSize
      while (
        next.timeline.page.hasMore &&
        next.timeline.entries.length < retainedCount
      ) {
        const page = await fetchPage(next.timeline.entries.length)
        next = mergePage(next, page)
      }
      if (stopped.current || sequence !== requestSequence.current) return
      setValue({ detail: next, state: "ready", error: null, refreshing: false })
    } catch (error) {
      if (stopped.current || sequence !== requestSequence.current) return
      const message = error instanceof Error ? error.message : "Request failed"
      setValue((current) => ({
        ...current,
        state:
          message === "not-found"
            ? "not-found"
            : current.detail
              ? "stale"
              : "failed",
        error: message,
        refreshing: false,
      }))
    }
  }, [fetchPage, value.detail?.timeline.entries.length])

  const loadMore = useCallback(async (): Promise<void> => {
    const current = value.detail
    if (!current?.timeline.page.hasMore || value.refreshing) return
    setValue((state) => ({ ...state, refreshing: true }))
    try {
      const page = await fetchPage(current.timeline.entries.length)
      if (stopped.current) return
      setValue({
        detail: mergePage(current, page),
        state: "ready",
        error: null,
        refreshing: false,
      })
    } catch (error) {
      if (stopped.current) return
      setValue((state) => ({
        ...state,
        state: "stale",
        error: error instanceof Error ? error.message : "Request failed",
        refreshing: false,
      }))
    }
  }, [fetchPage, value.detail, value.refreshing])

  useEffect(() => {
    stopped.current = false
    setValue({
      detail: null,
      state: "loading",
      error: null,
      refreshing: false,
    })
    void refresh()
    return () => {
      stopped.current = true
      requestSequence.current += 1
    }
  }, [runId])

  useEffect(() => {
    if (value.detail?.run.state !== "active") return
    const timer = window.setTimeout(() => void refresh(), 1500)
    return () => window.clearTimeout(timer)
  }, [refresh, value.detail?.generatedAt, value.detail?.run.state])

  return { value, refresh, loadMore }
}

function mergePage(base: RunDetail, page: RunDetail): RunDetail {
  const entries = [...base.timeline.entries, ...page.timeline.entries]
  return {
    ...page,
    timeline: {
      entries,
      page: {
        ...page.timeline.page,
        offset: 0,
        returned: entries.length,
      },
    },
  }
}

export function RunRoute({
  runId,
  close,
  navigate,
}: {
  runId: number
  close: () => void
  navigate: (runId: number) => void
}): JSX.Element {
  const { value, refresh, loadMore } = useRunDetail(runId)
  if (!value.detail && value.state === "loading")
    return (
      <div className="inspector-route-state" role="status">
        <span className="route-state-pulse" aria-hidden="true" />
        <strong>Loading run {runId}</strong>
        <span>Requesting the full run record independently.</span>
      </div>
    )
  if (!value.detail && value.state === "not-found")
    return (
      <div className="inspector-route-state">
        <strong>Run {runId} was not found</strong>
        <span>The run may have expired or the address may be incorrect.</span>
        <button type="button" onClick={close}>
          Back to runs
        </button>
      </div>
    )
  if (!value.detail)
    return (
      <div className="inspector-route-state" role="alert">
        <strong>Run data could not be loaded</strong>
        <span>{value.error}</span>
        <div>
          <button type="button" onClick={() => void refresh()}>
            Retry
          </button>
          <button type="button" onClick={close}>
            Back to runs
          </button>
        </div>
      </div>
    )
  return (
    <RunInspector
      detail={value.detail}
      requestState={value.state}
      refreshing={value.refreshing}
      onBack={close}
      onRefresh={() => void refresh()}
      onLoadMore={() => void loadMore()}
      onNavigateAttempt={navigate}
    />
  )
}

function attentionItems(
  detail: RunDetail,
  now: number,
): Array<{
  tone: "critical" | "warning" | "info"
  title: string
  body: string
}> {
  const counts = runSummaryCounts(detail)
  const items: Array<{
    tone: "critical" | "warning" | "info"
    title: string
    body: string
  }> = []
  if (detail.run.state === "failed")
    items.push({
      tone: "critical",
      title: "Run failed",
      body: detail.run.failureCategory
        ? `Failure category: ${stateLabel(detail.run.failureCategory)}.`
        : "No safe failure category was recorded.",
    })
  if (detail.run.state === "interrupted")
    items.push({
      tone: "critical",
      title: "Run interrupted",
      body: detail.run.terminationReason
        ? `Termination reason: ${stateLabel(detail.run.terminationReason)}.`
        : "Execution ended before a normal terminal outcome.",
    })
  if (counts.failedTools.value > 0)
    items.push({
      tone: detail.run.state === "succeeded" ? "warning" : "critical",
      title: `${countQualifier(counts.failedTools)} tool ${counts.failedTools.value === 1 ? "failure" : "failures"}`,
      body:
        detail.run.state === "succeeded"
          ? "The run recovered and reached a successful outcome."
          : "Review the failed tool phases in the timeline.",
    })
  if (counts.retries.value > 0)
    items.push({
      tone: "warning",
      title: `${countQualifier(counts.retries)} model ${counts.retries.value === 1 ? "retry" : "retries"}`,
      body: "Model retries are separate from event-level attempts.",
    })
  if (counts.incompleteCompactions.value > 0)
    items.push({
      tone: "warning",
      title: `${countQualifier(counts.incompleteCompactions)} incomplete ${counts.incompleteCompactions.value === 1 ? "compaction" : "compactions"}`,
      body: "A compaction failed, was aborted, or was interrupted.",
    })
  if (
    detail.run.state === "active" &&
    now - parseTime(detail.run.lastActivityAt) > staleThreshold
  )
    items.push({
      tone: "warning",
      title: `No telemetry for ${formatDuration(now - parseTime(detail.run.lastActivityAt))}`,
      body: "The run remains active. Dashboard connection health is reported separately.",
    })
  if (eventRunDisagree(detail))
    items.push({
      tone: "warning",
      title: "Event and run states disagree",
      body: `Event is ${detail.event.status}; execution is ${detail.run.state}.`,
    })
  if (detail.run.telemetry.completeness !== "complete")
    items.push({
      tone: "info",
      title: `${stateLabel(detail.run.telemetry.completeness)} telemetry`,
      body:
        detail.run.telemetry.completeness === "legacy"
          ? "Turn membership and time categories are unavailable for this legacy run."
          : "Some structured telemetry is missing. Values remain unavailable where the backend cannot account for them.",
    })
  if (detail.timeline.page.hasMore)
    items.push({
      tone: "info",
      title: "Timeline is truncated",
      body: `${detail.timeline.entries.length} of ${detail.timeline.page.total} entries are loaded.`,
    })
  return items
}

function countQualifier(count: SummaryCount): string {
  return count.exact ? `${count.value}` : `at least ${count.value}`
}

function outcomeVerdict(detail: RunDetail): {
  title: string
  health: string
  tone: string
} {
  const counts = runSummaryCounts(detail)
  if (
    detail.run.state === "succeeded" &&
    (counts.failedTools.value > 0 ||
      counts.retries.value > 0 ||
      counts.incompleteCompactions.value > 0)
  ) {
    const recoveries = [
      counts.failedTools.value > 0
        ? `${countQualifier(counts.failedTools)} failed ${counts.failedTools.value === 1 ? "tool call" : "tool calls"}`
        : null,
      counts.retries.value > 0
        ? `${countQualifier(counts.retries)} model ${counts.retries.value === 1 ? "retry" : "retries"}`
        : null,
      counts.incompleteCompactions.value > 0
        ? `${countQualifier(counts.incompleteCompactions)} incomplete ${counts.incompleteCompactions.value === 1 ? "compaction" : "compactions"}`
        : null,
    ].filter((value): value is string => value !== null)
    return {
      title: "Succeeded with recovered error",
      health: `${recoveries.join(", ")} recovered`,
      tone: "warning",
    }
  }
  if (
    detail.run.state === "succeeded" &&
    counts.failedTools.exact &&
    counts.retries.exact &&
    counts.incompleteCompactions.exact
  )
    return {
      title: "Succeeded cleanly",
      health: "No recorded failures",
      tone: "good",
    }
  if (detail.run.state === "succeeded")
    return {
      title: "Succeeded",
      health: "Recovery status unavailable",
      tone: "info",
    }
  if (detail.run.state === "active")
    return {
      title: "Execution in progress",
      health: "Outcome pending",
      tone: "active",
    }
  if (detail.run.state === "interrupted")
    return {
      title: "Execution interrupted",
      health: "Terminal without completion",
      tone: "critical",
    }
  return {
    title: "Execution failed",
    health: "Terminal failure",
    tone: "critical",
  }
}

export function RunInspector({
  detail,
  requestState = "ready",
  refreshing = false,
  onBack = () => {},
  onRefresh = () => {},
  onLoadMore = () => {},
  onNavigateAttempt = () => {},
}: {
  detail: RunDetail
  requestState?: RunDetailState["state"]
  refreshing?: boolean
  onBack?: () => void
  onRefresh?: () => void
  onLoadMore?: () => void
  onNavigateAttempt?: (runId: number) => void
}): JSX.Element {
  const now = useInspectorClock(detail.run.state === "active")
  const grouped = useMemo(() => groupTimeline(detail), [detail])
  const notices = useMemo(() => attentionItems(detail, now), [detail, now])
  const verdict = useMemo(() => outcomeVerdict(detail), [detail])
  const counts = useMemo(() => runSummaryCounts(detail), [detail])
  const budget = useMemo(() => timeBudget(detail.metrics), [detail.metrics])
  const [filter, setFilter] = useState<TimelineFilter>("all")
  const [expanded, setExpanded] = useState<Set<number>>(new Set())
  const [collapsed, setCollapsed] = useState<Set<number>>(new Set())
  const [following, setFollowing] = useState(detail.run.state === "active")
  const currentTurn = grouped.turns.at(-1)?.turn.ordinal ?? null
  const [rovingTurn, setRovingTurn] = useState(currentTurn)
  const timelineRef = useRef<HTMLElement>(null)

  useEffect(() => {
    if (!following || detail.run.state !== "active" || currentTurn === null)
      return
    document.getElementById(`turn-${currentTurn}`)?.scrollIntoView({
      block: "nearest",
      behavior: matchMedia("(prefers-reduced-motion: reduce)").matches
        ? "auto"
        : "smooth",
    })
  }, [currentTurn, detail.generatedAt, detail.run.state, following])

  const isExpanded = (group: TurnGroup, index: number): boolean => {
    const ordinal = group.turn.ordinal
    if (collapsed.has(ordinal)) return false
    if (expanded.has(ordinal)) return true
    return (
      group.needsAttention ||
      group.turn.state === "active" ||
      index === grouped.turns.length - 1
    )
  }
  const toggleTurn = (group: TurnGroup, index: number): void => {
    const ordinal = group.turn.ordinal
    if (isExpanded(group, index)) {
      setExpanded((values) => without(values, ordinal))
      setCollapsed((values) => withValue(values, ordinal))
    } else {
      setCollapsed((values) => without(values, ordinal))
      setExpanded((values) => withValue(values, ordinal))
    }
  }
  const focusTurn = (ordinal: number): void => {
    setRovingTurn(ordinal)
    setCollapsed((values) => without(values, ordinal))
    setExpanded((values) => withValue(values, ordinal))
    requestAnimationFrame(() => {
      const target = document.getElementById(`turn-${ordinal}`)
      target?.scrollIntoView({ block: "start" })
      target?.querySelector<HTMLButtonElement>(".turn-disclosure")?.focus()
    })
  }
  const firstFailure = grouped.turns.find(
    (group) =>
      group.turn.state === "interrupted" ||
      group.turn.stopReason === "error" ||
      group.spans.some((span) => span.state === "failed") ||
      group.phases.some(
        (phase) =>
          phase.state === "failed" ||
          phase.state === "aborted" ||
          phase.state === "interrupted",
      ),
  )
  const wall = Math.max(
    0,
    runEnd(detail, now) - parseTime(detail.run.startedAt),
  )
  const visibleTurns = grouped.turns.filter((group) => {
    if (filter === "gaps") return group.hasTelemetryGap
    if (filter === "all") return true
    if (filter === "attention") return group.needsAttention
    return [...group.spans, ...group.phases].some((entry) =>
      matchesFilter(entry, filter),
    )
  })
  const tabbableTurn = visibleTurns.some(
    (group) => group.turn.ordinal === rovingTurn,
  )
    ? rovingTurn
    : (visibleTurns[0]?.turn.ordinal ?? null)

  return (
    <div className="run-inspector">
      <header className="inspector-identity">
        <button className="back-button" type="button" onClick={onBack}>
          ← runs
        </button>
        <div className="identity-title">
          <span>
            {detail.event.source} / {detail.event.kind}
          </span>
          <h1>{detail.event.title}</h1>
        </div>
        <span className={`run-state run-state-${detail.run.state}`}>
          {stateLabel(detail.run.state)}
        </span>
        <label className="attempt-select">
          <span>Attempt</span>
          <select
            aria-label="Event-level attempt"
            value={detail.run.id}
            onChange={(event) => onNavigateAttempt(Number(event.target.value))}
          >
            {detail.siblingAttempts.map((attempt) => (
              <option value={attempt.id} key={attempt.id}>
                {attempt.attempt} · {stateLabel(attempt.state)}
              </option>
            ))}
          </select>
        </label>
        <div className="identity-time">
          <strong>{formatDuration(wall)}</strong>
          <span>
            {detail.run.state === "active"
              ? `last activity ${relativeAge(detail.run.lastActivityAt, now)}`
              : detail.run.endedAt
                ? `completed ${exactTime(detail.run.endedAt)}`
                : "completion time unavailable"}
          </span>
        </div>
        <button
          className="refresh-run"
          type="button"
          onClick={onRefresh}
          disabled={refreshing}
          aria-label="Refresh run data"
        >
          {refreshing ? "refreshing" : "refresh"}
        </button>
      </header>

      {requestState === "stale" ? (
        <div className="refresh-warning" role="alert">
          <strong>Refresh failed</strong>
          <span>Showing the last successful run data.</span>
          <button type="button" onClick={onRefresh}>
            Retry
          </button>
        </div>
      ) : null}

      <main className="inspector-body">
        <section
          className={`verdict-strip verdict-${verdict.tone}`}
          aria-labelledby="verdict-title"
        >
          <div className="verdict-outcome">
            <span>Verdict</span>
            <h2 id="verdict-title">{verdict.title}</h2>
            <p>{verdict.health}</p>
          </div>
          <VerdictFact label="Duration" value={formatDuration(wall)} />
          <VerdictFact
            label="Turns"
            value={nullableCount(detail.metrics.turnCount)}
          />
          <VerdictFact
            label="Tools"
            value={summaryCountLabel(counts.toolCalls)}
          />
          <VerdictFact
            label="Recovered failures"
            value={summaryCountLabel(counts.failedTools)}
          />
          <VerdictFact
            label="Model retries"
            value={summaryCountLabel(counts.retries)}
          />
          <VerdictFact
            label="Compactions"
            value={summaryCountLabel(counts.compactions)}
          />
          <div className="verdict-disposition">
            <span>Disposition</span>
            <strong>Event {stateLabel(detail.event.status)}</strong>
            <small>
              Telemetry {stateLabel(detail.run.telemetry.completeness)}
            </small>
          </div>
        </section>

        {notices.length ? (
          <section className="attention-stack" aria-label="Run attention">
            {notices.map((notice, index) => (
              <article
                className={`attention-banner attention-${notice.tone}`}
                key={`${notice.title}-${index}`}
              >
                <span aria-hidden="true">
                  {notice.tone === "critical"
                    ? "✕"
                    : notice.tone === "warning"
                      ? "!"
                      : "i"}
                </span>
                <div>
                  <strong>{notice.title}</strong>
                  <p>{notice.body}</p>
                </div>
              </article>
            ))}
          </section>
        ) : null}

        <MetricGrid
          detail={detail}
          wall={wall}
          attentionCount={notices.length}
        />
        <TimeBudget detail={detail} budget={budget} />
        <ExecutionTimeline
          detail={detail}
          now={now}
          groups={grouped.turns}
          visibleTurns={visibleTurns}
          unassigned={grouped.unassigned}
          filter={filter}
          setFilter={setFilter}
          isExpanded={isExpanded}
          toggleTurn={toggleTurn}
          focusTurn={focusTurn}
          firstFailure={firstFailure?.turn.ordinal ?? null}
          rovingTurn={tabbableTurn}
          setRovingTurn={setRovingTurn}
          following={following}
          setFollowing={setFollowing}
          timelineRef={timelineRef}
          onLoadMore={onLoadMore}
          refreshing={refreshing}
        />
        <RunDetails detail={detail} />
      </main>
    </div>
  )
}

function VerdictFact({
  label,
  value,
}: {
  label: string
  value: string
}): JSX.Element {
  return (
    <div className="verdict-fact">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}

function nullableCount(value: number | null): string {
  return value === null ? "unavailable" : formatNumber(value)
}

function summaryCountLabel(count: SummaryCount): string {
  if (!count.exact && count.value === 0) return "unavailable"
  return `${formatNumber(count.value)}${count.exact ? "" : "+"}`
}

function MetricGrid({
  detail,
  wall,
  attentionCount,
}: {
  detail: RunDetail
  wall: number
  attentionCount: number
}): JSX.Element {
  const metrics = detail.metrics
  const counts = runSummaryCounts(detail)
  const usageBreakdown = [
    metrics.usage.inputTokens === null
      ? null
      : `${formatNumber(metrics.usage.inputTokens)} in`,
    metrics.usage.outputTokens === null
      ? null
      : `${formatNumber(metrics.usage.outputTokens)} out`,
  ].filter((value): value is string => value !== null)
  const turnSignals = [
    counts.retries.exact || counts.retries.value > 0
      ? `${countQualifier(counts.retries)} model ${counts.retries.value === 1 ? "retry" : "retries"}`
      : null,
    counts.compactions.exact || counts.compactions.value > 0
      ? `${countQualifier(counts.compactions)} ${counts.compactions.value === 1 ? "compaction" : "compactions"}`
      : null,
  ].filter((value): value is string => value !== null)
  const cards: Array<{
    label: string
    value: string
    note: string
    tone?: string
  }> = [
    {
      label: "Wall duration",
      value: formatDuration(wall),
      note:
        detail.run.state === "active" ? "live elapsed" : "start to completion",
    },
  ]
  if (metrics.durationMs.thinking !== null)
    cards.push({
      label: "Model / thinking",
      value: formatDuration(metrics.durationMs.thinking),
      note: `${Math.round((metrics.durationMs.thinking / Math.max(1, wall)) * 100)}% of wall`,
    })
  if (metrics.durationMs.tool !== null)
    cards.push({
      label: "Tool wall time",
      value: formatDuration(metrics.durationMs.tool),
      note:
        counts.failedTools.exact || counts.failedTools.value > 0
          ? `${countQualifier(counts.failedTools)} failed · overlap counted once`
          : "failure count unavailable · overlap counted once",
      ...(counts.failedTools.value > 0 ? { tone: "warning" } : {}),
    })
  if (metrics.turnCount !== null)
    cards.push({
      label: "Turns",
      value: formatNumber(metrics.turnCount),
      note: turnSignals.length
        ? turnSignals.join(" · ")
        : "retry and compaction counts unavailable",
    })
  if (metrics.usage.totalTokens !== null)
    cards.push({
      label: "Recorded tokens",
      value: formatNumber(metrics.usage.totalTokens),
      note: usageBreakdown.length
        ? usageBreakdown.join(" · ")
        : "input and output breakdown unavailable",
    })
  if (metrics.peakContextTokens !== null)
    cards.push({
      label: "Peak context",
      value: formatNumber(metrics.peakContextTokens),
      note:
        metrics.peakContextPercent === null
          ? "window percentage unavailable"
          : `${Math.round(metrics.peakContextPercent)}% of context window`,
    })
  cards.push({
    label: "Attention",
    value: attentionCount
      ? `${attentionCount} ${attentionCount === 1 ? "item" : "items"}`
      : "None",
    note: attentionCount
      ? "review banners and marked turns"
      : "no recorded concerns",
    tone: attentionCount ? "warning" : "good",
  })
  return (
    <section className="metric-grid" aria-label="Run summary metrics">
      {cards.map((card) => (
        <article
          className={card.tone ? `metric-${card.tone}` : ""}
          key={card.label}
        >
          <span>{card.label}</span>
          <strong>{card.value}</strong>
          <small>{card.note}</small>
        </article>
      ))}
    </section>
  )
}

function TimeBudget({
  detail,
  budget,
}: {
  detail: RunDetail
  budget: ReturnType<typeof timeBudget>
}): JSX.Element {
  return (
    <section className="time-budget" aria-labelledby="budget-title">
      <header className="section-heading">
        <div>
          <span>Wall-clock accounting</span>
          <h2 id="budget-title">Where the run spent time</h2>
        </div>
        <strong>{formatDuration(detail.metrics.durationMs.wall)} total</strong>
      </header>
      {budget ? (
        <>
          <div
            className="budget-bar"
            role="img"
            aria-label={budget
              .map((part) => `${part.label} ${formatDuration(part.value)}`)
              .join(", ")}
          >
            {budget
              .filter((part) => part.value > 0)
              .map((part) => (
                <span
                  className={`budget-${part.key}`}
                  style={{
                    width: `${(part.value / Math.max(1, detail.metrics.durationMs.wall)) * 100}%`,
                  }}
                  title={`${part.label}: ${formatDuration(part.value)}`}
                  key={part.key}
                />
              ))}
          </div>
          <div className="budget-legend">
            {budget.map((part) => (
              <div key={part.key}>
                <i className={`budget-${part.key}`} aria-hidden="true" />
                <span>{part.label}</span>
                <strong>{formatDuration(part.value)}</strong>
              </div>
            ))}
          </div>
        </>
      ) : (
        <p className="honest-fallback">
          Time categories are unavailable for this telemetry record. Wall
          duration remains known.
        </p>
      )}
      <div className="secondary-timings">
        {detail.metrics.sourceLagMs !== null ? (
          <span>
            Source lag{" "}
            <strong>{formatDuration(detail.metrics.sourceLagMs)}</strong>
          </span>
        ) : null}
        {detail.metrics.queueWaitMs !== null ? (
          <span>
            Queue wait{" "}
            <strong>{formatDuration(detail.metrics.queueWaitMs)}</strong>
          </span>
        ) : null}
      </div>
    </section>
  )
}

interface TimelineProps {
  detail: RunDetail
  now: number
  groups: TurnGroup[]
  visibleTurns: TurnGroup[]
  unassigned: Array<GroupedSpan | PhaseEntry>
  filter: TimelineFilter
  setFilter: (filter: TimelineFilter) => void
  isExpanded: (group: TurnGroup, index: number) => boolean
  toggleTurn: (group: TurnGroup, index: number) => void
  focusTurn: (ordinal: number) => void
  firstFailure: number | null
  rovingTurn: number | null
  setRovingTurn: (ordinal: number) => void
  following: boolean
  setFollowing: (value: boolean) => void
  timelineRef: React.RefObject<HTMLElement>
  onLoadMore: () => void
  refreshing: boolean
}

function ExecutionTimeline(props: TimelineProps): JSX.Element {
  const { detail, groups, visibleTurns, filter } = props
  const terminalEmpty =
    detail.run.state !== "active" && detail.timeline.entries.length === 0
  return (
    <section
      className="execution-timeline"
      aria-labelledby="timeline-title"
      ref={props.timelineRef}
      onWheel={() => props.setFollowing(false)}
      onTouchMove={() => props.setFollowing(false)}
    >
      <header className="section-heading timeline-title-row">
        <div>
          <span>Shared wall-clock axis</span>
          <h2 id="timeline-title">Turn-by-turn execution</h2>
        </div>
        <strong>
          {detail.timeline.entries.length} of {detail.timeline.page.total}{" "}
          entries
        </strong>
      </header>
      {groups.length ? (
        <RunMinimap
          detail={detail}
          now={props.now}
          groups={groups}
          focusTurn={props.focusTurn}
        />
      ) : null}
      <div className="timeline-controls">
        <div
          className="timeline-filters"
          role="group"
          aria-label="Filter timeline"
        >
          {(Object.keys(filterLabels) as TimelineFilter[]).map((value) => (
            <button
              type="button"
              aria-pressed={filter === value}
              onClick={() => props.setFilter(value)}
              key={value}
            >
              {filterLabels[value]}
            </button>
          ))}
        </div>
        <div className="timeline-jumps">
          <button
            type="button"
            disabled={props.firstFailure === null}
            onClick={() =>
              props.firstFailure !== null && props.focusTurn(props.firstFailure)
            }
          >
            First failure
          </button>
          <button
            type="button"
            disabled={!groups.length}
            onClick={() =>
              groups.at(-1) && props.focusTurn(groups.at(-1)!.turn.ordinal)
            }
          >
            Latest activity
          </button>
          {detail.run.state === "active" ? (
            <button
              type="button"
              aria-pressed={props.following}
              onClick={() => props.setFollowing(!props.following)}
            >
              {props.following ? "Pause live follow" : "Resume live follow"}
            </button>
          ) : null}
        </div>
      </div>
      {filter === "gaps" && (detail.metrics.durationMs.gaps ?? 0) > 0 ? (
        <div className="gap-summary">
          <strong>
            {formatDuration(detail.metrics.durationMs.gaps!)} gaps / other
          </strong>
          <span>
            The backend accounts for this time without assigning an exact phase
            location.
          </span>
        </div>
      ) : null}
      {terminalEmpty ? (
        <p className="timeline-empty">
          No structured activity was recorded for this terminal run.
        </p>
      ) : null}
      {!terminalEmpty && !groups.length ? (
        <LegacyTimeline
          detail={detail}
          now={props.now}
          entries={props.unassigned}
          filter={filter}
        />
      ) : null}
      <div className="turn-list">
        {visibleTurns.map((group) => {
          const originalIndex = groups.indexOf(group)
          return (
            <TurnSection
              detail={detail}
              now={props.now}
              group={group}
              expanded={props.isExpanded(group, originalIndex)}
              toggle={() => props.toggleTurn(group, originalIndex)}
              tabIndex={group.turn.ordinal === props.rovingTurn ? 0 : -1}
              onFocus={() => props.setRovingTurn(group.turn.ordinal)}
              onMove={(direction) => {
                const currentIndex = visibleTurns.indexOf(group)
                const targetIndex =
                  direction === "first"
                    ? 0
                    : direction === "last"
                      ? visibleTurns.length - 1
                      : currentIndex + direction
                const target = visibleTurns[targetIndex]
                if (target) props.focusTurn(target.turn.ordinal)
              }}
              filter={filter}
              key={group.turn.id}
            />
          )
        })}
      </div>
      {props.unassigned.length > 0 && groups.length > 0 ? (
        <LegacyTimeline
          detail={detail}
          now={props.now}
          entries={props.unassigned}
          filter={filter}
        />
      ) : null}
      {!visibleTurns.length && groups.length ? (
        <p className="timeline-empty">No turns match this filter.</p>
      ) : null}
      {detail.timeline.page.hasMore ? (
        <div className="timeline-pagination" role="status">
          <div>
            <strong>Timeline continues</strong>
            <span>
              {detail.timeline.page.total - detail.timeline.entries.length}{" "}
              entries remain. Loaded data stays visible.
            </span>
          </div>
          <button
            type="button"
            onClick={props.onLoadMore}
            disabled={props.refreshing}
          >
            {props.refreshing
              ? "Loading"
              : `Load next ${Math.min(pageSize, detail.timeline.page.total - detail.timeline.entries.length)}`}
          </button>
        </div>
      ) : null}
    </section>
  )
}

function RunMinimap({
  detail,
  now,
  groups,
  focusTurn,
}: {
  detail: RunDetail
  now: number
  groups: TurnGroup[]
  focusTurn: (ordinal: number) => void
}): JSX.Element {
  const [roving, setRoving] = useState(groups.length - 1)
  const buttons = useRef<Array<HTMLButtonElement | null>>([])
  const move = (index: number): void => {
    const next = Math.max(0, Math.min(groups.length - 1, index))
    setRoving(next)
    buttons.current[next]?.focus()
  }
  return (
    <div className="run-minimap" aria-label="Whole-run waterfall">
      <div className="minimap-axis" aria-hidden="true">
        <span>0</span>
        <span>full run · {formatDuration(detail.metrics.durationMs.wall)}</span>
        <span>100%</span>
      </div>
      <div className="minimap-track">
        {groups.map((group, index) => {
          const position = entryPosition(detail, group.turn, now)
          return (
            <button
              key={group.turn.id}
              ref={(node) => {
                buttons.current[index] = node
              }}
              type="button"
              className={`minimap-turn${group.needsAttention ? " minimap-attention" : ""}${position.marker ? " is-marker" : ""}`}
              style={{
                left: `${position.left}%`,
                width: position.marker ? undefined : `${position.width}%`,
              }}
              tabIndex={index === roving ? 0 : -1}
              onFocus={() => setRoving(index)}
              onClick={() => focusTurn(group.turn.ordinal)}
              onKeyDown={(event) => {
                if (event.key === "ArrowRight") {
                  event.preventDefault()
                  move(index + 1)
                } else if (event.key === "ArrowLeft") {
                  event.preventDefault()
                  move(index - 1)
                } else if (event.key === "Home") {
                  event.preventDefault()
                  move(0)
                } else if (event.key === "End") {
                  event.preventDefault()
                  move(groups.length - 1)
                }
              }}
              aria-label={`Turn ${group.turn.ordinal}, ${offsetTime(detail, group.turn.startedAt)}, ${formatDuration(timelineDuration(detail, group.turn, now))}`}
            >
              {position.marker ? <i aria-hidden="true" /> : null}
              <span aria-hidden="true">{group.turn.ordinal}</span>
            </button>
          )
        })}
      </div>
    </div>
  )
}

function TurnSection({
  detail,
  now,
  group,
  expanded,
  toggle,
  tabIndex,
  onFocus,
  onMove,
  filter,
}: {
  detail: RunDetail
  now: number
  group: TurnGroup
  expanded: boolean
  toggle: () => void
  tabIndex: 0 | -1
  onFocus: () => void
  onMove: (direction: -1 | 1 | "first" | "last") => void
  filter: TimelineFilter
}): JSX.Element {
  const entries = [...group.spans, ...group.phases]
    .sort(
      (left, right) => parseTime(left.startedAt) - parseTime(right.startedAt),
    )
    .filter((entry) => matchesFilter(entry, filter))
  const tools = group.spans.filter((span) => span.kind === "tool")
  const failed = tools.filter((span) => span.state === "failed").length
  const retryCount = group.phases.filter(
    (phase) => phase.type === "retry",
  ).length
  const compactionCount = group.phases.filter(
    (phase) => phase.type === "compaction",
  ).length
  const turnDuration = timelineDuration(detail, group.turn, now)
  return (
    <article
      className={`turn turn-${group.turn.state}${group.needsAttention ? " turn-attention" : ""}`}
      id={`turn-${group.turn.ordinal}`}
    >
      <button
        className="turn-disclosure"
        type="button"
        onClick={toggle}
        tabIndex={tabIndex}
        onFocus={onFocus}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") {
            event.preventDefault()
            onMove(1)
          } else if (event.key === "ArrowUp") {
            event.preventDefault()
            onMove(-1)
          } else if (event.key === "Home") {
            event.preventDefault()
            onMove("first")
          } else if (event.key === "End") {
            event.preventDefault()
            onMove("last")
          }
        }}
        aria-expanded={expanded}
        aria-controls={`turn-body-${group.turn.id}`}
      >
        <span className="turn-chevron" aria-hidden="true">
          {expanded ? "▾" : "▸"}
        </span>
        <span className="turn-ordinal">
          <small>Turn</small>
          <strong>{group.turn.ordinal}</strong>
        </span>
        <span className="turn-time">
          <strong>{offsetTime(detail, group.turn.startedAt)}</strong>
          <small>{exactTime(group.turn.startedAt)}</small>
        </span>
        <span className="turn-duration">
          <strong>{formatDuration(turnDuration)}</strong>
          <small>
            {group.turn.stopReason
              ? `stop: ${stateLabel(group.turn.stopReason)}`
              : stateLabel(group.turn.state)}
          </small>
        </span>
        <span className="turn-counts">
          <strong>
            {tools.length} {tools.length === 1 ? "tool" : "tools"} · {failed}{" "}
            failed
          </strong>
          <small>
            {retryCount
              ? `${retryCount} ${retryCount === 1 ? "retry" : "retries"}`
              : "no retries"}{" "}
            · {compactionCount}{" "}
            {compactionCount === 1 ? "compaction" : "compactions"}
          </small>
        </span>
        <span className="turn-usage">
          <strong>
            {group.turn.usage.totalTokens === null
              ? "usage unavailable"
              : `${formatNumber(group.turn.usage.totalTokens)} tokens`}
          </strong>
          <small>
            {group.turn.contextTokens === null
              ? "context unavailable"
              : `${formatNumber(group.turn.contextTokens)} context${group.turn.contextWindow ? ` / ${formatNumber(group.turn.contextWindow)}` : ""}`}
          </small>
        </span>
        <span
          className={`turn-health${group.needsAttention ? " needs-attention" : ""}`}
        >
          {group.needsAttention ? "attention" : "clean"}
        </span>
      </button>
      <div id={`turn-body-${group.turn.id}`} hidden={!expanded}>
        {entries.length ? (
          <div className="phase-list">
            {entries.map((entry) => (
              <PhaseRow
                detail={detail}
                now={now}
                entry={entry}
                key={`${entry.type}-${entry.id}`}
              />
            ))}
          </div>
        ) : (
          <p className="turn-empty">
            No entries in this turn match the active filter.
          </p>
        )}
        {group.hasTelemetryGap ? (
          <div className="turn-gap">
            <span aria-hidden="true">···</span> Telemetry is incomplete for this
            turn.
          </div>
        ) : null}
      </div>
    </article>
  )
}

function PhaseRow({
  detail,
  now,
  entry,
}: {
  detail: RunDetail
  now: number
  entry: GroupedSpan | PhaseEntry
}): JSX.Element {
  const position = entryPosition(detail, entry, now)
  if (entry.type === "retry")
    return <RetryRow detail={detail} entry={entry} position={position} />
  if (entry.type === "compaction")
    return (
      <CompactionRow
        detail={detail}
        now={now}
        entry={entry}
        position={position}
      />
    )
  const elapsed = timelineDuration(detail, entry, now)
  const interrupted = entry.state === "interrupted"
  return (
    <div className={`phase-row phase-${entry.kind} phase-${entry.state}`}>
      <div className="phase-copy">
        <span className="phase-glyph" aria-hidden="true">
          {entry.kind === "thinking"
            ? "∴"
            : entry.state === "failed"
              ? "✕"
              : entry.state === "active"
                ? "◐"
                : "✓"}
        </span>
        <div>
          <strong>
            {entry.kind === "thinking" ? "Thinking / model" : entry.label}
          </strong>
          <small>
            {entry.blockCount > 1
              ? `${entry.blockCount} adjacent thinking blocks merged · `
              : ""}
            {interrupted
              ? "interrupted and clamped to run end"
              : stateLabel(entry.state)}
          </small>
        </div>
      </div>
      <time>{offsetTime(detail, entry.startedAt)}</time>
      <span className="phase-value">{formatDuration(elapsed)}</span>
      <WallTrack
        position={position}
        label={`${entry.kind} ${formatDuration(elapsed)}`}
        kind={entry.kind}
        state={entry.state}
      />
    </div>
  )
}

function RetryRow({
  detail,
  entry,
  position,
}: {
  detail: RunDetail
  entry: RetryEntry
  position: ReturnType<typeof entryPosition>
}): JSX.Element {
  return (
    <div className={`phase-row phase-retry phase-${entry.state}`}>
      <div className="phase-copy">
        <span className="phase-glyph" aria-hidden="true">
          ↻
        </span>
        <div>
          <strong>
            Model retry {entry.attempt} of {entry.maxAttempts}
          </strong>
          <small>
            {entry.errorCategory
              ? stateLabel(entry.errorCategory)
              : "error category unavailable"}{" "}
            · {stateLabel(entry.state)}
          </small>
        </div>
      </div>
      <time>{offsetTime(detail, entry.startedAt)}</time>
      <span className="phase-value">{formatDuration(entry.delayMs)} delay</span>
      <WallTrack
        position={position}
        label={`Model retry, ${formatDuration(entry.delayMs)} delay`}
        kind="retry"
        state={entry.state}
      />
    </div>
  )
}

function CompactionRow({
  detail,
  now,
  entry,
  position,
}: {
  detail: RunDetail
  now: number
  entry: CompactionEntry
  position: ReturnType<typeof entryPosition>
}): JSX.Element {
  const numeric =
    entry.tokensBefore !== null
      ? `${formatNumber(entry.tokensBefore)} before${entry.estimatedTokensAfter !== null ? ` · ${formatNumber(entry.estimatedTokensAfter)} estimated after` : ""}`
      : "token detail unavailable"
  return (
    <div className={`phase-row phase-compaction phase-${entry.state}`}>
      <div className="phase-copy">
        <span className="phase-glyph" aria-hidden="true">
          ⇲
        </span>
        <div>
          <strong>
            {entry.reason
              ? `${stateLabel(entry.reason)} compaction`
              : "Compaction"}
          </strong>
          <small>
            {stateLabel(entry.state)} · {numeric}
            {entry.willRetry === true ? " · retry planned" : ""}
          </small>
        </div>
      </div>
      <time>{offsetTime(detail, entry.startedAt)}</time>
      <span className="phase-value">
        {formatDuration(timelineDuration(detail, entry, now))}
      </span>
      <WallTrack
        position={position}
        label={`Compaction, ${stateLabel(entry.state)}`}
        kind="compaction"
        state={entry.state}
      />
    </div>
  )
}

function WallTrack({
  position,
  label,
  kind,
  state,
}: {
  position: ReturnType<typeof entryPosition>
  label: string
  kind: string
  state: string
}): JSX.Element {
  return (
    <span className="wall-track" aria-label={label}>
      <i
        className={`wall-segment segment-${kind} segment-${state}${position.marker ? " is-marker" : ""}`}
        style={{
          left: `${position.left}%`,
          width: position.marker ? undefined : `${position.width}%`,
        }}
        aria-hidden="true"
      />
    </span>
  )
}

function LegacyTimeline({
  detail,
  now,
  entries,
  filter,
}: {
  detail: RunDetail
  now: number
  entries: Array<GroupedSpan | PhaseEntry>
  filter: TimelineFilter
}): JSX.Element {
  const visible = entries.filter((entry) => matchesFilter(entry, filter))
  if (!visible.length)
    return (
      <p className="timeline-empty">
        No unassigned activity matches this filter.
      </p>
    )
  return (
    <section className="unassigned-activity" aria-labelledby="unassigned-title">
      <header>
        <div>
          <span>Run-level activity</span>
          <h3 id="unassigned-title">Turn membership unavailable</h3>
        </div>
        <p>
          Entries stay in exact timestamp order without inferred turn
          membership.
        </p>
      </header>
      <div className="phase-list">
        {visible.map((entry) => (
          <PhaseRow
            detail={detail}
            now={now}
            entry={entry}
            key={`${entry.type}-${entry.id}`}
          />
        ))}
      </div>
    </section>
  )
}

function RunDetails({ detail }: { detail: RunDetail }): JSX.Element {
  const eventUrl = safeExternalUrl(detail.event.url)
  return (
    <details className="run-details">
      <summary>Diagnostic details, effects, and privacy</summary>
      <div className="details-grid">
        <section>
          <h3>Effects</h3>
          {detail.effects.length ? (
            <dl>
              {detail.effects.map((effect) => (
                <div key={`${effect.type}-${effect.value}`}>
                  <dt>
                    {effect.type === "aven_reference"
                      ? "Aven reference"
                      : "Investigation handle"}
                  </dt>
                  <dd>
                    <strong>{effect.value}</strong>
                    <small>{exactTime(effect.recordedAt)}</small>
                  </dd>
                </div>
              ))}
            </dl>
          ) : (
            <p>No typed effects were recorded.</p>
          )}
        </section>
        <section>
          <h3>Model</h3>
          <dl>
            <Detail label="Model" value={detail.run.model.id} />
            <Detail label="Provider" value={detail.run.model.provider} />
            <Detail
              label="Thinking level"
              value={detail.run.model.thinkingLevel}
            />
            <Detail
              label="Context window"
              value={detail.run.model.contextWindow}
            />
            <Detail
              label="Max output tokens"
              value={detail.run.model.maxTokens}
            />
            {detail.metrics.usage.totalCost !== null ? (
              <Detail
                label="Recorded cost"
                value={formatMoney(detail.metrics.usage.totalCost)}
              />
            ) : null}
          </dl>
        </section>
        <section>
          <h3>Exact timing</h3>
          <dl>
            <Detail label="Started" value={exactTime(detail.run.startedAt)} />
            <Detail
              label="Last activity"
              value={exactTime(detail.run.lastActivityAt)}
            />
            <Detail
              label="Ended"
              value={detail.run.endedAt ? exactTime(detail.run.endedAt) : null}
            />
            <Detail
              label="Source occurred"
              value={exactTime(detail.event.occurredAt)}
            />
            <Detail
              label="Observed"
              value={exactTime(detail.event.observedAt)}
            />
          </dl>
        </section>
        <section>
          <h3>Event and limits</h3>
          <dl>
            <Detail label="Event entity" value={detail.event.entityId} />
            <Detail label="Event status" value={detail.event.status} />
            <Detail
              label="Event link"
              value={
                eventUrl ? (
                  <a href={eventUrl} rel="noreferrer">
                    Open source event
                  </a>
                ) : null
              }
            />
            <Detail label="Maximum turns" value={detail.limits.maxTurns} />
            <Detail
              label="Wall timeout"
              value={
                detail.limits.wallTimeoutMs === null
                  ? null
                  : formatDuration(detail.limits.wallTimeoutMs)
              }
            />
          </dl>
        </section>
        <section>
          <h3>Telemetry</h3>
          <dl>
            <Detail label="Schema" value={detail.run.telemetry.schemaVersion} />
            <Detail
              label="Completeness"
              value={detail.run.telemetry.completeness}
            />
            <Detail
              label="Loaded entries"
              value={`${detail.timeline.entries.length} of ${detail.timeline.page.total}`}
            />
          </dl>
          <p className="privacy-note">{privacyNote}</p>
        </section>
      </div>
    </details>
  )
}

function Detail({
  label,
  value,
}: {
  label: string
  value: React.ReactNode
}): JSX.Element {
  return (
    <div>
      <dt>{label}</dt>
      <dd>{value ?? "Unavailable"}</dd>
    </div>
  )
}

function withValue(values: Set<number>, value: number): Set<number> {
  const next = new Set(values)
  next.add(value)
  return next
}

function without(values: Set<number>, value: number): Set<number> {
  const next = new Set(values)
  next.delete(value)
  return next
}
