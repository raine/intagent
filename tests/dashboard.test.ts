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
        turnCount: 0,
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
      { type: "turn_end" },
      "2026-08-04T09:02:06.000Z",
    )
    database.finishTriageRun(runId, "succeeded", "2026-08-04T09:02:07.000Z")
    database.succeed(eventId)

    expect(dashboardSnapshot(database).runs[0]).toMatchObject({
      state: "succeeded",
      endedAt: "2026-08-04T09:02:07.000Z",
      turnCount: 1,
      steps: [{ label: "bash", state: "succeeded" }],
    })
  })

  test("serves the dashboard and read-only snapshot API", async () => {
    const handler = createDashboardHandler(createDatabase())

    const page = handler(new Request("http://localhost/"))
    expect(page.status).toBe(200)
    expect(page.headers.get("content-type")).toBe("text/html; charset=utf-8")
    expect(page.headers.get("content-security-policy")).toContain(
      "default-src 'none'",
    )
    const html = await page.text()
    expect(html).toContain("Intake events")
    expect(html).toContain('data-theme-choice="system"')
    expect(html).toContain('localStorage.getItem("im-theme")')
    expect(html).toContain('id="active-runs"')
    expect(html).toContain('id="run-detail-view"')
    expect(html).toContain('name="color-scheme" content="light dark"')
    expect(html).toContain("@media (max-width: 680px)")
    expect(html).toContain("@media (prefers-reduced-motion: reduce)")
    expect(html).toContain('aria-pressed="true"')

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
