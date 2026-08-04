import type { AgentSessionEvent } from "@earendil-works/pi-coding-agent"
import type { EventRecord } from "../database.ts"
import { terminalLine, type WritableTerminal } from "../terminal.ts"

export class TerminalTriageReporter {
  private pendingText = ""
  private pendingLabel = "assistant"
  private startedAt = 0
  private turns = 0

  constructor(
    private readonly output: WritableTerminal = process.stderr,
    private readonly redact: (value: string) => string = (value) => value,
  ) {}

  start(event: EventRecord): void {
    this.startedAt = Date.now()
    this.turns = 0
    this.line(
      `${this.style("cyan", "▶")} triage #${event.id} ${this.redact(event.title)}`,
    )
  }

  event(event: AgentSessionEvent): void {
    switch (event.type) {
      case "message_update": {
        const update = event.assistantMessageEvent
        if (update.type === "text_delta")
          this.stream("assistant", this.redact(update.delta))
        else if (update.type === "thinking_delta")
          this.stream("thinking", this.redact(update.delta))
        break
      }
      case "tool_execution_start": {
        this.flushStream()
        const args = event.args as { command?: unknown; cwd?: unknown }
        if (event.toolName === "bash" && typeof args.command === "string") {
          const cwd =
            typeof args.cwd === "string" ? `  (${this.redact(args.cwd)})` : ""
          this.line(
            `${this.style("yellow", "$")} ${this.redact(args.command)}${cwd}`,
          )
        } else {
          this.line(
            `${this.style("yellow", "◆")} ${event.toolName} ${this.preview(JSON.stringify(event.args))}`,
          )
        }
        break
      }
      case "tool_execution_end": {
        this.flushStream()
        const marker = event.isError
          ? this.style("red", "✗")
          : this.style("green", "✓")
        this.line(`${marker} ${event.toolName}`)
        const text = toolResultText(event.result)
        if (text) this.block(this.redact(text))
        break
      }
      case "turn_end":
        this.turns += 1
        this.flushStream()
        break
      case "auto_retry_start":
        this.flushStream()
        this.line(
          `${this.style("yellow", "↻")} retry ${event.attempt}/${event.maxAttempts}: ${this.redact(event.errorMessage)}`,
        )
        break
      case "compaction_start":
        this.flushStream()
        this.line(`${this.style("yellow", "◇")} compacting context`)
        break
    }
  }

  finish(error?: unknown): void {
    this.flushStream()
    const elapsed = ((Date.now() - this.startedAt) / 1000).toFixed(1)
    if (error) {
      const message = formatError(error)
      this.line(
        `${this.style("red", "✗")} triage failed after ${elapsed}s: ${this.redact(message)}`,
      )
    } else {
      this.line(
        `${this.style("green", "✓")} triage finished in ${elapsed}s, ${this.turns} turn${this.turns === 1 ? "" : "s"}`,
      )
    }
  }

  private stream(label: string, delta: string): void {
    if (this.pendingText && this.pendingLabel !== label) this.flushStream()
    this.pendingLabel = label
    this.pendingText += delta
    const lines = this.pendingText.split("\n")
    this.pendingText = lines.pop() ?? ""
    for (const line of lines) this.streamLine(label, line)
  }

  private flushStream(): void {
    if (!this.pendingText) return
    this.streamLine(this.pendingLabel, this.pendingText)
    this.pendingText = ""
  }

  private streamLine(label: string, value: string): void {
    const styledLabel =
      label === "thinking"
        ? this.style("dim", "thinking")
        : this.style("blue", "assistant")
    this.line(`${styledLabel} │ ${value}`)
  }

  private block(value: string): void {
    const preview = this.preview(value)
    for (const line of preview.split("\n")) this.line(`  │ ${line}`)
  }

  private preview(value: string): string {
    const limit = 4_000
    return value.length <= limit
      ? value
      : `${value.slice(0, limit)}\n… terminal output truncated`
  }

  private line(value: string): void {
    terminalLine(this.output, value)
  }

  private style(
    color: "red" | "green" | "yellow" | "blue" | "cyan" | "dim",
    value: string,
  ): string {
    if (!this.output.isTTY || process.env.NO_COLOR) return value
    const codes = {
      red: "31",
      green: "32",
      yellow: "33",
      blue: "34",
      cyan: "36",
      dim: "2",
    }
    return `[${codes[color]}m${value}[0m`
  }
}

function formatError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === "string") return error
  try {
    return JSON.stringify(error)
  } catch {
    return "unknown error"
  }
}

function toolResultText(result: unknown): string {
  if (!result || typeof result !== "object") return ""
  const content = (result as { content?: unknown }).content
  if (!Array.isArray(content)) return ""
  return content
    .filter(
      (item): item is { type: "text"; text: string } =>
        !!item &&
        typeof item === "object" &&
        (item as { type?: unknown }).type === "text" &&
        typeof (item as { text?: unknown }).text === "string",
    )
    .map((item) => item.text)
    .join("\n")
}
