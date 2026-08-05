import type { RunDetail, RunTimelineEntry } from "../run-detail.ts"

export type TimelineFilter =
  | "all"
  | "attention"
  | "tools"
  | "thinking"
  | "retries"
  | "compactions"
  | "gaps"

export type SpanEntry = Extract<RunTimelineEntry, { type: "span" }>
export type TurnEntry = Extract<RunTimelineEntry, { type: "turn" }>
export type RetryEntry = Extract<RunTimelineEntry, { type: "retry" }>
export type CompactionEntry = Extract<RunTimelineEntry, { type: "compaction" }>
export type PhaseEntry = RetryEntry | CompactionEntry

export interface GroupedSpan extends SpanEntry {
  blockCount: number
}

export interface TurnGroup {
  turn: TurnEntry
  spans: GroupedSpan[]
  phases: PhaseEntry[]
  hasTelemetryGap: boolean
  needsAttention: boolean
}

export interface SummaryCount {
  value: number
  exact: boolean
}

export interface RunSummaryCounts {
  toolCalls: SummaryCount
  failedTools: SummaryCount
  retries: SummaryCount
  compactions: SummaryCount
  incompleteCompactions: SummaryCount
}

export interface TimeBudgetPart {
  key:
    | "setup"
    | "thinking"
    | "tool"
    | "retryWait"
    | "compaction"
    | "gaps"
    | "finalization"
  label: string
  value: number
}

const budgetLabels: Record<TimeBudgetPart["key"], string> = {
  setup: "Setup",
  thinking: "Thinking / model",
  tool: "Tool wall time",
  retryWait: "Retry wait",
  compaction: "Compaction",
  gaps: "Gaps / other",
  finalization: "Finalization",
}

function summaryCount(
  reported: number | null,
  observed: number,
  timelineComplete: boolean,
): SummaryCount {
  return {
    value: Math.max(reported ?? 0, observed),
    exact: timelineComplete || (reported !== null && reported >= observed),
  }
}

export function runSummaryCounts(detail: RunDetail): RunSummaryCounts {
  const timelineComplete = !detail.timeline.page.hasMore
  const toolSpans = detail.timeline.entries.filter(
    (entry): entry is SpanEntry =>
      entry.type === "span" && entry.kind === "tool",
  )
  const retries = detail.timeline.entries.filter(
    (entry) => entry.type === "retry",
  ).length
  const compactions = detail.timeline.entries.filter(
    (entry) => entry.type === "compaction",
  )
  const incompleteCompactions = compactions.filter((entry) =>
    ["failed", "aborted", "interrupted"].includes(entry.state),
  ).length

  return {
    toolCalls: summaryCount(
      detail.metrics.toolCallCount,
      toolSpans.length,
      timelineComplete,
    ),
    failedTools: summaryCount(
      detail.metrics.failedToolCount,
      toolSpans.filter((entry) => entry.state === "failed").length,
      timelineComplete,
    ),
    retries: summaryCount(detail.metrics.retryCount, retries, timelineComplete),
    compactions: summaryCount(
      detail.metrics.compactionCount,
      compactions.length,
      timelineComplete,
    ),
    incompleteCompactions: {
      value: incompleteCompactions,
      exact: timelineComplete || detail.metrics.compactionCount === 0,
    },
  }
}

export function timeBudget(
  metrics: RunDetail["metrics"],
): TimeBudgetPart[] | null {
  const keys: TimeBudgetPart["key"][] = [
    "setup",
    "thinking",
    "tool",
    "retryWait",
    "compaction",
    "gaps",
    "finalization",
  ]
  const values = keys.map((key) => metrics.durationMs[key])
  if (values.some((value) => value === null)) return null
  const parts = keys.map((key, index) => ({
    key,
    label: budgetLabels[key],
    value: values[index] ?? 0,
  }))
  const accounted = parts.reduce((sum, part) => sum + part.value, 0)
  const remainder = metrics.durationMs.wall - accounted
  if (remainder !== 0) {
    const gaps = parts.find((part) => part.key === "gaps")!
    gaps.value = Math.max(0, gaps.value + remainder)
  }
  return parts
}

export function mergeThinkingSpans(spans: SpanEntry[]): GroupedSpan[] {
  const ordered = [...spans].sort(
    (left, right) => Date.parse(left.startedAt) - Date.parse(right.startedAt),
  )
  const result: GroupedSpan[] = []
  for (const span of ordered) {
    const previous = result.at(-1)
    const previousEnd = previous?.endedAt ? Date.parse(previous.endedAt) : null
    const adjacent =
      previousEnd !== null && Date.parse(span.startedAt) - previousEnd <= 100
    if (
      previous?.kind === "thinking" &&
      span.kind === "thinking" &&
      previous.turnOrdinal === span.turnOrdinal &&
      previous.state === span.state &&
      adjacent
    ) {
      previous.endedAt = span.endedAt
      previous.blockCount += 1
      continue
    }
    result.push({ ...span, blockCount: 1 })
  }
  return result
}

export function groupTimeline(detail: RunDetail): {
  turns: TurnGroup[]
  unassigned: Array<GroupedSpan | PhaseEntry>
} {
  const turns = detail.timeline.entries.filter(
    (entry): entry is TurnEntry => entry.type === "turn",
  )
  const spans = detail.timeline.entries.filter(
    (entry): entry is SpanEntry => entry.type === "span",
  )
  const phases = detail.timeline.entries.filter(
    (entry): entry is PhaseEntry =>
      entry.type === "retry" || entry.type === "compaction",
  )
  const groupedSpans = mergeThinkingSpans(spans)
  const byOrdinal = new Map<number, GroupedSpan[]>()
  const unassigned: Array<GroupedSpan | PhaseEntry> = []
  for (const span of groupedSpans) {
    if (span.turnOrdinal === null) unassigned.push(span)
    else {
      const entries = byOrdinal.get(span.turnOrdinal) ?? []
      entries.push(span)
      byOrdinal.set(span.turnOrdinal, entries)
    }
  }

  const groupedPhases = new Map<number, PhaseEntry[]>()
  for (const phase of phases) {
    if (phase.turnOrdinal === null) {
      unassigned.push(phase)
      continue
    }
    const entries = groupedPhases.get(phase.turnOrdinal) ?? []
    entries.push(phase)
    groupedPhases.set(phase.turnOrdinal, entries)
  }

  return {
    turns: turns.map((turn) => {
      const turnSpans = byOrdinal.get(turn.ordinal) ?? []
      const turnPhases = groupedPhases.get(turn.ordinal) ?? []
      const failedSpan = turnSpans.some((span) => span.state === "failed")
      const failedPhase = turnPhases.some(
        (phase) =>
          phase.state === "failed" ||
          phase.state === "aborted" ||
          phase.state === "interrupted",
      )
      const hasTelemetryGap =
        detail.run.telemetry.completeness !== "complete" ||
        turnSpans.some((span) => span.turnOrdinal === null)
      return {
        turn,
        spans: turnSpans,
        phases: turnPhases,
        hasTelemetryGap,
        needsAttention:
          turn.state === "interrupted" ||
          failedSpan ||
          failedPhase ||
          turnPhases.length > 0 ||
          hasTelemetryGap,
      }
    }),
    unassigned: unassigned.sort(
      (left, right) => Date.parse(left.startedAt) - Date.parse(right.startedAt),
    ),
  }
}

export function matchesFilter(
  entry: GroupedSpan | PhaseEntry,
  filter: TimelineFilter,
): boolean {
  if (filter === "all") return true
  if (filter === "tools") return entry.type === "span" && entry.kind === "tool"
  if (filter === "thinking")
    return entry.type === "span" && entry.kind === "thinking"
  if (filter === "retries") return entry.type === "retry"
  if (filter === "compactions") return entry.type === "compaction"
  if (filter === "gaps") return false
  return (
    (entry.type === "span" && entry.state === "failed") ||
    (entry.type === "retry" && entry.state !== "succeeded") ||
    (entry.type === "compaction" && entry.state !== "succeeded")
  )
}

export function runEnd(detail: RunDetail, now: number): number {
  return Date.parse(detail.run.endedAt ?? new Date(now).toISOString())
}

export function entryEnd(
  detail: RunDetail,
  entry: RunTimelineEntry,
  now: number,
): number {
  const explicit =
    entry.type === "retry"
      ? (entry.endedAt ?? entry.waitEndedAt)
      : entry.endedAt
  return Math.min(
    Date.parse(explicit ?? detail.run.endedAt ?? new Date(now).toISOString()),
    runEnd(detail, now),
  )
}

export function entryPosition(
  detail: RunDetail,
  entry: RunTimelineEntry,
  now: number,
): { left: number; width: number; marker: boolean } {
  const start = Date.parse(detail.run.startedAt)
  const wall = Math.max(1, runEnd(detail, now) - start)
  const entryStart = Math.max(start, Date.parse(entry.startedAt))
  const duration = Math.max(0, entryEnd(detail, entry, now) - entryStart)
  const width = (duration / wall) * 100
  return {
    left: Math.min(100, Math.max(0, ((entryStart - start) / wall) * 100)),
    width: Math.min(100, width),
    marker: width < 0.7,
  }
}

export function eventRunDisagree(detail: RunDetail): boolean {
  const event = detail.event.status
  const run = detail.run.state
  if (run === "active") return event !== "processing"
  if (run === "succeeded") return event !== "succeeded"
  if (run === "failed") return event !== "failed" && event !== "retryable"
  return event === "processing" || event === "succeeded"
}
