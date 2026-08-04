import { randomUUID } from "node:crypto"
import { chmod, mkdir, open } from "node:fs/promises"
import { dirname, join } from "node:path"
import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent"
import { errorMessage, expandPath } from "./config.ts"
import type { EventRecord } from "./database.ts"

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
        id: this.intakeEvent.id,
        attempt: this.intakeEvent.attemptCount,
        source: this.intakeEvent.source,
        entityId: this.intakeEvent.entityId,
        revisionId: this.intakeEvent.revisionId,
        kind: this.intakeEvent.kind,
        title: this.intakeEvent.title,
        occurredAt: this.intakeEvent.occurredAt,
        observedAt: this.intakeEvent.observedAt,
      },
    })
  }

  metadata(details: Record<string, unknown>): Promise<void> {
    return this.record("session_metadata", details)
  }

  prompt(value: string): Promise<void> {
    return this.record("prompt", { value })
  }

  event(event: AgentSessionEvent): Promise<void> {
    if (event.type === "message_update") {
      const update = event.assistantMessageEvent
      const details: Record<string, unknown> = {
        eventType: update.type,
      }
      if ("contentIndex" in update) details.contentIndex = update.contentIndex
      if ("delta" in update) details.delta = update.delta
      return this.record("message_update", details)
    }
    return this.record(event.type, event as unknown as Record<string, unknown>)
  }

  finish(
    outcome: "succeeded" | "failed",
    details: Record<string, unknown> = {},
  ): Promise<void> {
    return this.record("run_end", {
      outcome,
      durationMs: Date.now() - this.startedAt,
      ...details,
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
