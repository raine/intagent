import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import {
  mkdir,
  mkdtemp,
  realpath,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import {
  MAX_READ_FILE_BYTES,
  MAX_READ_LINES,
  MAX_READ_PATH_BYTES,
  ReadPolicy,
} from "../src/agent/read-policy.ts"

let root = ""
let project = ""
let skills = ""
let outside = ""
let policy: ReadPolicy

beforeEach(async () => {
  root = await realpath(await mkdtemp(join(tmpdir(), "intake-read-")))
  project = join(root, "project")
  skills = join(root, "skills")
  outside = await realpath(await mkdtemp(join(tmpdir(), "intake-private-")))
  await mkdir(project)
  await mkdir(skills)
  policy = new ReadPolicy(
    [await realpath(project), await realpath(skills)],
    1024,
  )
})

afterEach(async () => {
  await Promise.all([
    rm(root, { recursive: true, force: true }),
    rm(outside, { recursive: true, force: true }),
  ])
})

describe("restricted read policy", () => {
  test("reads project and approved skill files with line numbers", async () => {
    await writeFile(join(project, "notes.txt"), "alpha\nbeta")
    const skillFile = join(skills, "SKILL.md")
    await writeFile(skillFile, "rules\napply")

    const projectResult = await policy.read({ path: "notes.txt" }, project)
    const skillResult = await policy.read({ path: skillFile }, project)

    expect(projectResult.text).toBe("1\talpha\n2\tbeta")
    expect(projectResult.path).toBe(await realpath(join(project, "notes.txt")))
    expect(skillResult.text).toBe("1\trules\n2\tapply")
  })

  test("reads bounded line ranges with continuation details", async () => {
    await writeFile(join(project, "lines.txt"), "one\ntwo\nthree\nfour")

    const result = await policy.read(
      { path: "lines.txt", offset: 2, limit: 2 },
      project,
    )

    expect(result.text).toContain("2\ttwo\n3\tthree")
    expect(result.text).toContain("Showing lines 2-3 of 4")
    expect(result.text).toContain("offset=4")
    expect(result.truncated).toBe(true)
    expect(result.endLine).toBe(3)
  })

  test("enforces path, file, output, and range bounds", async () => {
    const largeFile = join(project, "large.txt")
    await writeFile(largeFile, Buffer.alloc(MAX_READ_FILE_BYTES + 1, "x"))
    await expect(policy.read({ path: largeFile }, project)).rejects.toThrow(
      "file exceeds",
    )
    await expect(
      policy.read({ path: "x".repeat(MAX_READ_PATH_BYTES + 1) }, project),
    ).rejects.toThrow("path length")
    await expect(
      policy.read({ path: "missing", limit: MAX_READ_LINES + 1 }, project),
    ).rejects.toThrow("limit")

    await writeFile(join(project, "output.txt"), "x".repeat(2000))
    const boundedPolicy = new ReadPolicy([await realpath(project)], 256)
    const result = await boundedPolicy.read({ path: "output.txt" }, project)
    expect(Buffer.byteLength(result.text)).toBeLessThanOrEqual(256)
    expect(result.text).toContain("Output truncated")
    expect(result.truncated).toBe(true)

    await writeFile(
      join(project, "many-lines.txt"),
      Array(100).fill("abcdefghij").join("\n"),
    )
    const manyLines = await boundedPolicy.read(
      { path: "many-lines.txt" },
      project,
    )
    expect(Buffer.byteLength(manyLines.text)).toBeLessThanOrEqual(256)
    expect(manyLines.text).toContain("Use offset=")
  })

  test("authorizes canonical symlink targets beneath an approved root", async () => {
    const target = join(skills, "reference.md")
    const link = join(project, "reference.md")
    await writeFile(target, "approved")
    await symlink(target, link)

    const result = await policy.read({ path: link }, project)

    expect(result.path).toBe(await realpath(target))
    expect(result.text).toBe("1\tapproved")
  })

  test("authorizes configured roots that are symlinks", async () => {
    const target = join(skills, "private-skill.md")
    const configuredRoot = join(outside, "configured-skills")
    await writeFile(target, "private skill")
    await symlink(skills, configuredRoot)
    const aliasedPolicy = new ReadPolicy(
      [configuredRoot, await realpath(configuredRoot)],
      1024,
    )

    const result = await aliasedPolicy.read(
      { path: join(configuredRoot, "private-skill.md") },
      project,
    )

    expect(result.path).toBe(await realpath(target))
    expect(result.text).toBe("1\tprivate skill")
  })

  test("rejects canonical escapes and unavailable or non-file paths", async () => {
    const secret = join(outside, "secret.txt")
    await writeFile(secret, "sensitive")
    await symlink(secret, join(project, "escape.txt"))

    await expect(
      policy.read({ path: join(project, "escape.txt") }, project),
    ).rejects.toThrow("canonical file path is outside approved roots")
    await expect(policy.read({ path: secret }, project)).rejects.toThrow(
      "file is outside approved roots",
    )
    await expect(policy.read({ path: "missing.txt" }, project)).rejects.toThrow(
      "unavailable",
    )
    await expect(policy.read({ path: "." }, project)).rejects.toThrow(
      "not a regular file",
    )
  })
})
