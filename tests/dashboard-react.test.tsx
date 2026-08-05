import { describe, expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import {
  ActiveRunCard,
  RunInspector,
  SourceList,
} from "../src/dashboard/app.tsx"

const run: Parameters<typeof RunInspector>[0]["run"] = {
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
  investigationHandle: "dashboard-timeline",
  steps: [
    {
      id: 1,
      kind: "tool",
      label: "Read",
      startedAt: "2026-08-05T10:00:01.000Z",
      endedAt: "2026-08-05T10:00:04.000Z",
      state: "succeeded",
    },
  ],
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
    expect(html).toContain('class="source-poll"')
    expect(html).toContain('class="source-error"')
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
    expect(html).toContain('class="privacy-inline"')
  })

  test("renders the dedicated run inspector", () => {
    const html = renderToStaticMarkup(<RunInspector run={run} />)

    expect(html).toContain('class="run-inspector"')
    expect(html).toContain("TIMELINE · 1 TURNS · 1 ENTRIES")
    expect(html).toContain("bar scale: 0–3s")
    expect(html).toContain("github/github-issue")
    expect(html).toContain("dashboard-timeline")
    expect(html).toContain("Tool arguments, commands, output")
  })
})
