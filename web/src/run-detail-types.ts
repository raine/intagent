export type RunOutcome = "succeeded" | "failed" | "interrupted"
export type TelemetryCompleteness = "complete" | "partial" | "legacy"
export type SafeErrorCategory =
  | "authentication"
  | "rate_limit"
  | "timeout"
  | "connection"
  | "not_found"
  | "model_unavailable"
  | "context_limit"
  | "turn_limit"
  | "aborted"
  | "interrupted"
  | "tool_failure"
  | "unknown"

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
      summary: string | null
      startedAt: string
      endedAt: string | null
      state: "active" | RunOutcome
    }
  | {
      type: "retry"
      id: number
      turnOrdinal: number | null
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
      turnOrdinal: number | null
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
  prompts: Array<{
    role: "system" | "user"
    content: string
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
