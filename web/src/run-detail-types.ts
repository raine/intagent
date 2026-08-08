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

export type ConclusionDecision =
  | "action_taken"
  | "no_action"
  | "needs_follow_up"
  | "blocked"
  | "failed"
  | "canceled"
  | "timed_out"
  | "turn_limit"

export type DispatchTrigger =
  | "initial"
  | "revision"
  | "backoff_retry"
  | "recovery_retry"
  | "operator_retry"
  | "manual_injection"
  | "superseding_claim"
  | "unknown"

export interface RunDispatch {
  sequence: number
  trigger: DispatchTrigger
  source: "recorded" | "derived" | "unavailable"
  attempt: number
  maxAttempts: number | null
  finalAttempt: boolean
  scheduledFor: string | null
  claimedAt: string
  priorAttempt: {
    runId: number
    sequence: number
    state: "active" | RunOutcome
    failureCategory: SafeErrorCategory | null
    terminationReason: string | null
    decision: ConclusionDecision | null
    endedAt: string | null
  } | null
  latency: {
    sourceLagMs: number | null
    backoffWaitMs: number | null
    claimDelayMs: number | null
  }
}

export interface TriageConclusion {
  decision: ConclusionDecision
  summary: string
  evidence: string[]
  actions: string[]
  outcome: string
  followUp: string | null
  source: "model" | "derived" | "unavailable"
}

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
      state: "active" | RunOutcome | "aborted"
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
    dispatch: RunDispatch
    conclusion: TriageConclusion
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
    sequence: number
    attempt: number
    startedAt: string
    endedAt: string | null
    state: "active" | RunOutcome
    failureCategory: SafeErrorCategory | null
    terminationReason: string | null
    decision: ConclusionDecision | null
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
