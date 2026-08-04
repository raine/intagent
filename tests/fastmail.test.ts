import { afterEach, describe, expect, test } from "bun:test"
import type { PollRequest } from "../src/protocol.ts"
import { pollFastmail } from "../src/sources/fastmail.ts"

const originalToken = process.env.FASTMAIL_API_TOKEN
afterEach(() => {
  if (originalToken === undefined) delete process.env.FASTMAIL_API_TOKEN
  else process.env.FASTMAIL_API_TOKEN = originalToken
})

function request(checkpoint: unknown): PollRequest {
  return {
    protocolVersion: 1,
    source: "fastmail",
    checkpoint,
    now: "2026-08-03T10:10:00.000Z",
    itemLimit: 10,
    options: { mailbox_id: "inbox", session_url: "https://mail.test/session" },
  }
}

function json(value: unknown): Response {
  return Response.json(value)
}

describe("Fastmail source", () => {
  test("establishes a mailbox-query baseline without historical events", async () => {
    process.env.FASTMAIL_API_TOKEN = "source-only-token"
    const calls: unknown[] = []
    const fetcher = (async (
      _input: string | URL | Request,
      init?: RequestInit,
    ) => {
      if (!init?.method) {
        return json({
          apiUrl: "https://mail.test/jmap",
          primaryAccounts: { "urn:ietf:params:jmap:mail": "account-1" },
        })
      }
      const body = JSON.parse(init.body as string)
      calls.push(body)
      return json({
        methodResponses: [
          ["Email/query", { queryState: "query-state-1", ids: [] }, "query"],
        ],
      })
    }) as typeof fetch
    const result = await pollFastmail(request(null), fetcher)
    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      queryState: "query-state-1",
      mailboxId: "inbox",
    })
    expect(calls).toHaveLength(1)
  })

  test("emits stable message events with complete bounded threads and attachment metadata", async () => {
    process.env.FASTMAIL_API_TOKEN = "source-only-token"
    const fetcher = (async (
      _input: string | URL | Request,
      init?: RequestInit,
    ) => {
      if (!init?.method) {
        return json({
          apiUrl: "https://mail.test/jmap",
          primaryAccounts: { "urn:ietf:params:jmap:mail": "account-1" },
        })
      }
      const body = JSON.parse(init.body as string)
      const [method, arguments_, callId] = body.methodCalls[0]
      if (method === "Email/queryChanges") {
        expect(arguments_.maxChanges).toBe(10)
        expect(arguments_.filter).toEqual({ inMailbox: "inbox" })
        return json({
          methodResponses: [
            [
              method,
              {
                added: [{ id: "message-2", index: 0 }],
                removed: [],
                newQueryState: "query-state-2",
                hasMoreChanges: false,
              },
              callId,
            ],
          ],
        })
      }
      if (method === "Thread/get") {
        return json({
          methodResponses: [
            [
              method,
              {
                list: [
                  { id: "thread-1", emailIds: ["message-1", "message-2"] },
                ],
              },
              callId,
            ],
          ],
        })
      }
      if (method === "Email/get") {
        const messages =
          arguments_.ids.length === 1
            ? [email("message-2", "2026-08-03T10:05:00.000Z", "Follow up")]
            : [
                email(
                  "message-1",
                  "2026-08-03T10:00:00.000Z",
                  "Initial request",
                ),
                email("message-2", "2026-08-03T10:05:00.000Z", "Follow up"),
              ]
        return json({ methodResponses: [[method, { list: messages }, callId]] })
      }
      throw new Error(`unexpected method ${method}`)
    }) as typeof fetch

    const result = await pollFastmail(
      request({ queryState: "query-state-1", mailboxId: "inbox" }),
      fetcher,
    )
    expect(result.items).toHaveLength(1)
    expect(result.items[0]).toMatchObject({
      entityId: "fastmail:account-1:thread:thread-1",
      revisionId: "message-2",
      kind: "email",
    })
    expect(result.items[0]?.body).toContain("Initial request")
    expect(result.items[0]?.body).toContain("Follow up")
    expect(result.items[0]?.metadata.attachments).toEqual([
      {
        name: "report.pdf",
        type: "application/pdf",
        size: 42,
        disposition: "attachment",
        cid: null,
      },
      {
        name: "report.pdf",
        type: "application/pdf",
        size: 42,
        disposition: "attachment",
        cid: null,
      },
    ])
    expect(JSON.stringify(result.items[0]?.metadata)).not.toContain(
      "blob-secret",
    )
    expect(result.checkpoint).toEqual({
      queryState: "query-state-2",
      mailboxId: "inbox",
    })
  })

  test("advances through removals without emitting intake", async () => {
    process.env.FASTMAIL_API_TOKEN = "source-only-token"
    const fetcher = (async (
      _input: string | URL | Request,
      init?: RequestInit,
    ) => {
      if (!init?.method) {
        return json({
          apiUrl: "https://mail.test/jmap",
          primaryAccounts: { "urn:ietf:params:jmap:mail": "account-1" },
        })
      }
      const body = JSON.parse(init.body as string)
      const [method, , callId] = body.methodCalls[0]
      expect(method).toBe("Email/queryChanges")
      return json({
        methodResponses: [
          [
            method,
            {
              added: [],
              removed: ["message-1"],
              newQueryState: "query-state-2",
              hasMoreChanges: false,
            },
            callId,
          ],
        ],
      })
    }) as typeof fetch

    const result = await pollFastmail(
      request({ queryState: "query-state-1", mailboxId: "inbox" }),
      fetcher,
    )
    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      queryState: "query-state-2",
      mailboxId: "inbox",
    })
  })
})
function email(id: string, receivedAt: string, value: string) {
  return {
    id,
    threadId: "thread-1",
    subject: "Question",
    from: [{ name: "Sender", email: "sender@example.test" }],
    to: [{ email: "recipient@example.test" }],
    receivedAt,
    mailboxIds: { inbox: true },
    textBody: [{ partId: "body", type: "text/plain" }],
    bodyValues: { body: { value } },
    bodyStructure: {
      subParts: [
        {
          blobId: "blob-secret",
          name: "report.pdf",
          type: "application/pdf",
          size: 42,
          disposition: "attachment",
        },
      ],
    },
  }
}
