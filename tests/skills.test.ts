import { afterEach, describe, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { DefaultResourceLoader } from "@earendil-works/pi-coding-agent"
import { validateSkills } from "../src/agent/skills.ts"
import { testConfig } from "./fixtures/config.ts"

const paths: string[] = []
afterEach(async () => {
  await Promise.all(
    paths.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

describe("skill boundaries", () => {
  test("accepts canonical references under approved roots and preserves invocation metadata", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-skills-"))
    paths.push(root)
    const wrappers = join(root, "wrappers")
    const references = join(root, "references")
    const wrapper = join(wrappers, "github-investigation")
    const canonical = join(references, "workmux")
    await mkdir(wrapper, { recursive: true })
    await mkdir(canonical, { recursive: true })
    await writeFile(
      join(wrapper, "SKILL.md"),
      "---\nname: github-investigation\ndescription: Investigate GitHub intake.\n---\nRead references/workmux/SKILL.md.\n",
    )
    await writeFile(
      join(canonical, "SKILL.md"),
      "---\nname: workmux\ndescription: Worktree reference.\ndisable-model-invocation: true\n---\nReference only.\n",
    )
    await mkdir(join(wrapper, "references"))
    await symlink(canonical, join(wrapper, "references", "workmux"))
    const config = testConfig(root, root)
    config.skills = { directories: [wrappers], approvedRoots: [root] }
    const validation = await validateSkills(config)
    expect(validation.diagnostics).toEqual([])
    expect(validation.skillPaths).toEqual([wrapper])

    const loader = new DefaultResourceLoader({
      cwd: root,
      agentDir: root,
      additionalSkillPaths: [wrapper, canonical],
      noSkills: true,
      noExtensions: true,
      noContextFiles: true,
    })
    await loader.reload()
    const skills = loader.getSkills().skills
    expect(
      skills.find((skill) => skill.name === "github-investigation")
        ?.disableModelInvocation,
    ).toBe(false)
    expect(
      skills.find((skill) => skill.name === "workmux")?.disableModelInvocation,
    ).toBe(true)
  })

  test("rejects wrapper links whose canonical target is outside approved roots", async () => {
    const root = await mkdtemp(join(tmpdir(), "intake-skills-"))
    const outside = await mkdtemp(join(tmpdir(), "intake-outside-"))
    paths.push(root, outside)
    const wrapper = join(root, "wrappers", "mail")
    await mkdir(join(wrapper, "references"), { recursive: true })
    await writeFile(
      join(wrapper, "SKILL.md"),
      "---\nname: mail\ndescription: Triage mail.\n---\n",
    )
    await writeFile(
      join(outside, "SKILL.md"),
      "---\nname: private\ndescription: Private.\n---\n",
    )
    await symlink(outside, join(wrapper, "references", "private"))
    const config = testConfig(root, root)
    config.skills = {
      directories: [join(root, "wrappers")],
      approvedRoots: [root],
    }
    const validation = await validateSkills(config)
    expect(validation.skillPaths).toEqual([])
    expect(validation.diagnostics.join("\n")).toContain(
      "outside approved roots",
    )
  })
})
