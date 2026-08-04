import { lstat, open, realpath } from "node:fs/promises"
import { isAbsolute, resolve } from "node:path"
import { isWithin } from "../config.ts"

export const MAX_READ_PATH_BYTES = 4096
export const MAX_READ_FILE_BYTES = 1_000_000
export const MAX_READ_LINES = 2000
export const MAX_READ_LINE_NUMBER = 1_000_000

export interface ReadInput {
  path: string
  offset?: number
  limit?: number
}

export interface ReadResult {
  path: string
  size: number
  totalLines: number
  startLine: number
  endLine: number
  truncated: boolean
  text: string
}

interface AuthorizedRead {
  path: string
  size: number
  offset: number
  limit: number | undefined
}

export class ReadPolicy {
  constructor(
    private readonly roots: string[],
    readonly maxOutputBytes: number,
  ) {}

  async authorize(input: ReadInput, cwd: string): Promise<AuthorizedRead> {
    validateInput(input)
    const requested = isAbsolute(input.path)
      ? resolve(input.path)
      : resolve(cwd, input.path)
    if (!isWithin(requested, this.roots))
      throw new Error(`file is outside approved roots: ${input.path}`)

    let canonical: string
    try {
      canonical = await realpath(requested)
    } catch {
      throw new Error(`file is unavailable: ${input.path}`)
    }
    if (!isWithin(canonical, this.roots))
      throw new Error(
        `canonical file path is outside approved roots: ${input.path}`,
      )

    const stat = await lstat(canonical)
    if (!stat.isFile())
      throw new Error(`path is not a regular file: ${input.path}`)
    if (stat.size > MAX_READ_FILE_BYTES) {
      throw new Error(
        `file exceeds the ${MAX_READ_FILE_BYTES} byte read limit: ${input.path}`,
      )
    }

    return {
      path: canonical,
      size: stat.size,
      offset: input.offset ?? 1,
      limit: input.limit,
    }
  }

  async read(input: ReadInput, cwd: string): Promise<ReadResult> {
    const authorized = await this.authorize(input, cwd)
    const bytes = await readBoundedFile(authorized.path)

    let text: string
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(bytes)
    } catch {
      throw new Error(`file is not valid UTF-8 text: ${input.path}`)
    }
    const lines = text.split(/\r?\n/)
    if (authorized.offset > lines.length) {
      throw new Error(
        `offset ${authorized.offset} is beyond end of file (${lines.length} lines)`,
      )
    }

    const startIndex = authorized.offset - 1
    const endIndex = Math.min(
      lines.length,
      startIndex + (authorized.limit ?? lines.length),
    )
    const formatted = formatLines(
      lines,
      startIndex,
      endIndex,
      this.maxOutputBytes,
    )

    return {
      path: authorized.path,
      size: bytes.byteLength,
      totalLines: lines.length,
      startLine: authorized.offset,
      endLine: formatted.endLine,
      truncated: formatted.truncated || endIndex < lines.length,
      text: formatted.text,
    }
  }
}

async function readBoundedFile(path: string): Promise<Buffer> {
  const file = await open(path, "r")
  try {
    const stat = await file.stat()
    if (!stat.isFile()) throw new Error(`path is not a regular file: ${path}`)
    if (stat.size > MAX_READ_FILE_BYTES) {
      throw new Error(
        `file exceeds the ${MAX_READ_FILE_BYTES} byte read limit: ${path}`,
      )
    }

    const buffer = Buffer.alloc(MAX_READ_FILE_BYTES + 1)
    let size = 0
    while (size < buffer.byteLength) {
      const { bytesRead } = await file.read(
        buffer,
        size,
        buffer.byteLength - size,
        size,
      )
      if (bytesRead === 0) break
      size += bytesRead
    }
    if (size > MAX_READ_FILE_BYTES) {
      throw new Error(
        `file exceeds the ${MAX_READ_FILE_BYTES} byte read limit: ${path}`,
      )
    }
    return buffer.subarray(0, size)
  } finally {
    await file.close()
  }
}

function validateInput(input: ReadInput): void {
  if (
    input.path.length === 0 ||
    Buffer.byteLength(input.path) > MAX_READ_PATH_BYTES
  ) {
    throw new Error("file path length is outside read policy bounds")
  }
  if (input.path.includes("\0")) throw new Error("NUL bytes are forbidden")
  if (input.offset !== undefined) {
    if (
      !Number.isInteger(input.offset) ||
      input.offset < 1 ||
      input.offset > MAX_READ_LINE_NUMBER
    ) {
      throw new Error("offset is outside read policy bounds")
    }
  }
  if (input.limit !== undefined) {
    if (
      !Number.isInteger(input.limit) ||
      input.limit < 1 ||
      input.limit > MAX_READ_LINES
    ) {
      throw new Error("limit is outside read policy bounds")
    }
  }
}

function formatLines(
  lines: string[],
  startIndex: number,
  endIndex: number,
  maxBytes: number,
): { text: string; endLine: number; truncated: boolean } {
  const output: string[] = []
  for (let index = startIndex; index < endIndex; index += 1) {
    const lineNumber = index + 1
    const rendered = `${lineNumber}\t${lines[index]}`
    const hasMore = index + 1 < lines.length
    const continuation = hasMore
      ? `\n[Output truncated at ${maxBytes} bytes. Showing lines ${startIndex + 1}-${lineNumber} of ${lines.length}. Use offset=${lineNumber + 1} to continue.]`
      : ""
    const candidate = [...output, rendered].join("\n") + continuation
    if (Buffer.byteLength(candidate) <= maxBytes) {
      output.push(rendered)
      continue
    }

    const priorEnd = lineNumber - 1
    if (output.length > 0) {
      const notice = `[Output truncated at ${maxBytes} bytes. Showing lines ${startIndex + 1}-${priorEnd} of ${lines.length}. Use offset=${lineNumber} to continue.]`
      return {
        text: `${output.join("\n")}\n${notice}`,
        endLine: priorEnd,
        truncated: true,
      }
    }

    const notice = `\n[Output truncated at ${maxBytes} bytes on line ${lineNumber} of ${lines.length}. Use offset=${lineNumber} to reread the line.]`
    const prefix = `${lineNumber}\t`
    const available = Math.max(
      0,
      maxBytes - Buffer.byteLength(prefix) - Buffer.byteLength(notice),
    )
    return {
      text: `${prefix}${truncateUtf8(lines[index] ?? "", available)}${notice}`,
      endLine: lineNumber,
      truncated: true,
    }
  }

  const endLine = Math.max(startIndex + 1, endIndex)
  if (endIndex < lines.length) {
    output.push(
      `[Showing lines ${startIndex + 1}-${endLine} of ${lines.length}. Use offset=${endLine + 1} to continue.]`,
    )
  }
  return { text: output.join("\n"), endLine, truncated: false }
}

function truncateUtf8(value: string, maxBytes: number): string {
  const bytes = Buffer.from(value)
  if (bytes.byteLength <= maxBytes) return value
  let end = maxBytes
  while (end > 0) {
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(
        bytes.subarray(0, end),
      )
    } catch {
      end -= 1
    }
  }
  return ""
}
