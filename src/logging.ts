import { randomUUID } from "node:crypto"
import { chmod, mkdir, open } from "node:fs/promises"
import { dirname, join } from "node:path"
import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent"
import { errorMessage, expandPath } from "./config.ts"
import { safeErrorCategory, type EventRecord } from "./database.ts"

const MAX_STRING_BYTES = 256 * 1024
const MAX_RECORD_BYTES = 2 * 1024 * 1024
const MAX_DEPTH = 24
const MAX_ARRAY_ITEMS = 10_000
const MAX_OBJECT_KEYS = 2_000

interface WritableWarning {
  write(value: string): unknown
}

export interface LogRecord {
  timestamp: string
  type: string
  [key: string]: unknown
}

export class DurableLogStore {
  readonly directory: string
  private readonly queues = new Map<string, Promise<void>>()
  private readonly warned = new Set<string>()

  constructor(
    directory: string,
    private readonly redact: (value: string) => string = (value) => value,
    private readonly warnings: WritableWarning = process.stderr,
  ) {
    this.directory = expandPath(directory)
  }

  monitor(type: string, details: Record<string, unknown> = {}): Promise<void> {
    return this.append(join(this.directory, "monitor.jsonl"), {
      timestamp: new Date().toISOString(),
      type,
      ...details,
    })
  }

  triage(event: EventRecord): TriageRunLog {
    const timestamp = filenameTimestamp(new Date())
    const source = filenamePart(event.source)
    const filename = [
      `triage-event-${event.id}`,
      `attempt-${event.attemptCount}`,
      source,
      timestamp,
      randomUUID().slice(0, 8),
    ]
      .filter(Boolean)
      .join("-")
    return new TriageRunLog(
      join(this.directory, "triage", `${filename}.jsonl`),
      event,
      this,
    )
  }

  append(path: string, record: LogRecord): Promise<void> {
    const previous = this.queues.get(path) ?? Promise.resolve()
    const queued = previous
      .then(() => this.write(path, record))
      .catch((error) => this.warn(path, error))
    this.queues.set(path, queued)
    return queued
  }

  private async write(path: string, record: LogRecord): Promise<void> {
    const normalized = normalize(record, this.redact)
    let line = JSON.stringify(normalized)
    if (Buffer.byteLength(line) > MAX_RECORD_BYTES) {
      line = JSON.stringify({
        timestamp: record.timestamp,
        type: record.type,
        value: boundedString(line, MAX_STRING_BYTES, this.redact),
        recordTruncated: true,
      })
    }
    const directory = dirname(path)
    await mkdir(directory, { recursive: true, mode: 0o700 })
    await chmod(directory, 0o700)
    const file = await open(path, "a", 0o600)
    try {
      await file.chmod(0o600)
      await file.writeFile(`${line}\n`)
      await file.sync()
    } finally {
      await file.close()
    }
  }

  private warn(path: string, error: unknown): void {
    if (this.warned.has(path)) return
    this.warned.add(path)
    const message = this.redact(errorMessage(error))
    this.warnings.write(
      `warning: intake logging failed for ${path}: ${message}\n`,
    )
    if (!path.endsWith("monitor.jsonl"))
      void this.monitor("logging_error", { path, error: message })
  }
}

export class TriageRunLog {
  readonly path: string
  private startedAt = Date.now()

  constructor(
    path: string,
    private readonly intakeEvent: EventRecord,
    private readonly store: DurableLogStore,
  ) {
    this.path = path
  }

  start(): Promise<void> {
    this.startedAt = Date.now()
    return this.record("run_start", {
      event: {
        source: this.intakeEvent.source,
        kind: this.intakeEvent.kind,
        occurredAt: this.intakeEvent.occurredAt,
        observedAt: this.intakeEvent.observedAt,
      },
    })
  }

  metadata(details: Record<string, unknown>): Promise<void> {
    const model = objectValue(details.model)
    return this.record("session_metadata", {
      model: model
        ? {
            id: stringValue(model.id),
            provider: stringValue(model.provider),
            api: stringValue(model.api),
            contextWindow: numberValue(model.contextWindow),
            maxTokens: numberValue(model.maxTokens),
          }
        : null,
      thinkingLevel: stringValue(details.thinkingLevel),
      tools: Array.isArray(details.tools)
        ? details.tools.filter(
            (tool): tool is string => typeof tool === "string",
          )
        : [],
    })
  }

  prompt(value: string): Promise<void> {
    return this.record("prompt_submitted", {
      byteLength: Buffer.byteLength(value),
    })
  }

  event(event: AgentSessionEvent): Promise<void> {
    const safe = safeAgentEvent(event)
    return safe ? this.record(event.type, safe) : Promise.resolve()
  }

  finish(
    outcome: "succeeded" | "failed",
    details: Record<string, unknown> = {},
  ): Promise<void> {
    return this.record("run_end", {
      outcome,
      durationMs: Date.now() - this.startedAt,
      failureCategory: safeErrorCategory(errorString(details.error)),
      terminationReason: stringValue(details.terminationReason),
    })
  }

  private record(
    type: string,
    details: Record<string, unknown> = {},
  ): Promise<void> {
    return this.store.append(this.path, {
      timestamp: new Date().toISOString(),
      type,
      eventId: this.intakeEvent.id,
      attempt: this.intakeEvent.attemptCount,
      ...details,
    })
  }
}

function safeAgentEvent(
  event: AgentSessionEvent,
): Record<string, unknown> | null {
  switch (event.type) {
    case "agent_start":
    case "agent_settled":
    case "turn_start":
      return {}
    case "agent_end":
      return { messageCount: event.messages.length, willRetry: event.willRetry }
    case "turn_end":
      return {
        role: event.message.role,
        stopReason:
          event.message.role === "assistant" ? event.message.stopReason : null,
        usage:
          event.message.role === "assistant"
            ? safeUsage(event.message.usage)
            : null,
        toolResultCount: event.toolResults.length,
        failedToolCount: event.toolResults.filter((result) => result.isError)
          .length,
      }
    case "message_start":
    case "message_end":
      return { role: event.message.role }
    case "message_update":
      return event.assistantMessageEvent.type === "thinking_start" ||
        event.assistantMessageEvent.type === "thinking_end"
        ? { phase: event.assistantMessageEvent.type }
        : null
    case "tool_execution_start":
      return { toolName: event.toolName }
    case "tool_execution_end":
      return { toolName: event.toolName, isError: event.isError }
    case "tool_execution_update":
    case "bash_execution_update":
    case "entry_appended":
    case "session_info_changed":
      return null
    case "queue_update":
      return {
        steeringCount: event.steering.length,
        followUpCount: event.followUp.length,
      }
    case "thinking_level_changed":
      return { level: event.level }
    case "compaction_start":
      return { reason: event.reason }
    case "compaction_end":
      return {
        reason: event.reason,
        aborted: event.aborted,
        willRetry: event.willRetry,
        outcome: event.aborted
          ? "aborted"
          : event.result
            ? "succeeded"
            : "failed",
        tokensBefore: event.result?.tokensBefore ?? null,
        estimatedTokensAfter: event.result?.estimatedTokensAfter ?? null,
        usage: safeUsage(event.result?.usage),
        errorCategory: safeErrorCategory(event.errorMessage),
      }
    case "auto_retry_start":
    case "summarization_retry_scheduled":
      return {
        attempt: event.attempt,
        maxAttempts: event.maxAttempts,
        delayMs: event.delayMs,
        errorCategory: safeErrorCategory(event.errorMessage),
      }
    case "auto_retry_end":
      return {
        attempt: event.attempt,
        outcome: event.success ? "succeeded" : "failed",
        errorCategory: safeErrorCategory(event.finalError),
      }
    case "summarization_retry_attempt_start":
      return {
        source: event.source,
        reason: event.source === "compaction" ? event.reason : null,
      }
    case "summarization_retry_finished":
      return {}
  }
}

function safeUsage(value: unknown): Record<string, unknown> | null {
  const usage = objectValue(value)
  if (!usage) return null
  const cost = objectValue(usage.cost)
  return {
    input: numberValue(usage.input),
    output: numberValue(usage.output),
    cacheRead: numberValue(usage.cacheRead),
    cacheWrite: numberValue(usage.cacheWrite),
    reasoning: numberValue(usage.reasoning),
    totalTokens: numberValue(usage.totalTokens),
    cost: cost
      ? {
          input: numberValue(cost.input),
          output: numberValue(cost.output),
          cacheRead: numberValue(cost.cacheRead),
          cacheWrite: numberValue(cost.cacheWrite),
          total: numberValue(cost.total),
        }
      : null,
  }
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" ? value : null
}

function numberValue(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null
}

function errorString(value: unknown): string | null {
  if (typeof value === "string") return value
  if (value instanceof Error) return value.message
  return null
}

function normalize(
  value: unknown,
  redact: (value: string) => string,
  depth = 0,
  seen = new WeakSet<object>(),
): unknown {
  if (typeof value === "string")
    return boundedString(value, MAX_STRING_BYTES, redact)
  if (value === null || typeof value === "number" || typeof value === "boolean")
    return value
  if (typeof value === "bigint") return value.toString()
  if (typeof value === "undefined") return null
  if (typeof value === "function" || typeof value === "symbol")
    return String(value)
  if (depth >= MAX_DEPTH) return "[TRUNCATED: maximum depth reached]"
  if (seen.has(value)) return "[CIRCULAR]"
  seen.add(value)
  if (Array.isArray(value)) {
    const result = value
      .slice(0, MAX_ARRAY_ITEMS)
      .map((item) => normalize(item, redact, depth + 1, seen))
    if (value.length > MAX_ARRAY_ITEMS)
      result.push(`[TRUNCATED: ${value.length - MAX_ARRAY_ITEMS} array items]`)
    return result
  }
  if (value instanceof Error) {
    return {
      name: value.name,
      message: boundedString(value.message, MAX_STRING_BYTES, redact),
      stack: value.stack
        ? boundedString(value.stack, MAX_STRING_BYTES, redact)
        : null,
      cause: normalize(value.cause, redact, depth + 1, seen),
    }
  }
  const result: Record<string, unknown> = {}
  const entries = Object.entries(value as Record<string, unknown>)
  for (const [key, item] of entries.slice(0, MAX_OBJECT_KEYS))
    result[key] = normalize(item, redact, depth + 1, seen)
  if (entries.length > MAX_OBJECT_KEYS)
    result.valueTruncated = `${entries.length - MAX_OBJECT_KEYS} object keys`
  return result
}

function boundedString(
  value: string,
  limit: number,
  redact: (value: string) => string,
): string {
  const filtered = redact(value)
  const bytes = Buffer.byteLength(filtered)
  if (bytes <= limit) return filtered
  let end = Math.min(filtered.length, limit)
  while (Buffer.byteLength(filtered.slice(0, end)) > limit) end -= 1
  return `${filtered.slice(0, end)}\n[TRUNCATED: ${bytes} bytes total]`
}

function filenamePart(value: string): string {
  return value
    .normalize("NFKD")
    .replace(/[^a-zA-Z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .toLowerCase()
}

function filenameTimestamp(date: Date): string {
  return date
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z")
}
