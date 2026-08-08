export function formatNumber(value: number): string {
  return new Intl.NumberFormat(undefined, { notation: "compact" }).format(value)
}

export function formatMoney(value: number): string {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 4,
  }).format(value)
}

export function stateLabel(value: string): string {
  return value.replaceAll("_", " ")
}

export function plainSummary(value: string): string {
  return value.replace(/\*\*(.+?)\*\*/gs, "$1").replace(/__(.+?)__/gs, "$1")
}
