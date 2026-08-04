export interface WritableTerminal {
  isTTY?: boolean
  write(value: string): unknown
}

export function terminalLine(output: WritableTerminal, value: string): void {
  const time = new Date().toLocaleTimeString("en-GB", { hour12: false })
  output.write(`${dim(output, time)}  ${value}\n`)
}

function dim(output: WritableTerminal, value: string): string {
  if (!output.isTTY || process.env.NO_COLOR) return value
  return `[2m${value}[0m`
}
