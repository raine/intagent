import { Database } from "bun:sqlite"
import { describe, expect, test } from "bun:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { loadConfig } from "../src/config.ts"
import { pollRequestSchema, pollResponseSchema } from "../src/protocol.ts"
import { compatibilityFixtureFiles } from "./fixtures/generate.ts"

const fixtureRoot = join(import.meta.dir, "fixtures")

describe("Phase 0 compatibility fixtures", () => {
  test("match deterministic regeneration", async () => {
    for (const [relativePath, expected] of compatibilityFixtureFiles())
      expect(await readFile(join(fixtureRoot, relativePath), "utf8")).toBe(
        expected,
      )
  })

  test("capture valid and duplicate-key configuration", async () => {
    const config = await loadConfig(join(fixtureRoot, "config/valid.yaml"))
    expect(config).toMatchObject({
      version: 1,
      triage: { model: "gpt-5.6-luna", thinking_level: "max" },
      sources: [
        { name: "fastmail", item_limit: 100 },
        { name: "github", item_limit: 100 },
      ],
    })
    expect(
      loadConfig(join(fixtureRoot, "config/invalid-duplicate.yaml")),
    ).rejects.toThrow("Map keys must be unique")
  })

  test("capture exact source protocol names and bounds", async () => {
    const request = JSON.parse(
      await readFile(join(fixtureRoot, "protocol/poll-request.json"), "utf8"),
    )
    const response = JSON.parse(
      await readFile(join(fixtureRoot, "protocol/poll-response.json"), "utf8"),
    )
    expect(pollRequestSchema.parse(request)).toEqual(request)
    expect(pollResponseSchema.parse(response)).toEqual(response)
    expect(
      pollRequestSchema.safeParse({ ...request, itemLimit: 1001 }).success,
    ).toBe(false)
    expect(
      pollRequestSchema.safeParse({
        ...request,
        item_limit: request.itemLimit,
        itemLimit: undefined,
      }).success,
    ).toBe(false)
    expect(
      pollResponseSchema.safeParse({
        ...response,
        items: [
          {
            ...response.items[0],
            entityId: "x".repeat(1025),
          },
        ],
      }).success,
    ).toBe(false)
  })

  test("keeps configuration edge cases aligned with Zod", async () => {
    const corpus = JSON.parse(
      await readFile(join(fixtureRoot, "config/differential.json"), "utf8"),
    ) as {
      cases: Array<{
        sourceYaml: string
        accepted: boolean
        optionValue?: unknown
      }>
    }
    const original = await readFile(
      join(fixtureRoot, "config/valid.yaml"),
      "utf8",
    )
    const root = await mkdtemp(join(tmpdir(), "intake-config-differential-"))
    const path = join(root, "config.yaml")
    try {
      for (const testCase of corpus.cases) {
        const yaml = original.replace(
          /sources:\n[\s\S]*?\ntriage:/,
          `${testCase.sourceYaml}\ntriage:`,
        )
        await writeFile(path, yaml)
        let config: Awaited<ReturnType<typeof loadConfig>> | undefined
        try {
          config = await loadConfig(path)
        } catch {}
        expect(config !== undefined).toBe(testCase.accepted)
        if (testCase.optionValue !== undefined)
          expect(Object.values(config?.sources[0]?.options ?? {})[0]).toEqual(
            testCase.optionValue,
          )
      }
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test("keeps protocol edge cases aligned with Zod", async () => {
    const corpus = JSON.parse(
      await readFile(join(fixtureRoot, "protocol/differential.json"), "utf8"),
    ) as {
      request: Array<{
        field: string
        value: unknown
        accepted: boolean
      }>
      item: Array<{
        field: string
        value?: unknown
        repeat?: string
        count?: number
        accepted: boolean
      }>
      rawRequestNumbers: Array<{ value: string; accepted: boolean }>
    }
    const request = JSON.parse(
      await readFile(join(fixtureRoot, "protocol/poll-request.json"), "utf8"),
    )
    const response = JSON.parse(
      await readFile(join(fixtureRoot, "protocol/poll-response.json"), "utf8"),
    )
    for (const testCase of corpus.request) {
      expect(
        pollRequestSchema.safeParse({
          ...request,
          [testCase.field]: testCase.value,
        }).success,
      ).toBe(testCase.accepted)
    }
    for (const testCase of corpus.item) {
      const value =
        testCase.repeat?.repeat(testCase.count ?? 0) ?? testCase.value
      expect(
        pollResponseSchema.safeParse({
          ...response,
          items: [{ ...response.items[0], [testCase.field]: value }],
        }).success,
      ).toBe(testCase.accepted)
    }
    for (const testCase of corpus.rawRequestNumbers) {
      const raw = `{"protocolVersion":1,"source":"source","checkpoint":${testCase.value},"now":"2026-08-07T10:00:00Z","itemLimit":1,"options":{}}`
      let accepted = false
      try {
        accepted = pollRequestSchema.safeParse(JSON.parse(raw)).success
      } catch {}
      expect(accepted).toBe(testCase.accepted)
    }
  })

  test("reconstruct every captured schema with canonical metadata", async () => {
    const expectations = JSON.parse(
      await readFile(
        join(fixtureRoot, "database/schema-expectations.json"),
        "utf8",
      ),
    ) as Record<string, { migrations: Array<{ version: number }> }>
    for (let version = 0; version <= 7; version++) {
      const fixture = await readFile(
        join(fixtureRoot, `database/schema-v${version}.sql`),
        "utf8",
      )
      const database = new Database(":memory:", { strict: true })
      database.exec(fixture)
      expect(database.query("PRAGMA integrity_check").get()).toEqual({
        integrity_check: "ok",
      })
      expect(
        database
          .query("SELECT version FROM schema_migrations ORDER BY version")
          .all(),
      ).toEqual(
        expectations[`v${version}`]?.migrations.map(({ version }) => ({
          version,
        })) ?? [],
      )
      database.close()
    }
  })
})
