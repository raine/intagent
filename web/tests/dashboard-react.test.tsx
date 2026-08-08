import { describe, expect, test } from "vitest"
import { renderToStaticMarkup } from "react-dom/server"
import { ActiveRunCard, EventRow, SourceList } from "../src/app.tsx"

const run: Parameters<typeof ActiveRunCard>[0]["run"] = {
  id: 7,
  eventId: 11,
  eventTitle: "Inspect the dashboard timeline",
  source: "github",
  eventKind: "github-issue",
  attempt: 2,
  startedAt: "2026-08-05T10:00:00.000Z",
  endedAt: "2026-08-05T10:00:05.000Z",
  lastActivityAt: "2026-08-05T10:00:05.000Z",
  state: "succeeded",
  modelId: "claude-sonnet-5",
  modelProvider: "anthropic",
  thinkingLevel: "medium",
  turnCount: 1,
  retryCount: 0,
  compactionCount: 0,
  telemetryCompleteness: "complete",
  timelineTruncated: false,
  dispatchSequence: 2,
  dispatchTrigger: "backoff_retry",
  conclusion: {
    decision: "action_taken",
    summary:
      "Created a task and dispatched an investigation for the reported issue.",
    evidence: ["The issue requests a dashboard timeline review."],
    actions: [
      "Created task OPS-7KQ9.",
      "Dispatched investigation dashboard-timeline.",
    ],
    outcome: "The issue is queued for investigation.",
    followUp: "Review the investigation result.",
    source: "model",
  },
  investigationHandle: "dashboard-timeline",
  steps: [
    {
      id: 1,
      turnOrdinal: 1,
      kind: "tool",
      label: "Read",
      summary: null,
      startedAt: "2026-08-05T10:00:01.000Z",
      endedAt: "2026-08-05T10:00:04.000Z",
      state: "succeeded",
    },
  ],
}

const event: Parameters<typeof EventRow>[0]["event"] = {
  id: 11,
  source: "github",
  entityId: "issue-11",
  kind: "github-issue",
  title: "Inspect the dashboard timeline",
  url: "https://github.com/example/repo/issues/11",
  occurredAt: "2026-08-05T09:59:00.000Z",
  observedAt: "2026-08-05T10:00:00.000Z",
  status: "succeeded",
  attemptCount: 2,
  nextAttemptAt: null,
  lastError: null,
  avenRef: null,
  investigationHandle: "dashboard-timeline",
}

describe("React dashboard components", () => {
  test("renders source cards with their design-system structure", () => {
    const html = renderToStaticMarkup(
      <SourceList
        now={Date.parse("2026-08-05T10:01:00.000Z")}
        sources={[
          {
            source: "github",
            lastSuccessAt: "2026-08-05T10:00:30.000Z",
            lastError: null,
            updatedAt: "2026-08-05T10:00:30.000Z",
          },
          {
            source: "fastmail",
            lastSuccessAt: "2026-08-05T09:55:00.000Z",
            lastError: "Authentication failed",
            updatedAt: "2026-08-05T10:00:20.000Z",
          },
        ]}
      />,
    )

    expect(html).toContain('class="source-card is-healthy"')
    expect(html).toContain('class="source-card is-failing"')
    expect(html).toContain('class="source-heading"')
    expect(html).toContain('class="source-marker"')
    expect(html).toContain('class="source-poll"')
    expect(html).toContain('class="source-error"')
    expect(html).toContain("last success 6m ago")
  })

  test("renders active activity using the expected component classes", () => {
    const html = renderToStaticMarkup(
      <ActiveRunCard
        run={{
          ...run,
          endedAt: null,
          state: "active",
          lastActivityAt: "2026-08-05T10:00:05.000Z",
          steps: [{ ...run.steps[0]!, endedAt: null, state: "active" }],
        }}
      />,
    )

    expect(html).toContain("active-run is-stalled")
    expect(html).toContain("active-run-activity is-expanded")
    expect(html).toContain('class="activity-marker"')
    expect(html).toContain('class="active-run-dispatch"')
    expect(html).toContain("Retry after failure")
    expect(html).toContain("Run 2")
    expect(html).toContain("inspect run →")
  })

  test("opens the matching run directly from an event row", () => {
    const html = renderToStaticMarkup(
      <EventRow
        event={event}
        now={Date.parse(event.observedAt)}
        run={run}
        openRun={() => {}}
      />,
    )

    expect(html).toContain(
      "Open run inspector for Inspect the dashboard timeline",
    )
    expect(html).toContain('class="status-glyph"')
    expect(html).toContain('class="event-conclusion"')
    expect(html).toContain("action taken")
    expect(html).toContain("Created a task and dispatched an investigation")
    expect(html).not.toContain("event-detail")
    expect(html).not.toContain("Open event inspector")
  })

  test("renders events without runs as static rows", () => {
    const html = renderToStaticMarkup(
      <EventRow
        event={event}
        now={Date.parse(event.observedAt)}
        run={null}
        openRun={() => {}}
      />,
    )

    expect(html).toContain('class="event-summary"')
    expect(html).not.toContain("<button")
    expect(html).not.toContain("event-detail")
  })
})
