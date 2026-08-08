import { afterEach, beforeEach, describe, expect, test } from "bun:test"
import {
  chmod,
  mkdtemp,
  readFile,
  realpath,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { CommandPolicy } from "../src/agent/command-policy.ts"
import { testConfig } from "./fixtures/config.ts"

let root = ""
let bin = ""
let log = ""
let policy: CommandPolicy

beforeEach(async () => {
  root = await realpath(await mkdtemp(join(tmpdir(), "intake-policy-")))
  bin = join(root, "bin")
  log = join(root, "calls.log")
  await Bun.write(log, "")
  await Bun.$`mkdir -p ${bin}`.quiet()
  for (const name of ["aven", "workmux", "tmux", "gh", "git", "rg", "fd"]) {
    const path = join(bin, name)
    const body = `#!/bin/sh\nprintf '%s' '${name}' >> '${log}'\nfor argument in "$@"; do printf '|%s' "$argument" >> '${log}'; done\nprintf '\\n' >> '${log}'\nif [ "$1" = slow ]; then /bin/sleep 2; fi\nif [ "$1" = large ]; then i=0; while [ $i -lt 400 ]; do printf '0123456789'; i=$((i+1)); done; fi\nif [ '${name}' = aven ]; then while IFS= read -r line || [ -n "$line" ]; do printf 'stdin:%s\\n' "$line" >> '${log}'; done; printf 'Created APP-TEST\\n'; else while IFS= read -r line; do printf '%s\\n' "$line"; done; printf 'token=secret-value\\n'; fi\n`
    await writeFile(path, body)
    await chmod(path, 0o755)
  }
  const config = testConfig(root, bin)
  config.commands.timeout_seconds = 5
  policy = new CommandPolicy(config, [await realpath(root)])
})

afterEach(async () => {
  await rm(root, { recursive: true, force: true })
})

describe("restricted command policy", () => {
  test("accepts quoted arguments and authorized pipelines", async () => {
    const parsed = policy.parseAndAuthorize(
      'aven search "login issue" | rg "APP-*"',
      root,
    )
    expect(parsed.stages).toEqual([
      ["aven", "search", "login issue"],
      ["rg", "APP-*"],
    ])
    const result = await policy.execute(
      'aven search "login issue" | rg "APP-*"',
      root,
    )
    expect(result.exitCode).toBe(0)
    expect(result.stdout).toContain("Created APP-TEST")
    expect(result.stdout).toContain("[REDACTED]")
    expect(await readFile(log, "utf8")).toContain("aven|search|login issue")
  })

  test("passes bounded multiline stdin without shell syntax", async () => {
    const input = "First paragraph.\n\nSecond paragraph."
    const result = await policy.execute(
      'aven add "Task title" --description-stdin',
      root,
      undefined,
      input,
    )
    expect(result.exitCode).toBe(0)
    expect(await readFile(log, "utf8")).toContain(
      "stdin:First paragraph.\nstdin:\nstdin:Second paragraph.\n",
    )
  })

  test("bounds command stdin", async () => {
    await expect(
      policy.execute(
        "aven add Task --description-stdin",
        root,
        undefined,
        "x".repeat(256 * 1024 + 1),
      ),
    ).rejects.toThrow("stdin exceeds policy bounds")
  })

  test("preserves newlines inside quoted arguments", () => {
    const command = `workmux add walkingmate-email -p "Investigate this email.

<untrusted-email>
Body text
</untrusted-email>"`
    expect(policy.parseAndAuthorize(command, root).stages).toEqual([
      [
        "workmux",
        "add",
        "walkingmate-email",
        "-p",
        "Investigate this email.\n\n<untrusted-email>\nBody text\n</untrusted-email>",
      ],
    ])
  })

  test.each([
    "aven search x; rg y",
    "aven search x && rg y",
    "aven search $(id)",
    "aven search $HOME",
    "aven search x > out",
    "VALUE=x aven search y",
    "aven search *.md",
    "(aven search x)",
    "aven search x;",
    "aven search x # hidden",
    "aven search x &",
    "aven search x\nrg y",
  ])("rejects forbidden shell syntax: %s", (command) => {
    expect(() => policy.parseAndAuthorize(command, root)).toThrow()
  })

  test("authorizes every pipeline stage before execution", async () => {
    await expect(
      policy.execute("aven search x | curl example.com", root),
    ).rejects.toThrow("not allowed")
    expect(await readFile(log, "utf8")).toBe("")
  })

  test("passes literal arguments for every command on the executable allowlist", () => {
    expect(
      policy.parseAndAuthorize("aven delete APP-TEST --all", root).stages,
    ).toEqual([["aven", "delete", "APP-TEST", "--all"]])
    expect(
      policy.parseAndAuthorize("gh issue close 42 --repo example/project", root)
        .stages,
    ).toEqual([["gh", "issue", "close", "42", "--repo", "example/project"]])
    expect(() => policy.parseAndAuthorize("aven search x", tmpdir())).toThrow(
      "outside approved roots",
    )
  })

  test("rejects canonical working-directory escapes", async () => {
    const escapedDirectory = await mkdtemp(join(tmpdir(), "intake-escape-"))
    const cwdLink = join(root, "cwd-link")
    await symlink(escapedDirectory, cwdLink)
    try {
      await expect(policy.execute("aven search x", cwdLink)).rejects.toThrow(
        "outside approved roots",
      )
    } finally {
      await rm(escapedDirectory, { recursive: true, force: true })
    }
  })

  test("enforces wall-clock timeout and output bounds", async () => {
    const timeoutConfig = testConfig(root, bin)
    timeoutConfig.commands.timeout_seconds = 1
    const timeoutPolicy = new CommandPolicy(timeoutConfig, [
      await realpath(root),
    ])
    await expect(timeoutPolicy.execute("rg slow", root)).rejects.toThrow()
    const result = await policy.execute("rg large", root)
    expect(result.stdout.length).toBeLessThanOrEqual(1024)
    expect(result.truncated).toBe(true)
  })
})
