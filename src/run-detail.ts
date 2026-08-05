import type {
  EventRecord,
  IntakeDatabase,
  RunOutcome,
  SafeErrorCategory,
  TelemetryCompleteness,
  TriageCompactionRecord,
  TriageRetryRecord,
  TriageRunRecord,
  TriageRunStepRecord,
  TriageTurnRecord,
} from "./database.ts"

export interface RunMetrics {
  durationMs: {
    wall: number
    setup: number | null
    thinking: number | null
    tool: number | null
    compaction: number | null
    retryWait: number | null
    gaps: number | null
    finalization: number | null
  }
  toolCallCount: number | null
  failedToolCount: number | null
  turnCount: number | null
  retryCount: number | null
  compactionCount: number | null
  usage: {
    inputTokens: number | null
    outputTokens: number | null
    cacheReadTokens: number | null
    cacheWriteTokens: number | null
    reasoningTokens: number | null
    totalTokens: number | null
    totalCost: number | null
  }
  peakContextTokens: number | null
  peakContextPercent: number | null
  sourceLagMs: number | null
  queueWaitMs: number | null
}

export type RunTimelineEntry =
  | {
      type: "turn"
      id: number
      ordinal: number
      startedAt: string
      endedAt: string | null
      state: "active" | "completed" | "interrupted"
      stopReason: string | null
      usage: {
        inputTokens: number | null
        outputTokens: number | null
        cacheReadTokens: number | null
        cacheWriteTokens: number | null
        reasoningTokens: number | null
        totalTokens: number | null
        totalCost: number | null
      }
      contextTokens: number | null
      contextWindow: number | null
    }
  | {
      type: "span"
      id: number
      turnOrdinal: number | null
      kind: "tool" | "thinking"
      label: string
      startedAt: string
      endedAt: string | null
      state: "active" | RunOutcome
    }
  | {
      type: "retry"
      id: number
      attempt: number
      maxAttempts: number
      delayMs: number
      startedAt: string
      waitEndedAt: string
      endedAt: string | null
      state: "active" | "succeeded" | "failed" | "interrupted"
      errorCategory: SafeErrorCategory | null
    }
  | {
      type: "compaction"
      id: number
      reason: "manual" | "threshold" | "overflow" | null
      startedAt: string
      endedAt: string | null
      state: "active" | "succeeded" | "failed" | "aborted" | "interrupted"
      aborted: boolean | null
      willRetry: boolean | null
      tokensBefore: number | null
      estimatedTokensAfter: number | null
      totalTokens: number | null
      totalCost: number | null
    }

export interface RunDetail {
  generatedAt: string
  run: {
    id: number
    eventId: number
    attempt: number
    startedAt: string
    endedAt: string | null
    lastActivityAt: string
    state: "active" | RunOutcome
    terminationReason: string | null
    failureCategory: SafeErrorCategory | null
    model: {
      id: string | null
      provider: string | null
      thinkingLevel: string | null
      contextWindow: number | null
      maxTokens: number | null
    }
    telemetry: {
      schemaVersion: number | null
      completeness: TelemetryCompleteness
    }
  }
  event: {
    id: number
    source: string
    entityId: string
    kind: string
    title: string
    url: string | null
    occurredAt: string
    observedAt: string
    status: string
    avenRef: string | null
    investigationHandle: string | null
  }
  siblingAttempts: Array<{
    id: number
    attempt: number
    startedAt: string
    endedAt: string | null
    state: "active" | RunOutcome
    failureCategory: SafeErrorCategory | null
    telemetryCompleteness: TelemetryCompleteness
  }>
  metrics: RunMetrics
  effects: Array<{
    type: "aven_reference" | "investigation_handle"
    value: string
    recordedAt: string
  }>
  limits: {
    maxTurns: number | null
    wallTimeoutMs: number | null
    modelContextWindow: number | null
    modelMaxTokens: number | null
  }
  timeline: {
    entries: RunTimelineEntry[]
    page: {
      offset: number
      limit: number
      returned: number
      total: number
      hasMore: boolean
      nextOffset: number | null
    }
  }
}

export interface RunDetailOptions {
  offset?: number
  limit?: number
  maxTurns?: number | null
  wallTimeoutMs?: number | null
  now?: Date
}

export function runDetail(
  database: IntakeDatabase,
  runId: number,
  options: RunDetailOptions = {},
): RunDetail | null {
  const run = database.triageRun(runId)
  if (!run) return null
  const event = database.event(run.eventId)
  if (!event) return null
  const now = options.now ?? new Date()
  const turns = database.triageRunTurns(run.id)
  const retries = database.triageRunRetries(run.id)
  const compactions = database.triageRunCompactions(run.id)
  const metrics = runMetrics(run, event, turns, retries, compactions, now)
  const allEntries = timelineEntries(run, turns, retries, compactions)
  const offset = boundedInteger(options.offset, 0, Number.MAX_SAFE_INTEGER, 0)
  const limit = boundedInteger(options.limit, 1, 500, 200)
  const entries = allEntries.slice(offset, offset + limit)
  const total = allEntries.length
  const hasMore = offset + entries.length < total

  return {
    generatedAt: now.toISOString(),
    run: {
      id: run.id,
      eventId: run.eventId,
      attempt: run.attempt,
      startedAt: run.startedAt,
      endedAt: run.endedAt,
      lastActivityAt: run.lastActivityAt,
      state: run.outcome ?? "active",
      terminationReason: run.terminationReason,
      failureCategory: run.failureCategory,
      model: {
        id: run.modelId,
        provider: run.modelProvider,
        thinkingLevel: run.thinkingLevel,
        contextWindow: run.contextWindow,
        maxTokens: run.maxTokens,
      },
      telemetry: {
        schemaVersion: run.telemetryVersion,
        completeness: run.telemetryCompleteness,
      },
    },
    event: {
      id: event.id,
      source: event.source,
      entityId: event.entityId,
      kind: event.kind,
      title: event.title,
      url: eventUrl(event),
      occurredAt: event.occurredAt,
      observedAt: event.observedAt,
      status: event.status,
      avenRef: event.avenRef,
      investigationHandle: event.investigationHandle,
    },
    siblingAttempts: database.triageRunsForEvent(event.id).map((sibling) => ({
      id: sibling.id,
      attempt: sibling.attempt,
      startedAt: sibling.startedAt,
      endedAt: sibling.endedAt,
      state: sibling.outcome ?? "active",
      failureCategory: sibling.failureCategory,
      telemetryCompleteness: sibling.telemetryCompleteness,
    })),
    metrics,
    effects: database
      .triageRunEffects(run.id)
      .map(({ type, value, recordedAt }) => ({
        type,
        value,
        recordedAt,
      })),
    limits: {
      maxTurns: options.maxTurns ?? null,
      wallTimeoutMs: options.wallTimeoutMs ?? null,
      modelContextWindow: run.contextWindow,
      modelMaxTokens: run.maxTokens,
    },
    timeline: {
      entries,
      page: {
        offset,
        limit,
        returned: entries.length,
        total,
        hasMore,
        nextOffset: hasMore ? offset + entries.length : null,
      },
    },
  }
}

export function runMetrics(
  run: TriageRunRecord,
  event: EventRecord,
  turns: TriageTurnRecord[],
  retries: TriageRetryRecord[],
  compactions: TriageCompactionRecord[],
  now = new Date(),
): RunMetrics {
  const start = Date.parse(run.startedAt)
  const end = Date.parse(
    run.endedAt ?? (run.outcome ? run.lastActivityAt : now.toISOString()),
  )
  const wall = Math.max(0, end - start)
  const completeEnough = run.telemetryCompleteness !== "legacy"
  const toolSteps = run.steps.filter((step) => step.kind === "tool")
  const contextValues = turns.flatMap((turn) =>
    turn.contextTokens === null ? [] : [turn.contextTokens],
  )
  const percentages = turns.flatMap((turn) =>
    turn.contextTokens !== null && turn.contextWindow
      ? [(turn.contextTokens / turn.contextWindow) * 100]
      : [],
  )

  return {
    durationMs: completeEnough
      ? partitionDurations(run, turns, retries, compactions, start, end)
      : {
          wall,
          setup: null,
          thinking: null,
          tool: null,
          compaction: null,
          retryWait: null,
          gaps: null,
          finalization: null,
        },
    toolCallCount: completeEnough ? toolSteps.length : null,
    failedToolCount: completeEnough
      ? toolSteps.filter((step) => step.outcome === "failed").length
      : null,
    turnCount: completeEnough ? turns.length : null,
    retryCount: completeEnough ? retries.length : null,
    compactionCount: completeEnough ? compactions.length : null,
    usage: {
      inputTokens: nullableSum([
        ...turns.map((turn) => turn.inputTokens),
        ...compactions.map((compaction) => compaction.inputTokens),
      ]),
      outputTokens: nullableSum([
        ...turns.map((turn) => turn.outputTokens),
        ...compactions.map((compaction) => compaction.outputTokens),
      ]),
      cacheReadTokens: nullableSum([
        ...turns.map((turn) => turn.cacheReadTokens),
        ...compactions.map((compaction) => compaction.cacheReadTokens),
      ]),
      cacheWriteTokens: nullableSum([
        ...turns.map((turn) => turn.cacheWriteTokens),
        ...compactions.map((compaction) => compaction.cacheWriteTokens),
      ]),
      reasoningTokens: nullableSum([
        ...turns.map((turn) => turn.reasoningTokens),
        ...compactions.map((compaction) => compaction.reasoningTokens),
      ]),
      totalTokens: nullableSum([
        ...turns.map((turn) => turn.totalTokens),
        ...compactions.map((compaction) => compaction.totalTokens),
      ]),
      totalCost: nullableSum([
        ...turns.map((turn) => turn.totalCost),
        ...compactions.map((compaction) => compaction.totalCost),
      ]),
    },
    peakContextTokens:
      contextValues.length > 0 ? Math.max(...contextValues) : null,
    peakContextPercent:
      percentages.length > 0 ? Math.max(...percentages) : null,
    sourceLagMs: elapsed(event.occurredAt, run.startedAt),
    queueWaitMs: elapsed(event.observedAt, run.startedAt),
  }
}

type DurationCategory =
  | "setup"
  | "thinking"
  | "tool"
  | "compaction"
  | "retryWait"
  | "finalization"

type Interval = { start: number; end: number; category: DurationCategory }

function partitionDurations(
  run: TriageRunRecord,
  turns: TriageTurnRecord[],
  retries: TriageRetryRecord[],
  compactions: TriageCompactionRecord[],
  start: number,
  end: number,
): RunMetrics["durationMs"] {
  const intervals: Interval[] = []
  const firstTurn = turns[0]
  const lastTurn = turns.at(-1)
  addInterval(
    intervals,
    "setup",
    start,
    firstTurn ? Date.parse(firstTurn.startedAt) : end,
    start,
    end,
  )
  if (run.endedAt && lastTurn?.endedAt)
    addInterval(
      intervals,
      "finalization",
      Date.parse(lastTurn.endedAt),
      end,
      start,
      end,
    )
  for (const step of run.steps) {
    if (step.kind === "compaction") continue
    addInterval(
      intervals,
      step.kind,
      Date.parse(step.startedAt),
      Date.parse(step.endedAt ?? run.endedAt ?? new Date(end).toISOString()),
      start,
      end,
    )
  }
  for (const retry of retries)
    addInterval(
      intervals,
      "retryWait",
      Date.parse(retry.startedAt),
      Math.min(
        Date.parse(retry.waitEndedAt),
        Date.parse(retry.endedAt ?? run.endedAt ?? new Date(end).toISOString()),
      ),
      start,
      end,
    )
  for (const compaction of compactions)
    addInterval(
      intervals,
      "compaction",
      Date.parse(compaction.startedAt),
      Date.parse(
        compaction.endedAt ?? run.endedAt ?? new Date(end).toISOString(),
      ),
      start,
      end,
    )

  const totals: Record<DurationCategory | "gaps", number> = {
    setup: 0,
    thinking: 0,
    tool: 0,
    compaction: 0,
    retryWait: 0,
    gaps: 0,
    finalization: 0,
  }
  const boundaries = [
    start,
    end,
    ...intervals.flatMap((interval) => [interval.start, interval.end]),
  ]
    .filter(Number.isFinite)
    .sort((a, b) => a - b)
  const priority: DurationCategory[] = [
    "retryWait",
    "compaction",
    "tool",
    "thinking",
    "setup",
    "finalization",
  ]
  for (let index = 0; index < boundaries.length - 1; index += 1) {
    const segmentStart = boundaries[index]!
    const segmentEnd = boundaries[index + 1]!
    if (segmentEnd <= segmentStart) continue
    const active = priority.find((category) =>
      intervals.some(
        (interval) =>
          interval.category === category &&
          interval.start < segmentEnd &&
          interval.end > segmentStart,
      ),
    )
    totals[active ?? "gaps"] += segmentEnd - segmentStart
  }

  return { wall: Math.max(0, end - start), ...totals }
}

function timelineEntries(
  run: TriageRunRecord,
  turns: TriageTurnRecord[],
  retries: TriageRetryRecord[],
  compactions: TriageCompactionRecord[],
): RunTimelineEntry[] {
  const entries: RunTimelineEntry[] = [
    ...turns.map((turn): RunTimelineEntry => ({
      type: "turn",
      id: turn.id,
      ordinal: turn.ordinal,
      startedAt: turn.startedAt,
      endedAt: turn.endedAt,
      state:
        run.outcome && turn.stopReason === "aborted"
          ? "interrupted"
          : turn.endedAt
            ? "completed"
            : run.outcome
              ? "interrupted"
              : "active",
      stopReason: turn.stopReason,
      usage: {
        inputTokens: turn.inputTokens,
        outputTokens: turn.outputTokens,
        cacheReadTokens: turn.cacheReadTokens,
        cacheWriteTokens: turn.cacheWriteTokens,
        reasoningTokens: turn.reasoningTokens,
        totalTokens: turn.totalTokens,
        totalCost: turn.totalCost,
      },
      contextTokens: turn.contextTokens,
      contextWindow: turn.contextWindow,
    })),
    ...run.steps.flatMap((step): RunTimelineEntry[] =>
      step.kind === "compaction"
        ? []
        : [
            {
              type: "span",
              id: step.id,
              turnOrdinal: step.turnOrdinal,
              kind: step.kind,
              label: step.label,
              startedAt: step.startedAt,
              endedAt: step.endedAt,
              state: step.outcome ?? "active",
            },
          ],
    ),
    ...retries.map((retry): RunTimelineEntry => ({
      type: "retry",
      id: retry.id,
      attempt: retry.attempt,
      maxAttempts: retry.maxAttempts,
      delayMs: retry.delayMs,
      startedAt: retry.startedAt,
      waitEndedAt: retry.waitEndedAt,
      endedAt: retry.endedAt,
      state: retry.outcome ?? "active",
      errorCategory: retry.errorCategory,
    })),
    ...compactions.map((compaction): RunTimelineEntry => ({
      type: "compaction",
      id: compaction.id,
      reason: compaction.reason,
      startedAt: compaction.startedAt,
      endedAt: compaction.endedAt,
      state: compaction.outcome ?? "active",
      aborted: compaction.aborted,
      willRetry: compaction.willRetry,
      tokensBefore: compaction.tokensBefore,
      estimatedTokensAfter: compaction.estimatedTokensAfter,
      totalTokens: compaction.totalTokens,
      totalCost: compaction.totalCost,
    })),
  ]
  return entries.sort(
    (left, right) =>
      Date.parse(left.startedAt) - Date.parse(right.startedAt) ||
      timelineOrder(left.type) - timelineOrder(right.type) ||
      left.id - right.id,
  )
}

function timelineOrder(type: RunTimelineEntry["type"]): number {
  return { turn: 0, span: 1, retry: 2, compaction: 3 }[type]
}

function addInterval(
  intervals: Interval[],
  category: DurationCategory,
  intervalStart: number,
  intervalEnd: number,
  runStart: number,
  runEnd: number,
): void {
  const start = Math.max(runStart, intervalStart)
  const end = Math.min(runEnd, intervalEnd)
  if (Number.isFinite(start) && Number.isFinite(end) && end > start)
    intervals.push({ start, end, category })
}

function nullableSum(values: Array<number | null>): number | null {
  const present = values.filter((value): value is number => value !== null)
  return present.length > 0
    ? present.reduce((sum, value) => sum + value, 0)
    : null
}

function elapsed(from: string, to: string): number | null {
  const value = Date.parse(to) - Date.parse(from)
  return Number.isFinite(value) ? Math.max(0, value) : null
}

function boundedInteger(
  value: number | undefined,
  minimum: number,
  maximum: number,
  fallback: number,
): number {
  return Number.isInteger(value) && value !== undefined
    ? Math.min(maximum, Math.max(minimum, value))
    : fallback
}

function eventUrl(event: EventRecord): string | null {
  try {
    const metadata = JSON.parse(event.operationalMetadata) as { url?: unknown }
    if (typeof metadata.url !== "string") return null
    const url = new URL(metadata.url)
    return url.protocol === "http:" || url.protocol === "https:"
      ? url.href
      : null
  } catch {
    return null
  }
}
