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

  test("keeps active triage processing across concurrent database opens", () => {
    const directory = mkdtempSync(join(tmpdir(), "intake-database-"))
    temporaryDirectories.push(directory)
    const path = join(directory, "intake.sqlite")
    const watcher = new IntakeDatabase(path)
    const observer = new IntakeDatabase(path)
    databases.push(watcher, observer)

    watcher.sourceSucceeded("mail", {}, [item()], "2026-08-03T10:01:00.000Z")
    const event = watcher.claimNext("2026-08-03T10:02:00.000Z")

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
