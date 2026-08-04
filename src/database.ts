import { mkdirSync } from "node:fs"
import { basename, dirname } from "node:path"
import { Database } from "bun:sqlite"
import type { IntakeItem } from "./protocol.ts"

export type EventStatus =
  | "pending"
  | "processing"
  | "retryable"
  | "succeeded"
  | "failed"
  | "ignored"

export interface EventRecord {
  id: number
  source: string
  entityId: string
  revisionId: string
  kind: string
  title: string
  payload: string | null
  operationalMetadata: string
  occurredAt: string
  observedAt: string
  status: EventStatus
  attemptCount: number
  nextAttemptAt: string | null
  lastError: string | null
  avenRef: string | null
  investigationHandle: string | null
}

const migrations = [
  `
  CREATE TABLE source_state (
    source TEXT PRIMARY KEY,
    checkpoint TEXT,
    last_success_at TEXT,
    last_error TEXT,
    updated_at TEXT NOT NULL
  );
  CREATE TABLE entities (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    external_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    aven_ref TEXT,
    investigation_handle TEXT,
    last_event_at TEXT NOT NULL,
    handling_status TEXT NOT NULL DEFAULT 'pending',
    operational_metadata TEXT NOT NULL DEFAULT '{}',
    UNIQUE(source, external_id)
  );
  CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    revision_id TEXT NOT NULL,
    payload TEXT,
    occurred_at TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT,
    updated_at TEXT NOT NULL,
    UNIQUE(entity_id, revision_id)
  );
  CREATE INDEX events_queue_idx ON events(status, next_attempt_at, observed_at);
  CREATE TABLE command_events (
    id INTEGER PRIMARY KEY,
    event_id INTEGER NOT NULL REFERENCES events(id),
    command TEXT NOT NULL,
    exit_code INTEGER NOT NULL,
    output_summary TEXT NOT NULL,
    created_at TEXT NOT NULL
  );
  `,
]

export class IntakeDatabase {
  readonly raw: Database

  constructor(path: string) {
    if (path !== ":memory:")
      mkdirSync(dirname(path), { recursive: true, mode: 0o700 })
    this.raw = new Database(path, { create: true, strict: true })
    this.raw.exec(
      "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
    )
    this.migrate()
    this.recoverInterrupted()
  }

  close(): void {
    this.raw.close()
  }

  private migrate(): void {
    this.raw.exec(
      "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )
    const applied = new Set(
      this.raw
        .query("SELECT version FROM schema_migrations")
        .all()
        .map((row: any) => Number(row.version)),
    )
    for (const [index, sql] of migrations.entries()) {
      const version = index + 1
      if (applied.has(version)) continue
      this.raw.transaction(() => {
        this.raw.exec(sql)
        this.raw
          .query(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?, ?)",
          )
          .run(version, new Date().toISOString())
      })()
    }
  }

  private recoverInterrupted(): void {
    const now = new Date().toISOString()
    this.raw
      .query(
        "UPDATE events SET status = 'retryable', next_attempt_at = ?, last_error = 'triage interrupted by process exit', updated_at = ? WHERE status = 'processing'",
      )
      .run(now, now)
  }

  sourceCheckpoint(source: string): unknown {
    const row = this.raw
      .query("SELECT checkpoint FROM source_state WHERE source = ?")
      .get(source) as { checkpoint: string | null } | null
    return row?.checkpoint ? JSON.parse(row.checkpoint) : null
  }

  sourceSucceeded(
    source: string,
    checkpoint: unknown,
    items: IntakeItem[],
    observedAt: string,
  ): number {
    return this.raw.transaction(() => {
      let inserted = 0
      for (const item of items) {
        this.raw
          .query(
            `INSERT INTO entities(source, external_id, kind, title, last_event_at, operational_metadata)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(source, external_id) DO UPDATE SET
               kind = excluded.kind,
               title = excluded.title,
               last_event_at = excluded.last_event_at,
               operational_metadata = excluded.operational_metadata`,
          )
          .run(
            source,
            item.entityId,
            item.kind,
            item.title,
            item.occurredAt,
            JSON.stringify({ url: item.url ?? null, kind: item.kind }),
          )
        const entity = this.raw
          .query("SELECT id FROM entities WHERE source = ? AND external_id = ?")
          .get(source, item.entityId) as { id: number }
        const result = this.raw
          .query(
            `INSERT OR IGNORE INTO events(entity_id, revision_id, payload, occurred_at, observed_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)`,
          )
          .run(
            entity.id,
            item.revisionId,
            JSON.stringify(item),
            item.occurredAt,
            observedAt,
            observedAt,
          )
        inserted += Number(result.changes)
      }
      this.raw
        .query(
          `INSERT INTO source_state(source, checkpoint, last_success_at, last_error, updated_at)
           VALUES (?, ?, ?, NULL, ?)
           ON CONFLICT(source) DO UPDATE SET checkpoint = excluded.checkpoint,
             last_success_at = excluded.last_success_at, last_error = NULL, updated_at = excluded.updated_at`,
        )
        .run(source, JSON.stringify(checkpoint), observedAt, observedAt)
      return inserted
    })()
  }

  sourceFailed(
    source: string,
    error: string,
    now = new Date().toISOString(),
  ): void {
    this.raw
      .query(
        `INSERT INTO source_state(source, last_error, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(source) DO UPDATE SET last_error = excluded.last_error, updated_at = excluded.updated_at`,
      )
      .run(source, error.slice(0, 4096), now)
  }

  claimNext(now = new Date().toISOString()): EventRecord | null {
    return this.raw.transaction(() => {
      const row = this.raw
        .query(
          `SELECT ev.id FROM events ev
           WHERE ev.status IN ('pending', 'retryable')
             AND (ev.next_attempt_at IS NULL OR ev.next_attempt_at <= ?)
             AND NOT EXISTS (
               SELECT 1 FROM events prior
               WHERE prior.entity_id = ev.entity_id
                 AND prior.status IN ('pending', 'retryable', 'processing')
                 AND (
                   prior.observed_at < ev.observed_at OR
                   (prior.observed_at = ev.observed_at AND prior.id < ev.id)
                 )
             )
           ORDER BY ev.observed_at, ev.id LIMIT 1`,
        )
        .get(now) as { id: number } | null
      if (!row) return null
      this.raw
        .query(
          "UPDATE events SET status = 'processing', attempt_count = attempt_count + 1, updated_at = ? WHERE id = ?",
        )
        .run(now, row.id)
      return this.event(row.id)
    })()
  }

  event(id: number): EventRecord | null {
    return this.raw
      .query(
        `SELECT ev.id, en.source, en.external_id AS entityId, ev.revision_id AS revisionId,
           en.kind, en.title, ev.payload, en.operational_metadata AS operationalMetadata,
           ev.occurred_at AS occurredAt, ev.observed_at AS observedAt, ev.status,
           ev.attempt_count AS attemptCount, ev.next_attempt_at AS nextAttemptAt,
           ev.last_error AS lastError, en.aven_ref AS avenRef,
           en.investigation_handle AS investigationHandle
         FROM events ev JOIN entities en ON en.id = ev.entity_id WHERE ev.id = ?`,
      )
      .get(id) as EventRecord | null
  }

  listEvents(limit = 50): EventRecord[] {
    return this.raw
      .query(
        `SELECT ev.id, en.source, en.external_id AS entityId, ev.revision_id AS revisionId,
           en.kind, en.title, ev.payload, en.operational_metadata AS operationalMetadata,
           ev.occurred_at AS occurredAt, ev.observed_at AS observedAt, ev.status,
           ev.attempt_count AS attemptCount, ev.next_attempt_at AS nextAttemptAt,
           ev.last_error AS lastError, en.aven_ref AS avenRef,
           en.investigation_handle AS investigationHandle
         FROM events ev JOIN entities en ON en.id = ev.entity_id
         ORDER BY ev.observed_at DESC, ev.id DESC LIMIT ?`,
      )
      .all(limit) as EventRecord[]
  }

  status(): Record<string, number> {
    const result: Record<string, number> = {}
    for (const row of this.raw
      .query("SELECT status, COUNT(*) AS count FROM events GROUP BY status")
      .all() as Array<{
      status: string
      count: number
    }>) {
      result[row.status] = row.count
    }
    return result
  }

  sourceStatuses(): Array<Record<string, unknown>> {
    return this.raw
      .query(
        "SELECT source, last_success_at AS lastSuccessAt, last_error AS lastError, updated_at AS updatedAt FROM source_state ORDER BY source",
      )
      .all() as Array<Record<string, unknown>>
  }

  succeed(id: number): void {
    const now = new Date().toISOString()
    this.raw.transaction(() => {
      this.raw
        .query(
          "UPDATE events SET status = 'succeeded', payload = NULL, last_error = NULL, next_attempt_at = NULL, updated_at = ? WHERE id = ?",
        )
        .run(now, id)
      this.raw
        .query(
          "UPDATE entities SET handling_status = 'succeeded' WHERE id = (SELECT entity_id FROM events WHERE id = ?)",
        )
        .run(id)
    })()
  }

  fail(
    id: number,
    error: string,
    maxAttempts: number,
    retryBaseSeconds: number,
  ): void {
    const event = this.event(id)
    if (!event) throw new Error(`Unknown event ${id}`)
    const retryable = event.attemptCount < maxAttempts
    const delay = retryBaseSeconds * 2 ** Math.max(0, event.attemptCount - 1)
    const next = retryable
      ? new Date(Date.now() + delay * 1000).toISOString()
      : null
    const status = retryable ? "retryable" : "failed"
    this.raw.transaction(() => {
      this.raw
        .query(
          "UPDATE events SET status = ?, next_attempt_at = ?, last_error = ?, updated_at = ? WHERE id = ? AND status = 'processing'",
        )
        .run(status, next, error.slice(0, 4096), new Date().toISOString(), id)
      this.raw
        .query(
          "UPDATE entities SET handling_status = ? WHERE id = (SELECT entity_id FROM events WHERE id = ?)",
        )
        .run(status, id)
    })()
  }

  retry(id: number): boolean {
    return this.raw.transaction(() => {
      const result = this.raw
        .query(
          "UPDATE events SET status = 'retryable', attempt_count = 0, next_attempt_at = NULL, last_error = NULL, updated_at = ? WHERE id = ? AND payload IS NOT NULL AND status != 'processing'",
        )
        .run(new Date().toISOString(), id)
      if (Number(result.changes) === 1) {
        this.raw
          .query(
            "UPDATE entities SET handling_status = 'retryable' WHERE id = (SELECT entity_id FROM events WHERE id = ?)",
          )
          .run(id)
      }
      return Number(result.changes) === 1
    })()
  }

  ignore(id: number): boolean {
    return this.raw.transaction(() => {
      const result = this.raw
        .query(
          "UPDATE events SET status = 'ignored', payload = NULL, next_attempt_at = NULL, updated_at = ? WHERE id = ? AND status != 'processing'",
        )
        .run(new Date().toISOString(), id)
      if (Number(result.changes) === 1) {
        this.raw
          .query(
            "UPDATE entities SET handling_status = 'ignored' WHERE id = (SELECT entity_id FROM events WHERE id = ?)",
          )
          .run(id)
      }
      return Number(result.changes) === 1
    })()
  }

  recordCommand(
    id: number,
    command: string,
    exitCode: number,
    output: string,
  ): void {
    const commandPrefix =
      command
        .match(/^\s*[a-zA-Z0-9._+-]+(?:\s+[a-zA-Z0-9._+-]+)?/)?.[0]
        .trim() ?? "command"
    const commandDigest = new Bun.CryptoHasher("sha256")
      .update(command)
      .digest("hex")
    const outputDigest = new Bun.CryptoHasher("sha256")
      .update(output)
      .digest("hex")
    this.raw
      .query(
        "INSERT INTO command_events(event_id, command, exit_code, output_summary, created_at) VALUES (?, ?, ?, ?, ?)",
      )
      .run(
        id,
        `${commandPrefix} sha256=${commandDigest}`,
        exitCode,
        `bytes=${Buffer.byteLength(output)} sha256=${outputDigest}`,
        new Date().toISOString(),
      )

    if (exitCode !== 0) return
    const executable = command.match(/^\s*([a-zA-Z0-9._+-]+)/)?.[1]
    const aven =
      executable === "aven"
        ? output.match(/\b([A-Z][A-Z0-9]*-[A-Z0-9]{3,})\b/)
        : null
    const explicitHandle =
      executable === "workmux"
        ? output.match(/handle(?:\s+name)?[:=]\s*([a-z0-9][a-z0-9._-]*)/i)?.[1]
        : undefined
    const worktreePath =
      executable === "workmux"
        ? output.match(/^\s*Worktree:\s*(\S+)/im)?.[1]
        : undefined
    const workmux =
      explicitHandle ?? (worktreePath ? basename(worktreePath) : null)
    if (aven) this.updateEntityReference(id, "aven_ref", aven[1] ?? null)
    if (workmux) this.updateEntityReference(id, "investigation_handle", workmux)
  }

  private updateEntityReference(
    id: number,
    column: "aven_ref" | "investigation_handle",
    value: string | null,
  ): void {
    if (!value) return
    this.raw
      .query(
        `UPDATE entities SET ${column} = ? WHERE id = (SELECT entity_id FROM events WHERE id = ?)`,
      )
      .run(value, id)
  }
}
