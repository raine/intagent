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

export interface TriageRunStepRecord {
  id: number
  turnId: number | null
  turnOrdinal: number | null
  kind: "tool" | "thinking" | "compaction"
  label: string
  startedAt: string
  endedAt: string | null
  outcome: RunOutcome | null
}

export interface TriageTurnRecord {
  id: number
  ordinal: number
  startedAt: string
  endedAt: string | null
  stopReason: string | null
  inputTokens: number | null
  outputTokens: number | null
  cacheReadTokens: number | null
  cacheWriteTokens: number | null
  reasoningTokens: number | null
  totalTokens: number | null
  inputCost: number | null
  outputCost: number | null
  cacheReadCost: number | null
  cacheWriteCost: number | null
  totalCost: number | null
  contextTokens: number | null
  contextWindow: number | null
}

export interface TriageRetryRecord {
  id: number
  turnId: number | null
  attempt: number
  maxAttempts: number
  delayMs: number
  startedAt: string
  waitEndedAt: string
  endedAt: string | null
  outcome: "succeeded" | "failed" | "interrupted" | null
  errorCategory: SafeErrorCategory | null
}

export interface TriageCompactionRecord {
  id: number
  turnId: number | null
  reason: "manual" | "threshold" | "overflow" | null
  startedAt: string
  endedAt: string | null
  outcome: "succeeded" | "failed" | "aborted" | "interrupted" | null
  aborted: boolean | null
  willRetry: boolean | null
  tokensBefore: number | null
  estimatedTokensAfter: number | null
  inputTokens: number | null
  outputTokens: number | null
  cacheReadTokens: number | null
  cacheWriteTokens: number | null
  reasoningTokens: number | null
  totalTokens: number | null
  totalCost: number | null
}

export interface TriageEffectRecord {
  id: number
  type: "aven_reference" | "investigation_handle"
  value: string
  recordedAt: string
}

export interface TriageRunRecord {
  id: number
  eventId: number
  attempt: number
  startedAt: string
  endedAt: string | null
  lastActivityAt: string
  outcome: RunOutcome | null
  terminationReason: string | null
  failureCategory: SafeErrorCategory | null
  modelId: string | null
  modelProvider: string | null
  thinkingLevel: string | null
  contextWindow: number | null
  maxTokens: number | null
  telemetryVersion: number | null
  telemetryCompleteness: TelemetryCompleteness
  turnCount: number
  retryCount: number
  compactionCount: number
  steps: TriageRunStepRecord[]
}

export interface TriageTelemetryEvent {
  type: string
  toolCallId?: string
  toolName?: string
  isError?: boolean
  attempt?: number
  maxAttempts?: number
  delayMs?: number
  success?: boolean
  errorMessage?: string
  finalError?: string
  reason?: "manual" | "threshold" | "overflow"
  aborted?: boolean
  willRetry?: boolean
  result?: {
    tokensBefore?: number
    estimatedTokensAfter?: number
    usage?: SafeUsage
  }
  message?: {
    role?: string
    stopReason?: string
    errorMessage?: string
    usage?: SafeUsage
  }
  contextUsage?: {
    tokens: number | null
    contextWindow: number
  }
  assistantMessageEvent?: {
    type?: string
    contentIndex?: number
  }
}

interface SafeUsage {
  input?: number
  output?: number
  cacheRead?: number
  cacheWrite?: number
  reasoning?: number
  totalTokens?: number
  cost?: {
    input?: number
    output?: number
    cacheRead?: number
    cacheWrite?: number
    total?: number
  }
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
  `
  ALTER TABLE events ADD COLUMN source TEXT;
  UPDATE events
  SET source = (SELECT source FROM entities WHERE entities.id = events.entity_id);

  CREATE TEMP TABLE entity_merge AS
  SELECT entity.id AS old_id,
    (SELECT MIN(candidate.id)
     FROM entities candidate
     WHERE candidate.external_id = entity.external_id) AS canonical_id
  FROM entities entity;

  CREATE TEMP TABLE event_merge AS
  SELECT event.id AS old_id,
    (SELECT MIN(candidate.id)
     FROM events candidate
     JOIN entity_merge candidate_entity ON candidate_entity.old_id = candidate.entity_id
     WHERE candidate_entity.canonical_id = event_entity.canonical_id
       AND candidate.revision_id = event.revision_id) AS canonical_id
  FROM events event
  JOIN entity_merge event_entity ON event_entity.old_id = event.entity_id;

  UPDATE command_events
  SET event_id = (
    SELECT canonical_id FROM event_merge WHERE old_id = command_events.event_id
  )
  WHERE event_id IN (SELECT old_id FROM event_merge WHERE old_id != canonical_id);

  DELETE FROM events
  WHERE id IN (SELECT old_id FROM event_merge WHERE old_id != canonical_id);

  UPDATE events
  SET entity_id = (
    SELECT canonical_id FROM entity_merge WHERE old_id = events.entity_id
  );

  UPDATE entities AS canonical
  SET aven_ref = COALESCE(
        (SELECT duplicate.aven_ref
         FROM entities duplicate
         JOIN entity_merge mapping ON mapping.old_id = duplicate.id
         WHERE mapping.canonical_id = canonical.id
           AND duplicate.aven_ref IS NOT NULL
         ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
         LIMIT 1),
        canonical.aven_ref
      ),
      investigation_handle = COALESCE(
        (SELECT duplicate.investigation_handle
         FROM entities duplicate
         JOIN entity_merge mapping ON mapping.old_id = duplicate.id
         WHERE mapping.canonical_id = canonical.id
           AND duplicate.investigation_handle IS NOT NULL
         ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
         LIMIT 1),
        canonical.investigation_handle
      ),
      kind = (SELECT duplicate.kind
              FROM entities duplicate
              JOIN entity_merge mapping ON mapping.old_id = duplicate.id
              WHERE mapping.canonical_id = canonical.id
              ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
              LIMIT 1),
      title = (SELECT duplicate.title
               FROM entities duplicate
               JOIN entity_merge mapping ON mapping.old_id = duplicate.id
               WHERE mapping.canonical_id = canonical.id
               ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
               LIMIT 1),
      last_event_at = (SELECT MAX(duplicate.last_event_at)
                       FROM entities duplicate
                       JOIN entity_merge mapping ON mapping.old_id = duplicate.id
                       WHERE mapping.canonical_id = canonical.id),
      handling_status = (SELECT duplicate.handling_status
                         FROM entities duplicate
                         JOIN entity_merge mapping ON mapping.old_id = duplicate.id
                         WHERE mapping.canonical_id = canonical.id
                         ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
                         LIMIT 1),
      operational_metadata = (SELECT duplicate.operational_metadata
                              FROM entities duplicate
                              JOIN entity_merge mapping ON mapping.old_id = duplicate.id
                              WHERE mapping.canonical_id = canonical.id
                              ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
                              LIMIT 1)
  WHERE canonical.id IN (SELECT canonical_id FROM entity_merge);

  DELETE FROM entities
  WHERE id IN (SELECT old_id FROM entity_merge WHERE old_id != canonical_id);

  CREATE UNIQUE INDEX entities_external_id_idx ON entities(external_id);
  DROP TABLE event_merge;
  DROP TABLE entity_merge;
  `,
  `
  CREATE TABLE triage_runs (
    id INTEGER PRIMARY KEY,
    event_id INTEGER NOT NULL REFERENCES events(id),
    attempt INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    last_activity_at TEXT NOT NULL,
    outcome TEXT,
    model_id TEXT,
    model_provider TEXT,
    thinking_level TEXT,
    turn_count INTEGER NOT NULL DEFAULT 0,
    retry_count INTEGER NOT NULL DEFAULT 0,
    compaction_count INTEGER NOT NULL DEFAULT 0
  );
  CREATE INDEX triage_runs_event_idx ON triage_runs(event_id, started_at DESC);
  CREATE INDEX triage_runs_recent_idx ON triage_runs(started_at DESC);
  CREATE TABLE triage_run_steps (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    step_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT,
    UNIQUE(run_id, step_key)
  );
  CREATE INDEX triage_run_steps_run_idx ON triage_run_steps(run_id, started_at);
  `,
  `
  ALTER TABLE triage_runs ADD COLUMN termination_reason TEXT;
  ALTER TABLE triage_runs ADD COLUMN failure_category TEXT;
  ALTER TABLE triage_runs ADD COLUMN context_window INTEGER;
  ALTER TABLE triage_runs ADD COLUMN max_tokens INTEGER;
  ALTER TABLE triage_runs ADD COLUMN telemetry_version INTEGER;
  ALTER TABLE triage_runs ADD COLUMN telemetry_completeness TEXT NOT NULL DEFAULT 'legacy';

  CREATE TABLE triage_run_turns (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    ordinal INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    stop_reason TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    input_cost REAL,
    output_cost REAL,
    cache_read_cost REAL,
    cache_write_cost REAL,
    total_cost REAL,
    context_tokens INTEGER,
    context_window INTEGER,
    UNIQUE(run_id, ordinal)
  );
  CREATE INDEX triage_run_turns_run_idx ON triage_run_turns(run_id, ordinal);
  ALTER TABLE triage_run_steps ADD COLUMN turn_id INTEGER REFERENCES triage_run_turns(id);

  CREATE TABLE triage_run_retries (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    turn_id INTEGER REFERENCES triage_run_turns(id),
    attempt INTEGER NOT NULL,
    max_attempts INTEGER NOT NULL,
    delay_ms INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    wait_ended_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT,
    error_category TEXT
  );
  CREATE INDEX triage_run_retries_run_idx ON triage_run_retries(run_id, started_at);

  CREATE TABLE triage_run_compactions (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    turn_id INTEGER REFERENCES triage_run_turns(id),
    reason TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT,
    aborted INTEGER,
    will_retry INTEGER,
    tokens_before INTEGER,
    estimated_tokens_after INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    total_cost REAL
  );
  CREATE INDEX triage_run_compactions_run_idx ON triage_run_compactions(run_id, started_at);

  CREATE TABLE triage_run_effects (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    type TEXT NOT NULL,
    value TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(run_id, type, value)
  );
  CREATE INDEX triage_run_effects_run_idx ON triage_run_effects(run_id, recorded_at);

  UPDATE triage_runs
  SET ended_at = last_activity_at,
      outcome = COALESCE(outcome, 'interrupted'),
      termination_reason = COALESCE(termination_reason, 'legacy_interruption'),
      failure_category = CASE
        WHEN outcome IS NULL THEN 'interrupted'
        ELSE failure_category
      END,
      telemetry_completeness = 'legacy'
  WHERE ended_at IS NULL
    AND (outcome IS NOT NULL OR event_id IN (
      SELECT id FROM events WHERE status != 'processing'
    ));

  UPDATE triage_run_steps SET step_key = lower(hex(randomblob(16)));

  UPDATE triage_run_steps
  SET ended_at = (SELECT run.ended_at FROM triage_runs run WHERE run.id = run_id),
      outcome = 'interrupted'
  WHERE ended_at IS NULL
    AND run_id IN (SELECT id FROM triage_runs WHERE ended_at IS NOT NULL);
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

  recoverInterrupted(
    staleBefore: string,
    now = new Date().toISOString(),
  ): number {
    return this.raw.transaction(() => {
      const runIds = this.raw
        .query(
          `SELECT run.id FROM triage_runs run
           JOIN events event ON event.id = run.event_id
           WHERE run.ended_at IS NULL AND event.status = 'processing'
             AND event.updated_at <= ?`,
        )
        .all(staleBefore) as Array<{ id: number }>
      this.interruptRuns(
        runIds.map((run) => run.id),
        now,
        "process_exit",
      )
      const result = this.raw
        .query(
          "UPDATE events SET status = 'retryable', next_attempt_at = ?, last_error = 'triage interrupted by process exit', updated_at = ? WHERE status = 'processing' AND updated_at <= ?",
        )
        .run(now, now, staleBefore)
      return Number(result.changes)
    })()
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
             ON CONFLICT(external_id) DO UPDATE SET
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
          .query("SELECT id FROM entities WHERE external_id = ?")
          .get(item.entityId) as { id: number }
        const result = this.raw
          .query(
            `INSERT OR IGNORE INTO events(entity_id, source, revision_id, payload, occurred_at, observed_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)`,
          )
          .run(
            entity.id,
            source,
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
      const orphanedRuns = this.raw
        .query(
          "SELECT id FROM triage_runs WHERE event_id = ? AND ended_at IS NULL",
        )
        .all(row.id) as Array<{ id: number }>
      this.interruptRuns(
        orphanedRuns.map((run) => run.id),
        now,
        "superseded_attempt",
      )
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
        `SELECT ev.id, ev.source, en.external_id AS entityId, ev.revision_id AS revisionId,
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
        `SELECT ev.id, ev.source, en.external_id AS entityId, ev.revision_id AS revisionId,
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

  oldestOpenEventAt(): string | null {
    const row = this.raw
      .query(
        "SELECT MIN(observed_at) AS observedAt FROM events WHERE status IN ('pending', 'processing', 'retryable')",
      )
      .get() as { observedAt: string | null }
    return row.observedAt
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

  startTriageRun(
    eventId: number,
    attempt: number,
    now = new Date().toISOString(),
  ): number {
    const result = this.raw
      .query(
        `INSERT INTO triage_runs(
           event_id, attempt, started_at, last_activity_at,
           telemetry_version, telemetry_completeness
         ) VALUES (?, ?, ?, ?, 1, 'partial')`,
      )
      .run(eventId, attempt, now, now)
    return Number(result.lastInsertRowid)
  }

  setTriageRunMetadata(
    runId: number,
    metadata: {
      modelId?: string | null
      modelProvider?: string | null
      thinkingLevel?: string | null
      contextWindow?: number | null
      maxTokens?: number | null
    },
    now = new Date().toISOString(),
  ): void {
    this.raw
      .query(
        `UPDATE triage_runs SET model_id = ?, model_provider = ?, thinking_level = ?,
           context_window = ?, max_tokens = ?, last_activity_at = ? WHERE id = ?`,
      )
      .run(
        metadata.modelId ?? null,
        metadata.modelProvider ?? null,
        metadata.thinkingLevel ?? null,
        finiteNumber(metadata.contextWindow),
        finiteNumber(metadata.maxTokens),
        now,
        runId,
      )
  }

  recordTriageRunEvent(
    runId: number,
    event: TriageTelemetryEvent,
    now = new Date().toISOString(),
  ): void {
    this.raw.transaction(() => {
      const currentTurn = () =>
        this.raw
          .query(
            `SELECT id FROM triage_run_turns
             WHERE run_id = ? AND ended_at IS NULL ORDER BY ordinal DESC LIMIT 1`,
          )
          .get(runId) as { id: number } | null

      if (event.type === "turn_start") {
        this.raw
          .query(
            `UPDATE triage_run_turns SET ended_at = ?, stop_reason = 'aborted'
             WHERE run_id = ? AND ended_at IS NULL`,
          )
          .run(now, runId)
        this.raw
          .query(
            `INSERT INTO triage_run_turns(run_id, ordinal, started_at)
             SELECT ?, COALESCE(MAX(ordinal), 0) + 1, ?
             FROM triage_run_turns WHERE run_id = ?`,
          )
          .run(runId, now, runId)
        this.raw
          .query(
            `UPDATE triage_runs SET turn_count = (
               SELECT COUNT(*) FROM triage_run_turns WHERE run_id = ?
             ) WHERE id = ?`,
          )
          .run(runId, runId)
      } else if (event.type === "turn_end") {
        const turn = currentTurn()
        const usage = event.message?.usage
        if (turn) {
          this.raw
            .query(
              `UPDATE triage_run_turns SET ended_at = ?, stop_reason = ?,
                 input_tokens = ?, output_tokens = ?, cache_read_tokens = ?,
                 cache_write_tokens = ?, reasoning_tokens = ?, total_tokens = ?,
                 input_cost = ?, output_cost = ?, cache_read_cost = ?,
                 cache_write_cost = ?, total_cost = ?, context_tokens = ?,
                 context_window = ? WHERE id = ?`,
            )
            .run(
              now,
              safeStopReason(event.message?.stopReason),
              finiteNumber(usage?.input),
              finiteNumber(usage?.output),
              finiteNumber(usage?.cacheRead),
              finiteNumber(usage?.cacheWrite),
              finiteNumber(usage?.reasoning),
              finiteNumber(usage?.totalTokens),
              finiteNumber(usage?.cost?.input),
              finiteNumber(usage?.cost?.output),
              finiteNumber(usage?.cost?.cacheRead),
              finiteNumber(usage?.cost?.cacheWrite),
              finiteNumber(usage?.cost?.total),
              finiteNumber(event.contextUsage?.tokens),
              finiteNumber(event.contextUsage?.contextWindow),
              turn.id,
            )
          this.raw
            .query(
              `UPDATE triage_runs SET turn_count = (
                 SELECT COUNT(*) FROM triage_run_turns WHERE run_id = ?
               ), failure_category = COALESCE(?, failure_category),
               termination_reason = COALESCE(?, termination_reason)
               WHERE id = ?`,
            )
            .run(
              runId,
              event.message?.stopReason === "error"
                ? (safeErrorCategory(event.message.errorMessage) ?? "unknown")
                : null,
              event.message?.stopReason === "error" ? "model_error" : null,
              runId,
            )
        }
      } else if (event.type === "tool_execution_start" && event.toolName) {
        this.raw
          .query(
            `INSERT INTO triage_run_steps(
               run_id, step_key, turn_id, kind, label, started_at
             ) VALUES (?, lower(hex(randomblob(16))), ?, 'tool', ?, ?)`,
          )
          .run(
            runId,
            currentTurn()?.id ?? null,
            safeToolName(event.toolName),
            now,
          )
      } else if (event.type === "tool_execution_end" && event.toolName) {
        this.raw
          .query(
            `UPDATE triage_run_steps SET ended_at = ?, outcome = ?
             WHERE id = (SELECT id FROM triage_run_steps
               WHERE run_id = ? AND kind = 'tool' AND label = ? AND ended_at IS NULL
               ORDER BY started_at, id LIMIT 1)`,
          )
          .run(
            now,
            event.isError ? "failed" : "succeeded",
            runId,
            safeToolName(event.toolName),
          )
      } else if (
        event.type === "message_update" &&
        event.assistantMessageEvent?.type === "thinking_start"
      ) {
        this.raw
          .query(
            `INSERT INTO triage_run_steps(
               run_id, step_key, turn_id, kind, label, started_at
             ) VALUES (?, lower(hex(randomblob(16))), ?, 'thinking', 'thinking', ?)`,
          )
          .run(runId, currentTurn()?.id ?? null, now)
      } else if (
        event.type === "message_update" &&
        event.assistantMessageEvent?.type === "thinking_end"
      ) {
        this.raw
          .query(
            `UPDATE triage_run_steps SET ended_at = ?, outcome = 'succeeded'
             WHERE id = (SELECT id FROM triage_run_steps
               WHERE run_id = ? AND kind = 'thinking' AND ended_at IS NULL
               ORDER BY id DESC LIMIT 1)`,
          )
          .run(now, runId)
      } else if (event.type === "auto_retry_start") {
        const delayMs = Math.max(0, finiteNumber(event.delayMs) ?? 0)
        this.raw
          .query(
            `INSERT INTO triage_run_retries(
               run_id, turn_id, attempt, max_attempts, delay_ms, started_at,
               wait_ended_at, error_category
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
          )
          .run(
            runId,
            currentTurn()?.id ?? null,
            Math.max(1, finiteNumber(event.attempt) ?? 1),
            Math.max(1, finiteNumber(event.maxAttempts) ?? 1),
            delayMs,
            now,
            new Date(Date.parse(now) + delayMs).toISOString(),
            safeErrorCategory(event.errorMessage),
          )
        this.raw
          .query(
            "UPDATE triage_runs SET retry_count = retry_count + 1 WHERE id = ?",
          )
          .run(runId)
      } else if (event.type === "auto_retry_end") {
        this.raw
          .query(
            `UPDATE triage_run_retries SET ended_at = ?, outcome = ?,
               error_category = COALESCE(?, error_category)
             WHERE id = (SELECT id FROM triage_run_retries
               WHERE run_id = ? AND ended_at IS NULL ORDER BY id DESC LIMIT 1)`,
          )
          .run(
            now,
            event.success ? "succeeded" : "failed",
            safeErrorCategory(event.finalError),
            runId,
          )
      } else if (event.type === "compaction_start") {
        const turnId = currentTurn()?.id ?? null
        this.raw
          .query(
            `INSERT INTO triage_run_compactions(run_id, turn_id, reason, started_at)
             VALUES (?, ?, ?, ?)`,
          )
          .run(runId, turnId, event.reason ?? null, now)
        this.raw
          .query(
            `INSERT INTO triage_run_steps(
               run_id, step_key, turn_id, kind, label, started_at
             ) VALUES (?, lower(hex(randomblob(16))), ?, 'compaction', 'compaction', ?)`,
          )
          .run(runId, turnId, now)
        this.raw
          .query(
            "UPDATE triage_runs SET compaction_count = compaction_count + 1 WHERE id = ?",
          )
          .run(runId)
      } else if (event.type === "compaction_end") {
        const usage = event.result?.usage
        const outcome = event.aborted
          ? "aborted"
          : event.result
            ? "succeeded"
            : "failed"
        this.raw
          .query(
            `UPDATE triage_run_compactions SET ended_at = ?, outcome = ?,
               aborted = ?, will_retry = ?, tokens_before = ?,
               estimated_tokens_after = ?, input_tokens = ?, output_tokens = ?,
               cache_read_tokens = ?, cache_write_tokens = ?, reasoning_tokens = ?,
               total_tokens = ?, total_cost = ?
             WHERE id = (SELECT id FROM triage_run_compactions
               WHERE run_id = ? AND ended_at IS NULL ORDER BY id DESC LIMIT 1)`,
          )
          .run(
            now,
            outcome,
            event.aborted ? 1 : 0,
            event.willRetry ? 1 : 0,
            finiteNumber(event.result?.tokensBefore),
            finiteNumber(event.result?.estimatedTokensAfter),
            finiteNumber(usage?.input),
            finiteNumber(usage?.output),
            finiteNumber(usage?.cacheRead),
            finiteNumber(usage?.cacheWrite),
            finiteNumber(usage?.reasoning),
            finiteNumber(usage?.totalTokens),
            finiteNumber(usage?.cost?.total),
            runId,
          )
        this.raw
          .query(
            `UPDATE triage_run_steps SET ended_at = ?, outcome = ?
             WHERE id = (SELECT id FROM triage_run_steps
               WHERE run_id = ? AND kind = 'compaction' AND ended_at IS NULL
               ORDER BY id DESC LIMIT 1)`,
          )
          .run(now, outcome === "succeeded" ? "succeeded" : "failed", runId)
      } else {
        return
      }
      this.raw
        .query("UPDATE triage_runs SET last_activity_at = ? WHERE id = ?")
        .run(now, runId)
    })()
  }

  finishTriageRun(
    runId: number,
    outcome: "succeeded" | "failed",
    now = new Date().toISOString(),
    details: {
      terminationReason?: string | null
      failureCategory?: SafeErrorCategory | null
    } = {},
  ): void {
    this.raw.transaction(() => {
      const result = this.raw
        .query(
          `UPDATE triage_runs SET ended_at = ?, last_activity_at = ?, outcome = ?,
             termination_reason = ?, failure_category = COALESCE(?, failure_category),
             telemetry_completeness = 'complete'
           WHERE id = ? AND ended_at IS NULL`,
        )
        .run(
          now,
          now,
          outcome,
          details.terminationReason ??
            (outcome === "succeeded" ? "completed" : "failed"),
          details.failureCategory ?? null,
          runId,
        )
      if (Number(result.changes) === 0) return
      this.closeOpenTelemetry(runId, now)
    })()
  }

  private closeOpenTelemetry(runId: number, endedAt: string): void {
    this.raw
      .query(
        `UPDATE triage_run_steps SET ended_at = ?, outcome = 'interrupted'
         WHERE run_id = ? AND ended_at IS NULL`,
      )
      .run(endedAt, runId)
    this.raw
      .query(
        `UPDATE triage_run_turns SET ended_at = ?, stop_reason = 'aborted'
         WHERE run_id = ? AND ended_at IS NULL`,
      )
      .run(endedAt, runId)
    this.raw
      .query(
        `UPDATE triage_run_retries SET ended_at = ?, outcome = 'interrupted'
         WHERE run_id = ? AND ended_at IS NULL`,
      )
      .run(endedAt, runId)
    this.raw
      .query(
        `UPDATE triage_run_compactions SET ended_at = ?, outcome = 'interrupted'
         WHERE run_id = ? AND ended_at IS NULL`,
      )
      .run(endedAt, runId)
    this.raw
      .query(
        `UPDATE triage_runs SET
           turn_count = (SELECT COUNT(*) FROM triage_run_turns WHERE run_id = ?),
           retry_count = (SELECT COUNT(*) FROM triage_run_retries WHERE run_id = ?),
           compaction_count = (SELECT COUNT(*) FROM triage_run_compactions WHERE run_id = ?)
         WHERE id = ?`,
      )
      .run(runId, runId, runId, runId)
  }

  private interruptRuns(
    runIds: number[],
    endedAt: string,
    terminationReason: string,
  ): void {
    if (runIds.length === 0) return
    const placeholders = runIds.map(() => "?").join(",")
    this.raw
      .query(
        `UPDATE triage_runs SET ended_at = ?, last_activity_at = ?,
           outcome = 'interrupted', termination_reason = ?,
           failure_category = 'interrupted', telemetry_completeness = 'partial'
         WHERE id IN (${placeholders}) AND ended_at IS NULL`,
      )
      .run(endedAt, endedAt, terminationReason, ...runIds)
    for (const runId of runIds) this.closeOpenTelemetry(runId, endedAt)
  }

  listTriageRuns(limit = 50): TriageRunRecord[] {
    const runs = this.raw
      .query(
        `SELECT id, event_id AS eventId, attempt, started_at AS startedAt,
           ended_at AS endedAt, last_activity_at AS lastActivityAt, outcome,
           termination_reason AS terminationReason,
           failure_category AS failureCategory,
           model_id AS modelId, model_provider AS modelProvider,
           thinking_level AS thinkingLevel, context_window AS contextWindow,
           max_tokens AS maxTokens, telemetry_version AS telemetryVersion,
           telemetry_completeness AS telemetryCompleteness,
           turn_count AS turnCount, retry_count AS retryCount,
           compaction_count AS compactionCount
         FROM triage_runs ORDER BY started_at DESC, id DESC LIMIT ?`,
      )
      .all(limit) as Array<Omit<TriageRunRecord, "steps">>
    if (runs.length === 0) return []
    const steps = this.raw
      .query(
        `SELECT step.id, step.run_id AS runId, step.turn_id AS turnId,
           turn.ordinal AS turnOrdinal, step.kind, step.label,
           step.started_at AS startedAt, step.ended_at AS endedAt, step.outcome
         FROM triage_run_steps step
         LEFT JOIN triage_run_turns turn ON turn.id = step.turn_id
         WHERE step.run_id IN (${runs.map(() => "?").join(",")})
         ORDER BY step.started_at, step.id`,
      )
      .all(...runs.map((run) => run.id)) as Array<
      TriageRunStepRecord & { runId: number }
    >
    const byRun = new Map<number, TriageRunStepRecord[]>()
    for (const { runId, ...step } of steps) {
      const list = byRun.get(runId)
      if (list) list.push(step)
      else byRun.set(runId, [step])
    }
    return runs.map((run) => ({ ...run, steps: byRun.get(run.id) ?? [] }))
  }

  triageRun(id: number): TriageRunRecord | null {
    const run = this.raw
      .query(
        `SELECT id, event_id AS eventId, attempt, started_at AS startedAt,
           ended_at AS endedAt, last_activity_at AS lastActivityAt, outcome,
           termination_reason AS terminationReason,
           failure_category AS failureCategory,
           model_id AS modelId, model_provider AS modelProvider,
           thinking_level AS thinkingLevel, context_window AS contextWindow,
           max_tokens AS maxTokens, telemetry_version AS telemetryVersion,
           telemetry_completeness AS telemetryCompleteness,
           turn_count AS turnCount, retry_count AS retryCount,
           compaction_count AS compactionCount
         FROM triage_runs WHERE id = ?`,
      )
      .get(id) as Omit<TriageRunRecord, "steps"> | null
    if (!run) return null
    const steps = this.raw
      .query(
        `SELECT step.id, step.turn_id AS turnId, turn.ordinal AS turnOrdinal,
           step.kind, step.label, step.started_at AS startedAt,
           step.ended_at AS endedAt, step.outcome
         FROM triage_run_steps step
         LEFT JOIN triage_run_turns turn ON turn.id = step.turn_id
         WHERE step.run_id = ? ORDER BY step.started_at, step.id`,
      )
      .all(id) as TriageRunStepRecord[]
    return { ...run, steps }
  }

  triageRunsForEvent(eventId: number): TriageRunRecord[] {
    const ids = this.raw
      .query(
        "SELECT id FROM triage_runs WHERE event_id = ? ORDER BY attempt, id",
      )
      .all(eventId) as Array<{ id: number }>
    return ids.flatMap((row) => {
      const run = this.triageRun(row.id)
      return run ? [run] : []
    })
  }

  triageRunTurns(runId: number): TriageTurnRecord[] {
    return this.raw
      .query(
        `SELECT id, ordinal, started_at AS startedAt, ended_at AS endedAt,
           stop_reason AS stopReason, input_tokens AS inputTokens,
           output_tokens AS outputTokens, cache_read_tokens AS cacheReadTokens,
           cache_write_tokens AS cacheWriteTokens,
           reasoning_tokens AS reasoningTokens, total_tokens AS totalTokens,
           input_cost AS inputCost, output_cost AS outputCost,
           cache_read_cost AS cacheReadCost, cache_write_cost AS cacheWriteCost,
           total_cost AS totalCost, context_tokens AS contextTokens,
           context_window AS contextWindow
         FROM triage_run_turns WHERE run_id = ? ORDER BY ordinal`,
      )
      .all(runId) as TriageTurnRecord[]
  }

  triageRunRetries(runId: number): TriageRetryRecord[] {
    return this.raw
      .query(
        `SELECT id, turn_id AS turnId, attempt, max_attempts AS maxAttempts,
           delay_ms AS delayMs, started_at AS startedAt,
           wait_ended_at AS waitEndedAt, ended_at AS endedAt, outcome,
           error_category AS errorCategory
         FROM triage_run_retries WHERE run_id = ? ORDER BY started_at, id`,
      )
      .all(runId) as TriageRetryRecord[]
  }

  triageRunCompactions(runId: number): TriageCompactionRecord[] {
    const rows = this.raw
      .query(
        `SELECT id, turn_id AS turnId, reason, started_at AS startedAt,
           ended_at AS endedAt, outcome, aborted, will_retry AS willRetry,
           tokens_before AS tokensBefore,
           estimated_tokens_after AS estimatedTokensAfter,
           input_tokens AS inputTokens, output_tokens AS outputTokens,
           cache_read_tokens AS cacheReadTokens,
           cache_write_tokens AS cacheWriteTokens,
           reasoning_tokens AS reasoningTokens,
           total_tokens AS totalTokens, total_cost AS totalCost
         FROM triage_run_compactions WHERE run_id = ? ORDER BY started_at, id`,
      )
      .all(runId) as Array<
      Omit<TriageCompactionRecord, "aborted" | "willRetry"> & {
        aborted: number | null
        willRetry: number | null
      }
    >
    return rows.map((row) => ({
      ...row,
      aborted: row.aborted === null ? null : row.aborted === 1,
      willRetry: row.willRetry === null ? null : row.willRetry === 1,
    }))
  }

  triageRunEffects(runId: number): TriageEffectRecord[] {
    return this.raw
      .query(
        `SELECT id, type, value, recorded_at AS recordedAt
         FROM triage_run_effects WHERE run_id = ? ORDER BY recorded_at, id`,
      )
      .all(runId) as TriageEffectRecord[]
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
    this.raw.transaction(() => {
      this.raw
        .query(
          `UPDATE entities SET ${column} = ? WHERE id = (SELECT entity_id FROM events WHERE id = ?)`,
        )
        .run(value, id)
      this.raw
        .query(
          `INSERT OR IGNORE INTO triage_run_effects(run_id, type, value, recorded_at)
           SELECT run.id, ?, ?, ? FROM triage_runs run
           WHERE run.event_id = ? ORDER BY run.id DESC LIMIT 1`,
        )
        .run(
          column === "aven_ref" ? "aven_reference" : "investigation_handle",
          value,
          new Date().toISOString(),
          id,
        )
    })()
  }
}

function finiteNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null
}

function safeToolName(value: string): string {
  const normalized = value.replace(/[^a-zA-Z0-9_.:-]/g, "_").slice(0, 80)
  return normalized || "tool"
}

function safeStopReason(value: string | undefined): string | null {
  return value &&
    ["stop", "length", "toolUse", "error", "aborted"].includes(value)
    ? value
    : null
}

export function safeErrorCategory(
  error: string | null | undefined,
): SafeErrorCategory | null {
  if (!error) return null
  const value = error.toLowerCase()
  if (/turn limit|max turns/.test(value)) return "turn_limit"
  if (/context|token limit|too long/.test(value)) return "context_limit"
  if (/auth|credential|bearer|api.?key|unauthorized|forbidden/.test(value))
    return "authentication"
  if (/rate.?limit|too many requests|429/.test(value)) return "rate_limit"
  if (/timeout|timed out|wall-clock/.test(value)) return "timeout"
  if (/connection|socket|network|econn/.test(value)) return "connection"
  if (/not found|\b404\b/.test(value)) return "not_found"
  if (/model.*unavailable|overloaded/.test(value)) return "model_unavailable"
  if (/interrupt|process exit/.test(value)) return "interrupted"
  if (/abort|cancel/.test(value)) return "aborted"
  if (/tool/.test(value)) return "tool_failure"
  return "unknown"
}
