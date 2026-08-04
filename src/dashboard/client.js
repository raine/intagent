const state = { snapshot: null, filter: "all" }

const statusLabels = {
  pending: "Pending",
  processing: "Processing",
  retryable: "Retrying",
  succeeded: "Succeeded",
  failed: "Failed",
  ignored: "Ignored",
}

function element(id) {
  return document.getElementById(id)
}

function setText(id, value) {
  element(id).textContent = String(value)
}

function relativeTime(value, now = Date.now()) {
  const timestamp = Date.parse(value)
  if (!Number.isFinite(timestamp)) return "unknown"
  const seconds = Math.round((timestamp - now) / 1000)
  const absolute = Math.abs(seconds)
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" })
  if (absolute < 60) return formatter.format(seconds, "second")
  if (absolute < 3600)
    return formatter.format(Math.round(seconds / 60), "minute")
  if (absolute < 86400)
    return formatter.format(Math.round(seconds / 3600), "hour")
  return formatter.format(Math.round(seconds / 86400), "day")
}

function durationSince(value) {
  const milliseconds = Date.now() - Date.parse(value)
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return "just observed"
  const minutes = Math.floor(milliseconds / 60000)
  if (minutes < 1) return "waiting under 1 minute"
  if (minutes < 60) return `oldest waiting ${minutes}m`
  const hours = Math.floor(minutes / 60)
  if (hours < 48) return `oldest waiting ${hours}h ${minutes % 60}m`
  return `oldest waiting ${Math.floor(hours / 24)}d ${hours % 24}h`
}

function statusBadge(status) {
  const badge = document.createElement("span")
  badge.className = `status status-${status}`
  badge.textContent = statusLabels[status] || status
  return badge
}

function detailField(label, value, className) {
  const field = document.createElement("div")
  field.className = `detail-field${className ? ` ${className}` : ""}`
  const caption = document.createElement("span")
  caption.textContent = label
  const content = document.createElement("strong")
  content.textContent = value || "None"
  field.append(caption, content)
  return field
}

function safeExternalUrl(value) {
  if (!value) return null
  try {
    const url = new URL(value)
    return url.protocol === "https:" || url.protocol === "http:" ? url : null
  } catch {
    return null
  }
}

function eventDetail(event) {
  const template = element("event-detail-template")
  const row = template.content.firstElementChild.cloneNode(true)
  const detail = row.querySelector(".event-detail")
  detail.append(
    detailField("Entity", event.entityId),
    detailField(
      "Revision observed",
      new Date(event.observedAt).toLocaleString(),
    ),
    detailField("Attempts", String(event.attemptCount)),
    detailField(
      "Next attempt",
      event.nextAttemptAt
        ? new Date(event.nextAttemptAt).toLocaleString()
        : null,
    ),
    detailField("Aven task", event.avenRef),
    detailField("Investigation", event.investigationHandle),
  )
  const url = safeExternalUrl(event.url)
  if (url) {
    const field = document.createElement("div")
    field.className = "detail-field"
    const caption = document.createElement("span")
    caption.textContent = "Original item"
    const link = document.createElement("a")
    link.href = url.href
    link.target = "_blank"
    link.rel = "noreferrer"
    link.textContent = "Open source item"
    field.append(caption, link)
    detail.append(field)
  }
  if (event.lastError)
    detail.append(detailField("Last error", event.lastError, "error"))
  return row
}

function eventMatches(event) {
  if (state.filter === "all") return true
  if (state.filter === "open")
    return ["pending", "processing", "retryable"].includes(event.status)
  if (state.filter === "attention")
    return ["retryable", "failed"].includes(event.status)
  return event.status === state.filter
}

function renderEvents(events) {
  const body = element("event-rows")
  const expanded = new Set(
    [...body.querySelectorAll('[aria-expanded="true"]')].map((button) =>
      Number(button.dataset.eventId),
    ),
  )
  body.replaceChildren()
  const visible = events.filter(eventMatches)
  element("activity-empty").hidden = visible.length > 0

  for (const event of visible) {
    const row = document.createElement("tr")
    row.className = "event-row"

    const statusCell = document.createElement("td")
    statusCell.append(statusBadge(event.status))

    const itemCell = document.createElement("td")
    itemCell.className = "item-cell"
    const title = document.createElement("strong")
    title.textContent = event.title
    title.title = event.title
    const identity = document.createElement("span")
    identity.textContent = `${event.kind} · #${event.id}`
    itemCell.append(title, identity)

    const sourceCell = document.createElement("td")
    const source = document.createElement("span")
    source.className = "source-chip"
    source.textContent = event.source
    sourceCell.append(source)

    const timeCell = document.createElement("td")
    const time = document.createElement("time")
    time.className = "event-time"
    time.dateTime = event.observedAt
    time.title = new Date(event.observedAt).toLocaleString()
    time.textContent = relativeTime(event.observedAt)
    timeCell.append(time)

    const actionCell = document.createElement("td")
    const toggle = document.createElement("button")
    toggle.type = "button"
    toggle.className = "detail-toggle"
    toggle.dataset.eventId = String(event.id)
    toggle.setAttribute("aria-label", `Show details for ${event.title}`)
    toggle.setAttribute("aria-expanded", "false")
    toggle.insertAdjacentHTML(
      "afterbegin",
      '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m3 6 5 5 5-5"/></svg>',
    )
    actionCell.append(toggle)
    row.append(statusCell, itemCell, sourceCell, timeCell, actionCell)
    body.append(row)

    const detailRow = eventDetail(event)
    detailRow.hidden = true
    body.append(detailRow)
    toggle.addEventListener("click", () => {
      const open = toggle.getAttribute("aria-expanded") !== "true"
      toggle.setAttribute("aria-expanded", String(open))
      toggle.setAttribute(
        "aria-label",
        `${open ? "Hide" : "Show"} details for ${event.title}`,
      )
      detailRow.hidden = !open
    })
    if (expanded.has(event.id)) toggle.click()
  }
}

function renderSources(sources) {
  const list = element("source-list")
  list.replaceChildren()
  setText("source-count", sources.length)
  if (sources.length === 0) {
    const empty = document.createElement("p")
    empty.className = "source-empty"
    empty.textContent = "Sources appear after their first poll."
    list.append(empty)
    return
  }

  for (const source of sources) {
    const card = document.createElement("article")
    card.className = "source-card"
    const head = document.createElement("div")
    head.className = "source-card-head"
    const name = document.createElement("strong")
    name.className = "source-name"
    name.textContent = source.source
    const health = document.createElement("span")
    health.className = `source-health${source.lastError ? " error" : ""}`
    health.textContent = source.lastError ? "Error" : "Healthy"
    head.append(name, health)
    const time = document.createElement("p")
    time.className = "source-time"
    time.textContent = source.lastSuccessAt
      ? `Last poll ${relativeTime(source.lastSuccessAt)}`
      : "Waiting for first successful poll"
    card.append(head, time)
    if (source.lastError) {
      const error = document.createElement("p")
      error.className = "source-error"
      error.textContent = source.lastError
      card.append(error)
    }
    list.append(card)
  }
}

function render(snapshot) {
  state.snapshot = snapshot
  setText("open-count", snapshot.open)
  setText("attention-count", snapshot.attention)
  setText(
    "queue-age",
    snapshot.oldestOpenAt
      ? durationSince(snapshot.oldestOpenAt)
      : "No waiting items",
  )
  setText("flow-total", `${snapshot.total} events retained`)
  setText("updated-at", `updated ${relativeTime(snapshot.generatedAt)}`)
  document.querySelectorAll("[data-count]").forEach((node) => {
    const key = node.dataset.count
    node.textContent =
      key === "handled"
        ? snapshot.handled + snapshot.counts.failed
        : snapshot.counts[key]
  })

  const attention = element("attention-card")
  attention.classList.toggle("alert", snapshot.attention > 0)
  attention.classList.toggle("clear", snapshot.attention === 0)
  setText(
    "hero-summary",
    snapshot.total === 0
      ? "The ledger is ready. Intake activity appears as sources report events."
      : snapshot.attention > 0
        ? `${snapshot.attention} ${snapshot.attention === 1 ? "item needs" : "items need"} review across ${snapshot.sources.length} ${snapshot.sources.length === 1 ? "source" : "sources"}.`
        : `${snapshot.handled} ${snapshot.handled === 1 ? "item is" : "items are"} handled, with no failures waiting for review.`,
  )
  renderEvents(snapshot.events)
  renderSources(snapshot.sources)
}

async function refresh() {
  const connection = document.querySelector(".connection")
  const label = element("connection-label")
  try {
    const response = await fetch("/api/snapshot", { cache: "no-store" })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    render(await response.json())
    connection.className = "connection connected"
    label.textContent = "live"
  } catch {
    connection.className = "connection disconnected"
    label.textContent = "disconnected"
  }
}

element("status-filter").addEventListener("change", (event) => {
  state.filter = event.target.value
  if (state.snapshot) renderEvents(state.snapshot.events)
})

refresh()
setInterval(refresh, 5000)
