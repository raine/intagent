import { afterEach, describe, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { PollRequest } from "../src/protocol.ts"
import {
  discoverGithubRepositories,
  githubIdentity,
  pollGithub,
} from "../src/sources/github.ts"

const paths: string[] = []
const originalToken = process.env.GITHUB_TOKEN
afterEach(async () => {
  await Promise.all(
    paths.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
  if (originalToken === undefined) delete process.env.GITHUB_TOKEN
  else process.env.GITHUB_TOKEN = originalToken
})

async function repositoryRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "intake-github-"))
  paths.push(root)
  const config = join(root, "project", ".git", "config")
  await mkdir(join(root, "project", ".git"), { recursive: true })
  await writeFile(
    config,
    '[remote "origin"]\n  url = git@github.com:Example/Project.git\n[remote "backup"]\n  url = https://gitlab.test/example/project.git\n',
  )
  return root
}

function request(root: string, checkpoint: unknown): PollRequest {
  return {
    protocolVersion: 1,
    source: "github",
    checkpoint,
    now: "2026-08-03T11:00:00.000Z",
    itemLimit: 10,
    options: {
      projectRoots: [root],
      apiBaseUrl: "https://github.test",
      maxPages: 3,
    },
  }
}

describe("GitHub source", () => {
  test("discovers canonical remotes from repositories and worktree markers", async () => {
    const root = await repositoryRoot()
    await mkdir(join(root, "linked-worktree"), { recursive: true })
    await mkdir(join(root, "shared.git"), { recursive: true })
    await writeFile(
      join(root, "shared.git", "config"),
      '[REMOTE "origin"]\n  URL = git://github.com/Example/Linked.git\n',
    )
    await writeFile(
      join(root, "linked-worktree", ".git"),
      "gitdir: ../shared.git\n",
    )
    expect((await discoverGithubRepositories([root])).sort()).toEqual([
      "example/linked",
      "example/project",
    ])
    expect(githubIdentity("ssh://git@github.com/Owner/Repo.git")).toBe(
      "owner/repo",
    )
    expect(githubIdentity("https://example.test/Owner/Repo.git")).toBeNull()
  })

  test("establishes a per-repository baseline", async () => {
    process.env.GITHUB_TOKEN = "source-only-token"
    const root = await repositoryRoot()
    const fetcher = (async (input: string | URL | Request) => {
      expect(requestUrl(input)).toContain("per_page=100&page=1")
      return Response.json([
        issue(8, "2026-08-03T10:00:00.000Z"),
        issue(7, "2026-08-03T10:00:00.000Z"),
        issue(6, "2026-08-03T09:59:59.000Z"),
      ])
    }) as typeof fetch
    const result = await pollGithub(request(root, null), fetcher)
    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      repositories: {
        "example/project": {
          createdAt: "2026-08-03T10:00:00.000Z",
          numbersAtTimestamp: [8, 7],
        },
      },
    })
  })

  test("paginates to the checkpoint and loads pull request head metadata", async () => {
    process.env.GITHUB_TOKEN = "source-only-token"
    const root = await repositoryRoot()
    const fetcher = (async (input: string | URL | Request) => {
      const url = requestUrl(input)
      if (url.endsWith("/pulls/9")) {
        return Response.json({
          head: {
            ref: "feature",
            sha: "abc",
            repo: { full_name: "contributor/project" },
          },
          base: { ref: "main", sha: "def" },
          draft: false,
        })
      }
      return Response.json([
        {
          ...issue(9, "2026-08-03T10:05:00.000Z"),
          pull_request: { url: "https://github.test/pulls/9" },
        },
        issue(8, "2026-08-03T10:02:00.000Z"),
        issue(7, "2026-08-03T10:00:00.000Z"),
      ])
    }) as typeof fetch
    const result = await pollGithub(
      request(root, {
        repositories: {
          "example/project": {
            createdAt: "2026-08-03T10:00:00.000Z",
            numbersAtTimestamp: [7],
          },
        },
      }),
      fetcher,
    )
    expect(result.items.map((item) => item.revisionId)).toEqual([
      "created:2026-08-03T10:02:00.000Z",
      "created:2026-08-03T10:05:00.000Z",
    ])
    expect(result.items[1]).toMatchObject({
      entityId: "github:example/project:pull:9",
      kind: "github-pull-request",
      metadata: {
        pullRequest: {
          head: { ref: "feature", sha: "abc" },
          base: { ref: "main" },
        },
      },
    })
    expect(result.checkpoint).toEqual({
      repositories: {
        "example/project": {
          createdAt: "2026-08-03T10:05:00.000Z",
          numbersAtTimestamp: [9],
        },
      },
    })
  })
})

function requestUrl(input: string | URL | Request): string {
  if (typeof input === "string") return input
  return input instanceof URL ? input.href : input.url
}

function issue(number: number, createdAt: string) {
  return {
    number,
    title: `Item ${number}`,
    body: "Details",
    html_url: `https://github.com/example/project/issues/${number}`,
    created_at: createdAt,
    updated_at: createdAt,
    user: { login: "reporter" },
    labels: [{ name: "bug" }],
  }
}
