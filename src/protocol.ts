import { z } from "zod"

export const PROTOCOL_VERSION = 1 as const

const jsonValue: z.ZodType<unknown> = z.lazy(() =>
  z.union([
    z.string(),
    z.number(),
    z.boolean(),
    z.null(),
    z.array(jsonValue),
    z.record(z.string(), jsonValue),
  ]),
)

export const pollRequestSchema = z
  .object({
    protocolVersion: z.literal(PROTOCOL_VERSION),
    source: z.string().min(1),
    checkpoint: jsonValue.nullable(),
    now: z.string().datetime(),
    itemLimit: z.number().int().min(1).max(1000),
    options: z.record(z.string(), jsonValue).default({}),
  })
  .strict()

export const intakeItemSchema = z
  .object({
    entityId: z.string().min(1).max(1024),
    revisionId: z.string().min(1).max(1024),
    kind: z.enum(["email", "github-issue", "github-pull-request", "generic"]),
    title: z.string().max(4096),
    body: z.string().max(1_000_000),
    url: z.string().url().optional(),
    occurredAt: z.string().datetime(),
    metadata: z.record(z.string(), jsonValue).default({}),
  })
  .strict()

export const pollResponseSchema = z
  .object({
    protocolVersion: z.literal(PROTOCOL_VERSION),
    checkpoint: jsonValue,
    items: z.array(intakeItemSchema).max(1000),
  })
  .strict()

export type PollRequest = z.infer<typeof pollRequestSchema>
export type PollResponse = z.infer<typeof pollResponseSchema>
export type IntakeItem = z.infer<typeof intakeItemSchema>

export async function readPollRequest(): Promise<PollRequest> {
  const input = await new Response(Bun.stdin.stream()).text()
  let value: unknown
  try {
    value = JSON.parse(input)
  } catch (error) {
    throw new Error(`stdin does not contain one JSON request: ${String(error)}`)
  }
  return pollRequestSchema.parse(value)
}

export function writePollResponse(response: PollResponse): void {
  const validated = pollResponseSchema.parse(response)
  process.stdout.write(`${JSON.stringify(validated)}\n`)
}

export function sourceMain(
  handler: (request: PollRequest) => Promise<PollResponse>,
): void {
  void (async () => {
    try {
      const request = await readPollRequest()
      writePollResponse(await handler(request))
    } catch (error) {
      process.stderr.write(
        `${error instanceof Error ? error.message : String(error)}\n`,
      )
      process.exitCode = 1
    }
  })()
}
