export function parseTime(value: string | null): number {
  return value ? Date.parse(value) : Date.now()
}

export function elapsed(start: string, end: string | null = null): number {
  return Math.max(0, parseTime(end) - parseTime(start))
}

export function formatDuration(value: number): string {
  if (value < 1000) return `${Math.round(value)}ms`
  const seconds = Math.round(value / 100) / 10
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  return `${minutes}m ${Math.round(seconds % 60)}s`
}

export function formatLongDuration(value: number): string {
  if (value < 1000) return `${Math.round(value)}ms`
  const seconds = Math.round(value / 100) / 10
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${Math.round(seconds % 60)}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

export function compactDuration(value: number): string {
  const duration = Math.max(0, value)
  if (duration < 1000) return `${Math.round(duration)}ms`
  if (duration < 60_000) return `${Math.round(duration / 100) / 10}s`
  return `${Math.floor(duration / 60_000)}m${Math.round((duration % 60_000) / 1000)}s`
}

export function relativeTime(value: string, now: number): string {
  const difference = parseTime(value) - now
  const absolute = Math.abs(difference)
  const suffix = difference > 0 ? "from now" : "ago"
  if (absolute < 10_000) return "now"
  if (absolute < 60_000) return `${Math.round(absolute / 1000)}s ${suffix}`
  if (absolute < 3_600_000) return `${Math.round(absolute / 60_000)}m ${suffix}`
  if (absolute < 86_400_000)
    return `${Math.round(absolute / 3_600_000)}h ${suffix}`
  return `${Math.round(absolute / 86_400_000)}d ${suffix}`
}

export function clockTime(value: string): string {
  return new Date(value).toLocaleTimeString([], { hour12: false })
}

export function exactTime(value: string): string {
  return new Date(value).toLocaleString([], {
    dateStyle: "medium",
    timeStyle: "medium",
  })
}

export function offsetTime(value: string, start: string): string {
  return `+${formatLongDuration(Math.max(0, parseTime(value) - parseTime(start)))}`
}

export function relativeAge(value: string, now: number): string {
  const age = Math.max(0, now - parseTime(value))
  return `${formatLongDuration(age)} ago`
}
