import { afterEach, describe, expect, test } from "bun:test"
import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { Agent, type StreamFn } from "@earendil-works/pi-agent-core"
import {
  type AssistantMessage,
  type AssistantMessageEvent,
  EventStream,
} from "@earendil-works/pi-ai/compat"
import type { IntakeConfig } from "../src/config.ts"
import { IntakeDatabase, type EventRecord } from "../src/database.ts"
import { DurableLogStore, type LogRecord } from "../src/logging.ts"
import { IntakeMonitor } from "../src/monitor.ts"
import type { TriageRunner } from "../src/agent/pi-runner.ts"
import { testConfig } from "./fixtures/config.ts"

const databases: IntakeDatabase[] = []
const temporaryDirectories: string[] = []
afterEach(async () => {
  for (const database of databases.splice(0)) database.close()
  await Promise.all(
    temporaryDirectories
      .splice(0)
      .map((path) => rm(path, { recursive: true, force: true })),
  )
})

class MockAssistantStream extends EventStream<
  AssistantMessageEvent,
  AssistantMessage
> {
  constructor() {
    super(
      (event) => event.type === "done" || event.type === "error",
      (event) => {
        if (event.type === "done") return event.message
        if (event.type === "error") return event.error
        throw new Error("unexpected event")
      },
    )
  }
}

class FakeModelRunner implements TriageRunner {
  readonly contextSizes: number[] = []

  async run(event: EventRecord): Promise<void> {
    const streamFn: StreamFn = (_model, context) => {
      this.contextSizes.push(context.messages.length)
      const stream = new MockAssistantStream()
      queueMicrotask(() => {
        stream.push({
          type: "done",
          reason: "stop",
          message: assistant(`handled ${event.id}`),
        })
      })
      return stream
    }
    const agent = new Agent({ streamFn })
    await agent.prompt(event.payload ?? "")
  }
}

describe("monitor scheduling", () => {
  test("uses a fresh fake-model run for each event and processes one at a time", async () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    const first = intake("entity-1", "revision-1")
    const second = intake("entity-2", "revision-2")
    database.sourceSucceeded(
      "fake",
      {},
      [first, second],
      "2026-08-03T12:00:00.000Z",
    )
    const runner = new FakeModelRunner()
    const root = await mkdtemp(join(tmpdir(), "intake-monitor-"))
    temporaryDirectories.push(root)
    const config: IntakeConfig = testConfig(root, "/bin")
    const logs = new DurableLogStore(config.state.logs)
    const monitor = new IntakeMonitor(config, database, runner, logs)
    const result = await monitor.check()
    expect(result).toEqual({ observed: 0, handled: 2, errors: [] })
    expect(runner.contextSizes).toEqual([1, 1])
    expect(database.status()).toEqual({ succeeded: 2 })
    const records = (
      await readFile(join(config.state.logs, "monitor.jsonl"), "utf8")
    )
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line) as LogRecord)
    expect(records.map((record) => record.type)).toEqual([
      "process_start",
      "queue_state",
      "triage_start",
      "triage_succeeded",
      "triage_start",
      "triage_succeeded",
      "process_stop",
    ])
    expect(records[2]).toMatchObject({ eventId: 1, attempt: 1 })
    expect(records[3]).toMatchObject({
      eventId: 1,
      attempt: 1,
      queue: { pending: 1, succeeded: 1 },
    })
    expect(JSON.stringify(records)).not.toContain("entity-1")
    expect(JSON.stringify(records)).not.toContain("entity-2")
  })

  test("categorizes triage failures without retaining raw errors", async () => {
    const database = new IntakeDatabase(":memory:")
    databases.push(database)
    database.sourceSucceeded(
      "fake",
      {},
      [intake("private-title", "revision-1")],
      "2026-08-03T12:00:00.000Z",
    )
    const root = await mkdtemp(join(tmpdir(), "intake-monitor-"))
    temporaryDirectories.push(root)
    const config: IntakeConfig = testConfig(root, "/bin")
    const monitor = new IntakeMonitor(
      config,
      database,
      {
        async run() {
          throw new Error("timeout reading /private/project/file")
        },
      },
      new DurableLogStore(config.state.logs),
    )

    const result = await monitor.check()
    expect(result.errors).toEqual([
      "event 1: timeout reading /private/project/file",
    ])
    const contents = await readFile(
      join(config.state.logs, "monitor.jsonl"),
      "utf8",
    )
    expect(contents).toContain('"failureCategory":"timeout"')
    expect(contents).not.toContain("private-title")
    expect(contents).not.toContain("/private/project/file")
  })
})

function intake(entityId: string, revisionId: string) {
  return {
    entityId,
    revisionId,
    kind: "generic" as const,
    title: entityId,
    body: "untrusted content",
    occurredAt: "2026-08-03T12:00:00.000Z",
    metadata: {},
  }
}

function assistant(text: string): AssistantMessage {
  return {
    role: "assistant",
    content: [{ type: "text", text }],
    api: "openai-responses",
    provider: "openai",
    model: "fake",
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: "stop",
    timestamp: Date.now(),
  }
}
