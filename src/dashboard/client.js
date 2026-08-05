const state = {
  snapshot: null,
  filter: "all",
  refreshTimer: null,
  contentKey: null,
  expandedEvents: new Set(),
  expandedRuns: new Set(),
}

const runStateLabels = {
  active: "Active",
  succeeded: "Succeeded",
  failed: "Failed",
  interrupted: "Interrupted",
}

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

function elapsedTime(start, end = Date.now()) {
  const milliseconds = Math.max(0, end - Date.parse(start))
  if (!Number.isFinite(milliseconds)) return "unknown"
  if (milliseconds < 1000) return "under 1s"
  const seconds = Math.floor(milliseconds / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

function runDuration(run) {
  return elapsedTime(
    run.startedAt,
    run.endedAt ? Date.parse(run.endedAt) : Date.now(),
  )
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
    const setExpanded = (open) => {
      toggle.setAttribute("aria-expanded", String(open))
      toggle.setAttribute(
        "aria-label",
        `${open ? "Hide" : "Show"} details for ${event.title}`,
      )
      detailRow.hidden = !open
    }
    toggle.addEventListener("click", () => {
      const open = !state.expandedEvents.has(event.id)
      if (open) state.expandedEvents.add(event.id)
      else state.expandedEvents.delete(event.id)
      setExpanded(open)
    })
    if (state.expandedEvents.has(event.id)) setExpanded(true)
  }
}

function runBadge(value) {
  const badge = document.createElement("span")
  badge.className = `run-state run-state-${value}`
  const dot = document.createElement("i")
  dot.setAttribute("aria-hidden", "true")
  const label = document.createElement("span")
  label.textContent = runStateLabels[value] || value
  badge.append(dot, label)
  return badge
}

function runActivity(run) {
  const tools = run.steps.length
  const parts = [
    `${tools} tool ${tools === 1 ? "call" : "calls"}`,
    `${run.turnCount} ${run.turnCount === 1 ? "turn" : "turns"}`,
  ]
  if (run.retryCount > 0) parts.push(`${run.retryCount} model retries`)
  if (run.compactionCount > 0) parts.push(`${run.compactionCount} compactions`)
  return parts.join(" · ")
}

function buildRunDetail(run) {
  const detail = document.createElement("div")
  const facts = document.createElement("div")
  facts.className = "run-facts"
  facts.append(
    detailField("Started", new Date(run.startedAt).toLocaleString()),
    detailField("Attempt", String(run.attempt)),
    detailField("Model", run.modelId),
    detailField("Provider", run.modelProvider),
    detailField("Thinking", run.thinkingLevel),
    detailField("Investigation", run.investigationHandle),
  )
  detail.append(facts)

  const heading = document.createElement("div")
  heading.className = "timeline-heading"
  const title = document.createElement("strong")
  title.textContent = "Tool activity"
  const privacy = document.createElement("span")
  privacy.textContent =
    "Commands, arguments, output, and intake content stay private."
  heading.append(title, privacy)
  detail.append(heading)

  if (run.steps.length === 0) {
    const empty = document.createElement("p")
    empty.className = "timeline-empty"
    empty.textContent =
      run.state === "active"
        ? "Waiting for the first tool call."
        : "This run completed without a recorded tool call."
    detail.append(empty)
    return detail
  }

  const timeline = document.createElement("ol")
  timeline.className = "run-timeline"
  for (const step of run.steps) {
    const item = document.createElement("li")
    item.className = `timeline-step timeline-step-${step.state}`
    const mark = document.createElement("span")
    mark.className = "timeline-mark"
    mark.setAttribute("aria-hidden", "true")
    const body = document.createElement("div")
    const label = document.createElement("strong")
    label.textContent = step.label
    const meta = document.createElement("span")
    const duration = elapsedTime(
      step.startedAt,
      step.endedAt ? Date.parse(step.endedAt) : Date.now(),
    )
    meta.textContent = `${runStateLabels[step.state]} · ${duration} · ${new Date(step.startedAt).toLocaleTimeString()}`
    body.append(label, meta)
    item.append(mark, body)
    timeline.append(item)
  }
  detail.append(timeline)
  return detail
}

function renderActiveRuns(runs) {
  const container = element("active-runs")
  container.replaceChildren()
  const active = runs.filter((run) => run.state === "active")
  container.hidden = active.length === 0
  for (const run of active) {
    const card = document.createElement("article")
    card.className = "active-run-card"
    card.dataset.runId = String(run.id)
    const pulse = document.createElement("span")
    pulse.className = "active-run-pulse"
    pulse.setAttribute("aria-hidden", "true")
    const copy = document.createElement("div")
    const eyebrow = document.createElement("span")
    eyebrow.textContent = `RUN ${run.id} · ATTEMPT ${run.attempt}`
    const title = document.createElement("strong")
    title.textContent = run.eventTitle
    const activity = document.createElement("span")
    activity.className = "active-run-activity"
    activity.textContent = `${runActivity(run)} · active ${runDuration(run)}`
    copy.append(eyebrow, title, activity)
    const live = document.createElement("span")
    live.className = "active-run-live"
    live.textContent = `Updated ${relativeTime(run.lastActivityAt)}`
    card.append(pulse, copy, live)
    container.append(card)
  }
}

function renderRuns(runs) {
  renderActiveRuns(runs)
  const body = element("run-rows")
  body.replaceChildren()
  element("runs-empty").hidden = runs.length > 0
  const activeCount = runs.filter((run) => run.state === "active").length
  setText(
    "runs-summary",
    runs.length === 0
      ? "No runs recorded"
      : `${activeCount} active · ${runs.length} recent ${runs.length === 1 ? "run" : "runs"}`,
  )

  for (const run of runs) {
    const row = document.createElement("tr")
    row.className = "run-row"
    row.dataset.runId = String(run.id)
    const stateCell = document.createElement("td")
    stateCell.append(runBadge(run.state))

    const runCell = document.createElement("td")
    runCell.className = "run-item-cell"
    const title = document.createElement("strong")
    title.textContent = run.eventTitle
    title.title = run.eventTitle
    const identity = document.createElement("span")
    identity.textContent = `${run.source} · event #${run.eventId} · attempt ${run.attempt}`
    runCell.append(title, identity)

    const activityCell = document.createElement("td")
    activityCell.className = "run-activity"
    activityCell.textContent = runActivity(run)
    const durationCell = document.createElement("td")
    durationCell.className = "run-duration"
    durationCell.textContent = runDuration(run)

    const actionCell = document.createElement("td")
    const toggle = document.createElement("button")
    toggle.type = "button"
    toggle.className = "detail-toggle"
    toggle.dataset.runId = String(run.id)
    toggle.setAttribute("aria-label", `Show details for run ${run.id}`)
    toggle.setAttribute("aria-expanded", "false")
    toggle.insertAdjacentHTML(
      "afterbegin",
      '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m3 6 5 5 5-5"/></svg>',
    )
    actionCell.append(toggle)
    row.append(stateCell, runCell, activityCell, durationCell, actionCell)
    body.append(row)

    const template = element("run-detail-template")
    const detailRow = template.content.firstElementChild.cloneNode(true)
    detailRow.hidden = true
    body.append(detailRow)
    const setExpanded = (open) => {
      toggle.setAttribute("aria-expanded", String(open))
      toggle.setAttribute(
        "aria-label",
        `${open ? "Hide" : "Show"} details for run ${run.id}`,
      )
      detailRow.hidden = !open
      if (open && !detailRow.querySelector(".run-detail").hasChildNodes())
        detailRow.querySelector(".run-detail").append(buildRunDetail(run))
    }
    toggle.addEventListener("click", () => {
      const open = !state.expandedRuns.has(run.id)
      if (open) state.expandedRuns.add(run.id)
      else state.expandedRuns.delete(run.id)
      setExpanded(open)
    })
    if (state.expandedRuns.has(run.id)) setExpanded(true)
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

function snapshotContentKey(snapshot) {
  const { generatedAt: _, ...content } = snapshot
  return JSON.stringify(content)
}

function refreshLiveTimes(snapshot) {
  state.snapshot = snapshot
  setText("updated-at", `updated ${relativeTime(snapshot.generatedAt)}`)
  setText(
    "queue-age",
    snapshot.oldestOpenAt
      ? durationSince(snapshot.oldestOpenAt)
      : "No waiting items",
  )
  for (const run of snapshot.runs) {
    const row = document.querySelector(`.run-row[data-run-id="${run.id}"]`)
    const duration = row?.querySelector(".run-duration")
    if (duration) duration.textContent = runDuration(run)
    const card = document.querySelector(
      `.active-run-card[data-run-id="${run.id}"]`,
    )
    const activity = card?.querySelector(".active-run-activity")
    if (activity)
      activity.textContent = `${runActivity(run)} · active ${runDuration(run)}`
    const live = card?.querySelector(".active-run-live")
    if (live) live.textContent = `Updated ${relativeTime(run.lastActivityAt)}`
  }
}

function render(snapshot) {
  state.snapshot = snapshot
  state.contentKey = snapshotContentKey(snapshot)
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
  renderRuns(snapshot.runs)
  renderEvents(snapshot.events)
  renderSources(snapshot.sources)
}

async function refresh() {
  const connection = document.querySelector(".connection")
  const label = element("connection-label")
  let delay = 5000
  try {
    const response = await fetch("/api/snapshot", { cache: "no-store" })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const snapshot = await response.json()
    if (snapshotContentKey(snapshot) === state.contentKey)
      refreshLiveTimes(snapshot)
    else render(snapshot)
    if (snapshot.runs.some((run) => run.state === "active")) delay = 1500
    connection.className = "connection connected"
    label.textContent = "live"
  } catch {
    connection.className = "connection disconnected"
    label.textContent = "disconnected"
  } finally {
    clearTimeout(state.refreshTimer)
    state.refreshTimer = setTimeout(refresh, delay)
  }
}

function resolvedTheme(choice) {
  if (choice !== "system") return choice
  return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
}

function applyTheme(choice, persist) {
  document.documentElement.dataset.theme = resolvedTheme(choice)
  document.documentElement.dataset.themeChoice = choice
  element("theme-select").value = choice
  if (persist) {
    try {
      localStorage.setItem("intake-theme", choice)
    } catch {}
  }
}

const themeChoice = document.documentElement.dataset.themeChoice || "system"
applyTheme(themeChoice, false)
element("theme-select").addEventListener("change", (event) => {
  applyTheme(event.target.value, true)
})
const systemTheme = matchMedia("(prefers-color-scheme: dark)")
systemTheme.addEventListener("change", () => {
  if (document.documentElement.dataset.themeChoice === "system")
    applyTheme("system", false)
})

element("status-filter").addEventListener("change", (event) => {
  state.filter = event.target.value
  if (state.snapshot) renderEvents(state.snapshot.events)
})

refresh()
