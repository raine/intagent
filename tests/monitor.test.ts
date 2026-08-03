import { afterEach, describe, expect, test } from "bun:test"
import { Agent, type StreamFn } from "@earendil-works/pi-agent-core"
import {
  type AssistantMessage,
  type AssistantMessageEvent,
  EventStream,
} from "@earendil-works/pi-ai/compat"
import type { IntakeConfig } from "../src/config.ts"
import { IntakeDatabase, type EventRecord } from "../src/database.ts"
import { IntakeMonitor } from "../src/monitor.ts"
import type { TriageRunner } from "../src/agent/pi-runner.ts"
import { testConfig } from "./fixtures/config.ts"

const databases: IntakeDatabase[] = []
afterEach(() => {
  for (const database of databases.splice(0)) database.close()
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
    const config: IntakeConfig = testConfig("/tmp", "/bin")
    const monitor = new IntakeMonitor(config, database, runner)
    const result = await monitor.check()
    expect(result).toEqual({ observed: 0, handled: 2, errors: [] })
    expect(runner.contextSizes).toEqual([1, 1])
    expect(database.status()).toEqual({ succeeded: 2 })
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
