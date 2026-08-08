import type { EventStatus } from "./api-types.ts"

export type EventFilter = "all" | "open" | "attention" | "handled"

export const eventFilters: EventFilter[] = [
  "all",
  "open",
  "attention",
  "handled",
]

const filterStatuses: Record<
  Exclude<EventFilter, "all">,
  ReadonlySet<EventStatus>
> = {
  open: new Set(["pending", "processing", "retryable"]),
  attention: new Set(["retryable", "failed"]),
  handled: new Set(["succeeded", "ignored"]),
}

export function matchesEventFilter(
  status: EventStatus,
  filter: EventFilter,
): boolean {
  return filter === "all" || filterStatuses[filter].has(status)
}
