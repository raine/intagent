import { mkdir, readFile, realpath, writeFile } from "node:fs/promises"
import { dirname, resolve } from "node:path"
import { parse, stringify } from "yaml"
import { canonicalRoots, expandPath, isWithin } from "./config.ts"

export const MAX_PROJECT_REGISTRY_BYTES = 64 * 1024
export const MAX_PROJECTS = 1000

export interface ProjectInventoryEntry {
  path: string
  remotes: string[]
  githubRepositories: string[]
  defaultBranch: string | null
}

export interface ProjectInventory {
  projects: ProjectInventoryEntry[]
  diagnostics: string[]
}

export async function ensureProjectRegistry(path: string): Promise<void> {
  if (await Bun.file(path).exists()) return
  await mkdir(dirname(path), { recursive: true, mode: 0o700 })
  await writeFile(path, stringify([]), { mode: 0o600, flag: "wx" }).catch(
    (error: NodeJS.ErrnoException) => {
      if (error.code !== "EEXIST") throw error
    },
  )
}

export async function loadProjectInventory(
  path: string,
  projectRoots: string[],
): Promise<ProjectInventory> {
  await ensureProjectRegistry(path)
  let paths: string[]
  try {
    paths = parseProjectPaths(await readRegistry(path))
  } catch (error) {
    return { projects: [], diagnostics: [errorMessage(error)] }
  }

  const roots = await canonicalRoots(projectRoots)
  const projects: ProjectInventoryEntry[] = []
  const diagnostics: string[] = []
  const seen = new Set<string>()
  for (const pathValue of paths) {
    try {
      const project = await inspectProject(pathValue, roots)
      if (seen.has(project.path)) {
        diagnostics.push(`duplicate project path: ${pathValue}`)
        continue
      }
      seen.add(project.path)
      projects.push(project)
    } catch (error) {
      diagnostics.push(`${pathValue}: ${errorMessage(error)}`)
    }
  }
  return { projects, diagnostics }
}

export async function validateProjectRegistryWrite(
  requestedPath: string,
  content: string,
  registryPath: string,
  projectRoots: string[],
): Promise<void> {
  const requested = resolve(expandPath(requestedPath))
  const registry = resolve(expandPath(registryPath))
  const canonicalRegistry = await realpath(registry)
  if (requested !== registry && requested !== canonicalRegistry)
    throw new Error("write access is limited to the project registry")
  await validateProjectRegistryContent(content, projectRoots)
}

export async function validateProjectRegistryContent(
  content: string,
  projectRoots: string[],
): Promise<ProjectInventoryEntry[]> {
  if (Buffer.byteLength(content) > MAX_PROJECT_REGISTRY_BYTES)
    throw new Error("project registry exceeds its size limit")
  const paths = parseProjectPaths(content)
  const roots = await canonicalRoots(projectRoots)
  const projects: ProjectInventoryEntry[] = []
  const seen = new Set<string>()
  for (const path of paths) {
    const project = await inspectProject(path, roots)
    if (seen.has(project.path))
      throw new Error(`duplicate project path: ${path}`)
    seen.add(project.path)
    projects.push(project)
  }
  return projects
}

async function readRegistry(path: string): Promise<string> {
  const content = await readFile(path, "utf8")
  if (Buffer.byteLength(content) > MAX_PROJECT_REGISTRY_BYTES)
    throw new Error("project registry exceeds its size limit")
  return content
}

async function inspectProject(
  pathValue: string,
  roots: string[],
): Promise<ProjectInventoryEntry> {
  const requested = expandPath(pathValue)
  let canonical: string
  try {
    canonical = await realpath(requested)
  } catch {
    throw new Error("path is unavailable")
  }
  if (!isWithin(canonical, roots))
    throw new Error("path is outside configured project roots")

  const repositoryRoot = await gitOutput(canonical, [
    "rev-parse",
    "--show-toplevel",
  ])
  if (!repositoryRoot) throw new Error("path is not a Git repository")
  const repositoryPath = await realpath(repositoryRoot)
  if (!isWithin(repositoryPath, roots))
    throw new Error("Git repository is outside configured project roots")

  const remoteOutput =
    (await gitOutput(repositoryPath, [
      "config",
      "--get-regexp",
      "^remote\\..*\\.url$",
    ])) ?? ""
  const remotes = [
    ...new Set(
      remoteOutput
        .split("\n")
        .map((line) => line.trim().split(/\s+/, 2)[1])
        .filter((value): value is string => Boolean(value)),
    ),
  ]
  const defaultRef = await gitOutput(repositoryPath, [
    "symbolic-ref",
    "--quiet",
    "--short",
    "refs/remotes/origin/HEAD",
  ])

  return {
    path: repositoryPath,
    remotes,
    githubRepositories: [
      ...new Set(
        remotes
          .map(githubRepository)
          .filter((value): value is string => Boolean(value)),
      ),
    ],
    defaultBranch: defaultRef?.replace(/^origin\//, "") ?? null,
  }
}

async function gitOutput(cwd: string, args: string[]): Promise<string | null> {
  const process = Bun.spawn(["git", "-C", cwd, ...args], {
    stdout: "pipe",
    stderr: "ignore",
  })
  const [exitCode, output] = await Promise.all([
    process.exited,
    new Response(process.stdout).text(),
  ])
  if (exitCode !== 0) return null
  if (Buffer.byteLength(output) > MAX_PROJECT_REGISTRY_BYTES)
    throw new Error("Git metadata exceeds its size limit")
  return output.trim()
}

function parseProjectPaths(content: string): string[] {
  if (Buffer.byteLength(content) > MAX_PROJECT_REGISTRY_BYTES)
    throw new Error("project registry exceeds its size limit")
  let value: unknown
  try {
    value = parse(content, { uniqueKeys: true, maxAliasCount: 0 })
  } catch (error) {
    throw new Error(`invalid project registry YAML: ${errorMessage(error)}`)
  }
  if (!Array.isArray(value))
    throw new Error("project registry must be a YAML list of paths")
  if (value.length > MAX_PROJECTS)
    throw new Error(`project registry exceeds its ${MAX_PROJECTS} path limit`)
  return value.map((path, index) => {
    if (typeof path !== "string" || path.trim().length === 0)
      throw new Error(`project registry entry ${index + 1} must be a path`)
    return path
  })
}

function githubRepository(remote: string): string | null {
  const scp = remote.match(/^git@github\.com:([^/]+\/[^/]+?)(?:\.git)?$/i)
  if (scp?.[1]) return scp[1].replace(/\.git$/i, "")
  try {
    const url = new URL(remote)
    if (url.hostname.toLowerCase() !== "github.com") return null
    const repository = url.pathname.replace(/^\//, "").replace(/\.git$/i, "")
    return repository.split("/").length === 2 ? repository : null
  } catch {
    return null
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
