import { Database } from "bun:sqlite"
import { afterEach, describe, expect, test } from "bun:test"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { IntakeDatabase } from "../src/database.ts"
import type { IntakeItem } from "../src/protocol.ts"

const databases: IntakeDatabase[] = []
const temporaryDirectories: string[] = []
afterEach(() => {
  for (const database of databases.splice(0)) database.close()
  for (const directory of temporaryDirectories.splice(0))
    rmSync(directory, { recursive: true, force: true })
})

function item(revisionId = "message-1"): IntakeItem {
  return {
    entityId: "mail:thread-1",
    revisionId,
    kind: "email",
    title: "Needs attention",
    body: "Complete content",
    occurredAt: "2026-08-03T10:00:00.000Z",
    metadata: { threadId: "thread-1" },
  }
}

describe("intake persistence", () => {
  test("commits checkpoints and events atomically without duplicates", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    expect(
      database.sourceSucceeded(
        "mail",
        { state: "a" },
        [item()],
        "2026-08-03T10:01:00.000Z",
      ),
    ).toBe(1)
    expect(
      database.sourceSucceeded(
        "mail",
        { state: "b" },
        [item()],
        "2026-08-03T10:02:00.000Z",
      ),
    ).toBe(0)
    expect(database.sourceCheckpoint("mail")).toEqual({ state: "b" })
    expect(database.listEvents()).toHaveLength(1)
  })

  test("deduplicates canonical events across source names", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)

    expect(
      database.sourceSucceeded(
        "mail-old",
        { state: "a" },
        [item()],
        "2026-08-03T10:01:00.000Z",
      ),
    ).toBe(1)
    expect(
      database.sourceSucceeded(
        "mail-new",
        { state: "b" },
        [item()],
        "2026-08-03T10:02:00.000Z",
      ),
    ).toBe(0)
    expect(
      database.sourceSucceeded(
        "mail-new",
        { state: "c" },
        [item("message-2")],
        "2026-08-03T10:03:00.000Z",
      ),
    ).toBe(1)

    expect(database.sourceCheckpoint("mail-old")).toEqual({ state: "a" })
    expect(database.sourceCheckpoint("mail-new")).toEqual({ state: "c" })
    expect(database.listEvents()).toMatchObject([
      { source: "mail-new", revisionId: "message-2" },
      { source: "mail-old", revisionId: "message-1" },
    ])
    expect(
      database.raw.query("SELECT COUNT(*) AS count FROM entities").get(),
    ).toEqual({ count: 1 })
  })

  test("retains retry content and removes content after success", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded("mail", {}, [item()], "2026-08-03T10:01:00.000Z")
    const claimed = database.claimNext()
    expect(claimed?.status).toBe("processing")
    database.fail(claimed?.id ?? 0, "model unavailable", 3, 1)
    const failed = database.event(claimed?.id ?? 0)
    expect(failed?.status).toBe("retryable")
    expect(failed?.payload).toContain("Complete content")
    expect(database.retry(failed?.id ?? 0)).toBe(true)
    const retried = database.claimNext()
    database.succeed(retried?.id ?? 0)
    expect(database.event(retried?.id ?? 0)?.payload).toBeNull()
    expect(database.retry(retried?.id ?? 0)).toBe(false)
  })

  test("serializes events for one entity", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded(
      "mail",
      {},
      [item(), item("message-2")],
      "2026-08-03T10:01:00.000Z",
    )
    const first = database.claimNext()
    expect(first).not.toBeNull()
    expect(database.claimNext()).toBeNull()
    database.succeed(first?.id ?? 0)
    expect(database.claimNext()?.revisionId).toBe("message-2")
  })

  test("keeps later entity events behind a delayed retry", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded(
      "mail",
      {},
      [item(), item("message-2")],
      "2026-08-03T10:01:00.000Z",
    )
    const first = database.claimNext()
    database.fail(first?.id ?? 0, "model unavailable", 3, 3_600)
    const nextAttemptAt = database.event(first?.id ?? 0)?.nextAttemptAt
    expect(nextAttemptAt).not.toBeNull()
    expect(
      database.claimNext(
        new Date(Date.parse(nextAttemptAt ?? "") - 1).toISOString(),
      ),
    ).toBeNull()
    expect(database.claimNext(nextAttemptAt ?? undefined)?.revisionId).toBe(
      "message-1",
    )
  })

  test("merges canonical entities when migrating source-scoped data", () => {
    const directory = mkdtempSync(join(tmpdir(), "intake-database-"))
    temporaryDirectories.push(directory)
    const path = join(directory, "intake.sqlite")
    const legacy = new Database(path)
    legacy.exec(`
      PRAGMA foreign_keys = ON;
      CREATE TABLE schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL
      );
      INSERT INTO schema_migrations VALUES (1, '2026-08-03T00:00:00Z');
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
      CREATE TABLE command_events (
        id INTEGER PRIMARY KEY,
        event_id INTEGER NOT NULL REFERENCES events(id),
        command TEXT NOT NULL,
        exit_code INTEGER NOT NULL,
        output_summary TEXT NOT NULL,
        created_at TEXT NOT NULL
      );
      INSERT INTO entities VALUES
        (1, 'mail-old', 'mail:thread-1', 'email', 'Old title', 'APP-OLD', NULL,
         '2026-08-03T10:00:00Z', 'succeeded', '{}'),
        (2, 'mail-new', 'mail:thread-1', 'email', 'Current title', 'APP-CURRENT',
         'investigate-thread', '2026-08-03T11:00:00Z', 'succeeded', '{}');
      INSERT INTO events VALUES
        (1, 1, 'message-1', NULL, '2026-08-03T10:00:00Z',
         '2026-08-03T10:01:00Z', 'succeeded', 1, NULL, NULL,
         '2026-08-03T10:02:00Z'),
        (2, 2, 'message-1', NULL, '2026-08-03T10:00:00Z',
         '2026-08-03T11:01:00Z', 'succeeded', 1, NULL, NULL,
         '2026-08-03T11:02:00Z');
      INSERT INTO command_events VALUES
        (1, 1, 'aven search', 0, 'first', '2026-08-03T10:01:00Z'),
        (2, 2, 'aven note', 0, 'duplicate', '2026-08-03T11:01:00Z');
    `)
    legacy.close()

    const database = new IntakeDatabase(path)
    databases.push(database)

    expect(database.listEvents()).toMatchObject([
      {
        id: 1,
        source: "mail-old",
        entityId: "mail:thread-1",
        revisionId: "message-1",
        avenRef: "APP-CURRENT",
        investigationHandle: "investigate-thread",
      },
    ])
    expect(
      database.raw
        .query("SELECT MAX(version) AS version FROM schema_migrations")
        .get(),
    ).toEqual({ version: 4 })
    expect(
      database.raw
        .query("SELECT name FROM pragma_table_info('triage_runs')")
        .all()
        .map((row) => (row as { name: string }).name),
    ).toEqual(
      expect.arrayContaining([
        "termination_reason",
        "failure_category",
        "telemetry_version",
        "telemetry_completeness",
      ]),
    )
    expect(
      database.raw.query("SELECT COUNT(*) AS count FROM entities").get(),
    ).toEqual({ count: 1 })
    expect(
      database.raw
        .query("SELECT event_id AS eventId FROM command_events ORDER BY id")
        .all(),
    ).toEqual([{ eventId: 1 }, { eventId: 1 }])
  })

  test("keeps active triage processing across concurrent database opens", () => {
    const directory = mkdtempSync(join(tmpdir(), "intake-database-"))
    temporaryDirectories.push(directory)
    const path = join(directory, "intake.sqlite")
    const watcher = new IntakeDatabase(path)
    const observer = new IntakeDatabase(path)
    databases.push(watcher, observer)

    watcher.sourceSucceeded("mail", {}, [item()], "2026-08-03T10:01:00.000Z")
    const event = watcher.claimNext("2026-08-03T10:02:00.000Z")
    const runId = watcher.startTriageRun(
      event?.id ?? 0,
      1,
      "2026-08-03T10:02:01.000Z",
    )
    watcher.recordTriageRunEvent(
      runId,
      { type: "turn_start" },
      "2026-08-03T10:02:02.000Z",
    )
    watcher.recordTriageRunEvent(
      runId,
      { type: "tool_execution_start", toolName: "read" },
      "2026-08-03T10:02:03.000Z",
    )

    expect(observer.event(event?.id ?? 0)?.status).toBe("processing")
    expect(
      observer.recoverInterrupted(
        "2026-08-03T10:01:59.999Z",
        "2026-08-03T10:03:00.000Z",
      ),
    ).toBe(0)
    expect(observer.event(event?.id ?? 0)?.status).toBe("processing")
    expect(
      observer.recoverInterrupted(
        "2026-08-03T10:02:00.000Z",
        "2026-08-03T10:33:00.000Z",
      ),
    ).toBe(1)
    expect(observer.event(event?.id ?? 0)).toMatchObject({
      status: "retryable",
      lastError: "triage interrupted by process exit",
    })
    expect(observer.triageRun(runId)).toMatchObject({
      endedAt: "2026-08-03T10:33:00.000Z",
      outcome: "interrupted",
      terminationReason: "process_exit",
      telemetryCompleteness: "partial",
      steps: [
        expect.objectContaining({
          endedAt: "2026-08-03T10:33:00.000Z",
          outcome: "interrupted",
        }),
      ],
    })
    expect(observer.triageRunTurns(runId)[0]).toMatchObject({
      endedAt: "2026-08-03T10:33:00.000Z",
      stopReason: "aborted",
    })
  })

  test("closes an orphaned run before reclaiming its event", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded("mail", {}, [item()], "2026-08-03T10:01:00.000Z")
    const event = database.claimNext("2026-08-03T10:02:00.000Z")!
    const runId = database.startTriageRun(
      event.id,
      1,
      "2026-08-03T10:02:01.000Z",
    )
    database.raw
      .query("UPDATE events SET status = 'retryable' WHERE id = ?")
      .run(event.id)

    expect(database.claimNext("2026-08-03T10:03:00.000Z")?.id).toBe(event.id)
    expect(database.triageRun(runId)).toMatchObject({
      endedAt: "2026-08-03T10:03:00.000Z",
      outcome: "interrupted",
      terminationReason: "superseded_attempt",
    })
  })

  test("migrates legacy terminal runs and open spans to bounded telemetry", () => {
    const directory = mkdtempSync(join(tmpdir(), "intake-database-"))
    temporaryDirectories.push(directory)
    const path = join(directory, "intake.sqlite")
    const legacy = new Database(path)
    legacy.exec(`
      CREATE TABLE schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL
      );
      INSERT INTO schema_migrations VALUES
        (1, '2026-08-03T00:00:00Z'),
        (2, '2026-08-03T00:00:00Z'),
        (3, '2026-08-03T00:00:00Z');
      CREATE TABLE events (id INTEGER PRIMARY KEY, status TEXT NOT NULL);
      INSERT INTO events VALUES (1, 'failed');
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
      INSERT INTO triage_runs VALUES (
        1, 1, 1, '2026-08-03T10:00:00Z', NULL,
        '2026-08-03T10:05:00Z', 'failed', NULL, NULL, NULL, 1, 0, 0
      );
      INSERT INTO triage_run_steps VALUES (
        1, 1, 'legacy-call-id', 'tool', 'read',
        '2026-08-03T10:01:00Z', NULL, NULL
      );
    `)
    legacy.close()

    const database = new IntakeDatabase(path)
    databases.push(database)
    expect(
      database.raw
        .query(
          `SELECT ended_at AS endedAt, outcome, telemetry_completeness AS completeness
           FROM triage_runs WHERE id = 1`,
        )
        .get(),
    ).toEqual({
      endedAt: "2026-08-03T10:05:00Z",
      outcome: "failed",
      completeness: "legacy",
    })
    expect(
      database.raw
        .query(
          "SELECT ended_at AS endedAt, outcome, step_key AS stepKey FROM triage_run_steps WHERE id = 1",
        )
        .get(),
    ).toEqual({
      endedAt: "2026-08-03T10:05:00Z",
      outcome: "interrupted",
      stepKey: expect.not.stringContaining("legacy-call-id"),
    })
  })

  test("derives durable Aven and investigation references", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded("mail", {}, [item()], "2026-08-03T10:01:00.000Z")
    const event = database.claimNext()
    database.recordCommand(
      event?.id ?? 0,
      "rg -n APP-EXAMPLE skills",
      0,
      "Created APP-EXAMPLE\nWorktree: /tmp/project__worktrees/example",
    )
    expect(database.event(event?.id ?? 0)).toMatchObject({
      avenRef: null,
      investigationHandle: null,
    })
    database.recordCommand(
      event?.id ?? 0,
      'aven add title --description "Complete content"',
      0,
      "Created APP-7KQ9 with Complete content",
    )
    database.recordCommand(
      event?.id ?? 0,
      "workmux add",
      0,
      "  Worktree: /tmp/project__worktrees/inspect-login",
    )
    expect(database.event(event?.id ?? 0)).toMatchObject({
      avenRef: "APP-7KQ9",
      investigationHandle: "inspect-login",
    })
    const commandRows = database.raw
      .query(
        "SELECT command, output_summary AS outputSummary FROM command_events",
      )
      .all() as Array<{ command: string; outputSummary: string }>
    expect(JSON.stringify(commandRows)).not.toContain("Complete content")
  })
})
