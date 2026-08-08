import type {
  RunDetail,
  TimelineEntry as RunTimelineEntry,
} from "./api-types.ts"

export type TimelineFilter = "all" | "tools" | "thinking" | "retries"

export type SpanEntry = Extract<RunTimelineEntry, { type: "span" }>
export type TurnEntry = Extract<RunTimelineEntry, { type: "turn" }>
export type RetryEntry = Extract<RunTimelineEntry, { type: "retry" }>
export type PhaseEntry = RetryEntry

export interface GroupedSpan extends SpanEntry {
  blockCount: number
}

export interface TurnGroup {
  turn: TurnEntry
  spans: GroupedSpan[]
  phases: PhaseEntry[]
  hasTelemetryGap: boolean
  hasSignal: boolean
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
}

export interface TimeBudgetPart {
  key: "setup" | "thinking" | "tool" | "retryWait" | "gaps" | "finalization"
  label: string
  value: number
}

const budgetLabels: Record<TimeBudgetPart["key"], string> = {
  setup: "Setup",
  thinking: "Thinking / model",
  tool: "Tool wall time",
  retryWait: "Retry wait",
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

  return {
    toolCalls: summaryCount(
      detail.metrics.toolCallCount,
      toolSpans.length,
      timelineComplete,
    ),
    failedTools: summaryCount(
      detail.metrics.failedToolCount,
      toolSpans.filter(
        (entry) => entry.state === "failed" || entry.state === "aborted",
      ).length,
      timelineComplete,
    ),
    retries: summaryCount(detail.metrics.retryCount, retries, timelineComplete),
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
      previous.summary =
        [previous.summary, span.summary]
          .filter((summary): summary is string => Boolean(summary))
          .join("\n\n") || null
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
    (entry): entry is PhaseEntry => entry.type === "retry",
  )
  const groupedSpans = mergeThinkingSpans(spans)
  const turnOrdinals = new Set(turns.map((turn) => turn.ordinal))
  const byOrdinal = new Map<number, GroupedSpan[]>()
  const unassigned: Array<GroupedSpan | PhaseEntry> = []
  for (const span of groupedSpans) {
    if (span.turnOrdinal === null || !turnOrdinals.has(span.turnOrdinal))
      unassigned.push(span)
    else {
      const entries = byOrdinal.get(span.turnOrdinal) ?? []
      entries.push(span)
      byOrdinal.set(span.turnOrdinal, entries)
    }
  }

  const groupedPhases = new Map<number, PhaseEntry[]>()
  for (const phase of phases) {
    if (phase.turnOrdinal === null || !turnOrdinals.has(phase.turnOrdinal)) {
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
      const failedSpan = turnSpans.some(
        (span) =>
          span.state === "failed" ||
          span.state === "aborted" ||
          span.state === "interrupted",
      )
      const failedPhase = turnPhases.some(
        (phase) => phase.state === "interrupted",
      )
      const hasTelemetryGap =
        detail.run.telemetry.completeness === "partial" &&
        turn.state === "interrupted"
      const needsAttention =
        turn.state === "interrupted" ||
        turn.stopReason === "error" ||
        failedSpan ||
        failedPhase ||
        hasTelemetryGap
      return {
        turn,
        spans: turnSpans,
        phases: turnPhases,
        hasTelemetryGap,
        hasSignal: turnPhases.length > 0,
        needsAttention,
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
  return false
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
