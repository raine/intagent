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
      const [method, , callId] = body.methodCalls[0]
      if (method === "Mailbox/get") {
        return json({
          methodResponses: [
            [
              method,
              {
                list: [
                  { id: "inbox", role: "inbox" },
                  { id: "sent", role: "sent" },
                ],
              },
              callId,
            ],
          ],
        })
      }
      expect(method).toBe("Email/query")
      return json({
        methodResponses: [
          [method, { queryState: "query-state-1", ids: [] }, callId],
        ],
      })
    }) as typeof fetch
    const result = await pollFastmail(request(null), fetcher)
    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      queryState: "query-state-1",
      mailboxId: "inbox",
      sentMailboxId: "sent",
    })
    expect(calls).toHaveLength(2)
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
                {
                  ...email(
                    "message-1",
                    "2026-08-03T10:00:00.000Z",
                    "Initial request",
                  ),
                  sentAt: "2026-08-03T10:00:00.000Z",
                  mailboxIds: { sent: true },
                },
                email("message-2", "2026-08-03T10:05:00.000Z", "Follow up"),
              ]
        return json({ methodResponses: [[method, { list: messages }, callId]] })
      }
      throw new Error(`unexpected method ${method}`)
    }) as typeof fetch

    const result = await pollFastmail(
      request({
        queryState: "query-state-1",
        mailboxId: "inbox",
        sentMailboxId: "sent",
      }),
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
      sentMailboxId: "sent",
    })
  })

  test("excludes messages by configured header before thread assembly", async () => {
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
        return json({
          methodResponses: [
            [
              method,
              {
                added: [{ id: "push-message", index: 0 }],
                removed: [],
                newQueryState: "query-state-2",
                hasMoreChanges: false,
              },
              callId,
            ],
          ],
        })
      }
      if (method === "Email/get") {
        expect(arguments_.properties).toContain("header:X-GitHub-Reason:asText")
        return json({
          methodResponses: [
            [
              method,
              {
                list: [
                  {
                    ...email(
                      "push-message",
                      "2026-08-03T10:05:00.000Z",
                      "Pushed one commit",
                    ),
                    "header:X-GitHub-Reason:asText": "push",
                  },
                ],
              },
              callId,
            ],
          ],
        })
      }
      throw new Error(`unexpected method ${method}`)
    }) as typeof fetch
    const pollRequest = request({
      queryState: "query-state-1",
      mailboxId: "inbox",
      sentMailboxId: "sent",
    })
    pollRequest.options.exclude_headers = { "X-GitHub-Reason": ["push"] }

    const result = await pollFastmail(pollRequest, fetcher)

    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      queryState: "query-state-2",
      mailboxId: "inbox",
      sentMailboxId: "sent",
    })
  })

  test("includes only configured headers and message ID kinds", async () => {
    process.env.FASTMAIL_API_TOKEN = "source-only-token"
    const messages = [
      {
        ...email(
          "comment-message",
          "2026-08-03T10:00:00.000Z",
          "Useful comment",
        ),
        "header:X-GitHub-Reason:asText": "comment",
        messageId: ["raine/example/issues/1/123@github.com"],
      },
      {
        ...email(
          "subscribed-message",
          "2026-08-03T10:01:00.000Z",
          "Useful pull request",
        ),
        "header:X-GitHub-Reason:asText": "subscribed",
        messageId: ["raine/example/pull/2@github.com"],
      },
      {
        ...email(
          "mention-message",
          "2026-08-03T10:02:00.000Z",
          "Noisy mention",
        ),
        "header:X-GitHub-Reason:asText": "mention",
        messageId: ["raine/example/issues/3@github.com"],
      },
      {
        ...email(
          "release-message",
          "2026-08-03T10:03:00.000Z",
          "Noisy release",
        ),
        "header:X-GitHub-Reason:asText": "subscribed",
        messageId: ["raine/example/releases/123@github.com"],
      },
    ]
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
        return json({
          methodResponses: [
            [
              method,
              {
                added: messages.map((message, index) => ({
                  id: message.id,
                  index,
                })),
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
                  { id: "thread-1", emailIds: messages.map(({ id }) => id) },
                ],
              },
              callId,
            ],
          ],
        })
      }
      if (method === "Email/get") {
        expect(arguments_.properties).toContain("header:X-GitHub-Reason:asText")
        return json({ methodResponses: [[method, { list: messages }, callId]] })
      }
      throw new Error(`unexpected method ${method}`)
    }) as typeof fetch
    const pollRequest = request({
      queryState: "query-state-1",
      mailboxId: "inbox",
      sentMailboxId: "sent",
    })
    pollRequest.options.include_headers = {
      "X-GitHub-Reason": ["comment", "subscribed"],
    }
    pollRequest.options.include_message_id_contains = ["/issues/", "/pull/"]

    const result = await pollFastmail(pollRequest, fetcher)

    expect(result.items.map(({ revisionId }) => revisionId)).toEqual([
      "comment-message",
      "subscribed-message",
    ])
    expect(result.items[0]?.body).not.toContain("Noisy mention")
    expect(result.items[0]?.body).not.toContain("Noisy release")
  })

  test("omits excluded messages from later thread context", async () => {
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
        return json({
          methodResponses: [
            [
              method,
              {
                added: [{ id: "comment-message", index: 0 }],
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
                  {
                    id: "thread-1",
                    emailIds: ["push-message", "comment-message"],
                  },
                ],
              },
              callId,
            ],
          ],
        })
      }
      if (method === "Email/get") {
        const push = {
          ...email(
            "push-message",
            "2026-08-03T10:00:00.000Z",
            "Pushed one commit",
          ),
          "header:X-GitHub-Reason:asText": "push",
        }
        const comment = {
          ...email(
            "comment-message",
            "2026-08-03T10:05:00.000Z",
            "Useful review comment",
          ),
          "header:X-GitHub-Reason:asText": "subscribed",
        }
        const messages =
          arguments_.ids.length === 1 ? [comment] : [push, comment]
        return json({ methodResponses: [[method, { list: messages }, callId]] })
      }
      throw new Error(`unexpected method ${method}`)
    }) as typeof fetch
    const pollRequest = request({
      queryState: "query-state-1",
      mailboxId: "inbox",
      sentMailboxId: "sent",
    })
    pollRequest.options.exclude_headers = { "X-GitHub-Reason": ["push"] }

    const result = await pollFastmail(pollRequest, fetcher)

    expect(result.items).toHaveLength(1)
    expect(result.items[0]?.body).toContain("Useful review comment")
    expect(result.items[0]?.body).not.toContain("Pushed one commit")
  })

  test("suppresses a thread when its newest message is sent mail", async () => {
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
      if (method === "Mailbox/get") {
        return json({
          methodResponses: [
            [
              method,
              {
                list: [
                  { id: "inbox", role: "inbox" },
                  { id: "sent", role: "sent" },
                ],
              },
              callId,
            ],
          ],
        })
      }
      if (method === "Email/queryChanges") {
        return json({
          methodResponses: [
            [
              method,
              {
                added: [{ id: "message-1", index: 0 }],
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
        const incoming = email(
          "message-1",
          "2026-08-03T10:00:00.000Z",
          "Initial request",
        )
        const reply = {
          ...email("message-2", "2026-08-03T10:05:00.000Z", "Sent reply"),
          sentAt: "2026-08-03T10:06:00.000Z",
          mailboxIds: { sent: true },
        }
        const messages =
          arguments_.ids.length === 1 ? [incoming] : [incoming, reply]
        return json({ methodResponses: [[method, { list: messages }, callId]] })
      }
      throw new Error(`unexpected method ${method}`)
    }) as typeof fetch

    const result = await pollFastmail(
      request({ queryState: "query-state-1", mailboxId: "inbox" }),
      fetcher,
    )

    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      queryState: "query-state-2",
      mailboxId: "inbox",
      sentMailboxId: "sent",
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
      request({
        queryState: "query-state-1",
        mailboxId: "inbox",
        sentMailboxId: "sent",
      }),
      fetcher,
    )
    expect(result.items).toEqual([])
    expect(result.checkpoint).toEqual({
      queryState: "query-state-2",
      mailboxId: "inbox",
      sentMailboxId: "sent",
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
