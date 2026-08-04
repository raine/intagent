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

  test("rejects camelCase configuration and source option keys", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-config-"))
    paths.push(root)
    const path = join(root, "config.yaml")
    await initializePrivateConfig(path, { XDG_STATE_HOME: join(root, "state") })
    const contents = await readFile(path, "utf8")

    await writeFile(path, contents.replace("project_roots:", "projectRoots:"))
    await expect(loadConfig(path)).rejects.toThrow("projectRoots")

    await writeFile(path, contents.replace("approved_roots:", "approvedRoots:"))
    await expect(loadConfig(path)).rejects.toThrow("approvedRoots")

    await writeFile(
      path,
      contents.replace(
        "sources: []",
        "sources:\n  - name: fastmail\n    command: intake-fastmail-source\n    options:\n      mailboxId: inbox",
      ),
    )
    await expect(loadConfig(path)).rejects.toThrow("mailboxId")
  })

  test("loads configured triage model and thinking level", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-config-"))
    paths.push(root)
    const path = join(root, "config.yaml")
    await initializePrivateConfig(path, { XDG_STATE_HOME: join(root, "state") })
    const contents = await readFile(path, "utf8")
    await writeFile(
      path,
      contents
        .replace("model: gpt-5.6-luna", "model: gpt-5.6-sol")
        .replace("thinking_level: max", "thinking_level: high"),
    )

    const config = await loadConfig(path)
    expect(config.triage.model).toBe("gpt-5.6-sol")
    expect(config.triage.thinking_level).toBe("high")
  })

  test("initializes generic private configuration with restrictive mode", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-init-"))
    paths.push(root)
    const path = join(root, "private", "config.yaml")
    const result = await initializePrivateConfig(path, {
      XDG_STATE_HOME: join(root, "state"),
    })
    expect(result.created).toEqual([path])
    const contents = await readFile(path, "utf8")
    expect(contents).toContain("project_roots:")
    expect(contents).toContain("approved_roots:")
    expect(contents).toContain("sources: []")
    const config = await loadConfig(path)
    expect(config.state).toEqual({
      database: join(root, "state", "intake", "intake.sqlite"),
      logs: join(root, "state", "intake", "logs"),
    })
    expect(config.triage).toMatchObject({
      model: "gpt-5.6-luna",
      thinking_level: "max",
      max_turns: 50,
      timeout_minutes: 30,
      max_attempts: 3,
    })
  })
})
