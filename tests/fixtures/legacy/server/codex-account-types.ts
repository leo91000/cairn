export interface CodexLoginFlow {
  accountId: string
  state: 'pending' | 'complete' | 'failed'
  phase?: 'starting' | 'authorizing' | 'verifying'
  code?: string
  url?: string
  expiresAt?: number | null
  error?: string
}

export interface UsageWindow {
  usedPercent: number
  windowDurationMins: number | null
  resetsAt: number | null
}
export interface UsageBucket {
  limitId: string | null
  limitName: string | null
  normalModelSlug?: string | null
  primary: UsageWindow | null
  secondary: UsageWindow | null
  rateLimitReachedType?: string | null
  spendControlReached?: boolean | null
}
export interface AccountLimits {
  ordinaryUsageAllowed?: boolean | null
  accountId?: string | null
  rateLimits: UsageBucket
  rateLimitsByLimitId?: Record<string, UsageBucket> | null
  rateLimitResetCredits?: {
    availableCount: number
    credits?: Array<{
      id: string
      resetType: string
      status: string
      expiresAt: number | null
    }> | null
  } | null
}
export interface CodexAccount {
  name: string
  enabled: boolean
  id: string
  email: string | null
  plan: string | null
  identity: string | null
  createdAt: number
  checkedAt: number | null
  state: 'pending' | 'ready' | 'error'
  error: string
  resetError?: string
  limits: AccountLimits | null
  lastUsedAt: number | null
  exhausted: { at: number, model: string, limits: AccountLimits | null } | null
}
export interface CodexAccountView extends Omit<CodexAccount, 'identity'> {
  remainingPercent: number | null
  stale: boolean
  activeRunId: string | null
  activeRunIds?: string[]
  maxConcurrentRuns?: number
}

function usageBuckets(limits: AccountLimits | null, model: string): Record<string, UsageBucket> {
  if (!limits)
    return {}
  return Object.fromEntries([
    ['$main', limits.rateLimits],
    ...Object.entries(limits.rateLimitsByLimitId || {}).filter(([, bucket]) => model && (bucket.normalModelSlug === model || bucket.limitId === model)),
  ])
}

export function remainingUsage(limits: AccountLimits | null, model = ''): number | null {
  const windows = Object.values(usageBuckets(limits, model)).flatMap(bucket => [bucket.primary, bucket.secondary]).filter((window): window is UsageWindow => !!window && Number.isFinite(window.usedPercent))
  if (!windows.length)
    return null
  return Math.max(0, Math.min(100, ...windows.map(window => 100 - window.usedPercent)))
}

export function usageBlocked(limits: AccountLimits, model = '') {
  return limits.ordinaryUsageAllowed === false || Object.values(usageBuckets(limits, model)).some(bucket => !!bucket.rateLimitReachedType || bucket.spendControlReached === true)
}

// Compare each window: one can reset while the other still limits total capacity.
export function usageRecovered(before: AccountLimits | null, after: AccountLimits, model: string) {
  if (usageBlocked(after, model) || (remainingUsage(after, model) ?? 0) <= 0)
    return false
  if (before?.ordinaryUsageAllowed === false && after.ordinaryUsageAllowed === true)
    return true
  const previous = usageBuckets(before, model)
  return Object.entries(usageBuckets(after, model)).some(([key, bucket]) => (['primary', 'secondary'] as const).some((window) => {
    const old = previous[key]?.[window]
    const current = bucket[window]
    return !!old && !!current && current.usedPercent < old.usedPercent
  }))
}
