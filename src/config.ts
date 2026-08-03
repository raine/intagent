import { homedir } from "node:os"
import { dirname, isAbsolute, join, resolve } from "node:path"
import { mkdir, readFile, realpath, writeFile } from "node:fs/promises"
import { parse, stringify } from "yaml"
import { z } from "zod"

const scalar = z.union([z.string(), z.number(), z.boolean(), z.null()])
const sourceOptions = z.record(z.string(), z.union([scalar, z.array(scalar)]))

const sourceSchema = z
  .object({
    name: z
      .string()
      .min(1)
      .regex(/^[a-z0-9][a-z0-9-]*$/),
    command: z.string().min(1),
    args: z.array(z.string()).default([]),
    intervalSeconds: z.number().int().min(10).default(60),
    timeoutSeconds: z.number().int().min(1).max(300).default(60),
    itemLimit: z.number().int().min(1).max(1000).default(100),
    environment: z.array(z.string().regex(/^[A-Z][A-Z0-9_]*$/)).default([]),
    options: sourceOptions.default({}),
  })
  .strict()

const commandRuleSchema = z
  .object({
    executable: z.string().regex(/^[a-zA-Z0-9._+-]+$/),
    subcommands: z.array(z.string()).optional(),
    allowedFlags: z
      .array(z.string().regex(/^--?[a-zA-Z0-9][a-zA-Z0-9-]*$/))
      .default([]),
    valueFlags: z
      .array(z.string().regex(/^--?[a-zA-Z0-9][a-zA-Z0-9-]*$/))
      .default([]),
    minPositionals: z.number().int().min(0).default(0),
    maxPositionals: z.number().int().min(0).max(100).default(20),
  })
  .strict()
  .refine((rule) => rule.minPositionals <= rule.maxPositionals, {
    message: "minPositionals must not exceed maxPositionals",
  })

export const configSchema = z
  .object({
    version: z.literal(1),
    projectRoots: z.array(z.string()).min(1).default(["~/code"]),
    state: z
      .object({
        database: z.string().default("~/.config/intake/state/intake.sqlite"),
      })
      .strict()
      .default({ database: "~/.config/intake/state/intake.sqlite" }),
    skills: z
      .object({
        directories: z.array(z.string()).min(1),
        approvedRoots: z.array(z.string()).min(1),
      })
      .strict(),
    sources: z.array(sourceSchema),
    triage: z
      .object({
        maxTurns: z.number().int().min(1).max(50).default(50),
        timeoutMinutes: z.number().int().min(1).max(30).default(30),
        maxAttempts: z.number().int().min(1).max(3).default(3),
        retryBaseSeconds: z.number().int().min(1).max(3600).default(60),
      })
      .strict()
      .default({
        maxTurns: 50,
        timeoutMinutes: 30,
        maxAttempts: 3,
        retryBaseSeconds: 60,
      }),
    commands: z
      .object({
        path: z.array(z.string()).min(1),
        timeoutSeconds: z.number().int().min(1).max(300).default(60),
        maxOutputBytes: z
          .number()
          .int()
          .min(1024)
          .max(1_000_000)
          .default(65_536),
        sensitivePatterns: z.array(z.string()).default([]),
        rules: z.array(commandRuleSchema).min(1),
      })
      .strict(),
  })
  .strict()

export type IntakeConfig = z.infer<typeof configSchema>
export type SourceConfig = IntakeConfig["sources"][number]
export type CommandRule = IntakeConfig["commands"]["rules"][number]

export function configDirectory(
  env: Record<string, string | undefined> = process.env,
): string {
  return env.XDG_CONFIG_HOME
    ? join(env.XDG_CONFIG_HOME, "intake")
    : join(homedir(), ".config", "intake")
}

export function applicationSkillsDirectory(): string {
  return resolve(import.meta.dir, "..", "skills")
}

export function defaultConfigPath(
  env: Record<string, string | undefined> = process.env,
): string {
  return join(configDirectory(env), "config.yaml")
}

export function expandPath(path: string): string {
  if (path === "~") return homedir()
  if (path.startsWith("~/")) return join(homedir(), path.slice(2))
  return isAbsolute(path) ? path : resolve(path)
}

export async function loadConfig(
  path = defaultConfigPath(),
): Promise<IntakeConfig> {
  let raw: string
  try {
    raw = await readFile(path, "utf8")
  } catch (error) {
    throw new Error(
      `Cannot read configuration at ${path}: ${errorMessage(error)}`,
    )
  }

  let parsed: unknown
  try {
    parsed = parse(raw, { uniqueKeys: true, maxAliasCount: 0 })
  } catch (error) {
    throw new Error(`Invalid YAML in ${path}: ${errorMessage(error)}`)
  }

  const result = configSchema.safeParse(parsed)
  if (!result.success) {
    const details = result.error.issues
      .map(
        (issue) =>
          `${issue.path.join(".") || "configuration"}: ${issue.message}`,
      )
      .join("\n")
    throw new Error(`Invalid configuration at ${path}:\n${details}`)
  }
  return result.data
}

export async function canonicalRoots(paths: string[]): Promise<string[]> {
  return Promise.all(
    paths.map(async (path) => {
      const expanded = expandPath(path)
      try {
        return await realpath(expanded)
      } catch {
        return resolve(expanded)
      }
    }),
  )
}

export function isWithin(path: string, roots: string[]): boolean {
  const normalized = resolve(path)
  return roots.some(
    (root) => normalized === root || normalized.startsWith(`${root}/`),
  )
}

export async function initializePrivateConfig(
  path = defaultConfigPath(),
): Promise<{ configPath: string; created: string[] }> {
  const directory = dirname(path)
  const skillsDirectory = join(directory, "skills")
  const stateDirectory = join(directory, "state")
  await mkdir(skillsDirectory, { recursive: true, mode: 0o700 })
  await mkdir(stateDirectory, { recursive: true, mode: 0o700 })

  const config = {
    version: 1,
    projectRoots: ["~/code"],
    state: { database: join(stateDirectory, "intake.sqlite") },
    skills: {
      directories: [applicationSkillsDirectory(), skillsDirectory],
      approvedRoots: [
        applicationSkillsDirectory(),
        skillsDirectory,
        join(homedir(), ".claude", "skills"),
      ],
    },
    sources: [],
    triage: {
      maxTurns: 50,
      timeoutMinutes: 30,
      maxAttempts: 3,
      retryBaseSeconds: 60,
    },
    commands: {
      path: ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"],
      timeoutSeconds: 60,
      maxOutputBytes: 65_536,
      sensitivePatterns: [],
      rules: defaultCommandRules,
    },
  }

  const created: string[] = []
  if (!(await Bun.file(path).exists())) {
    await writeFile(path, stringify(config, { lineWidth: 100 }), {
      mode: 0o600,
    })
    created.push(path)
  }
  return { configPath: path, created }
}

export const defaultCommandRules: CommandRule[] = [
  {
    executable: "aven",
    subcommands: ["list", "search", "show", "context", "add", "note", "prime"],
    allowedFlags: [
      "--project",
      "--status",
      "--priority",
      "--label",
      "--open",
      "--ready",
      "--blocked",
      "--limit",
      "--json",
      "--full",
      "--description",
      "--workspace",
    ],
    valueFlags: [
      "--project",
      "--status",
      "--priority",
      "--label",
      "--limit",
      "--description",
      "--workspace",
    ],
    minPositionals: 0,
    maxPositionals: 20,
  },
  {
    executable: "workmux",
    subcommands: ["status", "list", "path", "add"],
    allowedFlags: [
      "--json",
      "--pr",
      "--auto-name",
      "-A",
      "--background",
      "-b",
      "--prompt",
      "-p",
      "--base",
    ],
    valueFlags: ["--pr", "--prompt", "-p", "--base"],
    minPositionals: 0,
    maxPositionals: 4,
  },
  {
    executable: "tmux",
    subcommands: ["list-sessions", "list-windows", "has-session"],
    allowedFlags: ["-F", "-t"],
    valueFlags: ["-F", "-t"],
    minPositionals: 0,
    maxPositionals: 0,
  },
  {
    executable: "git",
    subcommands: ["remote", "status", "branch", "rev-parse", "config"],
    allowedFlags: ["-C", "--get", "--show-toplevel", "--porcelain", "--format"],
    valueFlags: ["-C", "--get", "--format"],
    minPositionals: 0,
    maxPositionals: 4,
  },
  {
    executable: "rg",
    allowedFlags: [
      "-n",
      "--line-number",
      "-l",
      "--files",
      "-g",
      "--glob",
      "-F",
      "--fixed-strings",
    ],
    valueFlags: ["-g", "--glob"],
    minPositionals: 0,
    maxPositionals: 10,
  },
  {
    executable: "fd",
    allowedFlags: ["-H", "--hidden", "-t", "--type", "-d", "--max-depth"],
    valueFlags: ["-t", "--type", "-d", "--max-depth"],
    minPositionals: 0,
    maxPositionals: 4,
  },
]

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
