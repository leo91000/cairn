import type { AccountUsage as Usage } from './generated/typescript/AccountUsage'
import type { AccountWindow as UsageWindow } from './generated/typescript/AccountWindow'
// Coding-agent account wire models are generated from Rust.
import type { AgentProvider as Provider } from './generated/typescript/AgentProvider'

export type { AccountSignIn as SignIn } from './generated/typescript/AccountSignIn'
export type { AccountStatus } from './generated/typescript/AccountStatus'
export type { AccountUsage as Usage } from './generated/typescript/AccountUsage'
export type { AccountWindow as UsageWindow } from './generated/typescript/AccountWindow'
export type { AgentProvider as Provider } from './generated/typescript/AgentProvider'
export type { ApiKeyProvider } from './generated/typescript/ApiKeyProvider'
export type { CodingAgentAccount as Account } from './generated/typescript/CodingAgentAccount'
export type { CodingAgentAccounts as Accounts } from './generated/typescript/CodingAgentAccounts'

export const providers: Record<Provider, {
  label: string
  vendor: string
  subscription: string
  connection: 'oauth' | 'apiKey'
  requiresModel: boolean
  /** Runs try accounts in the user's order instead of by remaining capacity. */
  ordered: boolean
}> = {
  codex: {
    label: 'Codex',
    vendor: 'OpenAI',
    subscription: 'ChatGPT account',
    connection: 'oauth',
    requiresModel: false,
    ordered: false,
  },
  claude: {
    label: 'Claude Code',
    vendor: 'Anthropic',
    subscription: 'Claude account',
    connection: 'oauth',
    requiresModel: false,
    ordered: false,
  },
  opencode: {
    label: 'OpenCode',
    vendor: 'Model providers',
    subscription: 'Provider API key',
    connection: 'apiKey',
    requiresModel: true,
    ordered: true,
  },
}

/** Remaining percentage under which a window is running low. */
export const lowPercent = 10

/** The windows limiting every model, shortest first. */
export function generalWindows(usage: Usage | null) {
  return (usage?.windows ?? []).filter(window => !window.models.length).sort((a, b) => (a.durationMins ?? 0) - (b.durationMins ?? 0))
}

export function remainingPercent(window: UsageWindow) {
  return Math.max(0, Math.min(100, 100 - (window.usedPercent ?? 0)))
}
