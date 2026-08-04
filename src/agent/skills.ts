import { lstat, readdir, realpath } from "node:fs/promises"
import { dirname, join, resolve } from "node:path"
import type { IntakeConfig } from "../config.ts"
import { canonicalRoots, expandPath, isWithin } from "../config.ts"

export interface SkillValidation {
  skillPaths: string[]
  diagnostics: string[]
}

export async function validateSkills(
  config: IntakeConfig,
): Promise<SkillValidation> {
  const approvedRoots = await canonicalRoots(config.skills.approved_roots)
  const skillPaths: string[] = []
  const diagnostics: string[] = []

  for (const configuredDirectory of config.skills.directories) {
    const directory = expandPath(configuredDirectory)
    let entries
    try {
      entries = await readdir(directory, { withFileTypes: true })
    } catch (error) {
      diagnostics.push(
        `${directory}: ${error instanceof Error ? error.message : String(error)}`,
      )
      continue
    }
    for (const entry of entries) {
      if (entry.name.startsWith(".")) continue
      const candidate = join(directory, entry.name)
      let valid = true
      try {
        const canonical = await realpath(candidate)
        const candidateStat = await lstat(canonical)
        if (!candidateStat.isDirectory()) {
          diagnostics.push(`${candidate}: skill entry is not a directory`)
          valid = false
        }
        if (!isWithin(canonical, approvedRoots)) {
          diagnostics.push(
            `${candidate}: canonical skill path is outside approved roots`,
          )
          valid = false
        }
        if (valid) {
          const skillFile = join(canonical, "SKILL.md")
          const skillStat = await lstat(skillFile).catch(() => null)
          if (!skillStat?.isFile()) {
            diagnostics.push(
              `${candidate}: SKILL.md is missing or is not a file`,
            )
            valid = false
          }
        }
        if (valid) {
          const errors = await validateLinks(candidate, approvedRoots)
          diagnostics.push(...errors)
          valid = errors.length === 0
        }
      } catch (error) {
        diagnostics.push(
          `${candidate}: ${error instanceof Error ? error.message : String(error)}`,
        )
        valid = false
      }
      if (valid) skillPaths.push(candidate)
    }
  }
  return { skillPaths, diagnostics }
}

async function validateLinks(
  root: string,
  approvedRoots: string[],
): Promise<string[]> {
  const errors: string[] = []
  const pending = [root]
  const visited = new Set<string>()
  while (pending.length > 0) {
    const path = pending.pop()
    if (!path) continue
    const lexical = resolve(path)
    if (visited.has(lexical)) continue
    visited.add(lexical)
    const stat = await lstat(path)
    if (stat.isSymbolicLink()) {
      let target: string
      try {
        target = await realpath(path)
      } catch (error) {
        errors.push(
          `${path}: broken symbolic link: ${error instanceof Error ? error.message : String(error)}`,
        )
        continue
      }
      if (!isWithin(target, approvedRoots)) {
        errors.push(
          `${path}: symbolic link target is outside approved roots: ${target}`,
        )
        continue
      }
      const targetStat = await lstat(target)
      if (targetStat.isDirectory()) {
        const skillFile = await lstat(join(target, "SKILL.md")).catch(
          () => null,
        )
        if (!skillFile?.isFile()) {
          errors.push(`${path}: linked skill target has no SKILL.md file`)
          continue
        }
        pending.push(target)
      }
      continue
    }
    if (!stat.isDirectory()) continue
    for (const entry of await readdir(path)) pending.push(join(path, entry))
  }
  return errors
}

export function skillWorkingDirectory(skillPath: string): string {
  return dirname(skillPath)
}
