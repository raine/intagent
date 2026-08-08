import type { DispatchTrigger } from "./api-types.ts"

export const dispatchLabels: Record<DispatchTrigger, string> = {
  initial: "First attempt",
  revision: "New revision",
  backoff_retry: "Retry after failure",
  recovery_retry: "Retry after restart",
  operator_retry: "Manual retry",
  manual_injection: "Manual injection",
  superseding_claim: "Superseding claim",
  unknown: "Dispatch unknown",
}
