import { afterEach, describe, expect, test } from "bun:test"
import { IntakeDatabase } from "../src/database.ts"
import type { IntakeItem } from "../src/protocol.ts"

const databases: IntakeDatabase[] = []
afterEach(() => {
  for (const database of databases.splice(0)) database.close()
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

  test("derives durable Aven and investigation references", () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded("mail", {}, [item()], "2026-08-03T10:01:00.000Z")
    const event = database.claimNext()
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
