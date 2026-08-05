import { afterEach, describe, expect, test } from "bun:test"
import { createDashboardHandler, dashboardSnapshot } from "../src/dashboard.ts"
import { IntakeDatabase } from "../src/database.ts"
import type { IntakeItem } from "../src/protocol.ts"

const databases: IntakeDatabase[] = []
afterEach(() => {
  for (const database of databases.splice(0)) database.close()
})

function createDatabase(): IntakeDatabase {
  const database = new IntakeDatabase(":memory:")
  databases.push(database)
  return database
}

const item: IntakeItem = {
  entityId: "github:example/intake#42",
  revisionId: "issue-42-updated-1",
  kind: "github-issue",
  title: "Investigate delayed notifications",
  body: "Private event content",
  url: "https://github.example/example/intake/issues/42",
  occurredAt: "2026-08-04T09:00:00.000Z",
  metadata: { repository: "example/intake" },
}

describe("intake dashboard", () => {
  test("summarizes queue state without exposing retained payloads", () => {
    const database = createDatabase()
    database.sourceSucceeded(
      "github",
      { cursor: "next" },
      [item],
      "2026-08-04T09:01:00.000Z",
    )
    const event = database.claimNext("2026-08-04T09:02:00.000Z")
    database.fail(event?.id ?? 0, "Authorization: Bearer private-token", 3, 60)
    database.sourceFailed(
      "fastmail",
      "password=private-source-secret",
      "2026-08-04T09:02:30.000Z",
    )
    database.sourceSucceeded(
      "manual-injection",
      { injected_at: "2026-08-04T09:02:45.000Z" },
      [],
      "2026-08-04T09:02:45.000Z",
    )

    const snapshot = dashboardSnapshot(
      database,
      new Date("2026-08-04T09:03:00.000Z"),
    )

    expect(snapshot).toMatchObject({
      generatedAt: "2026-08-04T09:03:00.000Z",
      total: 1,
      open: 1,
      attention: 1,
      handled: 0,
      oldestOpenAt: "2026-08-04T09:01:00.000Z",
      counts: { retryable: 1 },
      events: [
        {
          source: "github",
          title: "Investigate delayed notifications",
          url: "https://github.example/example/intake/issues/42",
          status: "retryable",
          attemptCount: 1,
          lastError: "Authentication failed",
        },
      ],
    })
    expect(snapshot.sources.map((source) => source.source)).not.toContain(
      "manual-injection",
    )
    const serialized = JSON.stringify(snapshot)
    expect(serialized).not.toContain("Private event content")
    expect(serialized).not.toContain("private-token")
    expect(serialized).not.toContain("private-source-secret")
  })

  test("projects structured active and completed run history", () => {
    const database = createDatabase()
    database.sourceSucceeded(
      "github",
      { cursor: "next" },
      [item],
      "2026-08-04T09:01:00.000Z",
    )
    const event = database.claimNext("2026-08-04T09:02:00.000Z")
    const eventId = event?.id ?? 0
    const runId = database.startTriageRun(
      eventId,
      1,
      "2026-08-04T09:02:01.000Z",
    )
    database.setTriageRunMetadata(
      runId,
      {
        modelId: "gpt-test",
        modelProvider: "openai-codex",
        thinkingLevel: "medium",
      },
      "2026-08-04T09:02:02.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      { type: "turn_start" },
      "2026-08-04T09:02:02.500Z",
    )
    database.recordTriageRunEvent(
      runId,
      {
        type: "tool_execution_start",
        toolCallId: "secret-call-id",
        toolName: "bash",
      },
      "2026-08-04T09:02:03.000Z",
    )

    const active = dashboardSnapshot(
      database,
      new Date("2026-08-04T09:02:04.000Z"),
    )
    expect(active.runs).toEqual([
      expect.objectContaining({
        id: runId,
        eventId,
        state: "active",
        modelId: "gpt-test",
        turnCount: 1,
        steps: [expect.objectContaining({ label: "bash", state: "active" })],
      }),
    ])
    const serializedRuns = JSON.stringify(active.runs)
    expect(serializedRuns).not.toContain("secret-call-id")
    expect(serializedRuns).not.toContain("toolCallId")
    expect(serializedRuns).not.toContain("Private event content")

    database.recordTriageRunEvent(
      runId,
      {
        type: "tool_execution_end",
        toolCallId: "secret-call-id",
        toolName: "bash",
        isError: false,
      },
      "2026-08-04T09:02:05.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      {
        type: "turn_end",
        message: {
          role: "assistant",
          stopReason: "stop",
          usage: {
            input: 10,
            output: 2,
            cacheRead: 0,
            cacheWrite: 0,
            totalTokens: 12,
            cost: {
              input: 0.1,
              output: 0.2,
              cacheRead: 0,
              cacheWrite: 0,
              total: 0.3,
            },
          },
        },
      },
      "2026-08-04T09:02:06.000Z",
    )
    database.finishTriageRun(runId, "succeeded", "2026-08-04T09:02:07.000Z")
    database.succeed(eventId)

    expect(dashboardSnapshot(database).runs[0]).toMatchObject({
      state: "succeeded",
      endedAt: "2026-08-04T09:02:07.000Z",
      turnCount: 1,
      steps: [],
      timelineTruncated: true,
    })
  })

  test("bounds orphaned runs when their event is terminal", () => {
    const database = createDatabase()
    database.sourceSucceeded("github", {}, [item], "2026-08-04T09:01:00.000Z")
    const event = database.claimNext("2026-08-04T09:02:00.000Z")!
    const runId = database.startTriageRun(
      event.id,
      1,
      "2026-08-04T09:02:01.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      { type: "turn_start" },
      "2026-08-04T09:02:02.000Z",
    )
    database.raw
      .query("UPDATE events SET status = 'failed' WHERE id = ?")
      .run(event.id)

    expect(dashboardSnapshot(database).runs[0]).toMatchObject({
      state: "interrupted",
      endedAt: "2026-08-04T09:02:02.000Z",
      steps: [],
    })
  })

  test("records privacy-safe thinking and compaction timing", () => {
    const database = createDatabase()
    database.sourceSucceeded(
      "github",
      { cursor: "next" },
      [item],
      "2026-08-04T09:01:00.000Z",
    )
    const event = database.claimNext("2026-08-04T09:02:00.000Z")
    const runId = database.startTriageRun(
      event?.id ?? 0,
      1,
      "2026-08-04T09:02:01.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      {
        type: "message_update",
        assistantMessageEvent: { type: "thinking_start", contentIndex: 0 },
      },
      "2026-08-04T09:02:02.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      {
        type: "message_update",
        assistantMessageEvent: {
          type: "thinking_end",
          contentIndex: 0,
          content: "private reasoning",
        },
      } as Parameters<IntakeDatabase["recordTriageRunEvent"]>[1],
      "2026-08-04T09:02:06.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      { type: "compaction_start", reason: "threshold" },
      "2026-08-04T09:02:07.000Z",
    )
    database.recordTriageRunEvent(
      runId,
      {
        type: "compaction_end",
        reason: "threshold",
        aborted: false,
        willRetry: false,
        result: { tokensBefore: 100, estimatedTokensAfter: 40 },
      },
      "2026-08-04T09:02:09.000Z",
    )

    const run = dashboardSnapshot(database).runs[0]!
    expect(run.compactionCount).toBe(1)
    expect(run.steps).toEqual([
      expect.objectContaining({
        kind: "thinking",
        label: "thinking",
        startedAt: "2026-08-04T09:02:02.000Z",
        endedAt: "2026-08-04T09:02:06.000Z",
        state: "succeeded",
      }),
      expect.objectContaining({
        kind: "compaction",
        label: "compaction",
        startedAt: "2026-08-04T09:02:07.000Z",
        endedAt: "2026-08-04T09:02:09.000Z",
        state: "succeeded",
      }),
    ])
    expect(JSON.stringify(run)).not.toContain("private reasoning")
    expect(JSON.stringify(run)).not.toContain("contentIndex")
  })

  test("serves the bundled React dashboard", async () => {
    const handler = createDashboardHandler(createDatabase())
    const page = handler(new Request("http://localhost/"))

    expect(page.status).toBe(200)
    expect(page.headers.get("content-type")).toBe("text/html; charset=utf-8")
    expect(page.headers.get("content-security-policy")).toContain(
      "default-src 'none'",
    )
    expect(page.headers.get("cache-control")).toBe("no-store")
    expect(page.headers.get("cross-origin-resource-policy")).toBe("same-origin")
    expect(page.headers.get("permissions-policy")).toContain("camera=()")
    const html = await page.text()
    expect(html).toContain('<div id="root"></div>')
    expect(html).toContain('<script type="module">')
    expect(html).toContain("ACTIVE RUNS")
    expect(html).toContain("RECENT EVENTS")
    expect(html).toContain("RECENT RUNS")
    expect(html).toContain('localStorage.getItem("im-theme")')
    expect(html).toContain('name="color-scheme" content="light dark"')
    expect(html).toContain("Event identity, title, source link")
    expect(html).toContain("intake bodies, thinking text")
    expect(html).toContain("@media (width<=700px)")
    expect(html).toContain("@media (prefers-reduced-motion:reduce)")
    expect(html).toContain("@media (forced-colors:active)")
    expect(html).not.toContain("function keyedList(")
    expect(html).not.toContain('id="design-select"')
  })

  test("ignores obsolete design query parameters", async () => {
    const handler = createDashboardHandler(createDatabase())
    const regular = await handler(new Request("http://localhost/")).text()
    const queried = await handler(
      new Request("http://localhost/?design=untrusted"),
    ).text()

    expect(queried).toBe(regular)
    expect(queried).not.toContain("untrusted")
  })

  test("rejects credential-bearing source links and invalid run routes", async () => {
    const database = createDatabase()
    database.sourceSucceeded(
      "github",
      {},
      [
        {
          ...item,
          url: "https://user:private-password@github.example/example/intake/issues/42",
        },
      ],
      "2026-08-04T09:01:00.000Z",
    )
    const event = database.claimNext("2026-08-04T09:02:00.000Z")!
    const runId = database.startTriageRun(event.id, 1)
    const handler = createDashboardHandler(database)

    expect(dashboardSnapshot(database).events[0]?.url).toBeNull()
    expect(
      (await handler(new Request(`http://localhost/api/runs/${runId}`)).json())
        .event.url,
    ).toBeNull()
    database.raw.query("UPDATE entities SET operational_metadata = ?").run(
      JSON.stringify({
        url: "https://github.example/example/intake/issues/42?access_token=private#secret",
      }),
    )
    expect(dashboardSnapshot(database).events[0]?.url).toBe(
      "https://github.example/example/intake/issues/42",
    )
    expect(
      JSON.stringify(
        await handler(new Request(`http://localhost/api/runs/${runId}`)).json(),
      ),
    ).not.toContain("private")
    for (const path of [
      "/api/runs/0",
      "/api/runs/-1",
      "/api/runs/1.5",
      "/api/runs/9007199254740992",
      "/api/runs/1/extra",
    ])
      expect(handler(new Request(`http://localhost${path}`)).status).toBe(404)
  })

  test("serves a read-only snapshot API", async () => {
    const handler = createDashboardHandler(createDatabase())
    const api = handler(new Request("http://localhost/api/snapshot"))
    expect(api.status).toBe(200)
    expect(await api.json()).toMatchObject({ total: 0, open: 0, sources: [] })

    const rejected = handler(
      new Request("http://localhost/api/snapshot", { method: "POST" }),
    )
    expect(rejected.status).toBe(405)
    expect(rejected.headers.get("allow")).toBe("GET")
  })
})
