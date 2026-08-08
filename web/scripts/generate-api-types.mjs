import { execFileSync } from "node:child_process"
import { writeFileSync } from "node:fs"
import { compile } from "json-schema-to-typescript"

const schemaSource = execFileSync(
  "cargo",
  [
    "run",
    "--quiet",
    "--manifest-path",
    "../Cargo.toml",
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
