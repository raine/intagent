import { afterEach, describe, expect, test } from "bun:test"
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { IntakeDatabase } from "../src/database.ts"
import { pollSource } from "../src/source-runner.ts"
import { testConfig } from "./fixtures/config.ts"

const paths: string[] = []
afterEach(async () => {
  await Promise.all(
    paths.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

describe("external source protocol", () => {
  test("queues a valid versioned response and commits its checkpoint", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-source-"))
    paths.push(root)
    const command = join(root, "source")
    const response = {
      protocolVersion: 1,
      checkpoint: { cursor: "next" },
      items: [
        {
          entityId: "external:1",
          revisionId: "revision:1",
          kind: "generic",
          title: "External item",
          body: "Payload",
          occurredAt: "2026-08-03T10:00:00.000Z",
          metadata: {},
        },
      ],
    }
    await writeFile(
      command,
      `#!/bin/sh\nread request\nprintf '%s\\n' '${JSON.stringify(response)}'\n`,
    )
    await chmod(command, 0o755)
    const config = testConfig(root, root)
    const source = {
      name: "fake",
      command,
      args: [],
      intervalSeconds: 60,
      timeoutSeconds: 5,
      itemLimit: 10,
      environment: [],
      options: {},
    }
    config.sources = [source]
    const database = new IntakeDatabase(":memory:")
    try {
      expect(
        await pollSource(
          source,
          config,
          database,
          new Date("2026-08-03T10:01:00.000Z"),
        ),
      ).toBe(1)
      expect(database.sourceCheckpoint("fake")).toEqual({ cursor: "next" })
      expect(database.claimNext()?.entityId).toBe("external:1")
    } finally {
      database.close()
    }
  })

  test("does not advance checkpoint after malformed stdout", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-source-"))
    paths.push(root)
    const command = join(root, "source")
    await writeFile(
      command,
      "#!/bin/sh\nread request\nprintf 'diagnostic' >&2\nprintf 'not json\\n'\n",
    )
    await chmod(command, 0o755)
    const config = testConfig(root, root)
    const source = {
      name: "fake",
      command,
      args: [],
      intervalSeconds: 60,
      timeoutSeconds: 5,
      itemLimit: 10,
      environment: [],
      options: {},
    }
    const database = new IntakeDatabase(":memory:")
    try {
      await expect(pollSource(source, config, database)).rejects.toThrow(
        "not one JSON response",
      )
      expect(database.sourceCheckpoint("fake")).toBeNull()
      expect(database.sourceStatuses()[0]?.lastError).toContain(
        "not one JSON response",
      )
    } finally {
      database.close()
    }
  })
})
