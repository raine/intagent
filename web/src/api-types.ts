/* Generated from the Rust dashboard response schemas by npm run generate:api. */

export type EventStatus =
  | "pending"
  | "processing"
  | "retryable"
  | "succeeded"
  | "failed"
  | "ignored"
export type TriageDecision =
  | "action_taken"
  | "no_action"
  | "needs_follow_up"
  | "blocked"
  | "failed"
  | "canceled"
  | "timed_out"
  | "turn_limit"
export type ConclusionSource = "model" | "derived" | "unavailable"
export type DispatchSource = "recorded" | "derived" | "unavailable"
export type DispatchTrigger =
  | "initial"
  | "revision"
  | "backoff_retry"
  | "recovery_retry"
  | "operator_retry"
  | "manual_injection"
  | "superseding_claim"
  | "unknown"
export type TimelineEntry =
  | {
      contextTokens: number | null
      contextWindow: number | null
      endedAt: string | null
      id: number
      ordinal: number
      startedAt: string
      state: string
      stopReason: string | null
      type: "turn"
      usage: UsageMetrics
    }
  | {
      endedAt: string | null
      id: number
      kind: string
      label: string
      startedAt: string
      state: string
      summary: string | null
      turnOrdinal: number | null
      type: "span"
    }
  | {
      attempt: number
      delayMs: number
      endedAt: string | null
      errorCategory: string | null
      id: number
      maxAttempts: number
      startedAt: string
      state: string
      turnOrdinal: number | null
      type: "retry"
      waitEndedAt: string
    }
  | {
      aborted: boolean | null
      endedAt: string | null
      estimatedTokensAfter: number | null
      id: number
      reason: string | null
      startedAt: string
      state: string
      tokensBefore: number | null
      totalCost: number | null
      totalTokens: number | null
      turnOrdinal: number | null
      type: "compaction"
      willRetry: boolean | null
    }

export interface WebApiContract {
  run_detail: RunDetail
  snapshot: DashboardSnapshot
}
export interface RunDetail {
  effects: EffectProjection[]
  event: EventProjection
  generatedAt: string
  limits: RunLimits
  metrics: RunMetrics
  prompts: PromptProjection[]
  run: RunProjection
  siblingAttempts: SiblingAttempt[]
  timeline: Timeline
}
export interface EffectProjection {
  recordedAt: string
  type: string
  value: string
}
export interface EventProjection {
  avenRef: string | null
  entityId: string
  id: number
  investigationHandle: string | null
  kind: string
  observedAt: string
  occurredAt: string
  source: string
  status: EventStatus
  title: string
  url: string | null
}
export interface RunLimits {
  maxTurns: number | null
  modelContextWindow: number | null
  modelMaxTokens: number | null
  wallTimeoutMs: number | null
}
export interface RunMetrics {
  compactionCount: number | null
  durationMs: DurationMetrics
  failedToolCount: number | null
  peakContextPercent: number | null
  peakContextTokens: number | null
  retryCount: number | null
  toolCallCount: number | null
  turnCount: number | null
  usage: UsageMetrics
}
export interface DurationMetrics {
  compaction: number | null
  finalization: number | null
  gaps: number | null
  retryWait: number | null
  setup: number | null
  thinking: number | null
  tool: number | null
  wall: number
}
export interface UsageMetrics {
  cacheReadTokens: number | null
  cacheWriteTokens: number | null
  inputTokens: number | null
  outputTokens: number | null
  reasoningTokens: number | null
  totalCost: number | null
  totalTokens: number | null
}
export interface PromptProjection {
  content: string
  recordedAt: string
  role: string
}
export interface RunProjection {
  attempt: number
  conclusion: TriageConclusion
  dispatch: DispatchProjection
  endedAt: string | null
  eventId: number
  failureCategory: string | null
  id: number
  lastActivityAt: string
  model: ModelProjection
  startedAt: string
  state: string
  telemetry: TelemetryProjection
  terminationReason: string | null
}
export interface TriageConclusion {
  actions: string[]
  decision: TriageDecision
  evidence: string[]
  followUp: string | null
  outcome: string
  source: ConclusionSource
  summary: string
}
export interface DispatchProjection {
  attempt: number
  claimedAt: string
  finalAttempt: boolean
  latency: DispatchLatency
  maxAttempts: number | null
  priorAttempt: DispatchPriorAttempt | null
  scheduledFor: string | null
  sequence: number
  source: DispatchSource
  trigger: DispatchTrigger
}
export interface DispatchLatency {
  backoffWaitMs: number | null
  claimDelayMs: number | null
  sourceLagMs: number | null
}
export interface DispatchPriorAttempt {
  decision: TriageDecision | null
  endedAt: string | null
  failureCategory: string | null
  runId: number
  sequence: number
  state: string
  terminationReason: string | null
}
export interface ModelProjection {
  contextWindow: number | null
  id: string | null
  maxTokens: number | null
  provider: string | null
  thinkingLevel: string | null
}
export interface TelemetryProjection {
  completeness: string
  schemaVersion: number | null
}
export interface SiblingAttempt {
  attempt: number
  decision: TriageDecision | null
  endedAt: string | null
  failureCategory: string | null
  id: number
  sequence: number
  startedAt: string
  state: string
  telemetryCompleteness: string
  terminationReason: string | null
}
export interface Timeline {
  entries: TimelineEntry[]
  page: TimelinePage
}
export interface TimelinePage {
  hasMore: boolean
  limit: number
  nextOffset: number | null
  offset: number
  returned: number
  total: number
}
export interface DashboardSnapshot {
  attention: number
  counts: DashboardCounts
  events: DashboardEvent[]
  generatedAt: string
  handled: number
  oldestOpenAt: string | null
  open: number
  runs: DashboardRun[]
  sources: DashboardSource[]
  total: number
}
export interface DashboardCounts {
  failed: number
  ignored: number
  pending: number
  processing: number
  retryable: number
  succeeded: number
}
export interface DashboardEvent {
  attemptCount: number
  avenRef: string | null
  entityId: string
  id: number
  investigationHandle: string | null
  kind: string
  lastError: string | null
  nextAttemptAt: string | null
  observedAt: string
  occurredAt: string
  source: string
  status: EventStatus
  title: string
  url: string | null
}
export interface DashboardRun {
  attempt: number
  compactionCount: number
  conclusion: TriageConclusion
  dispatchSequence: number
  dispatchTrigger: DispatchTrigger
  endedAt: string | null
  eventId: number
  eventKind: string
  eventTitle: string
  id: number
  investigationHandle: string | null
  lastActivityAt: string
  modelId: string | null
  modelProvider: string | null
  retryCount: number
  source: string
  startedAt: string
  state: string
  steps: DashboardStep[]
  telemetryCompleteness: string
  thinkingLevel: string | null
  timelineTruncated: boolean
  turnCount: number
}
export interface DashboardStep {
  endedAt: string | null
  id: number
  kind: string
  label: string
  startedAt: string
  state: string
  summary: string | null
  turnOrdinal: number | null
}
export interface DashboardSource {
  lastError: string | null
  lastSuccessAt: string | null
  source: string
  updatedAt: string
}
