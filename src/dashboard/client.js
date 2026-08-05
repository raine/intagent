const OPEN_STATES = ["pending", "processing", "retryable"]
const ATTENTION_STATES = ["retryable", "failed"]
const HANDLED_STATES = ["succeeded", "ignored"]
const PRIVACY_NOTE =
  "Tool arguments, commands, output, call identifiers, and intake content are not sent to the dashboard. Thinking content is not retained."

const eventStates = {
  pending: { glyph: "○", short: "PEND", label: "Pending" },
  processing: { glyph: "◐", short: "PROC", label: "Processing" },
  retryable: { glyph: "↻", short: "RTRY", label: "Retryable" },
  succeeded: { glyph: "✓", short: "OK", label: "Succeeded" },
  failed: { glyph: "✕", short: "FAIL", label: "Failed" },
  ignored: { glyph: "⊘", short: "IGN", label: "Ignored" },
}

const runStates = {
  active: { glyph: "◐", short: "RUN", label: "Running" },
  succeeded: { glyph: "✓", short: "OK", label: "Succeeded" },
  failed: { glyph: "✕", short: "FAIL", label: "Failed" },
  interrupted: { glyph: "◌", short: "STOP", label: "Interrupted" },
}

const state = {
  snapshot: null,
  contentKey: null,
  filter: "open",
  expandedRuns: new Set(),
  expandedEvents: new Set(),
  lastSuccessAt: null,
  failureCount: 0,
  refreshTimer: null,
  route: { kind: null, id: null },
  routeOrigin: null,
  lastAnnouncements: new Map(),
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

function setText(target, value) {
  const result = typeof target === "string" ? element(target) : target
  if (result && result.textContent !== String(value))
    result.textContent = String(value)
}

function parseTime(value) {
  const parsed = Date.parse(value)
  return Number.isFinite(parsed) ? parsed : null
}

function elapsed(value, end = Date.now()) {
  const start = parseTime(value)
  return start === null ? 0 : Math.max(0, end - start)
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

function compactDuration(milliseconds) {
  return formatDuration(milliseconds).replace(" ", "")
}

function relativeTime(value, now = Date.now()) {
  const timestamp = parseTime(value)
  if (timestamp === null) return "unknown"
  const difference = now - timestamp
  if (difference < 0) return `in ${formatDuration(-difference)}`
  const seconds = Math.floor(difference / 1000)
  if (seconds < 60) return `${Math.max(1, seconds)}s ago`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  return `${Math.floor(hours / 24)}d ago`
}

function clockTime(value) {
  const timestamp = parseTime(value)
  if (timestamp === null) return "unknown"
  return new Date(timestamp).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  })
}

function absoluteTime(value) {
  const timestamp = parseTime(value)
  return timestamp === null ? "unknown" : new Date(timestamp).toLocaleString()
}

function runDuration(run) {
  return elapsed(run.startedAt, parseTime(run.endedAt) ?? Date.now())
}

function stepDuration(step) {
  return elapsed(step.startedAt, parseTime(step.endedAt) ?? Date.now())
}

function stalled(run, now = Date.now()) {
  return run.state === "active" && elapsed(run.lastActivityAt, now) > 120000
}

function activeRuns(snapshot = state.snapshot) {
  return snapshot ? snapshot.runs.filter((run) => run.state === "active") : []
}

function completedRuns(snapshot = state.snapshot) {
  return snapshot ? snapshot.runs.filter((run) => run.state !== "active") : []
}

function isOpen(event) {
  return OPEN_STATES.includes(event.status)
}

function needsAttention(event) {
  return ATTENTION_STATES.includes(event.status)
}

function filteredEvents(snapshot) {
  if (state.filter === "open") return snapshot.events.filter(isOpen)
  if (state.filter === "attention")
    return snapshot.events.filter(needsAttention)
  if (state.filter === "handled")
    return snapshot.events.filter((event) =>
      HANDLED_STATES.includes(event.status),
    )
  return snapshot.events
}

function windowCounts(snapshot) {
  return {
    all: snapshot.events.length,
    open: snapshot.events.filter(isOpen).length,
    attention: snapshot.events.filter(needsAttention).length,
    handled: snapshot.events.filter((event) =>
      HANDLED_STATES.includes(event.status),
    ).length,
  }
}

function safeExternalUrl(value) {
  if (!value) return null
  try {
    const url = new URL(value)
    return url.protocol === "http:" || url.protocol === "https:" ? url : null
  } catch {
    return null
  }
}

function keyedList(container, items, keyOf, create, update) {
  const existing = new Map()
  for (const child of container.children) existing.set(child.dataset.key, child)
  items.forEach((item, index) => {
    const key = String(keyOf(item))
    let child = existing.get(key)
    if (child) existing.delete(key)
    else {
      child = create(item)
      child.dataset.key = key
    }
    update(child, item)
    const expected = container.children[index]
    if (expected !== child) container.insertBefore(child, expected || null)
  })
  for (const child of existing.values()) child.remove()
}

function statusMarkup(status, run = false) {
  const definition = (run ? runStates : eventStates)[status] || {
    glyph: "?",
    short: "UNKN",
    label: status,
  }
  return `<span class="status status-${status}" aria-label="${definition.label}"><span aria-hidden="true">${definition.glyph}</span><b aria-hidden="true">${definition.short}</b></span>`
}

function detailField(label, value) {
  const field = node("div", "detail-field")
  field.append(node("dt", "", label), node("dd", "", value || "-"))
  return field
}

function eventFacts(container, event) {
  container.replaceChildren()
  if (event.lastError) {
    const error = node("p", "callout callout-error", `✕ ${event.lastError}`)
    if (event.nextAttemptAt)
      error.append(` · retry ${relativeTime(event.nextAttemptAt)}`)
    container.append(error)
  }
  const facts = node("dl", "detail-grid")
  facts.append(
    detailField("Entity", event.entityId),
    detailField("Status", eventStates[event.status]?.label || event.status),
    detailField("Occurred", absoluteTime(event.occurredAt)),
    detailField("Observed", absoluteTime(event.observedAt)),
    detailField("Attempts", String(event.attemptCount)),
    detailField("Task", event.avenRef),
    detailField("Investigation", event.investigationHandle),
  )
  container.append(facts)
  const url = safeExternalUrl(event.url)
  if (url) {
    const link = node("a", "external-link", `Open in ${event.source} ↗`)
    link.href = url.href
    link.target = "_blank"
    link.rel = "noreferrer"
    container.append(link)
  }
}

function createActivityRow() {
  const row = node("div", "activity-row")
  row.tabIndex = 0
  row.innerHTML = `<time class="activity-clock"></time><span class="activity-turn"></span><strong class="activity-label"></strong><span class="activity-state"></span><span class="activity-track" aria-hidden="true"><i></i></span>`
  return row
}

function updateActivityRow(row, step, run, maxDuration) {
  const definition = runStates[step.state] || runStates.interrupted
  const kind = step.kind || "tool"
  const label =
    kind === "thinking"
      ? "∴ thinking"
      : kind === "compaction"
        ? "⇲ compaction"
        : step.label
  const glyph =
    step.state === "active"
      ? "◐"
      : kind === "thinking"
        ? "∴"
        : kind === "compaction"
          ? "⇲"
          : definition.glyph
  const duration = stepDuration(step)
  const width = Math.max(
    2,
    Math.round((duration / Math.max(maxDuration, 1)) * 100),
  )
  row.className = `activity-row activity-${kind} activity-${step.state}`
  setText(row.querySelector(".activity-clock"), clockTime(step.startedAt))
  setText(row.querySelector(".activity-turn"), kind)
  setText(row.querySelector(".activity-label"), label)
  const activityState = row.querySelector(".activity-state")
  setText(
    activityState,
    `${glyph} ${compactDuration(duration)}${step.state === "active" ? "..." : ""}`,
  )
  if (step.state === "active") {
    activityState.dataset.stepStartedAt = step.startedAt
    row.dataset.activityLabel = label
    row.dataset.runStartedAt = run.startedAt
  } else {
    delete activityState.dataset.stepStartedAt
    delete row.dataset.activityLabel
    delete row.dataset.runStartedAt
  }
  row.querySelector("i").style.width = `${width}%`
  const description = `${label}, ${definition.label}, ${formatDuration(duration)}, started ${formatDuration(elapsed(run.startedAt, parseTime(step.startedAt)))} after run start`
  row.setAttribute("aria-label", description)
  row.dataset.tooltip = description
}

function renderActivity(container, run, limit = null) {
  const steps = limit ? run.steps.slice(-limit) : run.steps
  if (!steps.length) {
    keyedList(
      container,
      [{ id: "empty" }],
      (item) => item.id,
      () => node("p", "empty-state"),
      (target) => setText(target, "Waiting for recorded activity"),
    )
    return
  }
  const maxDuration = Math.max(...run.steps.map(stepDuration), 1)
  keyedList(
    container,
    steps,
    (step) => step.id,
    createActivityRow,
    (row, step) => updateActivityRow(row, step, run, maxDuration),
  )
}

function createActiveRun() {
  const card = node("article", "active-run")
  const summary = node("button", "active-run-summary")
  summary.type = "button"
  summary.dataset.action = "toggle-run"
  summary.innerHTML = `<span class="disclosure" aria-hidden="true">▶</span><strong></strong><small></small><span class="slow-badge" hidden>SLOW?</span><time></time>`
  const metadata = node("div", "run-metadata")
  const activity = node("div", "active-run-activity")
  const footer = node("div", "activity-footer")
  card.append(summary, metadata, activity, footer)
  return card
}

function updateActiveRun(card, run) {
  const expanded = state.expandedRuns.has(run.id)
  const slow = stalled(run)
  card.className = `active-run ${slow ? "is-stalled" : ""}`
  const summary = card.querySelector(".active-run-summary")
  summary.dataset.id = String(run.id)
  summary.dataset.focusKey = `active-run-${run.id}`
  summary.setAttribute("aria-expanded", String(expanded))
  summary.setAttribute(
    "aria-label",
    `${expanded ? "Collapse" : "Expand"} activity for ${run.eventTitle}`,
  )
  setText(summary.querySelector(".disclosure"), expanded ? "▼" : "▶")
  setText(summary.querySelector("strong"), run.eventTitle)
  setText(summary.querySelector("small"), run.source)
  summary.querySelector(".slow-badge").hidden = !slow
  const duration = summary.querySelector("time")
  duration.dataset.startedAt = run.startedAt
  setText(duration, compactDuration(runDuration(run)))

  const metadata = card.querySelector(".run-metadata")
  metadata.replaceChildren()
  const values = [
    ["model", run.modelId || "unknown"],
    ["thinking", run.thinkingLevel || "unknown"],
    ["attempt", run.attempt],
    ["turns", run.turnCount],
    ["compactions", run.compactionCount],
  ]
  if (run.retryCount) values.push(["retries", run.retryCount])
  if (run.investigationHandle) values.push(["handle", run.investigationHandle])
  for (const [label, value] of values) {
    const item = node("span")
    item.append(`${label} `, node("b", "", String(value)))
    metadata.append(item)
  }

  const activity = card.querySelector(".active-run-activity")
  activity.classList.toggle("is-expanded", expanded)
  activity.setAttribute(
    "aria-label",
    expanded ? "All recorded activity" : "Latest recorded activity",
  )
  renderActivity(activity, run, expanded ? null : 4)
  const footer = card.querySelector(".activity-footer")
  footer.hidden = !expanded
  footer.replaceChildren()
  if (expanded) {
    const tools = run.steps.filter((step) => (step.kind || "tool") === "tool")
    const failed = tools.filter((step) => step.state === "failed").length
    const succeeded = tools.filter((step) => step.state === "succeeded").length
    const thinkingDuration = run.steps
      .filter((step) => step.kind === "thinking")
      .reduce((sum, step) => sum + stepDuration(step), 0)
    footer.append(
      node("span", "", `${run.steps.length} entries`),
      node("span", "", `tools ${tools.length} ✓${succeeded}`),
      node("span", failed ? "has-error" : "", `✕${failed}`),
      node(
        "span",
        "",
        `thinking ${thinkingDuration ? `${compactDuration(thinkingDuration)} total` : "waiting"}`,
      ),
      node("span", "privacy-inline", PRIVACY_NOTE),
    )
  }
}

function renderActiveRuns(container, runs) {
  if (!runs.length) {
    keyedList(
      container,
      [{ id: "empty" }],
      (item) => item.id,
      () => node("p", "empty-state active-empty"),
      (target) => setText(target, "No active runs. The queue is idle."),
    )
    return
  }
  keyedList(container, runs, (run) => run.id, createActiveRun, updateActiveRun)
}

function createEventRow() {
  const row = node("article", "event-row")
  const summary = node("button", "event-summary")
  summary.type = "button"
  summary.dataset.action = "toggle-event"
  summary.innerHTML = `<span class="event-status"></span><strong></strong><small></small><span class="event-attempt"></span><time></time><span class="event-disclosure" aria-hidden="true">▸</span>`
  const detail = node("div", "event-detail")
  row.append(summary, detail)
  return row
}

function updateEventRow(row, event) {
  const expanded = state.expandedEvents.has(event.id)
  row.className = `event-row event-${event.status}`
  const summary = row.querySelector(".event-summary")
  summary.dataset.id = String(event.id)
  summary.dataset.focusKey = `event-${event.id}`
  summary.setAttribute("aria-expanded", String(expanded))
  summary.setAttribute(
    "aria-label",
    `${expanded ? "Hide" : "Show"} details for ${event.title}`,
  )
  summary.querySelector(".event-status").innerHTML = statusMarkup(event.status)
  setText(summary.querySelector("strong"), event.title)
  setText(summary.querySelector("small"), `${event.source}/${event.kind}`)
  setText(
    summary.querySelector(".event-attempt"),
    event.attemptCount ? `att ${event.attemptCount}` : "-",
  )
  const time = summary.querySelector("time")
  time.dataset.relativeTime = event.observedAt
  setText(
    time,
    event.status === "retryable" && event.nextAttemptAt
      ? `retry ${relativeTime(event.nextAttemptAt)}`
      : relativeTime(event.observedAt),
  )
  setText(summary.querySelector(".event-disclosure"), expanded ? "▾" : "▸")
  const detail = row.querySelector(".event-detail")
  detail.hidden = !expanded
  if (expanded) {
    eventFacts(detail, event)
    const open = node("button", "event-route-button", "Open event inspector →")
    open.type = "button"
    open.dataset.action = "route-event"
    open.dataset.id = String(event.id)
    detail.append(open)
  }
}

function renderEvents(container, events) {
  if (!events.length) {
    keyedList(
      container,
      [{ id: "empty" }],
      (item) => item.id,
      () => node("p", "empty-state"),
      (target) => setText(target, "No events in this view"),
    )
    return
  }
  keyedList(
    container,
    events,
    (event) => event.id,
    createEventRow,
    updateEventRow,
  )
}

function createSourceCard() {
  const card = node("article", "source-card")
  card.innerHTML = `<div class="source-heading"><span aria-hidden="true"></span><strong></strong><b></b></div><p class="source-poll"></p><p class="source-error" hidden></p>`
  return card
}

function updateSourceCard(card, source) {
  const failing = Boolean(source.lastError)
  card.className = `source-card ${failing ? "is-failing" : "is-healthy"}`
  setText(card.querySelector(".source-heading > span"), failing ? "✕" : "✓")
  setText(card.querySelector("strong"), source.source)
  setText(card.querySelector("b"), failing ? "FAILING" : "HEALTHY")
  const poll = card.querySelector(".source-poll")
  poll.dataset.relativeTime = source.updatedAt
  poll.dataset.relativeTimePrefix = "last poll "
  setText(poll, `last poll ${relativeTime(source.updatedAt)}`)
  const error = card.querySelector(".source-error")
  error.hidden = !failing
  setText(error, source.lastError || "")
}

function renderSources(container, sources) {
  if (!sources.length) {
    keyedList(
      container,
      [{ source: "empty" }],
      (source) => source.source,
      () => node("p", "empty-state"),
      (target) => setText(target, "Sources appear after their first poll"),
    )
    return
  }
  keyedList(
    container,
    sources,
    (source) => source.source,
    createSourceCard,
    updateSourceCard,
  )
}

function createRecentRun() {
  const button = node("button", "recent-run")
  button.type = "button"
  button.dataset.action = "route-run"
  button.innerHTML = `<span class="recent-run-state" aria-hidden="true"></span><strong></strong><time></time>`
  return button
}

function updateRecentRun(button, run) {
  const definition = runStates[run.state]
  button.dataset.id = String(run.id)
  button.dataset.focusKey = `recent-run-${run.id}`
  button.setAttribute(
    "aria-label",
    `Inspect ${run.eventTitle}, ${definition.label}`,
  )
  button.className = `recent-run recent-run-${run.state}`
  setText(button.querySelector(".recent-run-state"), definition.glyph)
  setText(button.querySelector("strong"), run.eventTitle)
  const time = button.querySelector("time")
  time.dataset.startedAt = run.startedAt
  time.dataset.endedAt = run.endedAt || run.lastActivityAt
  setText(time, compactDuration(runDuration(run)))
}

function renderRecentRuns(container, runs) {
  if (!runs.length) {
    keyedList(
      container,
      [{ id: "empty" }],
      (item) => item.id,
      () => node("p", "empty-state"),
      (target) => setText(target, "No completed runs"),
    )
    return
  }
  keyedList(
    container,
    runs.slice(0, 8),
    (run) => run.id,
    createRecentRun,
    updateRecentRun,
  )
}

function mountDashboard(root) {
  root.innerHTML = `<div class="wire-dashboard">
    <section class="stat-strip" aria-label="Queue status">
      <article><span>OPEN</span><strong data-metric="open">0</strong></article>
      <article class="stat-attention"><span>⚠ NEEDS ATTENTION</span><strong data-metric="attention">0</strong></article>
      <article class="stat-active"><span>▶ ACTIVE RUNS</span><strong data-metric="active">0</strong></article>
      <article><span>HANDLED</span><strong data-metric="handled">0</strong></article>
      <article><span>OLDEST OPEN</span><strong data-metric="oldest">-</strong></article>
    </section>
    <div class="dashboard-grid">
      <div class="primary-column">
        <section class="active-section" aria-labelledby="active-title">
          <h1 id="active-title" class="section-label">ACTIVE RUNS <span>· refresh 1.5s</span></h1>
          <div data-active-runs></div>
        </section>
        <section class="events-section" aria-labelledby="events-title">
          <header class="events-header">
            <h2 id="events-title" class="section-label">RECENT EVENTS</h2>
            <div class="filters" role="group" aria-label="Filter recent events">
              <button type="button" data-filter="open" aria-pressed="true">open <b data-count="open">0</b></button>
              <button type="button" data-filter="attention" aria-pressed="false">attention <b data-count="attention">0</b></button>
              <button type="button" data-filter="handled" aria-pressed="false">handled <b data-count="handled">0</b></button>
              <button type="button" data-filter="all" aria-pressed="false">all <b data-count="all">0</b></button>
            </div>
          </header>
          <div class="events-list" data-events></div>
          <p class="window-note" data-window-note></p>
        </section>
      </div>
      <aside class="side-column" aria-label="Source health and completed runs">
        <section aria-labelledby="sources-title">
          <h2 id="sources-title" class="section-label">SOURCES</h2>
          <div class="sources-list" data-sources></div>
        </section>
        <section class="recent-runs-section" aria-labelledby="recent-runs-title">
          <h2 id="recent-runs-title" class="section-label">RECENT RUNS</h2>
          <div class="recent-runs" data-recent-runs></div>
        </section>
      </aside>
    </div>
  </div>`
}

function updateMetrics(root, snapshot) {
  const values = {
    open: snapshot.open,
    attention: snapshot.attention,
    active: activeRuns(snapshot).length,
    handled: snapshot.handled,
    oldest: snapshot.oldestOpenAt
      ? compactDuration(elapsed(snapshot.oldestOpenAt)).replace(/\d+s$/, "")
      : "-",
  }
  root.querySelectorAll("[data-metric]").forEach((target) => {
    setText(target, values[target.dataset.metric])
  })
}

function updateFilters(root, snapshot) {
  const counts = windowCounts(snapshot)
  root.querySelectorAll("[data-filter]").forEach((button) => {
    button.setAttribute(
      "aria-pressed",
      String(button.dataset.filter === state.filter),
    )
  })
  root.querySelectorAll("[data-count]").forEach((target) => {
    setText(target, counts[target.dataset.count] ?? 0)
  })
  setText(
    root.querySelector("[data-window-note]"),
    `Showing ${filteredEvents(snapshot).length} from the ${snapshot.events.length}-event recent window`,
  )
}

function renderDashboard() {
  if (!state.snapshot) return
  const root = element("dashboard-root")
  updateMetrics(root, state.snapshot)
  updateFilters(root, state.snapshot)
  renderActiveRuns(
    root.querySelector("[data-active-runs]"),
    activeRuns(state.snapshot),
  )
  renderEvents(
    root.querySelector("[data-events]"),
    filteredEvents(state.snapshot),
  )
  renderSources(root.querySelector("[data-sources]"), state.snapshot.sources)
  renderRecentRuns(
    root.querySelector("[data-recent-runs]"),
    completedRuns(state.snapshot),
  )
  renderRoute()
}

function inspectorMeta(run) {
  const rail = node("aside", "inspector-meta")
  const runTitle = node("h2", "section-label", "RUN")
  const runFacts = node("dl", "inspector-facts")
  runFacts.append(
    detailField("duration", formatDuration(runDuration(run))),
    detailField("started", clockTime(run.startedAt)),
    detailField("finished", run.endedAt ? clockTime(run.endedAt) : "running"),
    detailField("attempt", String(run.attempt)),
    detailField("turns", String(run.turnCount)),
    detailField("retries", String(run.retryCount)),
    detailField("compactions", String(run.compactionCount)),
  )
  const modelTitle = node("h2", "section-label inspector-divider", "MODEL")
  const modelFacts = node("dl", "inspector-facts")
  modelFacts.append(
    detailField("model", run.modelId),
    detailField("provider", run.modelProvider),
    detailField("thinking", run.thinkingLevel),
  )
  const eventTitle = node("h2", "section-label inspector-divider", "EVENT")
  const eventName = node("p", "inspector-event-title", run.eventTitle)
  const eventFactsList = node("dl", "inspector-facts")
  eventFactsList.append(
    detailField("source", run.source),
    detailField("handle", run.investigationHandle),
  )
  rail.append(
    runTitle,
    runFacts,
    modelTitle,
    modelFacts,
    eventTitle,
    eventName,
    eventFactsList,
    node("p", "privacy-note", PRIVACY_NOTE),
  )
  return rail
}

function runInspector(container, run) {
  const layout = node("div", "run-inspector")
  const timeline = node("section", "inspector-timeline")
  const heading = node("header", "timeline-heading")
  heading.append(
    node(
      "h2",
      "section-label",
      `TIMELINE · ${run.turnCount} TURNS · ${run.steps.length} ENTRIES`,
    ),
    node("span", "", "bars show relative duration"),
  )
  const list = node("div", "activity-list inspector-activity")
  renderActivity(list, run)
  const legend = node("div", "timeline-legend")
  legend.innerHTML = `<span><i class="legend-success"></i> tool ✓</span><span><i class="legend-failed"></i> tool ✕</span><span><i class="legend-thinking"></i> ∴ thinking</span><span><i class="legend-compaction"></i> ⇲ compaction</span><span><i class="legend-active"></i> running ◐</span>`
  timeline.append(heading, list, legend)
  layout.append(inspectorMeta(run), timeline)
  container.replaceChildren(layout)
}

function eventInspector(container, event) {
  const layout = node("div", "event-inspector")
  const facts = node("section", "event-inspector-facts")
  facts.append(node("h2", "section-label", "EVENT"))
  const body = node("div", "event-inspector-body")
  eventFacts(body, event)
  facts.append(body)
  const attempts = node("section", "event-attempts")
  attempts.append(node("h2", "section-label", "TRIAGE RUNS"))
  const list = node("div", "recent-runs event-run-list")
  renderRecentRuns(
    list,
    state.snapshot.runs.filter((run) => run.eventId === event.id),
  )
  attempts.append(list)
  layout.append(facts, attempts)
  container.replaceChildren(layout)
}

function parseRoute() {
  const match = location.hash.match(/^#\/(run|event)\/(\d+)$/)
  state.route = match
    ? { kind: match[1], id: Number(match[2]) }
    : { kind: null, id: null }
}

function renderRoute() {
  const layer = element("route-layer")
  if (!state.snapshot || !state.route.kind) {
    layer.hidden = true
    document.body.classList.remove("route-open")
    return
  }
  const record =
    state.route.kind === "run"
      ? state.snapshot.runs.find((run) => run.id === state.route.id)
      : state.snapshot.events.find((event) => event.id === state.route.id)
  if (!record) {
    layer.hidden = true
    document.body.classList.remove("route-open")
    return
  }
  const isRun = state.route.kind === "run"
  setText("route-title", isRun ? record.eventTitle : record.title)
  element("route-status").innerHTML = statusMarkup(
    isRun ? record.state : record.status,
    isRun,
  )
  setText(
    "route-finished",
    isRun
      ? record.endedAt
        ? `finished ${clockTime(record.endedAt)} · ${relativeTime(record.endedAt)}`
        : `started ${relativeTime(record.startedAt)}`
      : `observed ${relativeTime(record.observedAt)}`,
  )
  const content = element("route-content")
  const routeKey = `${state.route.kind}-${state.route.id}-${state.contentKey}`
  if (content.dataset.routeKey !== routeKey) {
    content.dataset.routeKey = routeKey
    if (isRun) runInspector(content, record)
    else eventInspector(content, record)
  }
  layer.hidden = false
  document.body.classList.add("route-open")
}

function navigate(kind, id, origin) {
  state.routeOrigin = origin || document.activeElement
  location.hash = `#/${kind}/${id}`
}

function dismissRoute() {
  location.hash = "#/"
}

function handleRouteChange() {
  const wasOpen = !element("route-layer").hidden
  parseRoute()
  renderRoute()
  const isOpen = !element("route-layer").hidden
  if (!wasOpen && isOpen) element("route-back").focus()
  if (wasOpen && !isOpen && state.routeOrigin?.isConnected)
    state.routeOrigin.focus()
}

function trapRouteFocus(event) {
  if (event.key !== "Tab" || element("route-layer").hidden) return
  const focusable = [
    ...element("route-panel").querySelectorAll(
      "button, a[href], [tabindex]:not([tabindex='-1'])",
    ),
  ].filter((target) => !target.hidden && target.offsetParent !== null)
  if (!focusable.length) return
  const first = focusable[0]
  const last = focusable.at(-1)
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault()
    first.focus()
  }
}

function snapshotContentKey(snapshot) {
  const content = { ...snapshot }
  delete content.generatedAt
  return JSON.stringify(content)
}

function announce(key, message) {
  if (state.lastAnnouncements.get(key) === message) return
  state.lastAnnouncements.set(key, message)
  setText("announcer", message)
}

function announceTransitions(previous, next) {
  if (!previous) return
  const previousActive = new Map(
    activeRuns(previous).map((run) => [run.id, run]),
  )
  const nextActive = new Map(activeRuns(next).map((run) => [run.id, run]))
  for (const run of nextActive.values()) {
    if (!previousActive.has(run.id))
      announce(`run-${run.id}`, `Triage started for ${run.eventTitle}`)
    if (
      stalled(run) &&
      !stalled(
        previousActive.get(run.id) || run,
        parseTime(previous.generatedAt) ?? 0,
      )
    )
      announce(`stall-${run.id}`, `Triage for ${run.eventTitle} may be stalled`)
  }
  for (const run of previousActive.values()) {
    if (!nextActive.has(run.id)) {
      const finished = next.runs.find((candidate) => candidate.id === run.id)
      announce(
        `run-${run.id}`,
        `Triage ${finished?.state || "finished"} for ${run.eventTitle}`,
      )
    }
  }
}

function renderSnapshot(snapshot) {
  const previous = state.snapshot
  state.snapshot = snapshot
  state.contentKey = snapshotContentKey(snapshot)
  element("loading-view").hidden = true
  element("dashboard-root").hidden = false
  announceTransitions(previous, snapshot)
  renderDashboard()
}

function setConnection(mode) {
  document.documentElement.dataset.connection = mode
  const connection = element("connection")
  connection.className = `connection connection-${mode}`
  const banner = element("connection-banner")
  banner.hidden = mode === "live" || mode === "connecting"
  if (mode === "live") setText("connection-label", "Live")
  else if (mode === "stale") {
    setText("connection-label", "Stale")
    setText("banner-heading", "Data may be stale")
    setText(
      "banner-copy",
      "The last refresh is older than expected. Retrying automatically.",
    )
  } else if (mode === "offline") {
    setText("connection-label", "Offline")
    setText("banner-heading", "Connection lost")
    setText(
      "banner-copy",
      "Showing the last known state while the dashboard reconnects.",
    )
  } else {
    setText("connection-label", "Connecting")
    setText("connection-note", "waiting for data")
  }
}

function updateConnectionAge() {
  if (!state.lastSuccessAt) return
  const age = Date.now() - state.lastSuccessAt
  setText("connection-note", `refreshed ${formatDuration(age)} ago`)
  if (age > 15000 && state.snapshot && state.failureCount === 0)
    setConnection("stale")
}

function updateLiveValues() {
  document.querySelectorAll("[data-relative-time]").forEach((target) => {
    const prefix = target.dataset.relativeTimePrefix || ""
    setText(target, `${prefix}${relativeTime(target.dataset.relativeTime)}`)
  })
  document.querySelectorAll("[data-started-at]").forEach((target) => {
    const end = parseTime(target.dataset.endedAt) ?? Date.now()
    setText(target, compactDuration(elapsed(target.dataset.startedAt, end)))
  })
  document.querySelectorAll("[data-step-started-at]").forEach((target) => {
    const duration = elapsed(target.dataset.stepStartedAt)
    setText(target, `◐ ${compactDuration(duration)}...`)
    const row = target.closest(".activity-row")
    if (!row) return
    const description = `${row.dataset.activityLabel}, Running, ${formatDuration(duration)}, started ${formatDuration(elapsed(row.dataset.runStartedAt, parseTime(target.dataset.stepStartedAt)))} after run start`
    row.setAttribute("aria-label", description)
    row.dataset.tooltip = description
  })
  setText(
    "dashboard-clock",
    new Date().toLocaleString([], {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hour12: false,
    }),
  )
  updateConnectionAge()
}

function nextDelay() {
  if (activeRuns().length) return 1500
  if (state.failureCount)
    return Math.min(30000, 5000 * 2 ** (state.failureCount - 1))
  return 5000
}

async function refresh() {
  if (document.visibilityState === "hidden") return
  try {
    const response = await fetch("/api/snapshot", { cache: "no-store" })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const snapshot = await response.json()
    state.lastSuccessAt = Date.now()
    state.failureCount = 0
    if (snapshotContentKey(snapshot) === state.contentKey)
      state.snapshot = snapshot
    else renderSnapshot(snapshot)
    setConnection("live")
    updateConnectionAge()
  } catch {
    state.failureCount += 1
    setConnection("offline")
  } finally {
    clearTimeout(state.refreshTimer)
    if (document.visibilityState !== "hidden")
      state.refreshTimer = setTimeout(refresh, nextDelay())
  }
}

function applyTheme(theme) {
  document.documentElement.dataset.theme = theme
  const toggle = element("theme-toggle")
  const isDark = theme === "dark"
  setText(toggle, isDark ? "◑ dark" : "☀ light")
  toggle.setAttribute(
    "aria-label",
    `Switch to ${isDark ? "light" : "dark"} theme`,
  )
}

function handleDashboardClick(event) {
  const control = event.target.closest("button")
  if (!control) return
  if (control.dataset.filter) {
    state.filter = control.dataset.filter
    renderDashboard()
    return
  }
  const id = Number(control.dataset.id)
  if (control.dataset.action === "toggle-run") {
    if (state.expandedRuns.has(id)) state.expandedRuns.delete(id)
    else state.expandedRuns.add(id)
    renderDashboard()
    requestAnimationFrame(() =>
      element("dashboard-root")
        .querySelector(`[data-focus-key="active-run-${id}"]`)
        ?.focus(),
    )
  } else if (control.dataset.action === "toggle-event") {
    if (state.expandedEvents.has(id)) state.expandedEvents.delete(id)
    else state.expandedEvents.add(id)
    renderDashboard()
    requestAnimationFrame(() =>
      element("dashboard-root")
        .querySelector(`[data-focus-key="event-${id}"]`)
        ?.focus(),
    )
  } else if (control.dataset.action === "route-run")
    navigate("run", id, control)
  else if (control.dataset.action === "route-event")
    navigate("event", id, control)
}

mountDashboard(element("dashboard-root"))
parseRoute()
applyTheme(document.documentElement.dataset.theme || "dark")
setConnection("connecting")
updateLiveValues()

element("theme-toggle").addEventListener("click", () => {
  const theme =
    document.documentElement.dataset.theme === "dark" ? "light" : "dark"
  applyTheme(theme)
  try {
    localStorage.setItem("im-theme", theme)
  } catch {}
})
element("dashboard-root").addEventListener("click", handleDashboardClick)
element("route-content").addEventListener("click", handleDashboardClick)
element("route-back").addEventListener("click", dismissRoute)
document.addEventListener("keydown", (event) => {
  trapRouteFocus(event)
  if (event.key === "Escape" && !element("route-layer").hidden) dismissRoute()
})
window.addEventListener("hashchange", handleRouteChange)
window.addEventListener("popstate", handleRouteChange)
document.addEventListener("visibilitychange", () => {
  clearTimeout(state.refreshTimer)
  if (document.visibilityState === "visible") refresh()
})
setInterval(updateLiveValues, 1000)
refresh()
