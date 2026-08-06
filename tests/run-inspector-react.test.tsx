import { describe, expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { RunInspector } from "../src/dashboard/run-inspector.tsx"
import {
  groupTimeline,
  mergeThinkingSpans,
  timeBudget,
} from "../src/dashboard/run-inspector-data.ts"
import {
  cleanEntries,
  compaction,
  legacyRecoveredRunDetailFixture,
  retry,
  runDetailFixture,
  span,
  turn,
} from "./fixtures/run-detail.ts"

function render(
  detail = runDetailFixture(),
  initialFilter: NonNullable<
    Parameters<typeof RunInspector>[0]["initialFilter"]
  > = "all",
): string {
  return renderToStaticMarkup(
    <RunInspector detail={detail} initialFilter={initialFilter} />,
  )
}

describe("activity-first run inspector", () => {
  test("renders clean success with a flat activity timeline", () => {
    const html = render()

    expect(html).toContain("Succeeded cleanly")
    expect(html).toContain("Where the run spent time")
    expect(html).toContain("Activity timeline")
    expect(html).toContain('aria-pressed="true">All activity')
    expect(html.indexOf(">All activity</button>")).toBeLessThan(
      html.indexOf(">Tool calls</button>"),
    )
    expect(html).toContain("4 shown · 4 loaded")
    expect(html).toContain("Source lag")
    expect(html).toContain("Queue wait")
    expect(html).toContain("Aven reference")
    expect(html).toContain("Investigation handle")
    expect(html).not.toContain('class="turn ')
    expect(html).not.toContain("Turn</small>")
    expect(html).not.toContain("aria-expanded")
  })

  test("shows only tool calls when filtered", () => {
    const html = render(runDetailFixture(), "tools")

    expect(html).toContain('aria-pressed="true">Tool calls')
    expect(html).toContain("Tool activity")
    expect(html).toContain("2 shown · 4 loaded")
    expect(html).toContain('class="phase-row phase-tool phase-succeeded"')
    expect(html).not.toContain('class="phase-row phase-thinking')
    expect(html).not.toContain("No tool calls")
    expect(html).not.toContain('class="turn ')
  })

  test("offers thinking summaries without adding row noise", () => {
    const entries = cleanEntries().map((entry) =>
      entry.type === "span" && entry.kind === "thinking" && entry.id === 1
        ? { ...entry, summary: "Checked queue state before reading the event." }
        : entry,
    )
    const html = render(runDetailFixture({ entries }))

    expect(html).toContain(">Thinking</strong>")
    expect(html).not.toContain("<strong>Thinking / model</strong>")
    expect(html).toContain('class="thinking-summary-trigger"')
    expect(html).toContain(
      'data-summary="Checked queue state before reading the event."',
    )
  })

  test("shows commands and read targets on tool rows", () => {
    const entries = cleanEntries().map((entry) =>
      entry.type === "span" && entry.id === 2
        ? { ...entry, label: "read", summary: "/workspace/src/dashboard.ts" }
        : entry.type === "span" && entry.id === 4
          ? {
              ...entry,
              label: "bash",
              summary: "bun test tests/dashboard.test.ts",
            }
          : entry,
    )
    const html = render(runDetailFixture({ entries }), "tools")

    expect(html).toContain('class="phase-summary"')
    expect(html).toContain("/workspace/src/dashboard.ts")
    expect(html).toContain("bun test tests/dashboard.test.ts")
    expect(html).not.toContain("Read target")
    expect(html).not.toContain(">Command<")
    expect(html).not.toContain("<strong>bash</strong>")
    expect(html).not.toContain("<small>succeeded</small>")
  })

  test("offers an accessible full view for long commands", () => {
    const command = `bun test ${"tests/run-inspector-react.test.tsx ".repeat(8)}`
    const entries = cleanEntries().map((entry) =>
      entry.type === "span" && entry.id === 4
        ? { ...entry, label: "Bash", summary: command }
        : entry,
    )
    const html = render(runDetailFixture({ entries }))

    expect(html).toContain("Show full")
    expect(html).toContain('class="phase-summary-full"')
    expect(html).toContain(command.trim())
  })

  test("distinguishes recovered tool failure from clean success", () => {
    const entries = cleanEntries().map((entry) =>
      entry.type === "span" && entry.id === 2
        ? { ...entry, state: "failed" as const }
        : entry,
    )
    const html = render(
      runDetailFixture({
        entries,
        metrics: { failedToolCount: 1 },
      }),
    )

    expect(html).toContain("Succeeded with recovered error")
    expect(html).toContain("1 failed tool call recovered")
    expect(html).not.toContain('class="attention-stack"')
  })

  test("derives a recovered legacy verdict from safe timeline spans", () => {
    const html = render(legacyRecoveredRunDetailFixture())

    expect(html).toContain("Succeeded with recovered error")
    expect(html).toContain("1 failed tool call recovered")
    expect(html).toContain("Tools</span><strong>2")
    expect(html).not.toContain("Turns</span>")
    expect(html).toContain("Time categories are unavailable")
    expect(html).not.toContain("Succeeded cleanly")
    expect(html).not.toContain("No recorded failures")
    expect(html).not.toContain("Tools</span><strong>unavailable")
    expect(html).not.toContain("Recovered failures</span><strong>unavailable")
  })

  test("renders a failed run and model retry as separate concerns", () => {
    const html = render(
      runDetailFixture({
        run: {
          state: "failed",
          failureCategory: "rate_limit",
          terminationReason: "model_error",
        },
        event: { status: "failed" },
        entries: [...cleanEntries(), retry("failed")],
        metrics: { retryCount: 1 },
      }),
      "all",
    )

    expect(html).toContain("Execution failed")
    expect(html).toContain("Run failed")
    expect(html).toContain("1 model retry")
    expect(html).toContain("Model retry 1 of 3")
  })

  test("marks interrupted execution and clamped activity", () => {
    const html = render(
      runDetailFixture({
        run: {
          state: "interrupted",
          terminationReason: "operator_interrupt",
        },
        event: { status: "failed" },
        entries: [
          turn(
            1,
            "2026-08-05T10:00:01.000Z",
            "2026-08-05T10:00:12.000Z",
            "interrupted",
          ),
          span(
            1,
            1,
            "tool",
            "Bash",
            "2026-08-05T10:00:04.000Z",
            "2026-08-05T10:00:12.000Z",
            "interrupted",
          ),
        ],
      }),
    )

    expect(html).toContain("Execution interrupted")
    expect(html).toContain("Run interrupted")
    expect(html).toContain("interrupted and clamped to run end")
  })

  test("renders live state, current phase, and follow control", () => {
    const html = render(
      runDetailFixture({
        run: {
          state: "active",
          endedAt: null,
          lastActivityAt: new Date().toISOString(),
        },
        event: { status: "processing" },
        entries: [
          turn(1, new Date(Date.now() - 5000).toISOString(), null, "active"),
          span(
            1,
            1,
            "thinking",
            "thinking",
            new Date(Date.now() - 3000).toISOString(),
            null,
            "active",
          ),
        ],
      }),
      "all",
    )

    expect(html).toContain("Execution in progress")
    expect(html).toContain("Pause live follow")
    expect(html).toContain(">Thinking</strong>")
    expect(html).not.toContain("No telemetry for")
  })

  test("does not call a succeeded retry run clean", () => {
    const html = render(
      runDetailFixture({
        entries: [...cleanEntries(), retry()],
        metrics: { retryCount: 1 },
      }),
    )

    expect(html).toContain("Succeeded with recovered error")
    expect(html).toContain("1 model retry recovered")
    expect(html).not.toContain("Succeeded cleanly")
  })

  test("renders an active stalled warning separately from connection state", () => {
    const html = render(
      runDetailFixture({
        run: {
          state: "active",
          endedAt: null,
          lastActivityAt: "2020-01-01T00:00:00.000Z",
        },
        event: { status: "processing" },
      }),
    )

    expect(html).toContain("No telemetry for")
    expect(html).toContain("Dashboard connection health is reported separately")
  })

  test("renders a bounded empty terminal timeline", () => {
    const html = render(
      runDetailFixture({
        entries: [],
        page: { returned: 0, total: 0 },
      }),
    )

    expect(html).toContain(
      "No tool or model activity was recorded for this run",
    )
  })

  test("renders compactions and retries as first-class phases", () => {
    const html = render(
      runDetailFixture({
        entries: [...cleanEntries(), retry(), compaction("aborted")],
        metrics: {
          retryCount: 1,
          compactionCount: 1,
          durationMs: {
            wall: 12_000,
            setup: 1_000,
            thinking: 3_000,
            tool: 3_000,
            compaction: 1_000,
            retryWait: 1_000,
            gaps: 1_000,
            finalization: 2_000,
          },
        },
      }),
      "all",
    )

    expect(html).toContain("Model retry 1 of 3")
    expect(html).toContain("threshold compaction")
    expect(html).toContain("180K before")
    expect(html).toContain("1 incomplete compaction")
  })

  test("uses an honest fallback for partial legacy telemetry", () => {
    const nullUsage = {
      inputTokens: null,
      outputTokens: null,
      cacheReadTokens: null,
      cacheWriteTokens: null,
      reasoningTokens: null,
      totalTokens: null,
      totalCost: null,
    }
    const html = render(
      runDetailFixture({
        run: { telemetry: { schemaVersion: null, completeness: "legacy" } },
        entries: [
          span(
            1,
            null,
            "tool",
            "Read",
            "2026-08-05T10:00:01.000Z",
            "2026-08-05T10:00:02.000Z",
          ),
        ],
        metrics: {
          durationMs: {
            wall: 12_000,
            setup: null,
            thinking: null,
            tool: null,
            compaction: null,
            retryWait: null,
            gaps: null,
            finalization: null,
          },
          toolCallCount: null,
          failedToolCount: null,
          turnCount: null,
          retryCount: null,
          compactionCount: null,
          usage: nullUsage,
          peakContextTokens: null,
          peakContextPercent: null,
        },
      }),
    )

    expect(html).toContain("Telemetry legacy")
    expect(html).toContain("Time categories are unavailable")
    expect(html).toContain('class="phase-row phase-tool phase-succeeded"')
    expect(html).not.toContain("Turn membership")
  })

  test("renders sibling event attempts as navigable options", () => {
    const html = render()

    expect(html).toContain("Attempt")
    expect(html).toContain("1 · failed")
    expect(html).toContain("2 · succeeded")
    expect(html).not.toContain(
      "Model retries are separate from event-level attempts",
    )
  })

  test("exposes pagination and truncation without hiding loaded data", () => {
    const html = render(
      runDetailFixture({
        page: {
          returned: 6,
          total: 306,
          hasMore: true,
          nextOffset: 6,
        },
      }),
    )

    expect(html).toContain("4 shown · 4 loaded")
    expect(html).toContain("Timeline continues")
    expect(html).toContain("300 entries remain")
    expect(html).toContain("Load next 200")
  })

  test("omits unavailable token and cost metrics", () => {
    const html = render(
      runDetailFixture({
        metrics: {
          usage: {
            inputTokens: null,
            outputTokens: null,
            cacheReadTokens: null,
            cacheWriteTokens: null,
            reasoningTokens: null,
            totalTokens: null,
            totalCost: null,
          },
          peakContextTokens: null,
          peakContextPercent: null,
        },
      }),
    )

    expect(html).not.toContain("Recorded tokens")
    expect(html).not.toContain("Recorded cost")
    expect(html).not.toContain("$0.00")
  })

  test("does not render turn records as activity", () => {
    const entries = Array.from({ length: 100 }, (_, index) =>
      turn(
        index + 1,
        new Date(
          Date.parse("2026-08-05T10:00:00.000Z") + index * 100,
        ).toISOString(),
        new Date(
          Date.parse("2026-08-05T10:00:00.000Z") + index * 100 + 50,
        ).toISOString(),
      ),
    )
    const html = render(
      runDetailFixture({
        entries,
        metrics: { turnCount: 100 },
        page: { returned: 100, total: 100 },
      }),
      "all",
    )

    expect(html).toContain("0 shown · 0 loaded")
    expect(html).toContain("No tool or model activity was recorded")
    expect(html).not.toContain('class="turn ')
    expect(html).not.toContain("Whole-run waterfall")
  })

  test("escapes structured labels and rejects unsafe source links", () => {
    const html = render(
      runDetailFixture({
        event: {
          title: '<img src=x onerror="alert(1)">',
          url: "javascript:alert(1)",
        },
        effects: [
          {
            type: "investigation_handle",
            value: "<script>alert(2)</script>",
            recordedAt: "2026-08-05T10:00:11.500Z",
          },
        ],
      }),
    )

    expect(html).toContain("&lt;img")
    expect(html).toContain("&lt;script&gt;")
    expect(html).not.toContain("<img src=x")
    expect(html).not.toContain("<script>alert")
    expect(html).not.toContain('href="javascript:')
  })

  test("renders only modeled dashboard telemetry", () => {
    const html = render()
    for (const unmodeled of [
      "private command",
      "raw tool output",
      "thinking prose",
      "/private/path",
    ])
      expect(html).not.toContain(unmodeled)
  })
})

describe("run inspector derivation", () => {
  test("partitions a complete wall budget and assigns rounding remainder to gaps", () => {
    const detail = runDetailFixture({
      metrics: {
        durationMs: {
          wall: 12_000,
          setup: 1_000,
          thinking: 4_000,
          tool: 4_000,
          compaction: 0,
          retryWait: 0,
          gaps: 500,
          finalization: 2_000,
        },
      },
    })
    const parts = timeBudget(detail.metrics)!

    expect(parts.reduce((sum, part) => sum + part.value, 0)).toBe(12_000)
    expect(parts.find((part) => part.key === "gaps")?.value).toBe(1000)
  })

  test("returns no budget partition when categories are unavailable", () => {
    const detail = runDetailFixture()
    detail.metrics.durationMs.tool = null
    expect(timeBudget(detail.metrics)).toBeNull()
  })

  test("merges only adjacent thinking blocks in the same turn", () => {
    const spans = [
      span(
        1,
        1,
        "thinking",
        "thinking",
        "2026-08-05T10:00:01.000Z",
        "2026-08-05T10:00:02.000Z",
      ),
      span(
        2,
        1,
        "thinking",
        "thinking",
        "2026-08-05T10:00:02.050Z",
        "2026-08-05T10:00:03.000Z",
      ),
      span(
        3,
        2,
        "thinking",
        "thinking",
        "2026-08-05T10:00:03.000Z",
        "2026-08-05T10:00:04.000Z",
      ),
    ]
    const merged = mergeThinkingSpans(spans)

    expect(merged).toHaveLength(2)
    expect(merged[0]?.blockCount).toBe(2)
    expect(merged[0]?.endedAt).toBe("2026-08-05T10:00:03.000Z")
  })

  test("keeps unterminated thinking spans separate", () => {
    const spans = [
      span(
        1,
        1,
        "thinking",
        "thinking",
        "2026-08-05T10:00:01.000Z",
        null,
        "active",
      ),
      span(
        2,
        1,
        "thinking",
        "thinking",
        "2026-08-05T10:00:05.000Z",
        null,
        "active",
      ),
    ]

    expect(mergeThinkingSpans(spans)).toHaveLength(2)
  })

  test("does not mark completed turns incomplete while a run is active", () => {
    const detail = runDetailFixture({
      run: {
        state: "active",
        endedAt: null,
        telemetry: { schemaVersion: 1, completeness: "partial" },
      },
      event: { status: "processing" },
      entries: [
        turn(1, "2026-08-05T10:00:01.000Z", "2026-08-05T10:00:05.000Z"),
        turn(2, "2026-08-05T10:00:06.000Z", null, "active"),
      ],
    })

    const grouped = groupTimeline(detail)
    expect(grouped.turns.map((group) => group.hasTelemetryGap)).toEqual([
      false,
      false,
    ])
    expect(render(detail)).not.toContain(
      "Telemetry is incomplete for this turn",
    )
  })

  test("marks only an interrupted partial turn as incomplete", () => {
    const detail = runDetailFixture({
      run: {
        state: "interrupted",
        telemetry: { schemaVersion: 1, completeness: "partial" },
      },
      event: { status: "failed" },
      entries: [
        turn(1, "2026-08-05T10:00:01.000Z", "2026-08-05T10:00:05.000Z"),
        turn(
          2,
          "2026-08-05T10:00:06.000Z",
          "2026-08-05T10:00:10.000Z",
          "interrupted",
        ),
      ],
    })

    expect(
      groupTimeline(detail).turns.map((group) => group.hasTelemetryGap),
    ).toEqual([false, true])
  })

  test("uses explicit turn association for retries and compactions", () => {
    const assignedRetry = { ...retry(), turnOrdinal: 2 }
    const unassignedCompaction = { ...compaction(), turnOrdinal: null }
    const detail = runDetailFixture({
      entries: [
        turn(1, "2026-08-05T10:00:01.000Z", "2026-08-05T10:00:05.000Z"),
        turn(2, "2026-08-05T10:00:06.000Z", "2026-08-05T10:00:10.000Z"),
        assignedRetry,
        unassignedCompaction,
      ],
    })

    const grouped = groupTimeline(detail)
    expect(grouped.turns[0]?.phases).toHaveLength(0)
    expect(grouped.turns[1]?.phases).toEqual([assignedRetry])
    expect(grouped.unassigned).toEqual([unassignedCompaction])
  })

  test("renders successful retries directly without error treatment", () => {
    const detail = runDetailFixture({
      entries: [...cleanEntries(), retry()],
      metrics: { retryCount: 1 },
    })
    const first = groupTimeline(detail).turns[0]!
    const html = render(detail, "all")

    expect(first.hasSignal).toBe(true)
    expect(first.needsAttention).toBe(false)
    expect(html).toContain("Model retry 1 of 3")
    expect(html).toContain("phase-row phase-retry phase-succeeded")
    expect(html).not.toContain('class="turn ')
  })

  test("derives model error stop reasons without rendering turn rows", () => {
    const entries = cleanEntries().map((entry) =>
      entry.type === "turn" && entry.ordinal === 2
        ? { ...entry, stopReason: "error" }
        : entry,
    )
    const detail = runDetailFixture({ entries })
    const second = groupTimeline(detail).turns[1]!
    const html = render(detail)

    expect(second.needsAttention).toBe(true)
    expect(html).not.toContain('class="turn ')
    expect(html).not.toContain("turn-health")
  })

  test("keeps activity with an unloaded turn reference unassigned", () => {
    const detail = runDetailFixture({
      entries: [
        turn(1, "2026-08-05T10:00:01.000Z", "2026-08-05T10:00:05.000Z"),
        span(
          1,
          99,
          "tool",
          "Read",
          "2026-08-05T10:00:02.000Z",
          "2026-08-05T10:00:03.000Z",
        ),
      ],
    })
    const grouped = groupTimeline(detail)

    expect(grouped.turns[0]?.spans).toHaveLength(0)
    expect(grouped.unassigned).toHaveLength(1)
    expect(render(detail)).toContain(
      'class="phase-row phase-tool phase-succeeded"',
    )
    expect(render(detail)).not.toContain("Turn membership")
  })

  test("keeps spans with unknown turn membership unassigned", () => {
    const detail = runDetailFixture({
      entries: [
        turn(1, "2026-08-05T10:00:01.000Z", "2026-08-05T10:00:05.000Z"),
        span(
          1,
          null,
          "tool",
          "Read",
          "2026-08-05T10:00:02.000Z",
          "2026-08-05T10:00:03.000Z",
        ),
      ],
    })
    const grouped = groupTimeline(detail)

    expect(grouped.turns[0]?.spans).toHaveLength(0)
    expect(grouped.unassigned).toHaveLength(1)
  })
})
