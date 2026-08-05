import { afterEach, describe, expect, test } from "bun:test"
import {
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent"
import type { EventRecord } from "../src/database.ts"
import { DurableLogStore, type LogRecord } from "../src/logging.ts"

const temporaryDirectories: string[] = []
afterEach(async () => {
  await Promise.all(
    temporaryDirectories
      .splice(0)
      .map((path) => rm(path, { recursive: true, force: true })),
  )
})

describe("durable logging", () => {
  test("writes a complete successful triage lifecycle without cumulative stream copies", async () => {
    const root = await temporaryDirectory()
    const logs = new DurableLogStore(root, redact)
    const run = logs.triage(event(17, 1))

    await run.start()
    await run.metadata({
      sessionId: "session-1",
      model: { provider: "openai-codex", id: "gpt-test" },
    })
    await run.prompt("inspect token=visible-secret")
    await run.event(
      sessionEvent({
        type: "agent_start",
      }),
    )
    await run.event(
      sessionEvent({
        type: "turn_start",
      }),
    )
    await run.event(
      sessionEvent({
        type: "message_update",
        message: { cumulative: "first second visible-secret" },
        assistantMessageEvent: {
          type: "thinking_delta",
          contentIndex: 0,
          delta: "first ",
          partial: { cumulative: "first " },
        },
      }),
    )
    await run.event(
      sessionEvent({
        type: "message_update",
        message: { cumulative: "first second visible-secret" },
        assistantMessageEvent: {
          type: "thinking_delta",
          contentIndex: 0,
          delta: "second",
          partial: { cumulative: "first second" },
        },
      }),
    )
    await run.event(
      sessionEvent({
        type: "tool_execution_start",
        toolCallId: "call-1",
        toolName: "bash",
        args: { command: "rg token=visible-secret" },
      }),
    )
    await run.event(
      sessionEvent({
        type: "tool_execution_end",
        toolCallId: "call-1",
        toolName: "bash",
        result: {
          content: [{ type: "text", text: "ordinary complete tool output" }],
        },
        isError: false,
      }),
    )
    await run.event(
      sessionEvent({
        type: "message_end",
        message: {
          role: "assistant",
          content: [{ type: "text", text: "complete final answer" }],
        },
      }),
    )
    await run.event(
      sessionEvent({
        type: "turn_end",
        message: { role: "assistant" },
        toolResults: [],
      }),
    )
    await run.event(
      sessionEvent({
        type: "agent_end",
        messages: [],
        willRetry: false,
      }),
    )
    await run.finish("succeeded", { turns: 1 })

    const records = await readRecords(run.path)
    expect(records.map((record) => record.type)).toEqual([
      "run_start",
      "session_metadata",
      "prompt_submitted",
      "agent_start",
      "turn_start",
      "tool_execution_start",
      "tool_execution_end",
      "message_end",
      "turn_end",
      "agent_end",
      "run_end",
    ])
    expect(records.some((record) => record.type === "message_update")).toBe(
      false,
    )
    const serialized = JSON.stringify(records)
    expect(serialized).not.toContain("ordinary complete tool output")
    expect(serialized).not.toContain("complete final answer")
    expect(serialized).not.toContain("visible-secret")
    expect(serialized).not.toContain("call-1")
    expect(serialized).not.toContain("session-1")
    expect(serialized).not.toContain("A human title")
    expect(serialized).not.toContain("sha256")
    expect(records.at(-1)).toMatchObject({
      type: "run_end",
      outcome: "succeeded",
      failureCategory: null,
    })
    expect((await stat(root)).mode & 0o777).toBe(0o700)
    expect((await stat(run.path)).mode & 0o777).toBe(0o600)
  })

  test("separates failed attempts into identifiable files", async () => {
    const root = await temporaryDirectory()
    const logs = new DurableLogStore(root, redact)
    const first = logs.triage(event(42, 1))
    const second = logs.triage(event(42, 2))

    await first.start()
    await first.event(
      sessionEvent({
        type: "auto_retry_start",
        attempt: 1,
        maxAttempts: 3,
        delayMs: 2_000,
        errorMessage: "provider unavailable",
      }),
    )
    await first.event(
      sessionEvent({
        type: "compaction_end",
        reason: "overflow",
        result: undefined,
        aborted: false,
        willRetry: true,
      }),
    )
    await first.finish("failed", { error: new Error("request failed") })
    await second.start()
    await second.finish("succeeded")

    expect(first.path).not.toBe(second.path)
    expect(first.path).toContain("triage-event-42-attempt-1-fake")
    expect(second.path).toContain("triage-event-42-attempt-2-fake")
    const files = await readdir(join(root, "triage"))
    expect(files).toHaveLength(2)
    const firstRecords = await readRecords(first.path)
    expect(firstRecords.map((record) => record.type)).toContain(
      "auto_retry_start",
    )
    expect(firstRecords.map((record) => record.type)).toContain(
      "compaction_end",
    )
    expect(firstRecords.at(-1)).toMatchObject({
      type: "run_end",
      outcome: "failed",
      failureCategory: "unknown",
    })
    expect(JSON.stringify(firstRecords)).not.toContain("request failed")
  })

  test("writes append-only main monitor records", async () => {
    const root = await temporaryDirectory()
    const logs = new DurableLogStore(root, redact)

    await logs.monitor("process_start", { mode: "watch" })
    await logs.monitor("source_poll_succeeded", {
      source: "fake",
      queued: 2,
    })
    await logs.monitor("triage_failed", {
      eventId: 9,
      retry: true,
      error: "token=visible-secret",
    })
    await logs.monitor("process_stop", { mode: "watch" })

    const path = join(root, "monitor.jsonl")
    const records = await readRecords(path)
    expect(records.map((record) => record.type)).toEqual([
      "process_start",
      "source_poll_succeeded",
      "triage_failed",
      "process_stop",
    ])
    expect(records[1]).toMatchObject({ source: "fake", queued: 2 })
    expect(records[2]).toMatchObject({ retry: true, error: "[REDACTED]" })
    expect((await stat(path)).mode & 0o777).toBe(0o600)
  })

  test("surfaces logging failures without rejecting operations", async () => {
    const root = await temporaryDirectory()
    const blocked = join(root, "blocked")
    await writeFile(blocked, "not a directory")
    let warning = ""
    const logs = new DurableLogStore(blocked, redact, {
      write(value) {
        warning += value
      },
    })

    await expect(logs.monitor("process_start")).resolves.toBeUndefined()
    expect(warning).toContain("warning: intake logging failed")
    expect(warning).toContain("monitor.jsonl")
  })
})

async function temporaryDirectory(): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "intake-logging-"))
  temporaryDirectories.push(path)
  return path
}

async function readRecords(path: string): Promise<LogRecord[]> {
  const contents = await readFile(path, "utf8")
  return contents
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line) as LogRecord)
}

function redact(value: string): string {
  return value.replace(/token=visible-secret/g, "[REDACTED]")
}

function event(id: number, attemptCount: number): EventRecord {
  return {
    id,
    source: "fake",
    entityId: `entity-${id}`,
    revisionId: "revision-1",
    kind: "generic",
    title: "A human title",
    payload: "{}",
    operationalMetadata: "{}",
    occurredAt: "2026-08-04T12:00:00.000Z",
    observedAt: "2026-08-04T12:00:01.000Z",
    status: "processing",
    attemptCount,
    nextAttemptAt: null,
    lastError: null,
    avenRef: null,
    investigationHandle: null,
  }
}

function sessionEvent(value: Record<string, unknown>): AgentSessionEvent {
  return value as unknown as AgentSessionEvent
}
