#!/usr/bin/env bun
import {
  sourceMain,
  type IntakeItem,
  type PollRequest,
  type PollResponse,
} from "../protocol.ts"

interface JmapSession {
  apiUrl: string
  primaryAccounts: Record<string, string>
}

interface Checkpoint {
  queryState: string
  mailboxId: string
  sentMailboxId?: string | null
}

interface Email {
  id: string
  threadId: string
  subject?: string
  from?: Address[]
  to?: Address[]
  cc?: Address[]
  bcc?: Address[]
  receivedAt: string
  sentAt?: string
  messageId?: string[]
  mailboxIds?: Record<string, boolean>
  bodyValues?: Record<string, { value: string; isTruncated?: boolean }>
  textBody?: Array<{ partId?: string; type?: string }>
  htmlBody?: Array<{ partId?: string; type?: string }>
  bodyStructure?: BodyPart
}

interface HeaderRule {
  property: string
  values: Set<string>
}

interface EmailFilters {
  includeHeaders: HeaderRule[]
  excludeHeaders: HeaderRule[]
  includeMessageIdContains: string[]
}

interface Address {
  name?: string
  email: string
}

interface BodyPart {
  partId?: string
  blobId?: string
  name?: string
  type?: string
  size?: number
  disposition?: string
  cid?: string
  subParts?: BodyPart[]
}

const EMAIL_ACCOUNT_CAPABILITY = "urn:ietf:params:jmap:mail"
const BODY_LIMIT = 64 * 1024
const THREAD_MESSAGE_LIMIT = 100
const ATTACHMENT_LIMIT = 100

export async function pollFastmail(
  request: PollRequest,
  fetcher: typeof fetch = fetch,
): Promise<PollResponse> {
  const token = process.env.FASTMAIL_API_TOKEN
  if (!token) throw new Error("FASTMAIL_API_TOKEN is required")
  const sessionUrl =
    stringOption(request, "session_url") ??
    "https://api.fastmail.com/jmap/session"
  const sessionResponse = await fetcher(sessionUrl, {
    headers: { Authorization: `Bearer ${token}` },
  })
  if (!sessionResponse.ok)
    throw new Error(
      `Fastmail JMAP session request failed with ${sessionResponse.status}`,
    )
  const session = (await sessionResponse.json()) as JmapSession
  const accountId =
    stringOption(request, "account_id") ??
    session.primaryAccounts[EMAIL_ACCOUNT_CAPABILITY]
  if (!accountId) throw new Error("Fastmail JMAP session has no mail account")
  const checkpoint = request.checkpoint
    ? parseCheckpoint(request.checkpoint)
    : undefined
  let mailboxId = stringOption(request, "mailbox_id") ?? checkpoint?.mailboxId
  let sentMailboxId = checkpoint?.sentMailboxId
  if (!mailboxId || sentMailboxId === undefined) {
    const mailboxes = await findMailboxes(
      session.apiUrl,
      accountId,
      token,
      fetcher,
    )
    mailboxId ??= mailboxes.inboxId
    sentMailboxId = mailboxes.sentId
  }
  if (!mailboxId) throw new Error("Fastmail account has no inbox mailbox")
  const bootstrapLimit = Math.min(
    integerOption(request, "bootstrap_limit") ?? 0,
    request.itemLimit,
  )
  const filters = emailFilters(request)

  if (!request.checkpoint) {
    const baseline = await queryMailbox(
      session.apiUrl,
      accountId,
      token,
      mailboxId,
      bootstrapLimit,
      fetcher,
    )
    const messages =
      baseline.ids.length > 0
        ? await getEmails(
            session.apiUrl,
            accountId,
            token,
            baseline.ids,
            filters,
            fetcher,
          )
        : []
    return {
      protocolVersion: 1,
      checkpoint: { queryState: baseline.queryState, mailboxId, sentMailboxId },
      items: await normalizeMessages(
        session.apiUrl,
        accountId,
        token,
        messages,
        sentMailboxId,
        filters,
        fetcher,
      ),
    }
  }

  if (!checkpoint || checkpoint.mailboxId !== mailboxId) {
    const baseline = await queryMailbox(
      session.apiUrl,
      accountId,
      token,
      mailboxId,
      0,
      fetcher,
    )
    return {
      protocolVersion: 1,
      checkpoint: { queryState: baseline.queryState, mailboxId, sentMailboxId },
      items: [],
    }
  }

  const changes = await jmapCall<any>(
    session.apiUrl,
    token,
    [
      "Email/queryChanges",
      {
        accountId,
        filter: { inMailbox: mailboxId },
        sort: [{ property: "receivedAt", isAscending: false }],
        sinceQueryState: checkpoint.queryState,
        maxChanges: request.itemLimit,
      },
      "changes",
    ],
    fetcher,
  )
  if (typeof changes.newQueryState !== "string")
    throw new Error("Fastmail query changes response has no query state")
  const ids = (changes.added ?? [])
    .map((addition: { id?: string }) => addition.id)
    .filter((id: unknown): id is string => typeof id === "string")
    .slice(0, request.itemLimit)
  const messages =
    ids.length > 0
      ? await getEmails(session.apiUrl, accountId, token, ids, filters, fetcher)
      : []

  return {
    protocolVersion: 1,
    checkpoint: { queryState: changes.newQueryState, mailboxId, sentMailboxId },
    items: await normalizeMessages(
      session.apiUrl,
      accountId,
      token,
      messages,
      sentMailboxId,
      filters,
      fetcher,
    ),
  }
}

async function queryMailbox(
  apiUrl: string,
  accountId: string,
  token: string,
  mailboxId: string,
  limit: number,
  fetcher: typeof fetch,
): Promise<{ queryState: string; ids: string[] }> {
  const result = await jmapCall<any>(
    apiUrl,
    token,
    [
      "Email/query",
      {
        accountId,
        filter: { inMailbox: mailboxId },
        sort: [{ property: "receivedAt", isAscending: false }],
        limit,
      },
      "query",
    ],
    fetcher,
  )
  if (typeof result.queryState !== "string")
    throw new Error("Fastmail mailbox query response has no query state")
  return { queryState: result.queryState, ids: result.ids ?? [] }
}

async function normalizeMessages(
  apiUrl: string,
  accountId: string,
  token: string,
  messages: Email[],
  sentMailboxId: string | null,
  filters: EmailFilters,
  fetcher: typeof fetch,
): Promise<IntakeItem[]> {
  const threadCache = new Map<string, Email[]>()
  const items: IntakeItem[] = []
  const ordered = [...messages].sort((left, right) =>
    messageTimestamp(left).localeCompare(messageTimestamp(right)),
  )
  for (const email of ordered) {
    if (!isAllowed(email, filters)) continue
    let thread = threadCache.get(email.threadId)
    if (!thread) {
      thread = await getThread(
        apiUrl,
        accountId,
        token,
        email.threadId,
        filters,
        fetcher,
      )
      threadCache.set(email.threadId, thread)
    }
    const latest = thread.at(-1)
    if (latest && sentMailboxId && latest.mailboxIds?.[sentMailboxId]) continue
    items.push(normalizeEmail(accountId, email, thread))
  }
  return items
}

async function findMailboxes(
  apiUrl: string,
  accountId: string,
  token: string,
  fetcher: typeof fetch,
): Promise<{ inboxId: string | undefined; sentId: string | null }> {
  const result = await jmapCall<any>(
    apiUrl,
    token,
    ["Mailbox/get", { accountId, properties: ["id", "role"] }, "mailboxes"],
    fetcher,
  )
  const mailboxes = (result.list ?? []) as Array<{
    id?: string
    role?: string | null
  }>
  return {
    inboxId: mailboxes.find((mailbox) => mailbox.role === "inbox")?.id,
    sentId: mailboxes.find((mailbox) => mailbox.role === "sent")?.id ?? null,
  }
}

async function getEmails(
  apiUrl: string,
  accountId: string,
  token: string,
  ids: string[],
  filters: EmailFilters,
  fetcher: typeof fetch,
): Promise<Email[]> {
  const result = await jmapCall<any>(
    apiUrl,
    token,
    [
      "Email/get",
      {
        accountId,
        ids,
        properties: [
          "id",
          "threadId",
          "subject",
          "from",
          "to",
          "cc",
          "bcc",
          "receivedAt",
          "sentAt",
          "messageId",
          "mailboxIds",
          "textBody",
          "htmlBody",
          "bodyValues",
          "bodyStructure",
          ...new Set(
            [...filters.includeHeaders, ...filters.excludeHeaders].map(
              (rule) => rule.property,
            ),
          ),
        ],
        fetchTextBodyValues: true,
        fetchHTMLBodyValues: true,
        maxBodyValueBytes: BODY_LIMIT,
      },
      "emails",
    ],
    fetcher,
  )
  return result.list ?? []
}

async function getThread(
  apiUrl: string,
  accountId: string,
  token: string,
  threadId: string,
  filters: EmailFilters,
  fetcher: typeof fetch,
): Promise<Email[]> {
  const thread = await jmapCall<any>(
    apiUrl,
    token,
    ["Thread/get", { accountId, ids: [threadId] }, "thread"],
    fetcher,
  )
  const ids = (thread.list?.[0]?.emailIds ?? []).slice(-THREAD_MESSAGE_LIMIT)
  const emails = await getEmails(
    apiUrl,
    accountId,
    token,
    ids,
    filters,
    fetcher,
  )
  return emails
    .filter((email) => isAllowed(email, filters))
    .sort((left, right) =>
      messageTimestamp(left).localeCompare(messageTimestamp(right)),
    )
}

async function jmapCall<T>(
  apiUrl: string,
  token: string,
  methodCall: [string, Record<string, unknown>, string],
  fetcher: typeof fetch,
): Promise<T> {
  const response = await fetcher(apiUrl, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      using: [EMAIL_ACCOUNT_CAPABILITY, "urn:ietf:params:jmap:core"],
      methodCalls: [methodCall],
    }),
  })
  if (!response.ok)
    throw new Error(`Fastmail JMAP request failed with ${response.status}`)
  const json = (await response.json()) as {
    methodResponses?: Array<
      [string, T & { type?: string; description?: string }]
    >
  }
  const result = json.methodResponses?.[0]
  if (!result) throw new Error("Fastmail JMAP response has no method result")
  if (result[0] === "error")
    throw new Error(`Fastmail JMAP error: ${result[1].type ?? "unknown"}`)
  return result[1]
}

function messageTimestamp(email: Email): string {
  return email.sentAt ?? email.receivedAt
}

function normalizeEmail(
  accountId: string,
  email: Email,
  thread: Email[],
): IntakeItem {
  const threadBody = thread
    .map((message) => {
      const sender = formatAddresses(message.from)
      const recipients = formatAddresses([
        ...(message.to ?? []),
        ...(message.cc ?? []),
        ...(message.bcc ?? []),
      ])
      return `From: ${sender}\nTo: ${recipients}\nDate: ${message.receivedAt}\nSubject: ${message.subject ?? "(no subject)"}\n\n${bodyText(message)}`
    })
    .join("\n\n---\n\n")
    .slice(0, BODY_LIMIT * 4)
  const attachments = thread
    .flatMap((message) => attachmentMetadata(message.bodyStructure))
    .slice(0, ATTACHMENT_LIMIT)
  return {
    entityId: `fastmail:${accountId}:thread:${email.threadId}`,
    revisionId: email.id,
    kind: "email",
    title: email.subject ?? "(no subject)",
    body: threadBody,
    occurredAt: email.receivedAt,
    metadata: {
      messageId: email.id,
      threadId: email.threadId,
      from: email.from ?? [],
      to: email.to ?? [],
      cc: email.cc ?? [],
      bcc: email.bcc ?? [],
      attachments,
      threadMessageCount: thread.length,
    },
  }
}

function bodyText(email: Email): string {
  const parts = email.textBody?.length ? email.textBody : (email.htmlBody ?? [])
  const values = parts.map((part) =>
    part.partId ? (email.bodyValues?.[part.partId]?.value ?? "") : "",
  )
  return values.join("\n").slice(0, BODY_LIMIT)
}

function attachmentMetadata(
  root?: BodyPart,
): Array<Record<string, string | number | null>> {
  if (!root) return []
  const result: Array<Record<string, string | number | null>> = []
  const pending = [root]
  while (pending.length > 0 && result.length < ATTACHMENT_LIMIT) {
    const part = pending.pop()
    if (!part) continue
    pending.push(...(part.subParts ?? []))
    if (!part.blobId || (!part.name && part.disposition !== "attachment"))
      continue
    result.push({
      name: part.name ?? null,
      type: part.type ?? "application/octet-stream",
      size: part.size ?? 0,
      disposition: part.disposition ?? null,
      cid: part.cid ?? null,
    })
  }
  return result
}

function formatAddresses(addresses?: Address[]): string {
  return (addresses ?? [])
    .map((address) =>
      address.name ? `${address.name} <${address.email}>` : address.email,
    )
    .join(", ")
}

function parseCheckpoint(value: unknown): Checkpoint | undefined {
  if (!value || typeof value !== "object")
    throw new Error("Fastmail checkpoint is invalid")
  const checkpoint = value as Partial<Checkpoint>
  if (
    typeof checkpoint.queryState !== "string" ||
    typeof checkpoint.mailboxId !== "string" ||
    (checkpoint.sentMailboxId !== undefined &&
      checkpoint.sentMailboxId !== null &&
      typeof checkpoint.sentMailboxId !== "string")
  ) {
    return undefined
  }
  return checkpoint as Checkpoint
}

function emailFilters(request: PollRequest): EmailFilters {
  return {
    includeHeaders: headerRules(request, "include_headers"),
    excludeHeaders: headerRules(request, "exclude_headers"),
    includeMessageIdContains: stringListOption(
      request,
      "include_message_id_contains",
    ).map((value) => value.toLowerCase()),
  }
}

function headerRules(request: PollRequest, optionName: string): HeaderRule[] {
  const option = request.options[optionName]
  if (option === undefined) return []
  if (!option || typeof option !== "object" || Array.isArray(option))
    throw new Error(`Fastmail ${optionName} must map header names to values`)

  return Object.entries(option).map(([name, values]) => {
    if (!/^[A-Za-z0-9-]+$/.test(name))
      throw new Error(`Fastmail ${optionName} header name is invalid: ${name}`)
    if (
      !Array.isArray(values) ||
      values.length === 0 ||
      values.some((value) => typeof value !== "string" || !value.trim())
    )
      throw new Error(
        `Fastmail ${optionName} header ${name} must have a non-empty value list`,
      )
    return {
      property: `header:${name}:asText`,
      values: new Set(
        values.map((value) => (value as string).trim().toLowerCase()),
      ),
    }
  })
}

function isAllowed(email: Email, filters: EmailFilters): boolean {
  const includedByMessageId =
    filters.includeMessageIdContains.length === 0 ||
    (email.messageId ?? []).some((messageId) =>
      filters.includeMessageIdContains.some((value) =>
        messageId.toLowerCase().includes(value),
      ),
    )
  return (
    includedByMessageId &&
    filters.includeHeaders.every((rule) => matchesHeaderRule(email, rule)) &&
    !filters.excludeHeaders.some((rule) => matchesHeaderRule(email, rule))
  )
}

function matchesHeaderRule(email: Email, rule: HeaderRule): boolean {
  const value = Reflect.get(email, rule.property)
  return (
    typeof value === "string" && rule.values.has(value.trim().toLowerCase())
  )
}

function stringListOption(request: PollRequest, name: string): string[] {
  const value = request.options[name]
  if (value === undefined) return []
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.some((item) => typeof item !== "string" || !item.trim())
  )
    throw new Error(`Fastmail ${name} must be a non-empty string list`)
  return value.map((item) => (item as string).trim())
}

function stringOption(request: PollRequest, name: string): string | undefined {
  const value = request.options[name]
  return typeof value === "string" ? value : undefined
}

function integerOption(request: PollRequest, name: string): number | undefined {
  const value = request.options[name]
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : undefined
}

if (import.meta.main) sourceMain(pollFastmail)
