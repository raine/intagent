import type { RunDetail, RunTimelineEntry } from "../../src/run-detail.ts"

type FixtureOptions = {
  run?: Partial<RunDetail["run"]>
  event?: Partial<RunDetail["event"]>
  metrics?: Partial<RunDetail["metrics"]>
  entries?: RunTimelineEntry[]
  siblingAttempts?: RunDetail["siblingAttempts"]
  effects?: RunDetail["effects"]
  page?: Partial<RunDetail["timeline"]["page"]>
}

const usage = {
  inputTokens: 800,
  outputTokens: 120,
  cacheReadTokens: 400,
  cacheWriteTokens: 20,
  reasoningTokens: 80,
  totalTokens: 1420,
  totalCost: 0.018,
}

export function runDetailFixture(options: FixtureOptions = {}): RunDetail {
  const entries = options.entries ?? cleanEntries()
  const run = {
    id: 7,
    eventId: 11,
    attempt: 2,
    startedAt: "2026-08-05T10:00:00.000Z",
    endedAt: "2026-08-05T10:00:12.000Z",
    lastActivityAt: "2026-08-05T10:00:12.000Z",
    state: "succeeded" as const,
    terminationReason: null,
    failureCategory: null,
    model: {
      id: "claude-sonnet-5",
      provider: "anthropic",
      thinkingLevel: "medium",
      contextWindow: 200_000,
      maxTokens: 16_000,
    },
    telemetry: { schemaVersion: 1, completeness: "complete" as const },
    ...options.run,
  }
  const metricDefaults: RunDetail["metrics"] = {
    durationMs: {
      wall: 12_000,
      setup: 1_000,
      thinking: 4_000,
      tool: 4_000,
      compaction: 0,
      retryWait: 0,
      gaps: 1_000,
      finalization: 2_000,
    },
    toolCallCount: 2,
    failedToolCount: 0,
    turnCount: 2,
    retryCount: 0,
    compactionCount: 0,
    usage,
    peakContextTokens: 45_000,
    peakContextPercent: 22.5,
    sourceLagMs: 60_000,
    queueWaitMs: 30_000,
  }
  const metrics = { ...metricDefaults, ...options.metrics }
  return {
    generatedAt: "2026-08-05T10:00:12.000Z",
    run,
    event: {
      id: 11,
      source: "github",
      entityId: "github:private/repo#11",
      kind: "github-issue",
      title: "Inspect the production triage run",
      url: "https://github.example/private/repo/issues/11",
      occurredAt: "2026-08-05T09:59:00.000Z",
      observedAt: "2026-08-05T09:59:30.000Z",
      status: "succeeded",
      avenRef: "OPS-7KQ9",
      investigationHandle: "triage-production-run",
      ...options.event,
    },
    siblingAttempts: options.siblingAttempts ?? [
      {
        id: 6,
        attempt: 1,
        startedAt: "2026-08-05T09:58:00.000Z",
        endedAt: "2026-08-05T09:58:05.000Z",
        state: "failed",
        failureCategory: "rate_limit",
        telemetryCompleteness: "complete",
      },
      {
        id: run.id,
        attempt: run.attempt,
        startedAt: run.startedAt,
        endedAt: run.endedAt,
        state: run.state,
        failureCategory: run.failureCategory,
        telemetryCompleteness: run.telemetry.completeness,
      },
    ],
    metrics,
    effects: options.effects ?? [
      {
        type: "aven_reference",
        value: "OPS-7KQ9",
        recordedAt: "2026-08-05T10:00:11.000Z",
      },
      {
        type: "investigation_handle",
        value: "triage-production-run",
        recordedAt: "2026-08-05T10:00:11.500Z",
      },
    ],
    limits: {
      maxTurns: 20,
      wallTimeoutMs: 300_000,
      modelContextWindow: 200_000,
      modelMaxTokens: 16_000,
    },
    timeline: {
      entries,
      page: {
        offset: 0,
        limit: 200,
        returned: entries.length,
        total: entries.length,
        hasMore: false,
        nextOffset: null,
        ...options.page,
      },
    },
  }
}

export function cleanEntries(): RunTimelineEntry[] {
  return [
    turn(1, "2026-08-05T10:00:01.000Z", "2026-08-05T10:00:06.000Z"),
    span(
      1,
      1,
      "thinking",
      "thinking",
      "2026-08-05T10:00:01.000Z",
      "2026-08-05T10:00:03.000Z",
    ),
    span(
      2,
      1,
      "tool",
      "Read",
      "2026-08-05T10:00:03.000Z",
      "2026-08-05T10:00:05.000Z",
    ),
    turn(2, "2026-08-05T10:00:06.000Z", "2026-08-05T10:00:10.000Z"),
    span(
      3,
      2,
      "thinking",
      "thinking",
      "2026-08-05T10:00:06.000Z",
      "2026-08-05T10:00:08.000Z",
    ),
    span(
      4,
      2,
      "tool",
      "Aven",
      "2026-08-05T10:00:08.000Z",
      "2026-08-05T10:00:10.000Z",
    ),
  ]
}

export function turn(
  ordinal: number,
  startedAt: string,
  endedAt: string | null,
  state: "active" | "completed" | "interrupted" = "completed",
): Extract<RunTimelineEntry, { type: "turn" }> {
  return {
    type: "turn",
    id: ordinal,
    ordinal,
    startedAt,
    endedAt,
    state,
    stopReason: state === "completed" ? "stop" : null,
    usage,
    contextTokens: 45_000,
    contextWindow: 200_000,
  }
}

export function span(
  id: number,
  turnOrdinal: number | null,
  kind: "tool" | "thinking",
  label: string,
  startedAt: string,
  endedAt: string | null,
  state: "active" | "succeeded" | "failed" | "interrupted" = "succeeded",
): Extract<RunTimelineEntry, { type: "span" }> {
  return {
    type: "span",
    id,
    turnOrdinal,
    kind,
    label,
    startedAt,
    endedAt,
    state,
  }
}

export function retry(
  state: "active" | "succeeded" | "failed" | "interrupted" = "succeeded",
): Extract<RunTimelineEntry, { type: "retry" }> {
  return {
    type: "retry",
    id: 20,
    attempt: 1,
    maxAttempts: 3,
    delayMs: 1000,
    startedAt: "2026-08-05T10:00:04.000Z",
    waitEndedAt: "2026-08-05T10:00:05.000Z",
    endedAt: "2026-08-05T10:00:05.000Z",
    state,
    errorCategory: "rate_limit",
  }
}

export function compaction(
  state:
    | "active"
    | "succeeded"
    | "failed"
    | "aborted"
    | "interrupted" = "succeeded",
): Extract<RunTimelineEntry, { type: "compaction" }> {
  return {
    type: "compaction",
    id: 30,
    reason: "threshold",
    startedAt: "2026-08-05T10:00:05.000Z",
    endedAt: "2026-08-05T10:00:06.000Z",
    state,
    aborted: state === "aborted",
    willRetry: state !== "succeeded",
    tokensBefore: 180_000,
    estimatedTokensAfter: 80_000,
    totalTokens: 1100,
    totalCost: 0.01,
  }
}
