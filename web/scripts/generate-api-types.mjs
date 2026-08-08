import { execFileSync } from "node:child_process"
import { writeFileSync } from "node:fs"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { compile } from "json-schema-to-typescript"

const scriptDirectory = dirname(fileURLToPath(import.meta.url))
const manifestPath =
  process.env.INTAKE_CARGO_MANIFEST_PATH ??
  resolve(scriptDirectory, "..", "..", "Cargo.toml")
const schemaSource = execFileSync(
  "cargo",
  [
    "run",
    "--quiet",
    "--manifest-path",
    manifestPath,
    "--example",
    "generate-web-schema",
  ],
  { encoding: "utf8" },
)
const generated = await compile(JSON.parse(schemaSource), "WebApiContract", {
  additionalProperties: false,
  bannerComment:
    "/* Generated from the Rust dashboard response schemas by npm run generate:api. */",
  format: false,
})
const formatted = execFileSync(
  "node_modules/.bin/oxfmt",
  ["--stdin-filepath", "src/api-types.ts"],
  { encoding: "utf8", input: generated },
)

writeFileSync("src/api-types.ts", formatted)
