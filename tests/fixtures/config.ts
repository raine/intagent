import type { IntakeConfig } from "../../src/config.ts"

export function testConfig(root: string, bin: string): IntakeConfig {
  return {
    version: 1,
    projectRoots: [root],
    state: { database: ":memory:" },
    skills: { directories: [root], approvedRoots: [root] },
    sources: [],
    triage: {
      maxTurns: 50,
      timeoutMinutes: 30,
      maxAttempts: 3,
      retryBaseSeconds: 1,
    },
    commands: {
      path: [bin],
      timeoutSeconds: 1,
      maxOutputBytes: 1024,
      sensitivePatterns: [],
      rules: [
        {
          executable: "aven",
          subcommands: ["search", "add"],
          allowedFlags: ["--status", "--description"],
          valueFlags: ["--status", "--description"],
          minPositionals: 0,
          maxPositionals: 4,
        },
        {
          executable: "rg",
          allowedFlags: ["-n"],
          valueFlags: [],
          minPositionals: 1,
          maxPositionals: 3,
        },
      ],
    },
  }
}
