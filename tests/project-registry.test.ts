import { afterEach, describe, expect, test } from "bun:test"
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import {
  ensureProjectRegistry,
  loadProjectInventory,
  validateProjectRegistryContent,
  validateProjectRegistryWrite,
} from "../src/project-registry.ts"

const paths: string[] = []
afterEach(async () => {
  await Promise.all(
    paths.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

describe("project registry", () => {
  test("creates an empty path list", async () => {
    const root = await temporaryRoot()
    const path = join(root, "config", "projects.yaml")

    await ensureProjectRegistry(path)

    expect(await Bun.file(path).text()).toBe("[]\n")
  })

  test("derives repository metadata from registered paths", async () => {
    const root = await temporaryRoot()
    const repository = join(root, "code", "example")
    initializeRepository(repository)
    const path = join(root, "projects.yaml")
    await writeFile(path, `- ${repository}\n`)

    const inventory = await loadProjectInventory(path, [join(root, "code")])

    const canonicalRepository = await realpath(repository)
    expect(inventory.diagnostics).toEqual([])
    expect(inventory.projects).toEqual([
      {
        path: canonicalRepository,
        remotes: ["git@github.com:owner/example.git"],
        githubRepositories: ["owner/example"],
        defaultBranch: "main",
      },
    ])
  })

  test("limits writes to a valid project registry", async () => {
    const root = await temporaryRoot()
    const repository = join(root, "code", "example")
    initializeRepository(repository)
    const registry = join(root, "config", "projects.yaml")
    await ensureProjectRegistry(registry)
    const content = `- ${repository}\n`

    await expect(
      validateProjectRegistryWrite(registry, content, registry, [
        join(root, "code"),
      ]),
    ).resolves.toBeUndefined()
    await expect(
      validateProjectRegistryWrite(
        join(root, "config", "other.yaml"),
        content,
        registry,
        [join(root, "code")],
      ),
    ).rejects.toThrow("limited to the project registry")
  })

  test("rejects invalid and out-of-root registry writes", async () => {
    const root = await temporaryRoot()
    const repository = join(root, "outside", "example")
    initializeRepository(repository)

    await expect(
      validateProjectRegistryContent("projects: []\n", [join(root, "code")]),
    ).rejects.toThrow("YAML list")
    await expect(
      validateProjectRegistryContent(`- ${repository}\n`, [join(root, "code")]),
    ).rejects.toThrow("outside configured project roots")
  })
})

async function temporaryRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "intake-projects-"))
  paths.push(root)
  return root
}

function initializeRepository(path: string): void {
  runGit(["init", "--initial-branch=main", path])
  runGit([
    "-C",
    path,
    "remote",
    "add",
    "origin",
    "git@github.com:owner/example.git",
  ])
  runGit([
    "-C",
    path,
    "symbolic-ref",
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
  ])
}

function runGit(args: string[]): void {
  const result = Bun.spawnSync(["git", ...args], {
    stdout: "ignore",
    stderr: "pipe",
  })
  if (result.exitCode !== 0)
    throw new Error(new TextDecoder().decode(result.stderr))
}
