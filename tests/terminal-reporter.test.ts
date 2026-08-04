import { describe, expect, test } from "bun:test"
import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent"
import type { EventRecord } from "../src/database.ts"
import { TerminalTriageReporter } from "../src/agent/terminal-reporter.ts"

const intakeEvent: EventRecord = {
  id: 7,
  source: "fastmail",
  entityId: "thread-1",
  revisionId: "message-1",
  kind: "email",
  title: "Needs attention",
  payload: "{}",
  operationalMetadata: "{}",
  occurredAt: "2026-08-04T07:00:00Z",
  observedAt: "2026-08-04T07:00:01Z",
  status: "processing",
  attemptCount: 1,
  nextAttemptAt: null,
  lastError: null,
  avenRef: null,
  investigationHandle: null,
}

describe("terminal triage reporter", () => {
  test("streams readable assistant and tool activity with redaction", () => {
    let output = ""
    const reporter = new TerminalTriageReporter(
      { isTTY: false, write: (value) => (output += value) },
      (value) => value.replaceAll("secret-value", "[REDACTED]"),
    )

    reporter.start(intakeEvent)
    reporter.event(
      event({
        type: "message_update",
        message: {} as never,
        assistantMessageEvent: {
          type: "text_delta",
          contentIndex: 0,
          delta: "Checking existing tasks.\n",
          partial: {} as never,
        },
      }),
    )
    reporter.event(
      event({
        type: "tool_execution_start",
        toolCallId: "tool-1",
        toolName: "bash",
        args: { command: "aven search secret-value" },
      }),
    )
    reporter.event(
      event({
        type: "tool_execution_end",
        toolCallId: "tool-1",
        toolName: "bash",
        result: {
          content: [{ type: "text", text: "exit code: 0\nstdout: found" }],
        },
        isError: false,
      }),
    )
    reporter.event(
      event({
        type: "turn_end",
        message: {} as never,
        toolResults: [],
      }),
    )
    reporter.finish()

    expect(output).toContain("▶ triage #7 Needs attention")
    expect(output).toContain("assistant │ Checking existing tasks.")
    expect(output).toContain("$ aven search [REDACTED]")
    expect(output).toContain("✓ bash")
    expect(output).toContain("│ stdout: found")
    expect(output).toContain("1 turn")
    expect(output).not.toContain("secret-value")
  })

  test("prints failures", () => {
    let output = ""
    const reporter = new TerminalTriageReporter({
      isTTY: false,
      write: (value) => (output += value),
    })
    reporter.start(intakeEvent)
    reporter.finish(new Error("model unavailable"))
    expect(output).toContain("✗ triage failed")
    expect(output).toContain("model unavailable")
  })
})

function event(value: AgentSessionEvent): AgentSessionEvent {
  return value
}
