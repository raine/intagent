import type { IntakeConfig } from "../../src/config.ts"

export function testConfig(root: string, bin: string): IntakeConfig {
  return {
    version: 1,
    project_roots: [root],
    state: { database: ":memory:", logs: `${root}/logs` },
    skills: { directories: [root], approved_roots: [root] },
    sources: [],
    triage: {
      model: "gpt-5.6-luna",
      thinking_level: "max",
      max_turns: 50,
      timeout_minutes: 30,
      max_attempts: 3,
      retry_base_seconds: 1,
    },
    commands: {
      path: [bin],
      timeout_seconds: 1,
      max_output_bytes: 1024,
      sensitive_patterns: [],
      rules: [
        { executable: "aven" },
        { executable: "workmux" },
        { executable: "rg" },
        { executable: "gh" },
      ],
    },
  }
}
