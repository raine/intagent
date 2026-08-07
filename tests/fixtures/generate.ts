import { Database } from "bun:sqlite"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { dashboardSnapshot } from "../../src/dashboard.ts"
import { databaseMigrations, IntakeDatabase } from "../../src/database.ts"
import { runDetail } from "../../src/run-detail.ts"

const fixtureRoot = import.meta.dir
const appliedAt = "2026-08-07T00:00:00.000Z"

type MasterRow = {
  type: string
  name: string
  tbl_name: string
  sql: string | null
}

type FixtureFiles = Map<string, string>

export function compatibilityFixtureFiles(): FixtureFiles {
  const files = new Map<string, string>()
  files.set(
    "config/valid.yaml",
    `version: 1
project_roots:
  - ~/code
state:
  database: ~/.config/intake/state/intake.sqlite
skills:
  directories:
    - ./skills
  approved_roots:
    - ./skills
    - ~/.claude/skills
sources:
  - name: fastmail
    command: intake-fastmail-source
    interval_seconds: 60
    timeout_seconds: 45
    item_limit: 100
    environment:
      - FASTMAIL_API_TOKEN
    options:
      mailbox_id: inbox
      include_headers:
        X-GitHub-Reason:
          - comment
          - subscribed
  - name: github
    command: intake-github-source
    environment:
      - GITHUB_TOKEN
    options: {}
triage:
  model: gpt-5.6-luna
  thinking_level: max
  max_turns: 50
  timeout_minutes: 30
  max_attempts: 3
  retry_base_seconds: 60
commands:
  path:
    - /opt/homebrew/bin
    - /usr/local/bin
    - /usr/bin
    - /bin
  timeout_seconds: 60
  max_output_bytes: 65536
  sensitive_patterns: []
  rules:
    - executable: aven
    - executable: workmux
    - executable: git
    - executable: rg
`,
  )
  files.set(
    "config/invalid-duplicate.yaml",
    `version: 1
project_roots:
  - ~/code
project_roots:
  - ~/projects
sources: []
`,
  )
  files.set(
    "protocol/bounds.json",
    json({
      protocolVersion: 1,
      request: {
        source: { minUtf16CodeUnits: 1 },
        now: { format: "UTC ISO 8601 date-time" },
        itemLimit: { integer: true, minimum: 1, maximum: 1000 },
        options: { default: {} },
      },
      response: { items: { maximum: 1000 } },
      item: {
        entityId: { minUtf16CodeUnits: 1, maxUtf16CodeUnits: 1024 },
        revisionId: { minUtf16CodeUnits: 1, maxUtf16CodeUnits: 1024 },
        kind: ["email", "github-issue", "github-pull-request", "generic"],
        title: { maxUtf16CodeUnits: 4096 },
        body: { maxUtf16CodeUnits: 1000000 },
        url: { format: "URL", optional: true },
        occurredAt: { format: "UTC ISO 8601 date-time" },
        metadata: { default: {} },
      },
      unknownFields: "rejected",
      jsonValues: ["string", "number", "boolean", "null", "array", "object"],
    }),
  )
  files.set(
    "protocol/poll-request.json",
    json({
      protocolVersion: 1,
      source: "github",
      checkpoint: {
        repositories: {
          "example/intake": {
            updatedAt: "2026-08-07T09:00:00.000Z",
            id: 42,
          },
        },
      },
      now: "2026-08-07T10:00:00.000Z",
      itemLimit: 1000,
      options: {
        repositories: ["example/intake"],
        includePullRequests: true,
        nested: { threshold: 1.5, optional: null },
      },
    }),
  )
  files.set(
    "protocol/poll-response.json",
    json({
      protocolVersion: 1,
      checkpoint: {
        repositories: {
          "example/intake": {
            updatedAt: "2026-08-07T09:59:59.999Z",
            id: 43,
          },
        },
      },
      items: [
        {
          entityId: "github:example/intake#42",
          revisionId: "issue-42-updated-2026-08-07T09:59:59.999Z",
          kind: "github-issue",
          title: "Investigate delayed notifications",
          body: "A deterministic compatibility fixture.",
          url: "https://github.example/example/intake/issues/42",
          occurredAt: "2026-08-07T09:59:59.999Z",
          metadata: {
            repository: "example/intake",
            labels: ["bug", "triage"],
            notificationReason: "comment",
          },
        },
        {
          entityId: "mail:thread-7",
          revisionId: "message-9",
          kind: "email",
          title: "Follow up on the release",
          body: "The source protocol preserves camelCase fields.",
          occurredAt: "2026-08-07T09:58:00.000Z",
          metadata: { threadId: "thread-7", attachmentCount: 0 },
        },
      ],
    }),
  )

  const database = new Database(":memory:", { strict: true })
  database.exec(
    "PRAGMA foreign_keys = ON; CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
  )
  files.set("database/schema-v0.sql", dumpDatabase(database))
  const expectations: Record<string, unknown> = {
    v0: schemaExpectation(database),
  }

  for (const [index, migration] of databaseMigrations.entries()) {
    const version = index + 1
    database.transaction(() => {
      database.exec(migration)
      database
        .query(
          "INSERT INTO schema_migrations(version, applied_at) VALUES (?, ?)",
        )
        .run(version, appliedAt)
    })()
    seedVersion(database, version)
    files.set(`database/schema-v${version}.sql`, dumpDatabase(database))
    expectations[`v${version}`] = schemaExpectation(database)
  }
  database.close()
  files.set("database/schema-expectations.json", json(expectations))

  const temporaryDirectory = mkdtempSync(join(tmpdir(), "intake-fixtures-"))
  const databasePath = join(temporaryDirectory, "schema-v7.sqlite")
  try {
    const fixtureDatabase = new Database(databasePath, { create: true })
    fixtureDatabase.exec(files.get("database/schema-v7.sql") ?? "")
    fixtureDatabase.close()
    const intakeDatabase = new IntakeDatabase(databasePath)
    try {
      files.set(
        "dashboard/snapshot.json",
        json(
          dashboardSnapshot(
            intakeDatabase,
            new Date("2026-08-07T10:05:00.000Z"),
          ),
        ),
      )
      const detail = runDetail(intakeDatabase, 1, {
        now: new Date("2026-08-07T10:05:00.000Z"),
        maxTurns: 50,
        wallTimeoutMs: 1_800_000,
      })
      if (!detail) throw new Error("Fixture run 1 is missing")
      files.set("dashboard/run-detail.json", json(detail))
    } finally {
      intakeDatabase.close()
    }
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true })
  }

  return files
}

export function writeCompatibilityFixtures(): void {
  for (const [relativePath, content] of compatibilityFixtureFiles()) {
    const path = join(fixtureRoot, relativePath)
    mkdirSync(dirname(path), { recursive: true })
    writeFileSync(path, content)
  }
}

function seedVersion(database: Database, version: number): void {
  if (version === 1) seedVersionOne(database)
  if (version === 2) seedVersionTwo(database)
  if (version === 3) seedVersionThree(database)
  if (version === 4) seedVersionFour(database)
  if (version === 5) {
    const command = database
      .query(
        "SELECT command, output_summary AS outputSummary FROM command_events WHERE id = 1",
      )
      .get() as { command: string; outputSummary: string }
    if (
      command.command !== "tool=legacy" ||
      command.outputSummary !== "unavailable"
    )
      throw new Error("Migration 5 did not redact legacy command telemetry")
  }
  if (version === 6)
    database
      .query("UPDATE triage_run_steps SET summary = ? WHERE id = 1")
      .run("rg -n compatibility tests")
  if (version === 7)
    database.exec(`
      INSERT INTO triage_run_prompts(id, run_id, role, content, recorded_at) VALUES
        (1, 1, 'system', 'Triage intake events using restricted tools.', '2026-08-07T10:00:00.000Z'),
        (2, 1, 'user', 'Triage the fixture event.', '2026-08-07T10:00:00.000Z');
    `)
}

function seedVersionOne(database: Database): void {
  database.exec(`
    INSERT INTO source_state(source, checkpoint, last_success_at, last_error, updated_at) VALUES
      ('fastmail', '{"state":"mail-2"}', '2026-08-07T09:59:00.000Z', NULL, '2026-08-07T09:59:00.000Z'),
      ('github', '{"cursor":"issue-42"}', '2026-08-07T10:00:00.000Z', NULL, '2026-08-07T10:00:00.000Z');
    INSERT INTO entities(id, source, external_id, kind, title, aven_ref,
      investigation_handle, last_event_at, handling_status, operational_metadata) VALUES
      (1, 'fastmail', 'github:example/intake#42', 'email', 'Older notification', NULL,
       NULL, '2026-08-07T09:57:00.000Z', 'pending', '{"url":null,"kind":"email"}'),
      (2, 'github', 'github:example/intake#42', 'github-issue',
       'Investigate delayed notifications', 'APP-7KQ9', 'inspect-notifications',
       '2026-08-07T09:59:59.999Z', 'retryable',
       '{"url":"https://github.example/example/intake/issues/42?token=removed#activity","kind":"github-issue"}');
    INSERT INTO events(id, entity_id, revision_id, payload, occurred_at, observed_at,
      status, attempt_count, next_attempt_at, last_error, updated_at) VALUES
      (1, 1, 'shared-revision', '{"private":"older"}', '2026-08-07T09:57:00.000Z',
       '2026-08-07T09:58:00.000Z', 'pending', 0, NULL, NULL, '2026-08-07T09:58:00.000Z'),
      (2, 2, 'shared-revision', '{"private":"duplicate"}', '2026-08-07T09:58:00.000Z',
       '2026-08-07T09:59:00.000Z', 'pending', 0, NULL, NULL, '2026-08-07T09:59:00.000Z'),
      (3, 2, 'issue-update-2', '{"private":"retained retry payload"}',
       '2026-08-07T09:59:59.999Z', '2026-08-07T10:00:00.000Z', 'retryable', 2,
       '2026-08-07T10:10:00.000Z', 'rate limit token=private', '2026-08-07T10:00:10.000Z');
    INSERT INTO command_events(id, event_id, command, exit_code, output_summary, created_at)
      VALUES (1, 2, 'aven add private title', 0, 'private command output', '2026-08-07T10:00:02.000Z');
  `)
}

function seedVersionTwo(database: Database): void {
  const duplicateCount = database
    .query("SELECT COUNT(*) AS count FROM entities WHERE external_id = ?")
    .get("github:example/intake#42") as { count: number }
  if (duplicateCount.count !== 1)
    throw new Error("Migration 2 did not merge global entity identity")
  database.exec(`
    INSERT INTO entities(id, source, external_id, kind, title, aven_ref,
      investigation_handle, last_event_at, handling_status, operational_metadata) VALUES
      (3, 'fastmail', 'mail:thread-7', 'email', 'Follow up on the release', NULL,
       NULL, '2026-08-07T09:58:00.000Z', 'succeeded',
       '{"url":null,"kind":"email"}');
    INSERT INTO events(id, entity_id, source, revision_id, payload, occurred_at,
      observed_at, status, attempt_count, next_attempt_at, last_error, updated_at) VALUES
      (4, 3, 'fastmail', 'message-9', NULL, '2026-08-07T09:58:00.000Z',
       '2026-08-07T09:58:30.000Z', 'succeeded', 1, NULL, NULL,
       '2026-08-07T10:03:00.000Z');
  `)
}

function seedVersionThree(database: Database): void {
  database.exec(`
    INSERT INTO triage_runs(id, event_id, attempt, started_at, ended_at,
      last_activity_at, outcome, model_id, model_provider, thinking_level,
      turn_count, retry_count, compaction_count) VALUES
      (1, 4, 1, '2026-08-07T10:00:00.000Z', '2026-08-07T10:03:00.000Z',
       '2026-08-07T10:03:00.000Z', 'succeeded', 'gpt-5.6-luna', 'openai-codex',
       'max', 1, 1, 1),
      (2, 3, 2, '2026-08-07T10:00:20.000Z', NULL,
       '2026-08-07T10:00:25.000Z', NULL, 'gpt-5.6-luna', 'openai-codex',
       'high', 1, 0, 0);
    INSERT INTO triage_run_steps(id, run_id, step_key, kind, label, started_at,
      ended_at, outcome) VALUES
      (1, 1, 'tool-1', 'tool', 'bash', '2026-08-07T10:00:30.000Z',
       '2026-08-07T10:00:40.000Z', 'succeeded'),
      (2, 2, 'thinking-1', 'thinking', 'thinking', '2026-08-07T10:00:21.000Z',
       NULL, NULL);
  `)
}

function seedVersionFour(database: Database): void {
  database.exec(`
    UPDATE triage_run_steps SET step_key = 'fixture-step-' || id;
    UPDATE triage_runs SET termination_reason = 'completed', context_window = 200000,
      max_tokens = 16000, telemetry_version = 1, telemetry_completeness = 'complete'
      WHERE id = 1;
    INSERT INTO triage_run_turns(id, run_id, ordinal, started_at, ended_at,
      stop_reason, input_tokens, output_tokens, cache_read_tokens,
      cache_write_tokens, reasoning_tokens, total_tokens, input_cost, output_cost,
      cache_read_cost, cache_write_cost, total_cost, context_tokens, context_window) VALUES
      (1, 1, 1, '2026-08-07T10:00:10.000Z', '2026-08-07T10:01:00.000Z',
       'toolUse', 800, 120, 400, 20, 80, 1420, 0.008, 0.006, 0.002, 0.001,
       0.017, 45000, 200000),
      (2, 1, 2, '2026-08-07T10:02:00.000Z', '2026-08-07T10:02:40.000Z',
       'stop', 500, 60, 200, 0, 30, 790, 0.005, 0.003, 0.001, 0,
       0.009, 80000, 200000);
    UPDATE triage_run_steps SET turn_id = 1 WHERE id = 1;
    INSERT INTO triage_run_steps(id, run_id, step_key, turn_id, kind, label,
      started_at, ended_at, outcome) VALUES
      (3, 1, 'fixture-step-3', 1, 'thinking', 'thinking',
       '2026-08-07T10:00:10.000Z', '2026-08-07T10:00:25.000Z', 'succeeded'),
      (4, 1, 'fixture-step-4', 1, 'compaction', 'compaction',
       '2026-08-07T10:01:20.000Z', '2026-08-07T10:01:50.000Z', 'succeeded');
    INSERT INTO triage_run_retries(id, run_id, turn_id, attempt, max_attempts,
      delay_ms, started_at, wait_ended_at, ended_at, outcome, error_category) VALUES
      (1, 1, 1, 1, 3, 1000, '2026-08-07T10:01:00.000Z',
       '2026-08-07T10:01:01.000Z', '2026-08-07T10:01:01.000Z', 'succeeded',
       'rate_limit');
    INSERT INTO triage_run_compactions(id, run_id, turn_id, reason, started_at,
      ended_at, outcome, aborted, will_retry, tokens_before,
      estimated_tokens_after, input_tokens, output_tokens, cache_read_tokens,
      cache_write_tokens, reasoning_tokens, total_tokens, total_cost) VALUES
      (1, 1, 1, 'threshold', '2026-08-07T10:01:20.000Z',
       '2026-08-07T10:01:50.000Z', 'succeeded', 0, 0, 180000, 80000,
       900, 100, 300, 0, 50, 1350, 0.01);
    INSERT INTO triage_run_effects(id, run_id, type, value, recorded_at) VALUES
      (1, 1, 'aven_reference', 'APP-7KQ9', '2026-08-07T10:02:50.000Z'),
      (2, 1, 'investigation_handle', 'inspect-notifications',
       '2026-08-07T10:02:55.000Z');
    INSERT INTO triage_runs(id, event_id, attempt, started_at, ended_at,
      last_activity_at, outcome, model_id, model_provider, thinking_level,
      turn_count, retry_count, compaction_count, termination_reason,
      failure_category, context_window, max_tokens, telemetry_version,
      telemetry_completeness) VALUES
      (3, 3, 3, '2026-08-07T10:04:00.000Z', '2026-08-07T10:04:30.000Z',
       '2026-08-07T10:04:30.000Z', 'failed', 'gpt-5.6-luna', 'openai-codex',
       'high', 1, 0, 0, 'model_error', 'model_unavailable', 200000, 16000, 1,
       'complete');
  `)
}

function dumpDatabase(database: Database): string {
  const objects = database
    .query(
      `SELECT type, name, tbl_name, sql FROM sqlite_master
       WHERE name NOT LIKE 'sqlite_%' AND sql IS NOT NULL
       ORDER BY CASE type WHEN 'table' THEN 0 ELSE 1 END, name`,
    )
    .all() as MasterRow[]
  const tables = objects.filter((object) => object.type === "table")
  const lines = ["PRAGMA foreign_keys = OFF;", "BEGIN TRANSACTION;"]
  for (const object of objects) lines.push(`${object.sql};`)
  for (const table of tables) {
    const rows = database
      .query(`SELECT * FROM ${identifier(table.name)} ORDER BY rowid`)
      .all() as Array<Record<string, unknown>>
    for (const row of rows) {
      const columns = Object.keys(row).map(identifier).join(", ")
      const values = Object.values(row).map(sqlValue).join(", ")
      lines.push(
        `INSERT INTO ${identifier(table.name)} (${columns}) VALUES (${values});`,
      )
    }
  }
  lines.push("COMMIT;", "PRAGMA foreign_keys = ON;", "")
  return lines.join("\n")
}

function schemaExpectation(database: Database): unknown {
  const sqliteMaster = database
    .query(
      `SELECT type, name, tbl_name AS tableName, sql FROM sqlite_master
       WHERE name NOT LIKE 'sqlite_%'
       ORDER BY type, name`,
    )
    .all() as Array<{
    type: string
    name: string
    tableName: string
    sql: string | null
  }>
  const tableNames = sqliteMaster
    .filter((object) => object.type === "table")
    .map((object) => object.name)
  const tableInfo = Object.fromEntries(
    tableNames.map((table) => [
      table,
      database.query(`PRAGMA table_info(${identifier(table)})`).all(),
    ]),
  )
  const indexes = sqliteMaster
    .filter((object) => object.type === "index")
    .map((index) => ({
      ...index,
      columns: database
        .query(`PRAGMA index_info(${identifier(index.name)})`)
        .all(),
    }))
  const migrations = database
    .query(
      "SELECT version, applied_at AS appliedAt FROM schema_migrations ORDER BY version",
    )
    .all()
  return { sqliteMaster, migrations, tableInfo, indexes }
}

function identifier(value: string): string {
  return `"${value.replaceAll('"', '""')}"`
}

function sqlValue(value: unknown): string {
  if (value === null) return "NULL"
  if (typeof value === "number") return String(value)
  if (typeof value === "string") return `'${value.replaceAll("'", "''")}'`
  if (value instanceof Uint8Array)
    return `X'${Buffer.from(value).toString("hex").toUpperCase()}'`
  throw new Error(`Unsupported SQLite fixture value: ${String(value)}`)
}

function json(value: unknown): string {
  return `${JSON.stringify(value, null, 2)}\n`
}

if (import.meta.main) writeCompatibilityFixtures()
