const state = {
  snapshot: null,
  filter: "all",
  contentKey: null,
  refreshTimer: null,
  expandedEvents: new Set(),
  expandedRuns: new Set(),
  selectedRun: null,
  lastSuccessAt: null,
}

const eventStates = {
  pending: { glyph: "○", label: "Pending" },
  processing: { glyph: "◐", label: "Processing" },
  retryable: { glyph: "↻", label: "Retryable" },
  succeeded: { glyph: "✓", label: "Succeeded" },
  failed: { glyph: "✕", label: "Failed" },
  ignored: { glyph: "-", label: "Ignored" },
}

const runStates = {
  active: { glyph: "●", label: "active" },
  succeeded: { glyph: "✓", label: "succeeded" },
  failed: { glyph: "✕", label: "failed" },
  interrupted: { glyph: "⏸", label: "interrupted" },
}

function element(id) {
  return document.getElementById(id)
}

function node(tag, className, text) {
  const result = document.createElement(tag)
  if (className) result.className = className
  if (text !== undefined) result.textContent = text
  return result
}

function setText(id, value) {
  element(id).textContent = String(value)
}

function parseTime(value) {
  const parsed = Date.parse(value)
  return Number.isFinite(parsed) ? parsed : null
}

function formatDuration(milliseconds) {
  const value = Math.max(0, milliseconds)
  if (!Number.isFinite(value)) return "unknown"
  if (value < 1000) return `${Math.round(value)}ms`
  if (value < 60000) {
    const seconds = value / 1000
    return `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)}s`
  }
  const minutes = Math.floor(value / 60000)
  const seconds = Math.floor((value % 60000) / 1000)
  if (minutes < 60) return `${minutes}m ${String(seconds).padStart(2, "0")}s`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

function elapsed(start, end = Date.now()) {
  const startedAt = parseTime(start)
  if (startedAt === null) return 0
  return Math.max(0, end - startedAt)
}

function runDuration(run) {
  return elapsed(run.startedAt, parseTime(run.endedAt) ?? Date.now())
}

function stepDuration(step) {
  return elapsed(step.startedAt, parseTime(step.endedAt) ?? Date.now())
}

function relativeTime(value, now = Date.now()) {
  const timestamp = parseTime(value)
  if (timestamp === null) return "unknown"
  const milliseconds = Math.max(0, now - timestamp)
  const seconds = Math.floor(milliseconds / 1000)
  if (seconds < 60) return `${Math.max(1, seconds)}s ago`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ${minutes % 60}m ago`
  return `${Math.floor(hours / 24)}d ${hours % 24}h ago`
}

function absoluteTime(value) {
  const timestamp = parseTime(value)
  return timestamp === null ? "unknown" : new Date(timestamp).toLocaleString()
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

function statusLabel(status) {
  const definition = eventStates[status] || { glyph: "?", label: status }
  const label = node("span", `status-label status-${status}`)
  label.append(
    node("span", "status-glyph", definition.glyph),
    document.createTextNode(definition.label),
  )
  return label
}

function detailField(label, value) {
  const field = node("div", "detail-field")
  field.append(
    node("span", "detail-label", label),
    node("span", "detail-value", value || "-"),
  )
  return field
}

function linkedEvent(run) {
  return (
    state.snapshot?.events.find((event) => event.id === run.eventId) || null
  )
}

function toolPresentation(step) {
  if (step.state === "active") return { glyph: "▸", className: "active" }
  if (step.state === "failed") return { glyph: "✕", className: "failed" }
  if (step.state === "interrupted")
    return { glyph: "⏸", className: "interrupted" }
  return { glyph: "✓", className: "succeeded" }
}

function toolWidth(duration, maxDuration) {
  if (maxDuration <= 0) return 4
  return Math.max(
    4,
    Math.min(
      38,
      Math.round((Math.log1p(duration) / Math.log1p(maxDuration)) * 38),
    ),
  )
}

function toolEntry(step, maxDuration, includeOffset, run) {
  const presentation = toolPresentation(step)
  const duration = stepDuration(step)
  const entry = node("div", `tool-entry tool-entry-${presentation.className}`)
  entry.dataset.stepStartedAt = step.startedAt
  if (step.endedAt) entry.dataset.stepEndedAt = step.endedAt
  entry.append(
    node("span", "tool-glyph", presentation.glyph),
    node("span", "tool-name", step.label),
  )
  const bar = node("span", "tool-bar")
  bar.style.setProperty("--tool-width", `${toolWidth(duration, maxDuration)}%`)
  bar.setAttribute("aria-hidden", "true")
  entry.append(bar)
  const durationLabel = node(
    "span",
    "tool-duration",
    `${formatDuration(duration)}${step.state === "active" ? "..." : ""}`,
  )
  entry.append(durationLabel)
  if (includeOffset)
    entry.append(
      node(
        "span",
        "tool-offset",
        `+${formatDuration(elapsed(run.startedAt, parseTime(step.startedAt)))}`,
      ),
    )
  return entry
}

function renderToolList(container, run, includeOffset = false) {
  if (run.steps.length === 0) return
  const maxDuration = Math.max(...run.steps.map(stepDuration), 1)
  for (const step of run.steps)
    container.append(toolEntry(step, maxDuration, includeOffset, run))
}

function runCounts(run) {
  const counts = node("div", "run-counts")
  const values = [
    ["turns", run.turnCount],
    ["retries", run.retryCount],
    ["compactions", run.compactionCount],
  ]
  for (const [label, value] of values) {
    const item = node("span")
    item.append(`${label} `, node("b", "", String(value)))
    counts.append(item)
  }
  return counts
}

function currentActivity(run) {
  const activeStep = run.steps.find((step) => step.state === "active")
  const activity = node("div", "current-activity")
  activity.append(node("span", "current-activity-glyph", "▸"))
  if (activeStep) {
    activity.append(
      node("strong", "", activeStep.label),
      node("span", "", `running ${formatDuration(stepDuration(activeStep))}`),
    )
  } else {
    activity.append(
      node("strong", "", "Agent"),
      node("span", "", `last activity ${relativeTime(run.lastActivityAt)}`),
    )
  }
  return activity
}

function activeRunCard(run) {
  const card = node("article", "active-run-card")
  card.dataset.runId = String(run.id)

  const heading = node("div", "run-card-head")
  heading.append(
    node("span", "active-pulse"),
    node("span", "active-label", "ACTIVE"),
    node("span", "attempt-label", `attempt ${run.attempt}`),
  )
  heading.firstElementChild.setAttribute("aria-hidden", "true")
  const duration = node(
    "time",
    "run-live-duration",
    formatDuration(runDuration(run)),
  )
  duration.dataset.startedAt = run.startedAt
  heading.append(duration)

  const titleBlock = node("div")
  titleBlock.append(
    node("h3", "run-title", run.eventTitle),
    node(
      "p",
      "run-subtitle",
      `${run.source} - ${run.modelId || "model pending"} - thinking ${run.thinkingLevel || "pending"}`,
    ),
  )

  card.append(heading, titleBlock, runCounts(run), currentActivity(run))

  if (elapsed(run.lastActivityAt) > 120000) {
    const warning = node("div", "stall-warning")
    warning.append(
      node("span", "", "⚠"),
      node(
        "span",
        "",
        `Possibly stalled - no activity for ${formatDuration(elapsed(run.lastActivityAt))}`,
      ),
    )
    card.append(warning)
  }

  const actions = node("div", "run-actions")
  const toolToggle = node(
    "button",
    "text-button",
    `${state.expandedRuns.has(run.id) ? "▾" : "▸"} Tool activity (${run.steps.length})`,
  )
  toolToggle.type = "button"
  toolToggle.setAttribute(
    "aria-expanded",
    String(state.expandedRuns.has(run.id)),
  )
  toolToggle.addEventListener("click", () => {
    if (state.expandedRuns.has(run.id)) state.expandedRuns.delete(run.id)
    else state.expandedRuns.add(run.id)
    renderActiveRuns(state.snapshot.runs)
  })
  const open = node("button", "text-button", "Full run view →")
  open.type = "button"
  open.addEventListener("click", () => openRun(run.id))
  actions.append(toolToggle, open)
  card.append(actions)

  if (state.expandedRuns.has(run.id)) {
    const tools = node("div", "inline-tools")
    if (run.steps.length) renderToolList(tools, run)
    else tools.append(node("p", "run-handle", "No tool activity recorded"))
    tools.append(
      node(
        "p",
        "run-handle",
        `Investigation handle: ${run.investigationHandle || "-"}`,
      ),
    )
    card.append(tools)
  }
  return card
}

function renderActiveRuns(runs) {
  const container = element("active-runs")
  container.replaceChildren()
  const active = runs.filter((run) => run.state === "active")
  if (active.length === 0) {
    container.className = ""
    container.append(node("p", "empty-panel", "No active runs - queue is idle"))
    return
  }
  container.className = "active-runs"
  for (const run of active) container.append(activeRunCard(run))
}

function eventMatches(event) {
  if (state.filter === "all") return true
  if (state.filter === "open")
    return ["pending", "processing", "retryable"].includes(event.status)
  if (state.filter === "attention")
    return ["retryable", "failed"].includes(event.status)
  return ["succeeded", "ignored"].includes(event.status)
}

function eventDetail(event) {
  const detail = node("div", "event-detail")
  if (event.lastError) {
    const error = node("div", "error-callout event-error")
    error.append(node("span", "", "✕"), node("span", "", event.lastError))
    if (event.nextAttemptAt)
      error.append(
        node("span", "", `- next retry ${relativeTime(event.nextAttemptAt)}`),
      )
    detail.append(error)
  }
  const fields = node("div", "detail-fields")
  fields.append(
    detailField("Entity", event.entityId),
    detailField("Status", eventStates[event.status]?.label || event.status),
    detailField("Occurred", absoluteTime(event.occurredAt)),
    detailField("Observed", absoluteTime(event.observedAt)),
    detailField("Attempts", String(event.attemptCount)),
    detailField("Task", event.avenRef),
    detailField("Investigation", event.investigationHandle),
  )
  if (event.nextAttemptAt)
    fields.append(detailField("Next retry", absoluteTime(event.nextAttemptAt)))
  detail.append(fields)
  const url = safeExternalUrl(event.url)
  if (url) {
    const link = node("a", "event-link", `Open in ${event.source} ↗`)
    link.href = url.href
    link.target = "_blank"
    link.rel = "noreferrer"
    detail.append(link)
  }
  return detail
}

function eventItem(event) {
  const item = node("div", "event-item")
  const expanded = state.expandedEvents.has(event.id)
  const button = node("button", "event-summary")
  button.type = "button"
  button.setAttribute("aria-expanded", String(expanded))
  button.setAttribute(
    "aria-label",
    `${expanded ? "Hide" : "Show"} details for ${event.title}`,
  )
  const age = node("span", "event-age", relativeTime(event.observedAt))
  age.dataset.relativeTime = event.observedAt
  button.append(
    statusLabel(event.status),
    node("span", "event-title", event.title),
    node("span", "event-meta", `${event.source} - ${event.kind}`),
    age,
    node("span", "chevron", expanded ? "▾" : "▸"),
  )
  button.addEventListener("click", () => {
    if (state.expandedEvents.has(event.id))
      state.expandedEvents.delete(event.id)
    else state.expandedEvents.add(event.id)
    renderEvents(state.snapshot.events)
  })
  item.append(button)
  if (expanded) item.append(eventDetail(event))
  return item
}

function renderEvents(events) {
  const list = element("event-list")
  list.replaceChildren()
  const visible = events.filter(eventMatches)
  if (visible.length === 0) {
    const noun = state.filter === "all" ? "recent" : state.filter
    list.append(
      node(
        "p",
        "empty-panel",
        events.length === 0
          ? "No events in the queue - all clear"
          : `No ${noun} events`,
      ),
    )
  } else {
    for (const event of visible) list.append(eventItem(event))
  }
  const note =
    state.filter === "handled"
      ? `Showing ${visible.length} most recent of ${state.snapshot.handled} handled events`
      : `Showing ${visible.length} events from the recent intake ledger`
  setText("event-list-note", note)
}

function renderFilters(snapshot) {
  const counts = {
    all: snapshot.events.length,
    open: snapshot.open,
    attention: snapshot.attention,
    handled: snapshot.handled,
  }
  document.querySelectorAll("[data-filter]").forEach((button) => {
    const filter = button.dataset.filter
    button.setAttribute("aria-pressed", String(filter === state.filter))
    button.querySelector("b").textContent = String(counts[filter])
  })
}

function renderSources(sources) {
  const container = element("source-list")
  container.replaceChildren()
  if (sources.length === 0) {
    container.append(
      node("p", "empty-panel", "Sources appear after their first poll"),
    )
    return
  }
  for (const source of sources) {
    const failing = Boolean(source.lastError)
    const row = node(
      "article",
      `source-row${failing ? " source-row-failing" : ""}`,
    )
    const heading = node("div", "source-heading")
    heading.append(
      node("span", "status-glyph", failing ? "✕" : "✓"),
      node("strong", "", source.source),
      node("strong", "source-health", failing ? "Failing" : "Healthy"),
    )
    const poll = node(
      "p",
      "source-poll",
      source.lastSuccessAt
        ? `last poll ${relativeTime(source.lastSuccessAt)}`
        : "waiting for first successful poll",
    )
    if (source.lastSuccessAt) poll.dataset.relativeTimePrefix = "last poll "
    if (source.lastSuccessAt) poll.dataset.relativeTime = source.lastSuccessAt
    row.append(heading, poll)
    if (source.lastError)
      row.append(node("p", "source-error", `✕ ${source.lastError}`))
    container.append(row)
  }
}

function historyItem(run) {
  const item = node("div", "history-item")
  const expanded = state.expandedRuns.has(run.id)
  const definition = runStates[run.state]
  const summary = node("button", "history-summary")
  summary.type = "button"
  summary.setAttribute("aria-expanded", String(expanded))
  summary.setAttribute(
    "aria-label",
    `${expanded ? "Hide" : "Show"} details for run ${run.id}`,
  )
  const glyph = node(
    "span",
    `run-state-glyph run-state-${run.state}`,
    definition.glyph,
  )
  const copy = node("span", "history-copy")
  const meta = node("span")
  meta.append(
    node("b", `history-state history-state-${run.state}`, definition.label),
    ` - ${formatDuration(runDuration(run))} - `,
  )
  const when = node(
    "span",
    "history-when",
    relativeTime(run.endedAt || run.lastActivityAt),
  )
  when.dataset.relativeTime = run.endedAt || run.lastActivityAt
  meta.append(when)
  copy.append(node("strong", "", run.eventTitle), meta)
  summary.append(glyph, copy, node("span", "chevron", expanded ? "▾" : "▸"))
  summary.addEventListener("click", () => {
    if (state.expandedRuns.has(run.id)) state.expandedRuns.delete(run.id)
    else state.expandedRuns.add(run.id)
    renderRunHistory(state.snapshot.runs)
  })
  item.append(summary)

  if (expanded) {
    const detail = node("div", "history-detail")
    const metaRow = node("div", "history-meta")
    metaRow.append(
      node("span", "", run.modelId || "model unavailable"),
      node("span", "", `attempt ${run.attempt}`),
      node("span", "", `turns ${run.turnCount}`),
      node("span", "", `compactions ${run.compactionCount}`),
    )
    detail.append(metaRow)
    const event = linkedEvent(run)
    if (run.state === "failed" && event?.lastError)
      detail.append(node("div", "error-callout", `✕ ${event.lastError}`))
    if (run.steps.length) {
      detail.append(node("p", "history-tools-title", "Tool activity"))
      const tools = node("div", "inline-tools")
      renderToolList(tools, run)
      detail.append(tools)
    }
    const footer = node("div", "history-footer")
    footer.append(
      node("p", "run-handle", `Handle: ${run.investigationHandle || "-"}`),
    )
    const open = node("button", "text-button", "Full run view →")
    open.type = "button"
    open.addEventListener("click", () => openRun(run.id))
    footer.append(open)
    detail.append(footer)
    item.append(detail)
  }
  return item
}

function renderRunHistory(runs) {
  const container = element("run-history")
  container.replaceChildren()
  const completed = runs.filter((run) => run.state !== "active")
  if (completed.length === 0) {
    container.append(node("p", "empty-panel", "No completed runs yet"))
    return
  }
  for (const run of completed) container.append(historyItem(run))
}

function updateOverview(snapshot) {
  const active = snapshot.runs.filter((run) => run.state === "active")
  const stalled = active.filter(
    (run) => elapsed(run.lastActivityAt) > 120000,
  ).length
  const oldestEvent = snapshot.oldestOpenAt
    ? snapshot.events
        .filter((event) =>
          ["pending", "processing", "retryable"].includes(event.status),
        )
        .sort((left, right) =>
          left.observedAt.localeCompare(right.observedAt),
        )[0]
    : null
  setText("stat-open", snapshot.open)
  setText(
    "stat-open-note",
    snapshot.open ? "pending - processing - retryable" : "queue is clear",
  )
  setText("stat-attention", snapshot.attention)
  setText(
    "stat-attention-note",
    snapshot.attention ? "retryable + failed" : "nothing to review",
  )
  element("attention-stat").classList.toggle(
    "stat-warn",
    snapshot.attention > 0,
  )
  setText("stat-active", active.length)
  setText(
    "stat-active-note",
    active.length
      ? stalled
        ? `${stalled} possibly stalled`
        : "triage in progress"
      : "idle",
  )
  setText("stat-handled", snapshot.handled)
  setText(
    "stat-oldest",
    snapshot.oldestOpenAt
      ? formatDuration(elapsed(snapshot.oldestOpenAt)).replace(/ \d+s$/, "")
      : "-",
  )
  setText("stat-oldest-note", oldestEvent?.title || "")
}

function renderDashboard(snapshot) {
  updateOverview(snapshot)
  renderFilters(snapshot)
  renderActiveRuns(snapshot.runs)
  renderEvents(snapshot.events)
  renderSources(snapshot.sources)
  renderRunHistory(snapshot.runs)
  setText("refresh-note", `connected to ${location.host}`)
  setText("detail-refresh-note", `connected to ${location.host}`)
}

function detailState(run) {
  const definition = runStates[run.state]
  const result = node("div", `detail-state detail-state-${run.state}`)
  result.append(
    node("span", "status-glyph", definition.glyph),
    node("span", "", definition.label),
    node("span", "detail-run-id", `run ${run.id}`),
  )
  return result
}

function runDetail(run) {
  const container = node("div", "run-detail-content")
  const event = linkedEvent(run)
  const hero = node("div", "detail-hero")
  const copy = node("div", "detail-hero-copy")
  copy.append(
    detailState(run),
    node("h1", "", run.eventTitle),
    node(
      "p",
      "",
      `${run.source} - investigation ${run.investigationHandle || "-"}`,
    ),
  )
  const duration = node("div", "detail-duration")
  const durationValue = node(
    "strong",
    "run-live-duration",
    formatDuration(runDuration(run)),
  )
  durationValue.dataset.startedAt = run.startedAt
  if (run.endedAt) durationValue.dataset.endedAt = run.endedAt
  duration.append(
    durationValue,
    node(
      "span",
      "",
      run.state === "active"
        ? "running - refreshes every 1.5s"
        : "total duration",
    ),
  )
  hero.append(copy, duration)
  container.append(hero)

  if (run.state === "active" && elapsed(run.lastActivityAt) > 120000)
    container.append(
      node(
        "div",
        "stall-warning",
        `⚠ Possibly stalled - no activity for ${formatDuration(elapsed(run.lastActivityAt))}`,
      ),
    )
  if (run.state === "failed" && event?.lastError)
    container.append(node("div", "error-callout", `✕ ${event.lastError}`))

  const facts = node("div", "detail-panel detail-facts")
  facts.append(
    detailField("Started", absoluteTime(run.startedAt)),
    detailField("Finished", run.endedAt ? absoluteTime(run.endedAt) : "-"),
    detailField("Last activity", relativeTime(run.lastActivityAt)),
    detailField("Model", run.modelId),
    detailField("Provider", run.modelProvider),
    detailField("Thinking", run.thinkingLevel),
    detailField("Attempt", String(run.attempt)),
    detailField("Turns", String(run.turnCount)),
    detailField("Retries", String(run.retryCount)),
    detailField("Compactions", String(run.compactionCount)),
  )
  container.append(facts)

  if (event) {
    const eventPanel = node("div", "detail-panel detail-event")
    eventPanel.append(node("span", "detail-label", "Intake event"))
    const row = node("div", "detail-event-row")
    row.append(
      statusLabel(event.status),
      node("span", "event-title", event.title),
    )
    const entity = node("span", "event-meta", event.entityId)
    row.append(entity)
    const url = safeExternalUrl(event.url)
    if (url) {
      const link = node("a", "", `Open in ${event.source} ↗`)
      link.href = url.href
      link.target = "_blank"
      link.rel = "noreferrer"
      row.append(link)
    }
    eventPanel.append(row)
    container.append(eventPanel)
  }

  const timelinePanel = node("div", "detail-panel timeline-panel")
  const timelineHeading = node("div", "timeline-heading")
  timelineHeading.append(
    node("span", "timeline-title", "Timeline"),
    node("span", "", "offsets from run start - tool names and durations only"),
  )
  timelinePanel.append(timelineHeading)
  const timeline = node("div", "full-timeline")
  if (run.steps.length) renderToolList(timeline, run, true)
  else
    timeline.append(
      node(
        "p",
        "empty-panel",
        run.state === "active"
          ? "Waiting for the first tool call"
          : "This run completed without a recorded tool call",
      ),
    )
  timelinePanel.append(timeline)
  container.append(
    timelinePanel,
    node(
      "p",
      "privacy-note",
      "Tool arguments, commands, output, call identifiers, and intake content are not sent to the dashboard.",
    ),
  )
  return container
}

function openRun(runId) {
  state.selectedRun = runId
  element("dashboard-view").hidden = true
  element("run-detail-view").hidden = false
  renderSelectedRun()
  window.scrollTo({ top: 0, behavior: "instant" })
}

function renderSelectedRun() {
  if (state.selectedRun === null || !state.snapshot) return
  const run = state.snapshot.runs.find(
    (candidate) => candidate.id === state.selectedRun,
  )
  if (!run) {
    closeRun()
    return
  }
  element("run-detail").replaceChildren(runDetail(run))
}

function closeRun() {
  state.selectedRun = null
  element("run-detail-view").hidden = true
  element("dashboard-view").hidden = false
}

function snapshotContentKey(snapshot) {
  const { generatedAt: _, ...content } = snapshot
  return JSON.stringify(content)
}

function render(snapshot) {
  state.snapshot = snapshot
  state.contentKey = snapshotContentKey(snapshot)
  element("loading-view").hidden = true
  if (state.selectedRun === null) element("dashboard-view").hidden = false
  renderDashboard(snapshot)
  if (state.selectedRun !== null) renderSelectedRun()
}

function updateLiveTimes() {
  if (!state.snapshot) return
  updateOverview(state.snapshot)
  document.querySelectorAll("[data-relative-time]").forEach((target) => {
    const prefix = target.dataset.relativeTimePrefix || ""
    target.textContent = `${prefix}${relativeTime(target.dataset.relativeTime)}`
  })
  document.querySelectorAll(".run-live-duration").forEach((target) => {
    const end = parseTime(target.dataset.endedAt) ?? Date.now()
    target.textContent = formatDuration(elapsed(target.dataset.startedAt, end))
  })
  document.querySelectorAll(".tool-entry").forEach((entry) => {
    if (entry.dataset.stepEndedAt) return
    const duration = entry.querySelector(".tool-duration")
    if (duration)
      duration.textContent = `${formatDuration(elapsed(entry.dataset.stepStartedAt))}...`
  })
  updateConnectionAge()
}

function setConnection(mode) {
  document.documentElement.dataset.connection = mode
  const connection = element("connection")
  connection.className = `connection connection-${mode}`
  const banner = element("connection-banner")
  banner.hidden = mode === "live" || mode === "connecting"
  banner.classList.toggle("is-offline", mode === "offline")
  if (mode === "live") {
    setText("connection-label", "Live")
  } else if (mode === "stale") {
    setText("connection-label", "Stale")
    setText("banner-heading", "Data may be stale")
    setText(
      "banner-copy",
      "The last successful refresh is older than expected. Retrying automatically.",
    )
  } else if (mode === "offline") {
    setText("connection-label", "Offline")
    setText("banner-heading", "Connection lost")
    setText(
      "banner-copy",
      "The dashboard cannot reach the intake daemon. Showing the last known state and reconnecting automatically.",
    )
  } else {
    setText("connection-label", "Connecting")
    setText("connection-note", "waiting for data")
  }
}

function updateConnectionAge() {
  if (!state.lastSuccessAt) return
  const age = Date.now() - state.lastSuccessAt
  setText("connection-note", `updated ${formatDuration(age)} ago`)
  if (age > 15000 && state.snapshot) setConnection("stale")
}

async function refresh() {
  let delay = 5000
  try {
    const response = await fetch("/api/snapshot", { cache: "no-store" })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const snapshot = await response.json()
    state.lastSuccessAt = Date.now()
    if (snapshotContentKey(snapshot) === state.contentKey)
      state.snapshot = snapshot
    else render(snapshot)
    if (snapshot.runs.some((run) => run.state === "active")) delay = 1500
    setConnection("live")
    updateConnectionAge()
  } catch {
    setConnection("offline")
  } finally {
    clearTimeout(state.refreshTimer)
    state.refreshTimer = setTimeout(refresh, delay)
  }
}

function resolvedTheme(choice) {
  if (choice !== "system") return choice
  return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark"
}

function applyTheme(choice, persist) {
  document.documentElement.dataset.theme = resolvedTheme(choice)
  document.documentElement.dataset.themeChoice = choice
  document.querySelectorAll("[data-theme-choice]").forEach((button) => {
    button.setAttribute(
      "aria-pressed",
      String(button.dataset.themeChoice === choice),
    )
  })
  if (persist) {
    try {
      localStorage.setItem("im-theme", choice)
    } catch {}
  }
}

for (const button of document.querySelectorAll("[data-theme-choice]"))
  button.addEventListener("click", () =>
    applyTheme(button.dataset.themeChoice, true),
  )

const themeChoice = document.documentElement.dataset.themeChoice || "system"
applyTheme(themeChoice, false)
const systemTheme = matchMedia("(prefers-color-scheme: light)")
systemTheme.addEventListener("change", () => {
  if (document.documentElement.dataset.themeChoice === "system")
    applyTheme("system", false)
})

for (const button of document.querySelectorAll("[data-filter]"))
  button.addEventListener("click", () => {
    state.filter = button.dataset.filter
    if (state.snapshot) {
      renderFilters(state.snapshot)
      renderEvents(state.snapshot.events)
    }
  })

element("back-to-dashboard").addEventListener("click", closeRun)
setText("daemon-label", `${location.host} - local daemon`)
setConnection("connecting")
setInterval(updateLiveTimes, 1000)
refresh()
