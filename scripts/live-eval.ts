import { readFile } from "node:fs/promises"
import { CommandPolicy } from "../src/agent/command-policy.ts"
import { PiTriageRunner } from "../src/agent/pi-runner.ts"
import {
  canonicalRoots,
  configDirectory,
  defaultConfigPath,
  expandPath,
  loadConfig,
} from "../src/config.ts"
import { IntakeDatabase } from "../src/database.ts"
import { intakeItemSchema } from "../src/protocol.ts"

const args = process.argv.slice(2)
if (!args.includes("--allow-local-effects")) {
  throw new Error(
    "live evaluation can create Aven tasks and workmux investigations; pass --allow-local-effects to continue",
  )
}
const fixturePath = args.find((argument) => !argument.startsWith("-"))
if (!fixturePath) throw new Error("a redacted intake fixture path is required")
const configIndex = args.indexOf("--config")
const configPath =
  configIndex >= 0 && args[configIndex + 1]
    ? expandPath(args[configIndex + 1] ?? "")
    : defaultConfigPath()
const config = await loadConfig(configPath)
const fixture = intakeItemSchema.parse(
  JSON.parse(await readFile(expandPath(fixturePath), "utf8")),
)
const database = new IntakeDatabase(expandPath(config.state.database))
try {
  database.sourceSucceeded(
    "live-eval",
    { fixture: fixturePath },
    [fixture],
    new Date().toISOString(),
  )
  const event = database.claimNext()
  if (!event)
    throw new Error("fixture already exists in the evaluation database")
  const roots = await canonicalRoots([
    ...config.projectRoots,
    ...config.skills.approvedRoots,
    configDirectory(),
  ])
  const runner = new PiTriageRunner(
    config,
    database,
    new CommandPolicy(config, roots),
  )
  await runner.run(event)
  process.stdout.write(`Live evaluation completed for event ${event.id}.\n`)
} finally {
  database.close()
}
