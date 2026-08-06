import { afterEach, describe, expect, test } from "bun:test"
import { createDashboardHandler } from "../src/dashboard.ts"
import { IntakeDatabase } from "../src/database.ts"
import { runDetail } from "../src/run-detail.ts"
import type { IntakeItem } from "../src/protocol.ts"

const databases: IntakeDatabase[] = []
afterEach(() => {
  for (const database of databases.splice(0)) database.close()
})

const intakeItem: IntakeItem = {
  entityId: "github:example/private#9",
  revisionId: "revision-private",
  kind: "github-issue",
  title: "Operational incident",
  body: "private intake body",
  url: "https://github.example/example/private/issues/9",
  occurredAt: "2026-08-05T09:59:00.000Z",
  metadata: { repository: "example/private", secret: "payload-secret" },
}

describe("run detail telemetry", () => {
  test("returns turn-centric attempts, safe telemetry, effects, and partitioned metrics", async () => {
    const database = createDatabase()
    database.sourceSucceeded(
      "github",
      {},
      [intakeItem],
      "2026-08-05T09:59:30.000Z",
    )
    const event = database.claimNext("2026-08-05T10:00:00.000Z")!
    const firstRun = database.startTriageRun(
      event.id,
      1,
      "2026-08-05T10:00:00.000Z",
    )
    database.finishTriageRun(firstRun, "failed", "2026-08-05T10:00:00.500Z", {
      terminationReason: "model_error",
      failureCategory: "rate_limit",
    })
    database.fail(event.id, "provider rate limit with raw-secret", 3, 0)
    database.raw
      .query("UPDATE events SET next_attempt_at = ? WHERE id = ?")
      .run("2026-08-05T10:00:00.500Z", event.id)

    const secondEvent = database.claimNext("2026-08-05T10:00:01.000Z")!
    const runId = database.startTriageRun(
      secondEvent.id,
      2,
      "2026-08-05T10:00:00.000Z",
    )
    database.setTriageRunMetadata(runId, {
      modelId: "gpt-test",
      modelProvider: "openai-codex",
      thinkingLevel: "medium",
      contextWindow: 100,
      maxTokens: 20,
    })
    database.recordTriageRunPrompt(
      runId,
      "system",
      "Use the restricted triage tools.",
      "2026-08-05T10:00:00.000Z",
    )
    database.recordTriageRunPrompt(
      runId,
      "user",
      "Triage this event payload.",
      "2026-08-05T10:00:00.001Z",
    )
    record(database, runId, "2026-08-05T10:00:01.000Z", {
      type: "turn_start",
    })
    record(database, runId, "2026-08-05T10:00:01.000Z", {
      type: "message_update",
      assistantMessageEvent: { type: "thinking_start" },
    })
    record(database, runId, "2026-08-05T10:00:03.000Z", {
      type: "message_update",
      assistantMessageEvent: {
        type: "thinking_end",
        content: "Checked the event context.",
      },
    })
    record(database, runId, "2026-08-05T10:00:03.000Z", {
      type: "tool_execution_start",
      toolCallId: "private-call-one",
      toolName: "bash",
      args: { command: "cat /private/path" },
    })
    record(database, runId, "2026-08-05T10:00:04.000Z", {
      type: "tool_execution_start",
      toolCallId: "private-call-two",
      toolName: "read",
      args: { path: "/private/path" },
    })
    record(database, runId, "2026-08-05T10:00:05.000Z", {
      type: "tool_execution_end",
      toolCallId: "private-call-one",
      toolName: "bash",
      result: { content: "private tool output" },
      isError: false,
    })
    record(database, runId, "2026-08-05T10:00:06.000Z", {
      type: "tool_execution_end",
      toolCallId: "private-call-two",
      toolName: "read",
      result: { error: "private raw tool error" },
      isError: true,
    })
    record(database, runId, "2026-08-05T10:00:06.000Z", {
      type: "auto_retry_start",
      attempt: 1,
      maxAttempts: 3,
      delayMs: 1_000,
      errorMessage: "429 with raw-secret",
    })
    record(database, runId, "2026-08-05T10:00:07.000Z", {
      type: "auto_retry_end",
      attempt: 1,
      success: true,
    })
    record(database, runId, "2026-08-05T10:00:07.000Z", {
      type: "compaction_start",
      reason: "threshold",
    })
    record(database, runId, "2026-08-05T10:00:08.000Z", {
      type: "compaction_end",
      reason: "threshold",
      aborted: false,
      willRetry: false,
      result: {
        tokensBefore: 90,
        estimatedTokensAfter: 40,
        usage: {
          input: 8,
          output: 2,
          totalTokens: 10,
          cost: { total: 0.25 },
        },
      },
    })
    record(database, runId, "2026-08-05T10:00:08.500Z", {
      type: "turn_end",
      message: {
        role: "assistant",
        stopReason: "stop",
        usage: {
          input: 60,
          output: 10,
          cacheRead: 5,
          cacheWrite: 2,
          reasoning: 4,
          totalTokens: 77,
          cost: {
            input: 0.1,
            output: 0.2,
            cacheRead: 0.01,
            cacheWrite: 0.02,
            total: 0.25,
          },
        },
      },
      contextUsage: { tokens: 75, contextWindow: 100 },
    })
    database.recordCommand(
      event.id,
      "aven add incident",
      0,
      "Created OPS-7KQ9 with private prose",
    )
    database.recordCommand(
      event.id,
      "workmux add",
      0,
      "handle: investigate-incident",
    )
    database.finishTriageRun(runId, "succeeded", "2026-08-05T10:00:10.000Z")
    database.succeed(event.id)

    const response = createDashboardHandler(database, {
      maxTurns: 8,
      wallTimeoutMs: 300_000,
    })(new Request(`http://localhost/api/runs/${runId}?limit=100`))
    expect(response.status).toBe(200)
    const detail = (await response.json()) as NonNullable<
      ReturnType<typeof runDetail>
    >

    expect(detail.run).toMatchObject({
      id: runId,
      attempt: 2,
      state: "succeeded",
      telemetry: { schemaVersion: 1, completeness: "complete" },
      model: { contextWindow: 100, maxTokens: 20 },
    })
    expect(detail.limits).toEqual({
      maxTurns: 8,
      wallTimeoutMs: 300_000,
      modelContextWindow: 100,
      modelMaxTokens: 20,
    })
    expect(detail.siblingAttempts).toHaveLength(2)
    expect(detail.siblingAttempts[0]).toMatchObject({
      id: firstRun,
      state: "failed",
      failureCategory: "rate_limit",
    })
    expect(detail.metrics).toMatchObject({
      durationMs: {
        wall: 10_000,
        setup: 1_000,
        thinking: 2_000,
        tool: 3_000,
        retryWait: 1_000,
        compaction: 1_000,
        gaps: 500,
        finalization: 1_500,
      },
      toolCallCount: 2,
      failedToolCount: 1,
      turnCount: 1,
      retryCount: 1,
      compactionCount: 1,
      usage: { totalTokens: 87, totalCost: 0.5 },
      peakContextTokens: 75,
      peakContextPercent: 75,
      sourceLagMs: 60_000,
      queueWaitMs: 30_000,
    })
    expect(detail.effects).toEqual([
      expect.objectContaining({ type: "aven_reference", value: "OPS-7KQ9" }),
      expect.objectContaining({
        type: "investigation_handle",
        value: "investigate-incident",
      }),
    ])
    expect(detail.prompts).toEqual([
      {
        role: "system",
        content: "Use the restricted triage tools.",
        recordedAt: "2026-08-05T10:00:00.000Z",
      },
      {
        role: "user",
        content: "Triage this event payload.",
        recordedAt: "2026-08-05T10:00:00.001Z",
      },
    ])
    expect(detail.timeline.entries).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          type: "span",
          kind: "thinking",
          summary: "Checked the event context.",
        }),
        expect.objectContaining({
          type: "span",
          label: "bash",
          summary: "cat /private/path",
        }),
        expect.objectContaining({
          type: "span",
          label: "read",
          summary: "/private/path",
        }),
        expect.objectContaining({ type: "turn", ordinal: 1 }),
        expect.objectContaining({
          type: "retry",
          turnOrdinal: 1,
          errorCategory: "rate_limit",
        }),
        expect.objectContaining({
          type: "compaction",
          reason: "threshold",
          tokensBefore: 90,
        }),
      ]),
    )
    const serialized = JSON.stringify(detail)
    for (const secret of [
      "private intake body",
      "payload-secret",
      "private-call-one",
      "private-call-two",
      "private tool output",
      "private raw tool error",
      "raw-secret",
    ])
      expect(serialized).not.toContain(secret)
  })

  test("clamps unterminated telemetry in terminal runs", () => {
    const database = createDatabase()
    const { eventId, runId } = startRun(database)
    record(database, runId, "2026-08-05T10:00:01.000Z", {
      type: "turn_start",
    })
    record(database, runId, "2026-08-05T10:00:02.000Z", {
      type: "tool_execution_start",
      toolName: "read",
    })
    database.finishTriageRun(runId, "failed", "2026-08-05T10:00:05.000Z", {
      failureCategory: "timeout",
      terminationReason: "wall_timeout",
    })
    database.fail(eventId, "timed out", 1, 1)

    const detail = runDetail(database, runId)!
    expect(detail.timeline.entries).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          type: "span",
          endedAt: "2026-08-05T10:00:05.000Z",
          state: "interrupted",
        }),
      ]),
    )
    expect(detail.metrics.durationMs.wall).toBe(5_000)
    expect(detail.metrics.durationMs.tool).toBe(3_000)
  })

  test("marks legacy and missing telemetry as unavailable", () => {
    const database = createDatabase()
    const { runId } = startRun(database)
    database.raw
      .query(
        `UPDATE triage_runs SET telemetry_version = NULL,
           telemetry_completeness = 'legacy', ended_at = ?, outcome = 'succeeded'`,
      )
      .run("2026-08-05T10:00:05.000Z")

    const detail = runDetail(database, runId)!
    expect(detail.run.telemetry).toEqual({
      schemaVersion: null,
      completeness: "legacy",
    })
    expect(detail.metrics).toMatchObject({
      durationMs: {
        wall: 5_000,
        setup: null,
        thinking: null,
        tool: null,
        compaction: null,
        retryWait: null,
        gaps: null,
        finalization: null,
      },
      toolCallCount: null,
      turnCount: null,
      peakContextTokens: null,
      usage: { totalTokens: null, totalCost: null },
    })
  })

  test("paginates long timelines with explicit truncation metadata", async () => {
    const database = createDatabase()
    const { runId } = startRun(database)
    record(database, runId, "2026-08-05T10:00:01.000Z", {
      type: "turn_start",
    })
    for (let index = 0; index < 205; index += 1) {
      const second = 2 + index * 0.01
      record(database, runId, iso(second), {
        type: "tool_execution_start",
        toolName: `tool_${index}`,
      })
      record(database, runId, iso(second + 0.005), {
        type: "tool_execution_end",
        toolName: `tool_${index}`,
        isError: false,
      })
    }
    record(database, runId, "2026-08-05T10:00:05.000Z", {
      type: "turn_end",
      message: { role: "assistant", stopReason: "stop" },
    })
    database.finishTriageRun(runId, "succeeded", "2026-08-05T10:00:06.000Z")

    const response = createDashboardHandler(database)(
      new Request(`http://localhost/api/runs/${runId}?offset=50&limit=50`),
    )
    const detail = (await response.json()) as NonNullable<
      ReturnType<typeof runDetail>
    >
    expect(detail.timeline.entries).toHaveLength(50)
    expect(detail.timeline.page).toEqual({
      offset: 50,
      limit: 50,
      returned: 50,
      total: 206,
      hasMore: true,
      nextOffset: 100,
    })
  })

  test("resolves details outside the snapshot run window", async () => {
    const database = createDatabase()
    const { runId } = startRun(database)
    database.finishTriageRun(runId, "succeeded", "2026-08-05T10:00:01.000Z")
    for (let index = 0; index < 55; index += 1)
      database.startTriageRun(1, index + 2, iso(index + 2))

    expect(database.listTriageRuns(50).some((run) => run.id === runId)).toBe(
      false,
    )
    const response = createDashboardHandler(database)(
      new Request(`http://localhost/api/runs/${runId}`),
    )
    expect(response.status).toBe(200)
    expect(await response.json()).toMatchObject({ run: { id: runId } })
  })

  test("clamps orphaned telemetry when its event is terminal", () => {
    const database = createDatabase()
    const { eventId, runId } = startRun(database)
    record(database, runId, "2026-08-05T10:00:01.000Z", {
      type: "turn_start",
    })
    record(database, runId, "2026-08-05T10:00:02.000Z", {
      type: "tool_execution_start",
      toolName: "read",
    })
    database.raw
      .query("UPDATE events SET status = 'failed', updated_at = ? WHERE id = ?")
      .run("2026-08-05T10:00:03.000Z", eventId)

    const detail = runDetail(database, runId, {
      now: new Date("2026-08-05T12:00:00.000Z"),
    })!
    expect(detail.run).toMatchObject({
      state: "interrupted",
      endedAt: "2026-08-05T10:00:02.000Z",
    })
    expect(detail.metrics.durationMs.wall).toBe(2_000)
    expect(detail.timeline.entries).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          type: "span",
          state: "interrupted",
          endedAt: "2026-08-05T10:00:02.000Z",
        }),
      ]),
    )
  })

  test("keeps timing partitions bounded under overlap, clock skew, and zero duration", () => {
    const database = createDatabase()
    const { runId } = startRun(database)
    record(database, runId, "2026-08-05T09:59:59.000Z", {
      type: "turn_start",
    })
    record(database, runId, "2026-08-05T10:00:00.000Z", {
      type: "tool_execution_start",
      toolName: "read",
    })
    record(database, runId, "2026-08-05T10:00:00.000Z", {
      type: "message_update",
      assistantMessageEvent: { type: "thinking_start" },
    })
    database.finishTriageRun(runId, "succeeded", "2026-08-05T10:00:00.000Z")

    const detail = runDetail(database, runId)!
    const parts = detail.metrics.durationMs
    expect(parts.wall).toBe(0)
    expect(
      [
        parts.setup,
        parts.thinking,
        parts.tool,
        parts.compaction,
        parts.retryWait,
        parts.gaps,
        parts.finalization,
      ].reduce<number>((sum, value) => sum + (value ?? 0), 0),
    ).toBe(0)
  })

  test("returns not found for unknown run ids", () => {
    const response = createDashboardHandler(createDatabase())(
      new Request("http://localhost/api/runs/999"),
    )
    expect(response.status).toBe(404)
  })
})

function createDatabase(): IntakeDatabase {
  const database = new IntakeDatabase(":memory:")
  databases.push(database)
  return database
}

function startRun(database: IntakeDatabase): {
  eventId: number
  runId: number
} {
  database.sourceSucceeded(
    "github",
    {},
    [intakeItem],
    "2026-08-05T09:59:30.000Z",
  )
  const event = database.claimNext("2026-08-05T10:00:00.000Z")!
  return {
    eventId: event.id,
    runId: database.startTriageRun(
      event.id,
      event.attemptCount,
      "2026-08-05T10:00:00.000Z",
    ),
  }
}

function record(
  database: IntakeDatabase,
  runId: number,
  timestamp: string,
  event: Record<string, unknown>,
): void {
  database.recordTriageRunEvent(
    runId,
    event as unknown as Parameters<IntakeDatabase["recordTriageRunEvent"]>[1],
    timestamp,
  )
}

function iso(second: number): string {
  return new Date(
    Date.parse("2026-08-05T10:00:00.000Z") + second * 1_000,
  ).toISOString()
}
