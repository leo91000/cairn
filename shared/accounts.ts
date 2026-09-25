// Coding-agent accounts, as served by /api/accounts.
export type Provider = 'codex' | 'claude'

/** What the account needs or does, computed by the server's selection rules. */
export type AccountStatus = 'signIn' | 'reconnect' | 'paused' | 'unavailable' | 'waiting' | 'full' | 'next' | 'low' | 'ready'

export interface UsageWindow {
  id: string
  label: string
  usedPercent: number
  resetsAt: number | null
  durationMins: number | null
  /** Empty when the window limits every model. */
  models: string[]
  reached?: boolean
}
export interface Usage {
  allowed: boolean
  windows: UsageWindow[]
  checkedAt: number | null
  error?: string | null
  /** Codex banked resets. */
  resets: { available: number } | null
}
export interface Account {
  id: string
  provider: Provider
  name: string
  enabled: boolean
  email: string | null
  plan: string | null
  state: 'pending' | 'ready' | 'error'
  status: AccountStatus
  error: string
  resetError?: string
  createdAt: number
  checkedAt: number | null
  lastUsedAt: number | null
  maxConcurrentRuns: number
  activeRunIds: string[]
  usage: Usage | null
  remainingPercent: number | null
  resetsAt: number | null
  stale: boolean
  exhausted: boolean
}
export interface SignIn {
  accountId: string
  provider: Provider
  state: 'pending' | 'complete' | 'failed' | 'cancelled'
  phase: 'starting' | 'authorizing' | 'verifying'
  url: string | null
  /** A code to enter on the sign-in page (Codex). */
  code: string | null
  /** Whether the sign-in page shows a code to paste back here (Claude Code). */
  acceptsCode: boolean
  expiresAt: number | null
  error: string | null
}
export interface Accounts {
  accounts: Account[]
  signIn: SignIn | null
  /** Coding agents whose runs wait for the user to connect, reconnect or resume an account. */
  required: Provider[]
}

export const providers: Record<Provider, { label: string, vendor: string, subscription: string }> = {
  codex: { label: 'Codex', vendor: 'OpenAI', subscription: 'ChatGPT account' },
  claude: { label: 'Claude Code', vendor: 'Anthropic', subscription: 'Claude account' },
}

/** Remaining percentage under which a window is running low. */
export const lowPercent = 10

/** The windows limiting every model, shortest first. */
export function generalWindows(usage: Usage | null) {
  return (usage?.windows ?? []).filter(window => !window.models.length).sort((a, b) => (a.durationMins ?? 0) - (b.durationMins ?? 0))
}

export function remainingPercent(window: UsageWindow) {
  return Math.max(0, Math.min(100, 100 - window.usedPercent))
}
