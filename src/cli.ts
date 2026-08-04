#!/usr/bin/env bun
import { access, readFile } from "node:fs/promises"
import { constants } from "node:fs"
import { CommandPolicy } from "./agent/command-policy.ts"
import { loginCodex, PiTriageRunner } from "./agent/pi-runner.ts"
import { validateSkills } from "./agent/skills.ts"
import {
  canonicalRoots,
  defaultConfigPath,
  errorMessage,
  expandPath,
  initializePrivateConfig,
  loadConfig,
} from "./config.ts"
import { IntakeDatabase } from "./database.ts"
import { startDashboard } from "./dashboard.ts"
import { DurableLogStore } from "./logging.ts"
import { IntakeMonitor } from "./monitor.ts"
import { intakeItemSchema } from "./protocol.ts"
import { terminalLine } from "./terminal.ts"

const usage = `Usage: intake [--config PATH] COMMAND

Commands:
  watch                 monitor sources and triage continuously
  check                 poll every source once and drain ready triage events
  status                show source and queue state
  dashboard [--host HOST] [--port PORT]
                        serve the local monitoring dashboard
  inject FILE           queue one IntakeItem JSON fixture
  show ID               show one intake event
  retry ID              queue a retained event for another attempt
  ignore ID             mark an event handled without action
  login                 authenticate the OpenAI Codex subscription provider
  init                  create private configuration directories and config
  validate-config       validate YAML, command boundaries, and skill links
`

async function main(argv: string[]): Promise<void> {
  const { configPath, args } = parseGlobalOptions(argv)
  const command = args.shift()
  if (
    !command ||
    command === "help" ||
    command === "--help" ||
    command === "-h"
  ) {
    process.stdout.write(usage)
    return
  }
  if (command === "init") {
    const result = await initializePrivateConfig(configPath)
    process.stdout.write(
      result.created.length > 0
        ? `Created private configuration at ${result.configPath}\n`
        : `Private configuration exists at ${result.configPath}\n`,
    )
    return
  }
  if (command === "login") {
    await loginCodex()
    return
  }

  const config = await loadConfig(configPath)
  if (command === "validate-config") {
    const diagnostics = await validateConfiguration(config)
    if (diagnostics.length > 0) throw new Error(diagnostics.join("\n"))
    process.stdout.write(`Configuration is valid: ${configPath}\n`)
    return
  }

  const database = new IntakeDatabase(expandPath(config.state.database))
  try {
    if (command === "status") {
      printStatus(database)
      return
    }
    if (command === "dashboard") {
      const { hostname, port } = parseDashboardOptions(args)
      const server = startDashboard(database, hostname, port)
      process.stdout.write(`Intake dashboard: ${server.url}\n`)
      await waitForShutdown(server)
      return
    }
    if (command === "inject") {
      const fixturePath = args[0]
      if (!fixturePath)
        throw new Error("inject requires an IntakeItem JSON file")
      const fixture = intakeItemSchema.parse(
        JSON.parse(await readFile(expandPath(fixturePath), "utf8")),
      )
      const now = new Date().toISOString()
      const queued = database.sourceSucceeded(
        "manual-injection",
        { injected_at: now },
        [fixture],
        now,
      )
      if (queued === 0)
        throw new Error("fixture entity and revision are already queued")
      process.stdout.write(`Queued fixture from ${fixturePath}.\n`)
      return
    }
    if (command === "show") {
      const id = parseEventId(args[0])
      const event = database.event(id)
      if (!event) throw new Error(`Unknown event ${id}`)
      process.stdout.write(`${JSON.stringify(event, null, 2)}\n`)
      return
    }
    if (command === "retry") {
      const id = parseEventId(args[0])
      if (!database.retry(id))
        throw new Error(`Event ${id} has no retained content to retry`)
      process.stdout.write(`Event ${id} is queued for retry.\n`)
      return
    }
    if (command === "ignore") {
      const id = parseEventId(args[0])
      if (!database.ignore(id)) throw new Error(`Unknown event ${id}`)
      process.stdout.write(`Event ${id} is ignored.\n`)
      return
    }
    if (command !== "check" && command !== "watch")
      throw new Error(`Unknown command: ${command}`)

    const diagnostics = await validateConfiguration(config)
    if (diagnostics.length > 0)
      throw new Error(
        `Configuration validation failed:\n${diagnostics.join("\n")}`,
      )
    const roots = await canonicalRoots(config.project_roots)
    const policy = new CommandPolicy(config, roots)
    const logs = new DurableLogStore(config.state.logs, (value) =>
      policy.filter(value),
    )
    const runner = new PiTriageRunner(config, database, policy, logs)
    const monitor = new IntakeMonitor(config, database, runner, logs)
    if (command === "check") {
      const result = await monitor.check()
      process.stdout.write(
        `Observed ${result.observed}; handled ${result.handled}; errors ${result.errors.length}.\n`,
      )
      if (result.errors.length > 0) {
        process.stderr.write(`${result.errors.join("\n")}\n`)
        process.exitCode = 1
      }
      return
    }

    let signals = 0
    const shutdown = () => {
      signals += 1
      if (signals === 1) {
        terminalLine(
          process.stderr,
          "Stopping schedules and waiting for active triage.",
        )
        monitor.stop()
      } else {
        terminalLine(process.stderr, "Forced shutdown requested.")
        process.exit(130)
      }
    }
    process.on("SIGINT", shutdown)
    process.on("SIGTERM", shutdown)
    try {
      await monitor.watch()
    } finally {
      process.off("SIGINT", shutdown)
      process.off("SIGTERM", shutdown)
    }
  } finally {
    database.close()
  }
}

async function validateConfiguration(
  config: Awaited<ReturnType<typeof loadConfig>>,
): Promise<string[]> {
  const diagnostics: string[] = []
  const names = new Set<string>()
  for (const source of config.sources) {
    if (names.has(source.name))
      diagnostics.push(`duplicate source name: ${source.name}`)
    names.add(source.name)
  }
  const executables = new Set<string>()
  for (const rule of config.commands.rules) {
    if (executables.has(rule.executable))
      diagnostics.push(`duplicate command rule: ${rule.executable}`)
    executables.add(rule.executable)
  }
  for (const path of config.commands.path) {
    try {
      await access(expandPath(path), constants.X_OK)
    } catch {
      diagnostics.push(`command PATH directory is unavailable: ${path}`)
    }
  }
  const skills = await validateSkills(config)
  diagnostics.push(...skills.diagnostics)
  return diagnostics
}

function printStatus(database: IntakeDatabase): void {
  process.stdout.write("Queue:\n")
  const statuses = database.status()
  if (Object.keys(statuses).length === 0) process.stdout.write("  empty\n")
  for (const [status, count] of Object.entries(statuses))
    process.stdout.write(`  ${status}: ${count}\n`)
  process.stdout.write("Sources:\n")
  const sources = database.sourceStatuses()
  if (sources.length === 0) process.stdout.write("  unchecked\n")
  for (const source of sources)
    process.stdout.write(`  ${JSON.stringify(source)}\n`)
  const recent = database.listEvents(10)
  if (recent.length > 0) {
    process.stdout.write("Recent events:\n")
    for (const event of recent)
      process.stdout.write(
        `  ${event.id} ${event.status} ${event.source}: ${event.title}\n`,
      )
  }
}

function parseDashboardOptions(args: string[]): {
  hostname: string
  port: number
} {
  let hostname = "127.0.0.1"
  let port = 4545
  const remaining = [...args]
  while (remaining.length > 0) {
    const option = remaining.shift()
    const value = remaining.shift()
    if (option === "--host" && value) {
      hostname = value
      continue
    }
    if (option === "--port" && value) {
      port = Number(value)
      if (!Number.isSafeInteger(port) || port < 1 || port > 65_535)
        throw new Error("dashboard port must be between 1 and 65535")
      continue
    }
    throw new Error(`Unknown dashboard option: ${option ?? ""}`)
  }
  return { hostname, port }
}

async function waitForShutdown(
  server: ReturnType<typeof Bun.serve>,
): Promise<void> {
  await new Promise<void>((resolve) => {
    const shutdown = () => {
      process.off("SIGINT", shutdown)
      process.off("SIGTERM", shutdown)
      server.stop(true)
      resolve()
    }
    process.on("SIGINT", shutdown)
    process.on("SIGTERM", shutdown)
  })
}

function parseEventId(value: string | undefined): number {
  const id = Number(value)
  if (!Number.isSafeInteger(id) || id < 1)
    throw new Error("A positive event ID is required")
  return id
}

function parseGlobalOptions(argv: string[]): {
  configPath: string
  args: string[]
} {
  const args = [...argv]
  let configPath = defaultConfigPath()
  const index = args.indexOf("--config")
  if (index >= 0) {
    const value = args[index + 1]
    if (!value) throw new Error("--config requires a path")
    configPath = expandPath(value)
    args.splice(index, 2)
  }
  return { configPath, args }
}

if (import.meta.main) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`intake: ${errorMessage(error)}\n`)
    process.exitCode = 1
  })
}
