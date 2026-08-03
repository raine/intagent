import { access, realpath } from "node:fs/promises"
import { constants } from "node:fs"
import { join, resolve } from "node:path"
import parse, {
  type CommandNode,
  type PipelineNode,
  type WordNode,
} from "bash-parser"
import type { CommandRule, IntakeConfig } from "../config.ts"
import { isWithin } from "../config.ts"

export interface ParsedCommand {
  stages: string[][]
}

export interface CommandResult {
  exitCode: number
  stdout: string
  stderr: string
  truncated: boolean
}

export class CommandPolicy {
  private readonly workingRoots: string[]
  private readonly path: string[]
  private readonly rules: Map<string, CommandRule>
  readonly timeoutMilliseconds: number
  readonly maxOutputBytes: number
  private readonly filters: RegExp[]

  constructor(config: IntakeConfig, canonicalWorkingRoots: string[]) {
    this.workingRoots = canonicalWorkingRoots
    this.path = config.commands.path.map((path) => resolve(path))
    this.rules = new Map(
      config.commands.rules.map((rule) => [rule.executable, rule]),
    )
    this.timeoutMilliseconds = config.commands.timeoutSeconds * 1000
    this.maxOutputBytes = config.commands.maxOutputBytes
    this.filters = [
      /(https?:\/\/)[^/\s:@]+:[^@\s/]+@/gi,
      /\b(?:sk-[a-zA-Z0-9_-]{16,}|gh[opurs]_[a-zA-Z0-9]{20,})\b/g,
      /\b(?:bearer|token|password|secret)\s*[:=]\s*\S+/gi,
      ...config.commands.sensitivePatterns.map(
        (pattern) => new RegExp(pattern, "gi"),
      ),
    ]
  }

  parseAndAuthorize(command: string, cwd: string): ParsedCommand {
    if (command.length === 0 || command.length > 32_768)
      throw new Error("command length is outside policy bounds")
    if (command.includes("\0")) throw new Error("NUL bytes are forbidden")
    rejectUnsupportedTokens(command)
    const resolvedCwd = resolve(cwd)
    if (!isWithin(resolvedCwd, this.workingRoots))
      throw new Error(`working directory is outside approved roots: ${cwd}`)

    let ast
    try {
      ast = parse(command, { insertLOC: true })
    } catch (error) {
      throw new Error(
        `command syntax is invalid: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
    if (ast.type !== "Script" || ast.commands.length !== 1) {
      throw new Error("only one command or one pipeline is permitted")
    }

    const top = ast.commands[0] as CommandNode | PipelineNode
    if ((top as CommandNode & { async?: boolean }).async)
      throw new Error("background execution is forbidden")
    const commands = top.type === "Pipeline" ? top.commands : [top]
    if (top.type !== "Command" && top.type !== "Pipeline")
      throw new Error(`shell construct is forbidden: ${(top as any).type}`)
    if (commands.length === 0 || commands.length > 8)
      throw new Error("pipeline stage count is outside policy bounds")

    const stages = commands.map((node) => this.commandArguments(node, command))
    for (const stage of stages) this.authorizeStage(stage)
    return { stages }
  }

  async execute(
    command: string,
    cwd: string,
    signal?: AbortSignal,
  ): Promise<CommandResult> {
    let canonicalCwd: string
    try {
      canonicalCwd = await realpath(cwd)
    } catch {
      throw new Error(`working directory is unavailable: ${cwd}`)
    }
    const parsed = this.parseAndAuthorize(command, canonicalCwd)
    let stdin: Uint8Array | undefined
    let stderr = ""
    let truncated = false
    let exitCode = 0

    for (const stage of parsed.stages) {
      const executable = await this.resolveExecutable(stage[0] ?? "")
      if (signal?.aborted) throw new Error("command cancelled")
      let timedOut = false
      let forceKill: ReturnType<typeof setTimeout> | undefined
      const child = Bun.spawn([executable, ...stage.slice(1)], {
        cwd: canonicalCwd,
        detached: true,
        env: {
          PATH: this.path.join(":"),
          HOME: process.env.HOME ?? "",
          LANG: "C.UTF-8",
          LC_ALL: "C.UTF-8",
          NO_COLOR: "1",
          TERM: "dumb",
        },
        stdin: stdin
          ? new Blob([new Uint8Array(stdin).buffer as ArrayBuffer])
          : "ignore",
        stdout: "pipe",
        stderr: "pipe",
      })
      const terminate = () => {
        killProcessGroup(child.pid, "SIGTERM")
        forceKill ??= setTimeout(
          () => killProcessGroup(child.pid, "SIGKILL"),
          1_000,
        )
      }
      const cancel = () => terminate()
      signal?.addEventListener("abort", cancel, { once: true })
      const timeout = setTimeout(() => {
        timedOut = true
        terminate()
      }, this.timeoutMilliseconds)
      try {
        const [code, out, err] = await Promise.all([
          child.exited,
          readBounded(child.stdout, this.maxOutputBytes),
          readBounded(child.stderr, this.maxOutputBytes),
        ])
        if (timedOut) throw new Error("command timed out")
        if (signal?.aborted) throw new Error("command cancelled")
        exitCode = code
        stdin = out.bytes
        stderr += `${err.text}${err.truncated ? "\n[stderr truncated]" : ""}`
        truncated ||= out.truncated || err.truncated
        if (code !== 0) break
      } finally {
        clearTimeout(timeout)
        if (forceKill) {
          clearTimeout(forceKill)
          killProcessGroup(child.pid, "SIGKILL")
        }
        signal?.removeEventListener("abort", cancel)
      }
    }

    return {
      exitCode,
      stdout: this.filter(new TextDecoder().decode(stdin ?? new Uint8Array())),
      stderr: this.filter(stderr),
      truncated,
    }
  }

  filter(value: string): string {
    let filtered = value
    for (const pattern of this.filters)
      filtered = filtered.replace(pattern, "[REDACTED]")
    return filtered
  }

  private commandArguments(node: unknown, source: string): string[] {
    if (!node || typeof node !== "object" || (node as any).type !== "Command") {
      throw new Error(
        `pipeline contains a forbidden construct: ${(node as any)?.type ?? "unknown"}`,
      )
    }
    const command = node as CommandNode
    if (!command.name || command.name.type !== "Word")
      throw new Error("assignments and empty commands are forbidden")
    if (command.prefix?.length)
      throw new Error("assignments and redirections are forbidden")
    const words: WordNode[] = [command.name]
    for (const suffix of command.suffix ?? []) {
      if (suffix.type !== "Word")
        throw new Error(`command suffix is forbidden: ${suffix.type}`)
      words.push(suffix as WordNode)
    }
    for (const word of words) {
      if (word.expansion?.length)
        throw new Error("shell expansions are forbidden")
      if (/[\r\n]/.test(word.text))
        throw new Error("newlines in arguments are forbidden")
      if (
        ["?", "*", "["].some((character) => word.text.includes(character)) &&
        !isFullyQuoted(word, source)
      ) {
        throw new Error("unquoted glob syntax is forbidden")
      }
    }
    return words.map((word) => word.text)
  }

  private authorizeStage(argv: string[]): void {
    const executable = argv[0] ?? ""
    if (!this.rules.has(executable)) {
      throw new Error(`executable is not allowed: ${executable}`)
    }
  }

  private async resolveExecutable(name: string): Promise<string> {
    for (const directory of this.path) {
      const candidate = join(directory, name)
      try {
        await access(candidate, constants.X_OK)
        return candidate
      } catch {}
    }
    throw new Error(
      `allowed executable is unavailable on the fixed PATH: ${name}`,
    )
  }
}

function rejectUnsupportedTokens(source: string): void {
  if (/[\r\n]/.test(source)) throw new Error("newlines are forbidden")
  let quote: "single" | "double" | null = null
  let escaped = false
  let wordStart = true
  for (const character of source) {
    if (escaped) {
      escaped = false
      wordStart = false
      continue
    }
    if (quote === "single") {
      if (character === "'") quote = null
      continue
    }
    if (quote === "double") {
      if (character === "\\") escaped = true
      else if (character === '"') quote = null
      else if (character === "$" || character === "`")
        throw new Error("shell expansions are forbidden")
      continue
    }
    if (character === "\\") {
      escaped = true
      wordStart = false
      continue
    }
    if (character === "'") {
      quote = "single"
      wordStart = false
      continue
    }
    if (character === '"') {
      quote = "double"
      wordStart = false
      continue
    }
    if (/\s/.test(character)) {
      wordStart = true
      continue
    }
    if (character === "#" && wordStart)
      throw new Error("shell comments are forbidden")
    if (
      [";", "&", "<", ">", "`", "$", "(", ")", "{", "}"].includes(character)
    ) {
      throw new Error(`shell operator is forbidden: ${character}`)
    }
    wordStart = character === "|"
  }
}

function isFullyQuoted(word: WordNode, source: string): boolean {
  if (!word.loc) return false
  const raw = source.slice(word.loc.start.char, word.loc.end.char + 1)
  return (
    (raw.startsWith('"') && raw.endsWith('"')) ||
    (raw.startsWith("'") && raw.endsWith("'"))
  )
}

function killProcessGroup(pid: number, signal: NodeJS.Signals): void {
  try {
    process.kill(-pid, signal)
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error
  }
}

async function readBounded(
  stream: ReadableStream<Uint8Array>,
  limit: number,
): Promise<{ bytes: Uint8Array; text: string; truncated: boolean }> {
  const reader = stream.getReader()
  const chunks: Uint8Array[] = []
  let size = 0
  let truncated = false
  while (true) {
    const { done, value } = await reader.read()
    if (done) break
    const remaining = limit - size
    if (remaining <= 0) {
      truncated = true
      continue
    }
    const accepted =
      value.byteLength > remaining ? value.slice(0, remaining) : value
    if (accepted.byteLength < value.byteLength) truncated = true
    chunks.push(accepted)
    size += accepted.byteLength
  }
  const bytes = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return { bytes, text: new TextDecoder().decode(bytes), truncated }
}
