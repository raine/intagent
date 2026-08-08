import type { IntakeConfig, SourceConfig } from "./config.ts"
import { errorMessage } from "./config.ts"
import type { IntakeDatabase } from "./database.ts"
import {
  PROTOCOL_VERSION,
  pollResponseSchema,
  type PollRequest,
} from "./protocol.ts"

const SOURCE_OUTPUT_LIMIT = 8 * 1024 * 1024

async function boundedText(
  stream: ReadableStream<Uint8Array>,
  limit: number,
): Promise<string> {
  const reader = stream.getReader()
  const chunks: Uint8Array[] = []
  let size = 0
  while (true) {
    const { done, value } = await reader.read()
    if (done) break
    if (size + value.byteLength > limit) {
      await reader.cancel()
      throw new Error(`source output exceeded ${limit} bytes`)
    }
    chunks.push(value)
    size += value.byteLength
  }
  const output = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) {
    output.set(chunk, offset)
    offset += chunk.byteLength
  }
  return new TextDecoder().decode(output)
}

export async function pollSource(
  source: SourceConfig,
  config: IntakeConfig,
  database: IntakeDatabase,
  now = new Date(),
): Promise<number> {
  const request: PollRequest = {
    protocolVersion: PROTOCOL_VERSION,
    source: source.name,
    checkpoint: database.sourceCheckpoint(source.name),
    now: now.toISOString(),
    itemLimit: source.item_limit,
    options: { project_roots: config.project_roots, ...source.options },
  }
  const environment: Record<string, string> = {
    PATH: config.commands.path.join(":"),
    HOME: process.env.HOME ?? "",
    LANG: "C.UTF-8",
    LC_ALL: "C.UTF-8",
    NO_COLOR: "1",
  }
  for (const name of source.environment) {
    const value = process.env[name]
    if (value !== undefined) environment[name] = value
  }

  const processHandle = (() => {
    try {
      return Bun.spawn([source.command, ...source.args], {
        detached: true,
        env: environment,
        stdin: new Blob([JSON.stringify(request)]),
        stdout: "pipe",
        stderr: "pipe",
      })
    } catch (error) {
      const message = redactSourceError(errorMessage(error), source.environment)
      database.sourceFailed(source.name, message, now.toISOString())
      throw new Error(message)
    }
  })()
  let timedOut = false
  let forceKill: ReturnType<typeof setTimeout> | undefined
  const kill = (signal: NodeJS.Signals) => {
    if (killProcessGroup(processHandle.pid, signal)) return
    try {
      processHandle.kill(signal)
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error
    }
  }
  const terminate = () => {
    kill("SIGTERM")
    forceKill ??= setTimeout(() => kill("SIGKILL"), 1_000)
  }
  const timeout = setTimeout(() => {
    timedOut = true
    terminate()
  }, source.timeout_seconds * 1000)
  try {
    const stdoutPromise = boundedText(processHandle.stdout, SOURCE_OUTPUT_LIMIT)
    const stderrPromise = boundedText(processHandle.stderr, 65_536).catch(
      (error) => errorMessage(error),
    )
    const [exitCode, stdout, stderr] = await Promise.all([
      processHandle.exited,
      stdoutPromise,
      stderrPromise,
    ])
    if (timedOut) throw new Error("source poll timed out")
    if (exitCode !== 0)
      throw new Error(
        `source exited ${exitCode}: ${stderr.trim() || "no diagnostics"}`,
      )

    let value: unknown
    try {
      value = JSON.parse(stdout.trim())
    } catch (error) {
      throw new Error(
        `source stdout is not one JSON response: ${errorMessage(error)}`,
      )
    }
    const response = pollResponseSchema.parse(value)
    if (response.items.length > source.item_limit) {
      throw new Error(
        `source returned ${response.items.length} items for a limit of ${source.item_limit}`,
      )
    }
    return database.sourceSucceeded(
      source.name,
      response.checkpoint,
      response.items,
      now.toISOString(),
    )
  } catch (error) {
    if (processHandle.exitCode === null) terminate()
    const message = redactSourceError(errorMessage(error), source.environment)
    database.sourceFailed(source.name, message, now.toISOString())
    throw new Error(message)
  } finally {
    clearTimeout(timeout)
    if (forceKill) {
      clearTimeout(forceKill)
      kill("SIGKILL")
    }
  }
}

function killProcessGroup(pid: number, signal: NodeJS.Signals): boolean {
  try {
    process.kill(-pid, signal)
    return true
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code
    if (code === "ESRCH") return true
    if (code === "EPERM") return false
    throw error
  }
}

function redactSourceError(
  message: string,
  environmentNames: string[],
): string {
  let redacted = message
  for (const name of environmentNames) {
    const value = process.env[name]
    if (value && value.length >= 6)
      redacted = redacted.replaceAll(value, "[REDACTED]")
  }
  return redacted.replace(
    /(bearer|token|password|secret)\s*[:=]\s*\S+/gi,
    "$1=[REDACTED]",
  )
}
