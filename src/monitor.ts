import type { IntakeConfig, SourceConfig } from "./config.ts"
import { errorMessage } from "./config.ts"
import type { EventRecord, IntakeDatabase } from "./database.ts"
import { DurableLogStore } from "./logging.ts"
import { pollSource } from "./source-runner.ts"
import type { TriageRunner } from "./agent/pi-runner.ts"

const TRIAGE_RECOVERY_GRACE_MS = 60_000

export class IntakeMonitor {
  private stopping = false
  private readonly scheduleAbort = new AbortController()
  private nextRecoveryAt = 0

  constructor(
    private readonly config: IntakeConfig,
    private readonly database: IntakeDatabase,
    private readonly runner: TriageRunner,
    private readonly logs: DurableLogStore = new DurableLogStore(
      config.state.logs,
    ),
  ) {}

  stop(): void {
    this.stopping = true
    this.scheduleAbort.abort()
    void this.logs.monitor("stop_requested")
  }

  async check(): Promise<{
    observed: number
    handled: number
    errors: string[]
  }> {
    await this.logs.monitor("process_start", {
      mode: "check",
      pid: process.pid,
      sources: this.config.sources.map((source) => source.name),
      queue: this.database.status(),
    })
    try {
      const results = await Promise.allSettled(
        this.config.sources.map((source) => this.poll(source)),
      )
      let observed = 0
      const errors: string[] = []
      for (const result of results) {
        if (result.status === "fulfilled") observed += result.value
        else errors.push(errorMessage(result.reason))
      }
      await this.logs.monitor("queue_state", {
        reason: "polls_complete",
        observed,
        counts: this.database.status(),
      })
      let handled = 0
      while (!this.stopping) {
        const event = this.claimNext()
        if (!event) break
        const result = await this.triage(event)
        if (result.error) errors.push(`event ${event.id}: ${result.error}`)
        else handled += 1
      }
      return { observed, handled, errors }
    } catch (error) {
      await this.logs.monitor("operational_error", {
        operation: "check",
        error,
      })
      throw error
    } finally {
      await this.logs.monitor("process_stop", {
        mode: "check",
        queue: this.database.status(),
      })
    }
  }

  async watch(): Promise<void> {
    const schedules = this.config.sources
      .map(
        (source) =>
          `${source.name} every ${source.interval_seconds} second${source.interval_seconds === 1 ? "" : "s"}`,
      )
      .join(", ")
    process.stdout.write(
      `Watching ${schedules || "no configured sources"}. Press Ctrl-C to stop.\n`,
    )
    await this.logs.monitor("process_start", {
      mode: "watch",
      pid: process.pid,
      schedules,
      sources: this.config.sources.map((source) => source.name),
      queue: this.database.status(),
    })
    try {
      const pollers = this.config.sources.map((source) => this.pollLoop(source))
      const worker = this.workerLoop()
      await Promise.all([...pollers, worker])
    } catch (error) {
      await this.logs.monitor("operational_error", {
        operation: "watch",
        error,
      })
      throw error
    } finally {
      await this.logs.monitor("process_stop", {
        mode: "watch",
        queue: this.database.status(),
      })
    }
  }

  private async poll(source: SourceConfig): Promise<number> {
    const startedAt = Date.now()
    await this.logs.monitor("source_poll_start", { source: source.name })
    try {
      const observed = await pollSource(source, this.config, this.database)
      await this.logs.monitor("source_poll_succeeded", {
        source: source.name,
        queued: observed,
        durationMs: Date.now() - startedAt,
        queue: this.database.status(),
      })
      return observed
    } catch (error) {
      await this.logs.monitor("source_poll_failed", {
        source: source.name,
        durationMs: Date.now() - startedAt,
        error,
      })
      throw error
    }
  }

  private async pollLoop(source: SourceConfig): Promise<void> {
    let first = true
    while (!this.stopping) {
      if (!first)
        await sleep(source.interval_seconds * 1000, this.scheduleAbort.signal)
      first = false
      if (this.stopping) break
      try {
        const observed = await this.poll(source)
        if (observed > 0) {
          const time = new Date().toLocaleTimeString("en-GB", { hour12: false })
          process.stdout.write(
            `${time}  ${source.name}: queued ${observed} event${observed === 1 ? "" : "s"}\n`,
          )
        }
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
      const result = await this.triage(event)
      if (result.error)
        process.stderr.write(`event ${event.id}: ${result.error}\n`)
      else process.stdout.write(`event ${event.id}: handled ${event.title}\n`)
    }
  }

  private claimNext(): EventRecord | null {
    const now = Date.now()
    if (now >= this.nextRecoveryAt) {
      const staleBefore = new Date(
        now -
          this.config.triage.timeout_minutes * 60_000 -
          TRIAGE_RECOVERY_GRACE_MS,
      ).toISOString()
      this.database.recoverInterrupted(staleBefore)
      this.nextRecoveryAt = now + 60_000
    }
    return this.database.claimNext()
  }

  private async triage(event: EventRecord): Promise<{ error?: string }> {
    const startedAt = Date.now()
    await this.logs.monitor("triage_start", {
      eventId: event.id,
      attempt: event.attemptCount,
      source: event.source,
      title: event.title,
      queue: this.database.status(),
    })
    try {
      await this.runner.run(event)
      this.database.succeed(event.id)
      await this.logs.monitor("triage_succeeded", {
        eventId: event.id,
        attempt: event.attemptCount,
        durationMs: Date.now() - startedAt,
        queue: this.database.status(),
      })
      return {}
    } catch (error) {
      const message = errorMessage(error)
      this.database.fail(
        event.id,
        message,
        this.config.triage.max_attempts,
        this.config.triage.retry_base_seconds,
      )
      const failed = this.database.event(event.id)
      await this.logs.monitor("triage_failed", {
        eventId: event.id,
        attempt: event.attemptCount,
        durationMs: Date.now() - startedAt,
        error,
        outcome: failed?.status,
        retry: failed?.status === "retryable",
        nextAttemptAt: failed?.nextAttemptAt,
        queue: this.database.status(),
      })
      return { error: message }
    }
  }
}

function sleep(milliseconds: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve()
  return new Promise((resolve) => {
    const finish = () => {
      clearTimeout(timeout)
      signal.removeEventListener("abort", finish)
      resolve()
    }
    const timeout = setTimeout(finish, milliseconds)
    signal.addEventListener("abort", finish, { once: true })
  })
}
