const DESIGN_IDS = ["ledger", "console", "pipeline", "briefing", "wall"]
const OPEN_STATES = ["pending", "processing", "retryable"]
const ATTENTION_STATES = ["retryable", "failed"]
const HANDLED_STATES = ["succeeded", "ignored"]
const PRIVACY_NOTE =
  "Tool arguments, commands, output, call identifiers, and intake content are not sent to the dashboard."

const eventStates = {
  pending: { glyph: "○", label: "Pending" },
  processing: { glyph: "◐", label: "Processing" },
  retryable: { glyph: "↻", label: "Retryable" },
  succeeded: { glyph: "✓", label: "Succeeded" },
  failed: { glyph: "✕", label: "Failed" },
  ignored: { glyph: "-", label: "Ignored" },
}

const runStates = {
  active: { glyph: "●", label: "Active" },
  succeeded: { glyph: "✓", label: "Succeeded" },
  failed: { glyph: "✕", label: "Failed" },
  interrupted: { glyph: "⏸", label: "Interrupted" },
}

const state = {
  snapshot: null,
  contentKey: null,
  filter: "all",
  expanded: new Set(),
  mountedDesign: null,
  lastSuccessAt: null,
  refreshTimer: null,
  failureCount: 0,
  route: { kind: null, id: null },
  routeOrigin: null,
  selectedConsoleEvent: null,
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
  if (hours < 24) return `${hours}h ${minutes % 60}m ago`
  return `${Math.floor(hours / 24)}d ${hours % 24}h ago`
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

function safeExternalUrl(value) {
  if (!value) return null
  try {
    const url = new URL(value)
    return url.protocol === "http:" || url.protocol === "https:" ? url : null
  } catch {
    return null
  }
}

function currentDesign() {
  const design = document.documentElement.dataset.design
  return DESIGN_IDS.includes(design) ? design : "ledger"
}

function isOpen(event) {
  return OPEN_STATES.includes(event.status)
}

function needsAttention(event) {
  return ATTENTION_STATES.includes(event.status)
}

function activeRuns(snapshot = state.snapshot) {
  return snapshot ? snapshot.runs.filter((run) => run.state === "active") : []
}

function stalled(run, now = Date.now()) {
  return run.state === "active" && elapsed(run.lastActivityAt, now) > 120000
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
    label: status,
  }
  return `<span class="status status-${status}"><span aria-hidden="true">${definition.glyph}</span>${definition.label}</span>`
}

function filterMarkup(label = "Filter intake events") {
  return `<div class="filters" role="group" aria-label="${label}">
    <button type="button" data-filter="all" aria-pressed="true">Recent <b data-count="all">0</b></button>
    <button type="button" data-filter="open" aria-pressed="false">Open <b data-count="open">0</b></button>
    <button type="button" data-filter="attention" aria-pressed="false">Needs you <b data-count="attention">0</b></button>
    <button type="button" data-filter="handled" aria-pressed="false">Handled <b data-count="handled">0</b></button>
  </div>`
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
  root.querySelectorAll("[data-window-note]").forEach((target) => {
    setText(
      target,
      `Showing ${filteredEvents(snapshot).length} from the ${snapshot.events.length}-event recent window`,
    )
  })
}

function detailField(label, value) {
  const field = node("div", "detail-field")
  field.append(node("dt", "", label), node("dd", "", value || "-"))
  return field
}

function eventDetails(container, event) {
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
    link.dataset.focusKey = `event-link-${event.id}`
    container.append(link)
  }
}

function createEventRow(surface, options = {}) {
  const item = node("article", `event-row ${options.className || ""}`.trim())
  const button = node("button", "event-summary")
  button.type = "button"
  button.dataset.action = options.route ? "route-event" : "expand-event"
  button.dataset.surface = surface
  button.innerHTML = `<span class="event-status"></span><span class="event-copy"><strong></strong><small></small></span><time></time><span class="chevron" aria-hidden="true">▸</span>`
  const detail = node("div", "event-detail")
  item.append(button, detail)
  return item
}

function updateEventRow(item, event, surface, options = {}) {
  const key = `event:${surface}:${event.id}`
  const expanded = state.expanded.has(key)
  const button = item.querySelector("button")
  item.className =
    `event-row status-border-${event.status} ${options.className || ""}`.trim()
  button.dataset.id = String(event.id)
  button.dataset.expandKey = key
  button.dataset.focusKey = key
  button.setAttribute("aria-expanded", String(options.route ? false : expanded))
  button.setAttribute(
    "aria-label",
    `${options.route ? "Open" : expanded ? "Hide" : "Show"} details for ${event.title}`,
  )
  button.querySelector(".event-status").innerHTML = statusMarkup(event.status)
  setText(button.querySelector("strong"), event.title)
  setText(button.querySelector("small"), `${event.source} · ${event.kind}`)
  const time = button.querySelector("time")
  time.dataset.relativeTime = event.observedAt
  setText(time, relativeTime(event.observedAt))
  setText(
    button.querySelector(".chevron"),
    options.route ? "→" : expanded ? "▾" : "▸",
  )
  const detail = item.querySelector(".event-detail")
  detail.hidden = !expanded || Boolean(options.route)
  if (expanded && !options.route) eventDetails(detail, event)
}

function renderEventList(container, events, surface, options = {}) {
  if (!events.length) {
    keyedList(
      container,
      [{ id: "empty" }],
      (item) => item.id,
      () => node("p", "empty-state"),
      (target) => setText(target, options.empty || "No events in this view"),
    )
    return
  }
  keyedList(
    container,
    events,
    (event) => event.id,
    () => createEventRow(surface, options),
    (item, event) => updateEventRow(item, event, surface, options),
  )
}

function createToolRow() {
  const row = node("div", "tool-row")
  row.innerHTML = `<span class="tool-state"></span><strong></strong><span class="tool-track" aria-hidden="true"><i></i></span><time></time>`
  return row
}

function updateToolRow(row, step, run, maxDuration) {
  const definition = runStates[step.state] || runStates.interrupted
  setText(
    row.querySelector(".tool-state"),
    step.state === "active" ? "▸" : definition.glyph,
  )
  setText(row.querySelector("strong"), step.label)
  const duration = stepDuration(step)
  const width = Math.max(
    3,
    Math.round((duration / Math.max(maxDuration, 1)) * 100),
  )
  row.querySelector("i").style.width = `${width}%`
  const time = row.querySelector("time")
  time.dataset.stepStartedAt = step.startedAt
  if (step.endedAt) time.dataset.stepEndedAt = step.endedAt
  else delete time.dataset.stepEndedAt
  setText(
    time,
    `${formatDuration(duration)}${step.state === "active" ? "…" : ""}`,
  )
  row.setAttribute(
    "aria-label",
    `${step.label}, ${definition.label}, ${formatDuration(duration)}, started ${formatDuration(elapsed(run.startedAt, parseTime(step.startedAt)))} after run start`,
  )
}

function renderTools(container, run) {
  const maxDuration = Math.max(...run.steps.map(stepDuration), 1)
  keyedList(
    container,
    run.steps,
    (step) => step.id,
    createToolRow,
    (row, step) => updateToolRow(row, step, run, maxDuration),
  )
  if (!run.steps.length) {
    container.append(node("p", "empty-state", "No tool activity recorded"))
  }
}

function runDetails(container, run, includeEvent = true) {
  const focused = document.activeElement?.dataset?.focusKey
  container.replaceChildren()
  const linked = state.snapshot?.events.find(
    (event) => event.id === run.eventId,
  )
  const facts = node("dl", "detail-grid")
  facts.append(
    detailField("Started", absoluteTime(run.startedAt)),
    detailField("Finished", run.endedAt ? absoluteTime(run.endedAt) : "-"),
    detailField("Model", run.modelId),
    detailField("Provider", run.modelProvider),
    detailField("Thinking", run.thinkingLevel),
    detailField("Attempt", String(run.attempt)),
    detailField("Turns", String(run.turnCount)),
    detailField("Retries", String(run.retryCount)),
    detailField("Compactions", String(run.compactionCount)),
  )
  container.append(facts)
  if (stalled(run))
    container.append(
      node(
        "p",
        "callout callout-warn",
        `⚠ Possibly stalled · no activity for ${formatDuration(elapsed(run.lastActivityAt))}`,
      ),
    )
  if (run.state === "failed" && linked?.lastError)
    container.append(
      node("p", "callout callout-error", `✕ ${linked.lastError}`),
    )
  if (includeEvent && linked) {
    const eventPanel = node("section", "linked-event")
    eventPanel.append(node("h3", "eyebrow", "Intake event"))
    const eventButton = node("button", "linked-event-button", linked.title)
    eventButton.type = "button"
    eventButton.dataset.action = "route-event"
    eventButton.dataset.id = String(linked.id)
    eventButton.dataset.focusKey = `linked-event-${linked.id}`
    eventPanel.append(eventButton)
    container.append(eventPanel)
  }
  const tools = node("section", "tool-section")
  tools.append(node("h3", "eyebrow", "Tool activity"))
  const list = node("div", "tool-list")
  renderTools(list, run)
  tools.append(list)
  container.append(tools, node("p", "privacy-note", PRIVACY_NOTE))
  if (focused)
    requestAnimationFrame(() =>
      container
        .querySelector(`[data-focus-key="${CSS.escape(focused)}"]`)
        ?.focus(),
    )
}

function createRunRow(surface, options = {}) {
  const item = node("article", `run-row ${options.className || ""}`.trim())
  const button = node("button", "run-summary")
  button.type = "button"
  button.dataset.action = options.route ? "route-run" : "expand-run"
  button.dataset.surface = surface
  button.innerHTML = `<span class="run-status"></span><span class="run-copy"><strong></strong><small></small></span><time class="run-live-duration"></time><span class="chevron" aria-hidden="true">▸</span>`
  const detail = node("div", "run-inline-detail")
  item.append(button, detail)
  return item
}

function updateRunRow(item, run, surface, options = {}) {
  const key = `run:${surface}:${run.id}`
  const expanded = state.expanded.has(key)
  const button = item.querySelector("button")
  item.className =
    `run-row state-border-${run.state} ${stalled(run) ? "is-stalled" : ""} ${options.className || ""}`.trim()
  button.dataset.id = String(run.id)
  button.dataset.expandKey = key
  button.dataset.focusKey = key
  button.setAttribute("aria-expanded", String(options.route ? false : expanded))
  button.setAttribute(
    "aria-label",
    `${options.route ? "Open" : expanded ? "Hide" : "Show"} details for ${run.eventTitle}`,
  )
  button.querySelector(".run-status").innerHTML = statusMarkup(run.state, true)
  setText(button.querySelector("strong"), run.eventTitle)
  const activeStep = run.steps.find((step) => step.state === "active")
  setText(
    button.querySelector("small"),
    `${run.source} · attempt ${run.attempt}${activeStep ? ` · ${activeStep.label}` : ""}`,
  )
  const duration = button.querySelector("time")
  duration.dataset.startedAt = run.startedAt
  if (run.endedAt) duration.dataset.endedAt = run.endedAt
  else delete duration.dataset.endedAt
  setText(duration, formatDuration(runDuration(run)))
  setText(
    button.querySelector(".chevron"),
    options.route ? "→" : expanded ? "▾" : "▸",
  )
  const detail = item.querySelector(".run-inline-detail")
  detail.hidden = !expanded || Boolean(options.route)
  if (expanded && !options.route) runDetails(detail, run)
}

function renderRunList(container, runs, surface, options = {}) {
  if (!runs.length) {
    keyedList(
      container,
      [{ id: "empty" }],
      (item) => item.id,
      () => node("p", "empty-state"),
      (target) => setText(target, options.empty || "No runs in this view"),
    )
    return
  }
  keyedList(
    container,
    runs,
    (run) => run.id,
    () => createRunRow(surface, options),
    (item, run) => updateRunRow(item, run, surface, options),
  )
}

function sourceRow(source) {
  const row = node(
    "article",
    `source-row ${source.lastError ? "is-failing" : ""}`,
  )
  row.dataset.key = source.source
  row.innerHTML = `<span class="source-glyph" aria-hidden="true"></span><span><strong></strong><small></small></span><b></b>`
  return row
}

function updateSourceRow(row, source) {
  row.className = `source-row ${source.lastError ? "is-failing" : ""}`
  setText(row.querySelector(".source-glyph"), source.lastError ? "✕" : "✓")
  setText(row.querySelector("strong"), source.source)
  const small = row.querySelector("small")
  if (source.lastError) setText(small, source.lastError)
  else if (source.lastSuccessAt) {
    small.dataset.relativeTimePrefix = "last success "
    small.dataset.relativeTime = source.lastSuccessAt
    setText(small, `last success ${relativeTime(source.lastSuccessAt)}`)
  } else setText(small, "waiting for first success")
  setText(row.querySelector("b"), source.lastError ? "Failing" : "Healthy")
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
    sourceRow,
    updateSourceRow,
  )
}

function metric(label, value, note, tone = "") {
  return `<article class="metric ${tone}"><span>${label}</span><strong data-metric="${label.toLowerCase().replaceAll(" ", "-")}">${value}</strong><small>${note}</small></article>`
}

function updateSharedMetrics(root, snapshot) {
  const values = {
    open: snapshot.open,
    "needs-you": snapshot.attention,
    active: activeRuns(snapshot).length,
    handled: snapshot.handled,
    "oldest-open": snapshot.oldestOpenAt
      ? formatDuration(elapsed(snapshot.oldestOpenAt)).replace(/ \d+s$/, "")
      : "-",
  }
  root.querySelectorAll("[data-metric]").forEach((target) => {
    setText(target, values[target.dataset.metric] ?? target.textContent)
  })
}

function ledgerMount(root) {
  root.innerHTML = `<div class="ledger layout-shell">
    <section class="ledger-strip" aria-label="Queue status">
      <p><strong data-metric="open">0</strong> open</p><p><strong data-metric="needs-you">0</strong> need you</p><p><strong data-metric="active">0</strong> active</p><p><strong data-metric="handled">0</strong> handled</p><p>oldest <strong data-metric="oldest-open">-</strong></p>
    </section>
    <section class="ledger-alerts" aria-labelledby="ledger-alerts-title" hidden><h2 id="ledger-alerts-title">Source alerts</h2><div data-ledger-sources></div></section>
    <section class="ledger-active" aria-labelledby="ledger-active-title"><header><h1 id="ledger-active-title">Active runs</h1><span>pinned while working</span></header><div data-ledger-active></div></section>
    <section class="ledger-feed" aria-labelledby="ledger-feed-title"><header><div><p class="eyebrow">Chronological record</p><h2 id="ledger-feed-title">Intake ledger</h2></div>${filterMarkup("Filter ledger events")}</header><div class="timeline-feed" data-ledger-feed></div><p class="window-note" data-window-note></p></section>
  </div>`
}

function ledgerEntry() {
  const item = node("article", "ledger-entry")
  item.innerHTML = `<time></time><span class="ledger-node" aria-hidden="true"></span><div class="ledger-entry-body"></div>`
  return item
}

function updateLedgerEntry(item, entry) {
  const time = item.querySelector("time")
  time.dataset.relativeTime = entry.at
  time.title = absoluteTime(entry.at)
  setText(time, relativeTime(entry.at))
  setText(
    item.querySelector(".ledger-node"),
    entry.type === "event"
      ? eventStates[entry.value.status].glyph
      : runStates[entry.value.state].glyph,
  )
  const body = item.querySelector(".ledger-entry-body")
  if (!body.firstElementChild) {
    body.append(
      entry.type === "event"
        ? createEventRow("ledger-feed")
        : createRunRow("ledger-feed"),
    )
  }
  const expectedClass = entry.type === "event" ? "event-row" : "run-row"
  if (!body.firstElementChild.classList.contains(expectedClass)) {
    body.replaceChildren(
      entry.type === "event"
        ? createEventRow("ledger-feed")
        : createRunRow("ledger-feed"),
    )
  }
  if (entry.type === "event")
    updateEventRow(body.firstElementChild, entry.value, "ledger-feed")
  else updateRunRow(body.firstElementChild, entry.value, "ledger-feed")
}

function ledgerRender(root, snapshot) {
  updateSharedMetrics(root, snapshot)
  updateFilters(root, snapshot)
  const active = activeRuns(snapshot)
  renderRunList(
    root.querySelector("[data-ledger-active]"),
    active,
    "ledger-active",
    {
      empty: "No active runs. The queue is idle.",
    },
  )
  const failures = snapshot.sources.filter((source) => source.lastError)
  const alerts = root.querySelector(".ledger-alerts")
  alerts.hidden = failures.length === 0
  renderSources(root.querySelector("[data-ledger-sources]"), failures)
  const events = filteredEvents(snapshot).map((value) => ({
    key: `event-${value.id}`,
    type: "event",
    at: value.observedAt,
    value,
  }))
  const runs = snapshot.runs
    .filter((run) => run.state !== "active")
    .map((value) => ({
      key: `run-${value.id}`,
      type: "run",
      at: value.endedAt || value.lastActivityAt,
      value,
    }))
  const entries = [...events, ...runs].sort((left, right) =>
    right.at.localeCompare(left.at),
  )
  keyedList(
    root.querySelector("[data-ledger-feed]"),
    entries,
    (entry) => entry.key,
    ledgerEntry,
    updateLedgerEntry,
  )
}

function consoleMount(root) {
  root.innerHTML = `<div class="console layout-shell">
    <aside class="console-scopes" aria-label="Event scopes"><p class="eyebrow">Work queue</p>${filterMarkup("Select queue scope")}<section><h2>Sources</h2><div data-console-sources></div></section></aside>
    <section class="console-master" aria-labelledby="console-title"><header><div><p class="eyebrow">Event master</p><h1 id="console-title">Intake events</h1></div><span class="key-hint">↑↓ select · Enter open</span></header><div class="console-list" role="listbox" aria-label="Intake events" tabindex="0" data-console-events></div><p class="window-note" data-window-note></p></section>
    <section class="console-detail" aria-labelledby="console-detail-title"><div data-console-detail><div class="console-empty"><p class="eyebrow">Event detail</p><h2 id="console-detail-title">Select an event</h2><p>Choose an item to inspect its facts and triage attempts.</p></div></div></section>
    <footer class="console-status"><span><b data-metric="open">0</b> open · <b data-metric="needs-you">0</b> need you · <b data-metric="active">0</b> running</span><span data-console-health></span></footer>
  </div>`
}

function renderConsoleDetail(container, event, snapshot) {
  if (!event) return
  let header = container.querySelector(".console-detail-heading")
  if (!header) {
    container.replaceChildren()
    header = node("header", "console-detail-heading")
    header.innerHTML = `<button class="console-detail-back" type="button" data-action="console-close"><span aria-hidden="true">←</span> Events</button><p class="eyebrow">Selected event</p><h2 id="console-detail-title"></h2><div class="console-event-facts"></div>`
    container.append(header, node("section", "console-attempts"))
  }
  setText(header.querySelector("h2"), event.title)
  eventDetails(header.querySelector(".console-event-facts"), event)
  const attempts = container.querySelector(".console-attempts")
  let attemptsTitle = attempts.querySelector("h3")
  if (!attemptsTitle) {
    attemptsTitle = node("h3", "eyebrow", "Triage attempts")
    attempts.append(attemptsTitle, node("div", "console-run-list"))
  }
  renderRunList(
    attempts.querySelector(".console-run-list"),
    snapshot.runs.filter((run) => run.eventId === event.id),
    `console-event-${event.id}`,
    { empty: "No triage attempts for this event" },
  )
}

function consoleRender(root, snapshot) {
  updateSharedMetrics(root, snapshot)
  updateFilters(root, snapshot)
  renderSources(root.querySelector("[data-console-sources]"), snapshot.sources)
  const events = filteredEvents(snapshot)
  renderEventList(
    root.querySelector("[data-console-events]"),
    events,
    "console",
    {
      route: true,
      empty: "No events match this scope",
    },
  )
  root.querySelectorAll("[data-console-events] .event-row").forEach((row) => {
    const id = Number(row.querySelector("button")?.dataset.id)
    row.classList.toggle("is-selected", id === state.selectedConsoleEvent)
    row.setAttribute("role", "option")
    row.setAttribute("aria-selected", String(id === state.selectedConsoleEvent))
  })
  const selected = snapshot.events.find(
    (event) => event.id === state.selectedConsoleEvent,
  )
  const detail = root.querySelector("[data-console-detail]")
  if (selected) renderConsoleDetail(detail, selected, snapshot)
  else if (!detail.querySelector(".console-empty"))
    detail.innerHTML = `<div class="console-empty"><p class="eyebrow">Event detail</p><h2 id="console-detail-title">Select an event</h2><p>Choose an item to inspect its facts and triage attempts.</p></div>`
  const failing = snapshot.sources.filter((source) => source.lastError).length
  setText(
    root.querySelector("[data-console-health]"),
    failing
      ? `✕ ${failing} source${failing === 1 ? "" : "s"} failing`
      : "✓ sources healthy",
  )
}

function pipelineMount(root) {
  root.innerHTML = `<div class="pipeline layout-shell">
    <header class="pipeline-heading"><div><p class="eyebrow">Queue flow</p><h1>Intake pipeline</h1></div><p>Position shows where work is waiting.</p></header>
    <section class="intake-gutter" aria-labelledby="intake-title"><h2 id="intake-title">Source inlets</h2><div data-pipeline-sources></div></section>
    <nav class="pipeline-tabs" aria-label="Pipeline stages"></nav>
    <section class="pipeline-board" aria-label="Event pipeline">
      <section class="stage stage-observed" data-stage="observed"><header><span>01</span><h2>Observed</h2><b data-stage-count>0</b><small>ready for triage</small></header><div data-stage-list></div></section>
      <section class="stage stage-waiting" data-stage="waiting"><header><span>02</span><h2>Waiting</h2><b data-stage-count>0</b><small>retry backoff</small></header><div data-stage-list></div></section>
      <section class="stage stage-triaging" data-stage="triaging"><header><span>03</span><h2>Triaging</h2><b data-stage-count>0</b><small>work in progress</small></header><div data-stage-list></div></section>
      <section class="stage stage-settled" data-stage="settled"><header><span>04</span><h2>Settled</h2><b data-stage-count>0</b><small>recent outcomes</small></header><div data-stage-list></div></section>
    </section>
  </div>`
}

function pipelineCard() {
  const card = node("article", "pipeline-card")
  const button = node("button", "pipeline-card-button")
  button.type = "button"
  button.innerHTML = `<span class="pipeline-card-status"></span><strong></strong><small></small><time></time><span class="pipeline-card-arrow" aria-hidden="true">→</span>`
  card.append(button)
  return card
}

function updatePipelineCard(card, item) {
  const button = card.querySelector("button")
  button.dataset.action = item.type === "run" ? "route-run" : "route-event"
  button.dataset.id = String(item.value.id)
  button.dataset.focusKey = `pipeline-${item.type}-${item.value.id}`
  const isRun = item.type === "run"
  button.querySelector(".pipeline-card-status").innerHTML = statusMarkup(
    isRun ? item.value.state : item.value.status,
    isRun,
  )
  setText(
    button.querySelector("strong"),
    isRun ? item.value.eventTitle : item.value.title,
  )
  if (isRun) {
    const activeStep = item.value.steps.find((step) => step.state === "active")
    setText(
      button.querySelector("small"),
      `attempt ${item.value.attempt}${activeStep ? ` · ${activeStep.label}` : ""}`,
    )
    const time = button.querySelector("time")
    time.dataset.startedAt = item.value.startedAt
    setText(time, formatDuration(runDuration(item.value)))
  } else {
    setText(
      button.querySelector("small"),
      `${item.value.source} · ${item.value.kind}`,
    )
    const time = button.querySelector("time")
    if (item.value.status === "retryable" && item.value.nextAttemptAt) {
      time.dataset.relativeTime = item.value.nextAttemptAt
      setText(time, `retry ${relativeTime(item.value.nextAttemptAt)}`)
    } else {
      time.dataset.relativeTime = item.value.observedAt
      setText(time, relativeTime(item.value.observedAt))
    }
  }
  card.className = `pipeline-card ${isRun && stalled(item.value) ? "is-stalled" : ""}`
}

function pipelineRender(root, snapshot) {
  renderSources(root.querySelector("[data-pipeline-sources]"), snapshot.sources)
  const triagingRuns = activeRuns(snapshot)
  const stages = {
    observed: snapshot.events
      .filter((event) => event.status === "pending")
      .map((value) => ({ type: "event", value })),
    waiting: snapshot.events
      .filter((event) => event.status === "retryable")
      .map((value) => ({ type: "event", value })),
    triaging: [
      ...triagingRuns.map((value) => ({ type: "run", value })),
      ...snapshot.events
        .filter(
          (event) =>
            event.status === "processing" &&
            !triagingRuns.some((run) => run.eventId === event.id),
        )
        .map((value) => ({ type: "event", value })),
    ],
    settled: snapshot.events
      .filter(
        (event) =>
          HANDLED_STATES.includes(event.status) || event.status === "failed",
      )
      .map((value) => ({ type: "event", value })),
  }
  for (const [name, items] of Object.entries(stages)) {
    const stage = root.querySelector(`[data-stage="${name}"]`)
    setText(stage.querySelector("[data-stage-count]"), items.length)
    keyedList(
      stage.querySelector("[data-stage-list]"),
      items,
      (item) => `${item.type}-${item.value.id}`,
      pipelineCard,
      updatePipelineCard,
    )
  }
}

function briefingMount(root) {
  root.innerHTML = `<article class="briefing layout-shell">
    <header class="briefing-verdict"><p class="eyebrow">Your intake briefing</p><h1 data-briefing-verdict>Checking the queue.</h1><p data-briefing-summary></p></header>
    <section class="briefing-section briefing-attention" aria-labelledby="briefing-attention-title" hidden><h2 id="briefing-attention-title">Needs you</h2><div data-briefing-attention></div></section>
    <section class="briefing-section briefing-working" aria-labelledby="briefing-working-title" hidden><h2 id="briefing-working-title">Working now</h2><div data-briefing-active></div></section>
    <section class="briefing-section" aria-labelledby="briefing-sources-title"><h2 id="briefing-sources-title">Sources</h2><div data-briefing-sources></div></section>
    <details class="briefing-record"><summary><span>The recent record</span><b data-briefing-record-summary></b></summary><div class="briefing-record-body">${filterMarkup("Filter the recent record")}<div data-briefing-events></div><h3>Completed runs</h3><div data-briefing-runs></div><p class="window-note" data-window-note></p></div></details>
  </article>`
}

function briefingRender(root, snapshot) {
  updateFilters(root, snapshot)
  const failing = snapshot.sources.filter((source) => source.lastError)
  const active = activeRuns(snapshot)
  const attention = snapshot.events.filter(needsAttention)
  let verdict = "Nothing needs you."
  if (failing.length)
    verdict = `${failing.length} source${failing.length === 1 ? " is" : "s are"} failing.`
  else if (attention.length)
    verdict = `${attention.length} item${attention.length === 1 ? " needs" : "s need"} you.`
  else if (active.length)
    verdict = `${active.length} triage run${active.length === 1 ? " is" : "s are"} working.`
  setText(root.querySelector("[data-briefing-verdict]"), verdict)
  const sourceHealth = failing.length
    ? `${snapshot.sources.length - failing.length} of ${snapshot.sources.length} sources healthy.`
    : `All ${snapshot.sources.length} source${snapshot.sources.length === 1 ? " is" : "s are"} healthy.`
  setText(
    root.querySelector("[data-briefing-summary]"),
    `${snapshot.open} open, oldest ${snapshot.oldestOpenAt ? formatDuration(elapsed(snapshot.oldestOpenAt)) : "clear"}. ${sourceHealth}`,
  )
  const attentionSection = root.querySelector(".briefing-attention")
  attentionSection.hidden = attention.length === 0 && failing.length === 0
  renderEventList(
    root.querySelector("[data-briefing-attention]"),
    attention,
    "briefing-attention",
    {
      empty: failing.length
        ? "Source health needs attention below."
        : "Nothing needs attention.",
    },
  )
  const working = root.querySelector(".briefing-working")
  working.hidden = active.length === 0
  renderRunList(
    root.querySelector("[data-briefing-active]"),
    active,
    "briefing-active",
  )
  renderSources(root.querySelector("[data-briefing-sources]"), snapshot.sources)
  renderEventList(
    root.querySelector("[data-briefing-events]"),
    filteredEvents(snapshot),
    "briefing-record",
  )
  const completed = snapshot.runs.filter((run) => run.state !== "active")
  renderRunList(
    root.querySelector("[data-briefing-runs]"),
    completed,
    "briefing-runs",
  )
  const succeeded = completed.filter((run) => run.state === "succeeded").length
  setText(
    root.querySelector("[data-briefing-record-summary]"),
    `${snapshot.events.length} events · ${succeeded} of ${completed.length} runs succeeded`,
  )
}

function wallMount(root) {
  root.innerHTML = `<div class="wall layout-shell">
    <header class="wall-heading"><div><p class="eyebrow">Continuous telemetry</p><h1>Intake wall</h1></div><span data-wall-verdict>Connecting</span></header>
    <section class="wall-kpis" aria-label="Queue telemetry">
      ${metric("Open", "0", "queue depth", "metric-info")}${metric("Needs you", "0", "attention", "metric-warn")}${metric("Active", "0", "triage runs", "metric-ok")}${metric("Oldest open", "-", "queue age")}${metric("Handled", "0", "all time")}${metric("Success rate", "-", "recent runs")}
    </section>
    <section class="wall-panel wall-active" aria-labelledby="wall-active-title"><header><h2 id="wall-active-title">Active lanes</h2><span>shared elapsed scale</span></header><div data-wall-active></div></section>
    <section class="wall-panel wall-health" aria-labelledby="wall-health-title"><header><h2 id="wall-health-title">Source matrix</h2><span>current poll state</span></header><div data-wall-sources></div></section>
    <section class="wall-panel wall-events" aria-labelledby="wall-events-title"><header><h2 id="wall-events-title">Event ledger</h2>${filterMarkup("Filter event telemetry")}</header><div class="wall-table-scroll"><table><thead><tr><th>Status</th><th>Event</th><th>Source</th><th>Age</th></tr></thead><tbody data-wall-events></tbody></table></div><p class="window-note" data-window-note></p></section>
    <section class="wall-panel wall-runs" aria-labelledby="wall-runs-title"><header><h2 id="wall-runs-title">Run telemetry</h2><span>newest first</span></header><div class="wall-table-scroll"><table><thead><tr><th>State</th><th>Run</th><th>Duration</th></tr></thead><tbody data-wall-runs></tbody></table></div></section>
  </div>`
}

function wallRunLane() {
  const lane = node("button", "wall-run-lane")
  lane.type = "button"
  lane.innerHTML = `<span><strong></strong><small></small></span><span class="lane-track"><i></i></span><time></time>`
  return lane
}

function wallEventRow() {
  const row = node("tr")
  row.innerHTML = `<td data-label="Status"></td><td data-label="Event"><button type="button"></button></td><td data-label="Source"></td><td data-label="Age"><time></time></td>`
  return row
}

function wallRunRow() {
  const row = node("tr")
  row.innerHTML = `<td data-label="State"></td><td data-label="Run"><button type="button"></button></td><td data-label="Duration"><time></time></td>`
  return row
}

function wallRender(root, snapshot) {
  updateSharedMetrics(root, snapshot)
  updateFilters(root, snapshot)
  const completed = snapshot.runs.filter((run) => run.state !== "active")
  const succeeded = completed.filter((run) => run.state === "succeeded").length
  setText(
    root.querySelector('[data-metric="success-rate"]'),
    completed.length
      ? `${Math.round((succeeded / completed.length) * 100)}%`
      : "-",
  )
  const failing = snapshot.sources.filter((source) => source.lastError).length
  setText(
    root.querySelector("[data-wall-verdict]"),
    failing
      ? `✕ ${failing} source${failing === 1 ? "" : "s"} failing`
      : snapshot.attention
        ? `↻ ${snapshot.attention} need you`
        : activeRuns(snapshot).length
          ? "● Working"
          : "✓ All clear",
  )
  const active = activeRuns(snapshot)
  const maxElapsed = Math.max(...active.map(runDuration), 1)
  const lanes = active.length ? active : [{ id: "empty" }]
  keyedList(
    root.querySelector("[data-wall-active]"),
    lanes,
    (run) => run.id,
    (run) =>
      run.id === "empty"
        ? node("p", "empty-state", "No active runs")
        : wallRunLane(),
    (lane, run) => {
      if (run.id === "empty") return
      lane.dataset.action = "route-run"
      lane.dataset.id = String(run.id)
      lane.dataset.focusKey = `wall-run-${run.id}`
      setText(lane.querySelector("strong"), run.eventTitle)
      const step = run.steps.find((candidate) => candidate.state === "active")
      setText(
        lane.querySelector("small"),
        `${run.source}${step ? ` · ${step.label}` : ""}`,
      )
      lane.querySelector("i").style.width =
        `${Math.max(4, (runDuration(run) / maxElapsed) * 100)}%`
      const time = lane.querySelector("time")
      time.dataset.startedAt = run.startedAt
      setText(time, formatDuration(runDuration(run)))
      lane.classList.toggle("is-stalled", stalled(run))
    },
  )
  renderSources(root.querySelector("[data-wall-sources]"), snapshot.sources)
  keyedList(
    root.querySelector("[data-wall-events]"),
    filteredEvents(snapshot),
    (event) => event.id,
    wallEventRow,
    (row, event) => {
      row.querySelector("td").innerHTML = statusMarkup(event.status)
      const button = row.querySelector("button")
      setText(button, event.title)
      button.dataset.action = "route-event"
      button.dataset.id = String(event.id)
      button.dataset.focusKey = `wall-event-${event.id}`
      setText(row.children[2], event.source)
      const time = row.querySelector("time")
      time.dataset.relativeTime = event.observedAt
      setText(time, relativeTime(event.observedAt))
    },
  )
  keyedList(
    root.querySelector("[data-wall-runs]"),
    completed,
    (run) => run.id,
    wallRunRow,
    (row, run) => {
      row.querySelector("td").innerHTML = statusMarkup(run.state, true)
      const button = row.querySelector("button")
      setText(button, run.eventTitle)
      button.dataset.action = "route-run"
      button.dataset.id = String(run.id)
      button.dataset.focusKey = `wall-history-${run.id}`
      const time = row.querySelector("time")
      time.dataset.startedAt = run.startedAt
      time.dataset.endedAt = run.endedAt
      setText(time, formatDuration(runDuration(run)))
    },
  )
}

const presenters = {
  ledger: { mount: ledgerMount, render: ledgerRender },
  console: { mount: consoleMount, render: consoleRender },
  pipeline: { mount: pipelineMount, render: pipelineRender },
  briefing: { mount: briefingMount, render: briefingRender },
  wall: { mount: wallMount, render: wallRender },
}

function renderCurrent() {
  if (!state.snapshot) return
  const root = element("dashboard-root")
  const design = currentDesign()
  if (state.mountedDesign !== design) {
    presenters[design].mount(root)
    state.mountedDesign = design
  }
  presenters[design].render(root, state.snapshot)
  renderRoute()
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
  renderCurrent()
}

function parseRoute() {
  const match = location.hash.match(/^#\/(run|event)\/(\d+)$/)
  state.route = match
    ? { kind: match[1], id: Number(match[2]) }
    : { kind: null, id: null }
  if (currentDesign() === "console")
    state.selectedConsoleEvent =
      state.route.kind === "event" ? state.route.id : null
}

function navigate(kind, id, origin) {
  state.routeOrigin = origin || document.activeElement
  location.hash = `#/${kind}/${id}`
}

function closeRoute() {
  history.back()
}

function dismissRoute() {
  location.hash = "#/"
}

function renderRoute() {
  const layer = element("route-layer")
  if (!state.snapshot || !state.route.kind) {
    layer.hidden = true
    return
  }
  if (currentDesign() === "console" && state.route.kind === "event") {
    layer.hidden = true
    return
  }
  const record =
    state.route.kind === "run"
      ? state.snapshot.runs.find((run) => run.id === state.route.id)
      : state.snapshot.events.find((event) => event.id === state.route.id)
  if (!record) {
    layer.hidden = true
    return
  }
  const panel = element("route-content")
  const routeKey = `${state.route.kind}-${state.route.id}-${state.contentKey}`
  if (panel.dataset.routeKey !== routeKey) {
    const focusedKey = panel.contains(document.activeElement)
      ? document.activeElement.dataset.focusKey
      : null
    panel.dataset.routeKey = routeKey
    const heading = node("div", "route-title-block")
    heading.append(
      node(
        "p",
        "eyebrow",
        state.route.kind === "run" ? `Run ${record.id}` : `Event ${record.id}`,
      ),
      node(
        "h1",
        "",
        state.route.kind === "run" ? record.eventTitle : record.title,
      ),
      node("div", "route-status"),
    )
    heading.querySelector("h1").id = "route-title"
    heading.querySelector(".route-status").innerHTML = statusMarkup(
      state.route.kind === "run" ? record.state : record.status,
      state.route.kind === "run",
    )
    const body = node("div", "route-body")
    panel.replaceChildren(heading, body)
    if (state.route.kind === "run") runDetails(body, record)
    else {
      eventDetails(body, record)
      const attempts = state.snapshot.runs.filter(
        (run) => run.eventId === record.id,
      )
      const attemptsSection = node("section", "route-attempts")
      attemptsSection.append(node("h2", "eyebrow", "Triage attempts"))
      const list = node("div")
      renderRunList(list, attempts, `route-event-${record.id}`)
      attemptsSection.append(list)
      body.append(attemptsSection)
    }
    if (focusedKey)
      requestAnimationFrame(() =>
        panel
          .querySelector(`[data-focus-key="${CSS.escape(focusedKey)}"]`)
          ?.focus(),
      )
  }
  setText(
    "route-kind",
    state.route.kind === "run" ? "Run detail" : "Event detail",
  )
  layer.hidden = false
}

function handleRouteChange() {
  const wasOpen = !element("route-layer").hidden
  parseRoute()
  renderCurrent()
  const isOpen = !element("route-layer").hidden
  if (!wasOpen && isOpen) element("route-back").focus()
  if (wasOpen && !isOpen && state.routeOrigin?.isConnected)
    state.routeOrigin.focus()
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
  setText("connection-note", `updated ${formatDuration(age)} ago`)
  if (age > 15000 && state.snapshot && state.failureCount === 0)
    setConnection("stale")
}

function updateLiveTimes() {
  document.querySelectorAll("[data-relative-time]").forEach((target) => {
    const prefix = target.dataset.relativeTimePrefix || ""
    setText(target, `${prefix}${relativeTime(target.dataset.relativeTime)}`)
  })
  document
    .querySelectorAll("[data-started-at], .run-live-duration")
    .forEach((target) => {
      if (!target.dataset.startedAt) return
      const end = parseTime(target.dataset.endedAt) ?? Date.now()
      setText(target, formatDuration(elapsed(target.dataset.startedAt, end)))
    })
  document.querySelectorAll("[data-step-started-at]").forEach((target) => {
    if (target.dataset.stepEndedAt) return
    setText(target, `${formatDuration(elapsed(target.dataset.stepStartedAt))}…`)
  })
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

function trapRouteFocus(event) {
  if (event.key !== "Tab" || element("route-layer").hidden) return
  const focusable = [
    ...element("route-panel").querySelectorAll(
      "button, a[href], select, [tabindex]:not([tabindex='-1'])",
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

function handleRootClick(event) {
  const control = event.target.closest("button")
  if (!control) return
  if (control.dataset.filter) {
    state.filter = control.dataset.filter
    renderCurrent()
    return
  }
  if (
    control.dataset.action === "expand-event" ||
    control.dataset.action === "expand-run"
  ) {
    const key = control.dataset.expandKey
    if (state.expanded.has(key)) state.expanded.delete(key)
    else state.expanded.add(key)
    renderCurrent()
    return
  }
  if (control.dataset.action === "console-close") {
    state.selectedConsoleEvent = null
    history.pushState(null, "", "#/")
    parseRoute()
    renderCurrent()
    element("dashboard-root").querySelector("[data-console-events]")?.focus()
    return
  }
  if (control.dataset.action === "route-event")
    navigate("event", control.dataset.id, control)
  if (control.dataset.action === "route-run")
    navigate("run", control.dataset.id, control)
}

function handleConsoleKeys(event) {
  if (currentDesign() !== "console" || !state.snapshot) return
  const list = event.target.closest("[data-console-events]")
  if (!list || !["ArrowDown", "ArrowUp", "j", "k", "Enter"].includes(event.key))
    return
  const events = filteredEvents(state.snapshot)
  if (!events.length) return
  event.preventDefault()
  let index = events.findIndex((item) => item.id === state.selectedConsoleEvent)
  if (event.key === "Enter") {
    const selected = events[Math.max(0, index)]
    if (selected) navigate("event", selected.id, list)
    return
  }
  index += event.key === "ArrowUp" || event.key === "k" ? -1 : 1
  index = Math.max(0, Math.min(events.length - 1, index))
  state.selectedConsoleEvent = events[index].id
  history.pushState(null, "", `#/event/${events[index].id}`)
  parseRoute()
  renderCurrent()
  list
    .querySelector(`[data-id="${events[index].id}"]`)
    ?.scrollIntoView({ block: "nearest" })
}

const designSelect = element("design-select")
designSelect.value = currentDesign()
designSelect.addEventListener("change", () => {
  const design = DESIGN_IDS.includes(designSelect.value)
    ? designSelect.value
    : "ledger"
  try {
    localStorage.setItem("im-design", design)
  } catch {}
  const url = new URL(location.href)
  url.searchParams.set("design", design)
  location.assign(url)
})

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

element("dashboard-root").addEventListener("click", handleRootClick)
element("dashboard-root").addEventListener("keydown", handleConsoleKeys)
element("route-back").addEventListener("click", closeRoute)
element("route-close").addEventListener("click", dismissRoute)
element("route-backdrop").addEventListener("click", dismissRoute)
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

parseRoute()
setText("daemon-label", `${location.host} · local daemon`)
setConnection("connecting")
setInterval(updateLiveTimes, 1000)
refresh()
