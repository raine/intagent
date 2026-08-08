import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import type {
  DispatchTrigger,
  RunDetail,
  RunTimelineEntry,
} from "./run-detail-types.ts"
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
} from "./run-inspector-data.ts"

const staleThreshold = 120_000
const pageSize = 200

const filterLabels: Record<TimelineFilter, string> = {
  all: "All activity",
  tools: "Tool calls",
  attention: "Attention",
  thinking: "Thinking",
  retries: "Retries",
  compactions: "Compactions",
}

const timelineTitles: Record<TimelineFilter, string> = {
  tools: "Tool activity",
  all: "Activity timeline",
  attention: "Activity needing attention",
  thinking: "Model activity",
  retries: "Model retries",
  compactions: "Compactions",
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

const dispatchLabels: Record<DispatchTrigger, string> = {
  initial: "First attempt",
  revision: "New revision",
  backoff_retry: "Retry after failure",
  recovery_retry: "Retry after restart",
  operator_retry: "Manual retry",
  manual_injection: "Manual injection",
  superseding_claim: "Superseding claim",
  unknown: "Dispatch unknown",
}

function stateLabel(value: string): string {
  return value.replaceAll("_", " ")
}

function plainSummary(value: string): string {
  return value.replace(/\*\*(.+?)\*\*/gs, "$1").replace(/__(.+?)__/gs, "$1")
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
      key={value.detail.run.id}
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
  if (counts.failedTools.value > 0 && detail.run.state !== "succeeded")
    items.push({
      tone: "critical",
      title: `${countQualifier(counts.failedTools)} tool ${counts.failedTools.value === 1 ? "failure" : "failures"}`,
      body: "Review the failed tool phases in the timeline.",
    })
  if (
    counts.incompleteCompactions.value > 0 &&
    detail.run.state !== "succeeded"
  )
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
  const recordedSignals = [
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
  if (
    detail.run.state === "succeeded" &&
    (counts.failedTools.value > 0 ||
      counts.retries.value > 0 ||
      counts.incompleteCompactions.value > 0)
  ) {
    return {
      title: "Succeeded with recovered error",
      health: `${recordedSignals.join(", ")} recovered`,
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
      health: recordedSignals.length
        ? `${recordedSignals.join(", ")} recorded`
        : "Terminal without completion",
      tone: "critical",
    }
  return {
    title: "Execution failed",
    health: recordedSignals.length
      ? `${recordedSignals.join(", ")} recorded`
      : "Terminal failure",
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
  initialFilter = "all",
}: {
  detail: RunDetail
  requestState?: RunDetailState["state"]
  refreshing?: boolean
  onBack?: () => void
  onRefresh?: () => void
  onLoadMore?: () => void
  onNavigateAttempt?: (runId: number) => void
  initialFilter?: TimelineFilter
}): JSX.Element {
  const now = useInspectorClock(detail.run.state === "active")
  const grouped = useMemo(() => groupTimeline(detail), [detail])
  const notices = useMemo(() => attentionItems(detail, now), [detail, now])
  const verdict = useMemo(() => outcomeVerdict(detail), [detail])
  const counts = useMemo(() => runSummaryCounts(detail), [detail])
  const budget = useMemo(() => timeBudget(detail.metrics), [detail.metrics])
  const [filter, setFilter] = useState<TimelineFilter>(initialFilter)
  const [following, setFollowing] = useState(detail.run.state === "active")
  const timelineRef = useRef<HTMLElement>(null)
  const activity = useMemo(
    () =>
      [
        ...grouped.turns.flatMap((group) => [...group.spans, ...group.phases]),
        ...grouped.unassigned,
      ].sort(
        (left, right) => parseTime(left.startedAt) - parseTime(right.startedAt),
      ),
    [grouped],
  )
  const visibleActivity = activity.filter((entry) =>
    matchesFilter(entry, filter),
  )

  useEffect(() => {
    if (!following || detail.run.state !== "active") return
    timelineRef.current
      ?.querySelector(".phase-list > .phase-row:last-child")
      ?.scrollIntoView({
        block: "nearest",
        behavior: matchMedia("(prefers-reduced-motion: reduce)").matches
          ? "auto"
          : "smooth",
      })
  }, [activity.length, detail.generatedAt, detail.run.state, following])

  const wall = Math.max(
    0,
    runEnd(detail, now) - parseTime(detail.run.startedAt),
  )

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
        <span
          className={`dispatch-chip dispatch-${detail.run.dispatch.trigger}`}
        >
          {dispatchLabels[detail.run.dispatch.trigger]}
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
                {attempt.sequence} · {stateLabel(attempt.state)}
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
            label="Tools"
            value={summaryCountLabel(counts.toolCalls)}
          />
          {detail.metrics.usage.totalTokens !== null ? (
            <VerdictFact
              label="Recorded tokens"
              value={formatNumber(detail.metrics.usage.totalTokens)}
            />
          ) : null}
          {detail.metrics.peakContextTokens !== null ? (
            <VerdictFact
              label="Peak context"
              value={`${formatNumber(detail.metrics.peakContextTokens)}${detail.metrics.peakContextPercent === null ? "" : ` · ${Math.round(detail.metrics.peakContextPercent)}%`}`}
            />
          ) : null}
          <div className="verdict-disposition">
            <span>Disposition</span>
            <strong>Event {stateLabel(detail.event.status)}</strong>
            <small>
              Telemetry {stateLabel(detail.run.telemetry.completeness)}
            </small>
            {detail.effects.length ? (
              <small className="verdict-effects">
                {detail.effects
                  .map((effect) => effect.value)
                  .slice(0, 2)
                  .join(" · ")}
              </small>
            ) : null}
          </div>
        </section>

        <DispatchStrip detail={detail} onNavigateAttempt={onNavigateAttempt} />
        <ConclusionPanel detail={detail} />

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

        <ExecutionTimeline
          detail={detail}
          now={now}
          activity={activity}
          visibleActivity={visibleActivity}
          filter={filter}
          setFilter={setFilter}
          following={following}
          setFollowing={setFollowing}
          timelineRef={timelineRef}
          onLoadMore={onLoadMore}
          refreshing={refreshing}
        />
        <TimeBudget detail={detail} budget={budget} />
        <RunPrompts detail={detail} />
        <RunDetails detail={detail} />
      </main>
    </div>
  )
}

function DispatchStrip({
  detail,
  onNavigateAttempt,
}: {
  detail: RunDetail
  onNavigateAttempt: (runId: number) => void
}): JSX.Element | null {
  const dispatch = detail.run.dispatch
  const notable =
    dispatch.trigger !== "initial" ||
    dispatch.finalAttempt ||
    (dispatch.latency.sourceLagMs ?? 0) > 300_000 ||
    (dispatch.latency.claimDelayMs ?? 0) > 60_000
  if (!notable) return null

  const prior = dispatch.priorAttempt
  const timing = [
    dispatch.latency.sourceLagMs !== null && dispatch.latency.sourceLagMs > 1000
      ? `observed ${formatDuration(dispatch.latency.sourceLagMs)} after occurrence`
      : null,
    dispatch.latency.backoffWaitMs !== null
      ? `${formatDuration(dispatch.latency.backoffWaitMs)} backoff`
      : null,
    dispatch.latency.claimDelayMs !== null &&
    dispatch.latency.claimDelayMs > 1000
      ? `claimed ${formatDuration(dispatch.latency.claimDelayMs)} after eligibility`
      : null,
  ].filter((value): value is string => value !== null)

  return (
    <section
      className={`dispatch-strip dispatch-${dispatch.trigger}`}
      aria-label="Dispatch context"
    >
      <strong>{dispatchLabels[dispatch.trigger]}</strong>
      <span>
        Run {dispatch.sequence}
        {dispatch.maxAttempts === null
          ? ""
          : ` · policy attempt ${dispatch.attempt} of ${dispatch.maxAttempts}`}
        {dispatch.finalAttempt ? " · final policy attempt" : ""}
      </span>
      {prior ? (
        <span>
          after{" "}
          <button type="button" onClick={() => onNavigateAttempt(prior.runId)}>
            run {prior.sequence}
          </button>{" "}
          {stateLabel(prior.state)}
          {prior.failureCategory
            ? ` · ${stateLabel(prior.failureCategory)}`
            : ""}
        </span>
      ) : null}
      {timing.length ? <span>{timing.join(" · ")}</span> : null}
      {dispatch.source !== "recorded" ? (
        <small>Trigger {dispatch.source} from available history</small>
      ) : null}
    </section>
  )
}

function ConclusionPanel({ detail }: { detail: RunDetail }): JSX.Element {
  const conclusion = detail.run.conclusion
  const sourceLabel =
    conclusion.source === "model"
      ? "Agent conclusion"
      : conclusion.source === "derived"
        ? "Derived from recorded facts"
        : "Conclusion unavailable"
  return (
    <section className="conclusion-panel" aria-labelledby="conclusion-title">
      <article className="triage-conclusion">
        <header>
          <div>
            <span>{sourceLabel}</span>
            <h2 id="conclusion-title">Triage conclusion</h2>
          </div>
          <strong className={`decision decision-${conclusion.decision}`}>
            {stateLabel(conclusion.decision)}
          </strong>
        </header>
        <p className="conclusion-summary">{conclusion.summary}</p>
        <dl className="conclusion-facts">
          {conclusion.evidence.length ? (
            <ConclusionList label="Key evidence" items={conclusion.evidence} />
          ) : null}
          {conclusion.actions.length ? (
            <ConclusionList
              label="Actions performed"
              items={conclusion.actions}
            />
          ) : null}
          <div>
            <dt>Outcome</dt>
            <dd>{conclusion.outcome}</dd>
          </div>
          {conclusion.followUp ? (
            <div>
              <dt>Follow-up</dt>
              <dd>{conclusion.followUp}</dd>
            </div>
          ) : null}
        </dl>
      </article>
    </section>
  )
}

function ConclusionList({
  label,
  items,
}: {
  label: string
  items: string[]
}): JSX.Element {
  return (
    <div>
      <dt>{label}</dt>
      <dd>
        <ul>
          {items.map((item, index) => (
            <li key={`${label}-${index}`}>{item}</li>
          ))}
        </ul>
      </dd>
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

function summaryCountLabel(count: SummaryCount): string {
  if (!count.exact && count.value === 0) return "unavailable"
  return `${formatNumber(count.value)}${count.exact ? "" : "+"}`
}

function TimeBudget({
  detail,
  budget,
}: {
  detail: RunDetail
  budget: ReturnType<typeof timeBudget>
}): JSX.Element {
  const total = Math.max(1, detail.metrics.durationMs.wall)
  let offset = 0
  const positioned = budget?.map((part) => {
    const left = offset
    offset += part.value
    return {
      ...part,
      left: (left / total) * 100,
      width: (part.value / total) * 100,
    }
  })
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
          <div className="budget-bar" aria-hidden="true">
            {positioned!
              .filter((part) => part.value > 0)
              .map((part) => (
                <span
                  className={`budget-${part.key}`}
                  style={{
                    left: `${part.left}%`,
                    width: `${part.width}%`,
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
    </section>
  )
}

interface TimelineProps {
  detail: RunDetail
  now: number
  activity: Array<GroupedSpan | PhaseEntry>
  visibleActivity: Array<GroupedSpan | PhaseEntry>
  filter: TimelineFilter
  setFilter: (filter: TimelineFilter) => void
  following: boolean
  setFollowing: (value: boolean) => void
  timelineRef: React.RefObject<HTMLElement>
  onLoadMore: () => void
  refreshing: boolean
}

function ExecutionTimeline(props: TimelineProps): JSX.Element {
  const { detail, activity, visibleActivity, filter } = props
  const empty = activity.length === 0
  const countLabel = `${visibleActivity.length} shown · ${activity.length} loaded`
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
          <h2 id="timeline-title">{timelineTitles[filter]}</h2>
        </div>
        <strong>{countLabel}</strong>
      </header>
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
        {detail.run.state === "active" ? (
          <div className="timeline-jumps">
            <button
              type="button"
              aria-pressed={props.following}
              onClick={() => props.setFollowing(!props.following)}
            >
              {props.following ? "Pause live follow" : "Resume live follow"}
            </button>
          </div>
        ) : null}
      </div>
      {empty ? (
        <p className="timeline-empty">
          {detail.run.state === "active"
            ? "Waiting for tool or model activity."
            : "No tool or model activity was recorded for this run."}
        </p>
      ) : visibleActivity.length ? (
        <div className="phase-list activity-list">
          {visibleActivity.map((entry) => (
            <PhaseRow
              detail={detail}
              now={props.now}
              entry={entry}
              key={`${entry.type}-${entry.id}`}
            />
          ))}
        </div>
      ) : (
        <p className="timeline-empty">No activity matches this filter.</p>
      )}
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
  const hasToolDetail = entry.kind === "tool" && entry.summary !== null
  const showToolStatus = !hasToolDetail || entry.state !== "succeeded"
  const thinkingSummary =
    entry.kind === "thinking" && entry.summary
      ? plainSummary(entry.summary)
      : null
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
          {hasToolDetail && !showToolStatus ? (
            <span className="visually-hidden">
              {entry.label}, {stateLabel(entry.state)}
            </span>
          ) : null}
          {entry.kind === "thinking" || showToolStatus ? (
            <div className="phase-heading">
              <strong>
                {entry.kind === "thinking" ? "Thinking" : entry.label}
              </strong>
              {thinkingSummary ? (
                <button
                  className="thinking-summary-trigger"
                  type="button"
                  aria-label={`Thinking summary: ${thinkingSummary}`}
                  data-summary={thinkingSummary}
                  title={thinkingSummary}
                >
                  i
                </button>
              ) : null}
            </div>
          ) : null}
          {hasToolDetail ? <ToolDetail value={entry.summary!} /> : null}
          {entry.kind === "thinking" || showToolStatus ? (
            <small>
              {entry.blockCount > 1
                ? `${entry.blockCount} adjacent thinking blocks merged · `
                : ""}
              {interrupted
                ? "interrupted and clamped to run end"
                : stateLabel(entry.state)}
            </small>
          ) : null}
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

function ToolDetail({ value }: { value: string }): JSX.Element {
  if (value.length <= 160) return <code className="phase-summary">{value}</code>
  return (
    <details className="tool-detail-long">
      <summary>
        <small>Show full</small>
        <code className="phase-summary">{value}</code>
      </summary>
      <code className="phase-summary-full">{value}</code>
    </details>
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
    <span className="wall-track" role="img" aria-label={label}>
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

function RunPrompts({ detail }: { detail: RunDetail }): JSX.Element {
  return (
    <details className="run-prompts">
      <summary>
        Agent prompts
        <small>
          {detail.prompts.length
            ? `${detail.prompts.length} captured`
            : "unavailable for this run"}
        </small>
      </summary>
      {detail.prompts.length ? (
        <div className="prompt-list">
          {detail.prompts.map((prompt) => (
            <section key={prompt.role}>
              <header>
                <h3>
                  {prompt.role === "system"
                    ? "Triage system instructions"
                    : "Event prompt"}
                </h3>
                <time>{exactTime(prompt.recordedAt)}</time>
              </header>
              <pre>
                <code>{prompt.content}</code>
              </pre>
            </section>
          ))}
        </div>
      ) : (
        <p className="prompt-unavailable">
          Prompt capture is unavailable for this run.
        </p>
      )}
    </details>
  )
}

function RunDetails({ detail }: { detail: RunDetail }): JSX.Element {
  const eventUrl = safeExternalUrl(detail.event.url)
  return (
    <details className="run-details">
      <summary>Diagnostic details and effects</summary>
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
          <h3>Dispatch</h3>
          <dl>
            <Detail
              label="Trigger"
              value={dispatchLabels[detail.run.dispatch.trigger]}
            />
            <Detail label="Run sequence" value={detail.run.dispatch.sequence} />
            <Detail label="Trigger source" value={detail.run.dispatch.source} />
            <Detail
              label="Scheduled for"
              value={
                detail.run.dispatch.scheduledFor
                  ? exactTime(detail.run.dispatch.scheduledFor)
                  : null
              }
            />
            <Detail
              label="Source observation lag"
              value={
                detail.run.dispatch.latency.sourceLagMs === null
                  ? null
                  : formatDuration(detail.run.dispatch.latency.sourceLagMs)
              }
            />
            <Detail
              label="Backoff wait"
              value={
                detail.run.dispatch.latency.backoffWaitMs === null
                  ? null
                  : formatDuration(detail.run.dispatch.latency.backoffWaitMs)
              }
            />
            <Detail
              label="Claim delay"
              value={
                detail.run.dispatch.latency.claimDelayMs === null
                  ? null
                  : formatDuration(detail.run.dispatch.latency.claimDelayMs)
              }
            />
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
