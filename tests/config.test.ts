import { afterEach, describe, expect, test } from "bun:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { initializePrivateConfig, loadConfig } from "../src/config.ts"

const paths: string[] = []
afterEach(async () => {
  await Promise.all(
    paths.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

describe("configuration", () => {
  test("rejects unknown keys and duplicate YAML keys", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-config-"))
    paths.push(root)
    const path = join(root, "config.yaml")
    await writeFile(path, "version: 1\nunknown: true\n")
    await expect(loadConfig(path)).rejects.toThrow("Unrecognized key")
    await writeFile(path, "version: 1\nversion: 1\n")
    await expect(loadConfig(path)).rejects.toThrow("Map keys must be unique")
  })

  test("initializes generic private configuration with restrictive mode", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-init-"))
    paths.push(root)
    const path = join(root, "private", "config.yaml")
    const result = await initializePrivateConfig(path)
    expect(result.created).toEqual([path])
    const contents = await readFile(path, "utf8")
    expect(contents).toContain("projectRoots:")
    expect(contents).toContain("sources: []")
    const config = await loadConfig(path)
    expect(config.triage).toMatchObject({
      maxTurns: 50,
      timeoutMinutes: 30,
      maxAttempts: 3,
    })
  })
})
