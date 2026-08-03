#!/usr/bin/env bun
import { lstat, readFile, realpath } from "node:fs/promises"
import { dirname, join, resolve } from "node:path"
import { expandPath, isWithin } from "../config.ts"
import {
  sourceMain,
  type IntakeItem,
  type PollRequest,
  type PollResponse,
} from "../protocol.ts"

interface RepoCheckpoint {
  createdAt: string
  numbersAtTimestamp: number[]
}

interface GithubCheckpoint {
  repositories: Record<string, RepoCheckpoint>
}

interface GithubItem {
  number: number
  title: string
  body: string | null
  html_url: string
  created_at: string
  updated_at: string
  user: { login: string } | null
  labels: Array<{ name?: string } | string>
  pull_request?: { url: string }
}

const PAGE_SIZE = 100
const DEFAULT_MAX_PAGES = 100

export async function pollGithub(
  request: PollRequest,
  fetcher: typeof fetch = fetch,
): Promise<PollResponse> {
  const token = process.env.GITHUB_TOKEN
  if (!token) throw new Error("GITHUB_TOKEN is required")
  const roots = stringArrayOption(request, "projectRoots")
  if (roots.length === 0)
    throw new Error(
      "GitHub source options.projectRoots must contain at least one path",
    )
  const repositories = await discoverGithubRepositories(roots)
  const previous = request.checkpoint
    ? parseCheckpoint(request.checkpoint)
    : { repositories: {} }
  const next: GithubCheckpoint = {
    repositories: structuredClone(previous.repositories),
  }
  const items: IntakeItem[] = []
  const apiBase =
    stringOption(request, "apiBaseUrl") ?? "https://api.github.com"
  const maxPages = numberOption(request, "maxPages") ?? DEFAULT_MAX_PAGES
  if (!Number.isInteger(maxPages) || maxPages < 1 || maxPages > 1_000)
    throw new Error(
      "GitHub source options.maxPages must be an integer from 1 to 1000",
    )

  for (const repository of repositories.sort()) {
    const checkpoint = previous.repositories[repository]
    if (!checkpoint) {
      next.repositories[repository] = await baselineRepository(
        repository,
        apiBase,
        token,
        maxPages,
        fetcher,
      )
      continue
    }
    if (items.length >= request.itemLimit) continue

    const unseen = await listNewItems(
      repository,
      checkpoint,
      apiBase,
      token,
      maxPages,
      fetcher,
    )
    const selected = unseen.slice(0, request.itemLimit - items.length)
    for (const item of selected) {
      let pull: Record<string, unknown> | undefined
      if (item.pull_request) {
        pull = await githubGet<Record<string, unknown>>(
          `${apiBase}/repos/${repository}/pulls/${item.number}`,
          token,
          fetcher,
        )
      }
      items.push(normalizeGithubItem(repository, item, pull))
      advanceCheckpoint(next, repository, item)
    }
  }

  return { protocolVersion: 1, checkpoint: next, items }
}

export async function discoverGithubRepositories(
  roots: string[],
): Promise<string[]> {
  const repositories = new Set<string>()
  for (const rootValue of roots) {
    const root = await realpath(expandPath(rootValue))
    const glob = new Bun.Glob("**/.git")
    for await (const marker of glob.scan({
      cwd: root,
      absolute: true,
      onlyFiles: false,
      dot: true,
    })) {
      const canonical = await realpath(marker).catch(() => null)
      if (!canonical || !isWithin(canonical, [root])) continue
      const content = await readRepositoryConfig(marker)
      for (const remote of gitRemoteUrls(content)) {
        const identity = githubIdentity(remote)
        if (identity) repositories.add(identity)
      }
    }
  }
  return [...repositories]
}

async function readRepositoryConfig(marker: string): Promise<string> {
  const stat = await lstat(marker)
  if (stat.isDirectory())
    return readFile(join(marker, "config"), "utf8").catch(() => "")
  if (!stat.isFile()) return ""
  const markerText = await readFile(marker, "utf8").catch(() => "")
  const gitDirValue = markerText.match(/^gitdir:\s*(.+?)\s*$/im)?.[1]
  if (!gitDirValue) return ""
  const gitDirectory = resolve(dirname(marker), gitDirValue)
  const direct = await readFile(join(gitDirectory, "config"), "utf8").catch(
    () => "",
  )
  if (direct) return direct
  const commonValue = await readFile(join(gitDirectory, "commondir"), "utf8")
    .then((value) => value.trim())
    .catch(() => "")
  if (!commonValue) return ""
  return readFile(
    join(resolve(gitDirectory, commonValue), "config"),
    "utf8",
  ).catch(() => "")
}

function gitRemoteUrls(config: string): string[] {
  const urls: string[] = []
  let inRemote = false
  for (const line of config.split(/\r?\n/)) {
    const section = line.match(/^\s*\[([^\]]+)\]\s*$/)
    if (section) {
      inRemote = /^remote\s+"/i.test(section[1] ?? "")
      continue
    }
    if (!inRemote) continue
    const url = line.match(/^\s*url\s*=\s*(.+?)\s*$/i)?.[1]
    if (url) urls.push(url)
  }
  return urls
}

export function githubIdentity(remote: string): string | null {
  const normalized = remote.trim().replace(/\.git\/?$/, "")
  const match = normalized.match(
    /^(?:(?:https?|git):\/\/|ssh:\/\/git@|git@)github\.com[/:]([^/]+)\/([^/]+)$/i,
  )
  if (
    !match?.[1] ||
    !match[2] ||
    !/^[a-zA-Z0-9_.-]+$/.test(match[1]) ||
    !/^[a-zA-Z0-9_.-]+$/.test(match[2])
  )
    return null
  return `${match[1]}/${match[2]}`.toLowerCase()
}

async function baselineRepository(
  repository: string,
  apiBase: string,
  token: string,
  maxPages: number,
  fetcher: typeof fetch,
): Promise<RepoCheckpoint> {
  let createdAt: string | undefined
  const numbersAtTimestamp: number[] = []
  for (let page = 1; page <= maxPages; page += 1) {
    const response = await githubGet<GithubItem[]>(
      `${apiBase}/repos/${repository}/issues?state=all&sort=created&direction=desc&per_page=${PAGE_SIZE}&page=${page}`,
      token,
      fetcher,
    )
    const first = response[0]
    if (!first)
      return createdAt
        ? { createdAt, numbersAtTimestamp }
        : { createdAt: "1970-01-01T00:00:00.000Z", numbersAtTimestamp: [] }
    createdAt ??= first.created_at
    for (const item of response) {
      if (item.created_at !== createdAt)
        return { createdAt, numbersAtTimestamp }
      numbersAtTimestamp.push(item.number)
    }
    if (response.length < PAGE_SIZE) return { createdAt, numbersAtTimestamp }
  }
  throw new Error(
    `GitHub baseline pagination bound reached for ${repository} at one creation timestamp`,
  )
}

async function listNewItems(
  repository: string,
  checkpoint: RepoCheckpoint,
  apiBase: string,
  token: string,
  maxPages: number,
  fetcher: typeof fetch,
): Promise<GithubItem[]> {
  const unseen: GithubItem[] = []
  let reachedBoundary = false
  for (let page = 1; page <= maxPages; page += 1) {
    const response = await githubGet<GithubItem[]>(
      `${apiBase}/repos/${repository}/issues?state=all&sort=created&direction=desc&per_page=${PAGE_SIZE}&page=${page}`,
      token,
      fetcher,
    )
    for (const item of response) {
      if (item.created_at < checkpoint.createdAt) {
        reachedBoundary = true
        break
      }
      if (
        item.created_at === checkpoint.createdAt &&
        checkpoint.numbersAtTimestamp.includes(item.number)
      ) {
        continue
      }
      unseen.push(item)
    }
    if (reachedBoundary || response.length < PAGE_SIZE) {
      reachedBoundary = true
      break
    }
  }
  if (!reachedBoundary)
    throw new Error(
      `GitHub pagination bound reached for ${repository} without finding its checkpoint`,
    )
  return unseen.sort(
    (left, right) =>
      left.created_at.localeCompare(right.created_at) ||
      left.number - right.number,
  )
}

async function githubGet<T>(
  url: string,
  token: string,
  fetcher: typeof fetch,
): Promise<T> {
  const response = await fetcher(url, {
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "personal-intake-source",
    },
  })
  if (!response.ok)
    throw new Error(`GitHub API request failed with ${response.status}`)
  return (await response.json()) as T
}

function normalizeGithubItem(
  repository: string,
  item: GithubItem,
  pull?: Record<string, unknown>,
): IntakeItem {
  const isPull = Boolean(item.pull_request)
  return {
    entityId: `github:${repository}:${isPull ? "pull" : "issue"}:${item.number}`,
    revisionId: `created:${item.created_at}`,
    kind: isPull ? "github-pull-request" : "github-issue",
    title: item.title,
    body: (item.body ?? "").slice(0, 256 * 1024),
    url: item.html_url,
    occurredAt: item.created_at,
    metadata: {
      repository,
      number: item.number,
      itemType: isPull ? "pull-request" : "issue",
      author: item.user?.login ?? null,
      labels: item.labels.map((label) =>
        typeof label === "string" ? label : (label.name ?? ""),
      ),
      updatedAt: item.updated_at,
      pullRequest: pull
        ? {
            head: pull.head ?? null,
            base: pull.base ?? null,
            draft: pull.draft ?? false,
          }
        : null,
    },
  }
}

function advanceCheckpoint(
  checkpoint: GithubCheckpoint,
  repository: string,
  item: GithubItem,
): void {
  const current = checkpoint.repositories[repository]
  if (!current || item.created_at > current.createdAt) {
    checkpoint.repositories[repository] = {
      createdAt: item.created_at,
      numbersAtTimestamp: [item.number],
    }
  } else if (
    item.created_at === current.createdAt &&
    !current.numbersAtTimestamp.includes(item.number)
  ) {
    current.numbersAtTimestamp.push(item.number)
  }
}

function parseCheckpoint(value: unknown): GithubCheckpoint {
  if (!value || typeof value !== "object" || !("repositories" in value))
    throw new Error("GitHub checkpoint is invalid")
  const repositories = (value as GithubCheckpoint).repositories
  if (!repositories || typeof repositories !== "object")
    throw new Error("GitHub checkpoint is invalid")
  for (const entry of Object.values(repositories)) {
    if (
      typeof entry.createdAt !== "string" ||
      !Array.isArray(entry.numbersAtTimestamp) ||
      !entry.numbersAtTimestamp.every((number) => Number.isSafeInteger(number))
    ) {
      throw new Error("GitHub checkpoint is invalid")
    }
  }
  return { repositories }
}

function stringOption(request: PollRequest, name: string): string | undefined {
  const value = request.options[name]
  return typeof value === "string" ? value : undefined
}

function numberOption(request: PollRequest, name: string): number | undefined {
  const value = request.options[name]
  return typeof value === "number" ? value : undefined
}

function stringArrayOption(request: PollRequest, name: string): string[] {
  const value = request.options[name]
  return Array.isArray(value) &&
    value.every((entry) => typeof entry === "string")
    ? value
    : []
}

if (import.meta.main) sourceMain(pollGithub)
