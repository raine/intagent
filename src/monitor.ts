import type { IntakeConfig, SourceConfig } from "./config.ts"
import { errorMessage } from "./config.ts"
import type { IntakeDatabase } from "./database.ts"
import { pollSource } from "./source-runner.ts"
import type { TriageRunner } from "./agent/pi-runner.ts"

export class IntakeMonitor {
  private stopping = false
  private readonly scheduleAbort = new AbortController()

  constructor(
    private readonly config: IntakeConfig,
    private readonly database: IntakeDatabase,
    private readonly runner: TriageRunner,
  ) {}

  stop(): void {
    this.stopping = true
    this.scheduleAbort.abort()
  }

  async check(): Promise<{
    observed: number
    handled: number
    errors: string[]
  }> {
    const results = await Promise.allSettled(
      this.config.sources.map((source) =>
        pollSource(source, this.config, this.database),
      ),
    )
    let observed = 0
    const errors: string[] = []
    for (const result of results) {
      if (result.status === "fulfilled") observed += result.value
      else errors.push(errorMessage(result.reason))
    }
    let handled = 0
    while (!this.stopping) {
      const event = this.database.claimNext()
      if (!event) break
      try {
        await this.runner.run(event)
        this.database.succeed(event.id)
        handled += 1
      } catch (error) {
        const message = errorMessage(error)
        this.database.fail(
          event.id,
          message,
          this.config.triage.maxAttempts,
          this.config.triage.retryBaseSeconds,
        )
        errors.push(`event ${event.id}: ${message}`)
      }
    }
    return { observed, handled, errors }
  }

  async watch(): Promise<void> {
    const pollers = this.config.sources.map((source) => this.pollLoop(source))
    const worker = this.workerLoop()
    await Promise.all([...pollers, worker])
  }

  private async pollLoop(source: SourceConfig): Promise<void> {
    let first = true
    while (!this.stopping) {
      if (!first)
        await sleep(source.intervalSeconds * 1000, this.scheduleAbort.signal)
      first = false
      if (this.stopping) break
      try {
        const observed = await pollSource(source, this.config, this.database)
        if (observed > 0)
          process.stdout.write(`${source.name}: queued ${observed} event(s)\n`)
      } catch (error) {
        process.stderr.write(`${source.name}: ${errorMessage(error)}\n`)
      }
    }
  }

  private async workerLoop(): Promise<void> {
    while (!this.stopping) {
      const event = this.database.claimNext()
      if (!event) {
        await sleep(500, this.scheduleAbort.signal)
        continue
      }
      try {
        await this.runner.run(event)
        this.database.succeed(event.id)
        process.stdout.write(`event ${event.id}: handled ${event.title}\n`)
      } catch (error) {
        const message = errorMessage(error)
        this.database.fail(
          event.id,
          message,
          this.config.triage.maxAttempts,
          this.config.triage.retryBaseSeconds,
        )
        process.stderr.write(`event ${event.id}: ${message}\n`)
      }
    }
  }
}

function sleep(milliseconds: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve()
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, milliseconds)
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timeout)
        resolve()
      },
      { once: true },
    )
  })
}
