import { describe, expect, test } from "bun:test"
import { terminalLine } from "../src/terminal.ts"

describe("terminal output", () => {
  test("dims timestamps on color terminals", () => {
    const previous = process.env.NO_COLOR
    delete process.env.NO_COLOR
    let output = ""
    try {
      terminalLine(
        { isTTY: true, write: (value) => (output += value) },
        "handled event",
      )
    } finally {
      if (previous === undefined) delete process.env.NO_COLOR
      else process.env.NO_COLOR = previous
    }

    const dim = "[2m"
    const reset = "[0m"
    expect(output.startsWith(dim)).toBe(true)
    expect(output.slice(dim.length, dim.length + 8)).toMatch(
      /^\d{2}:\d{2}:\d{2}$/,
    )
    expect(output.endsWith(`${reset}  handled event\n`)).toBe(true)
  })
})
