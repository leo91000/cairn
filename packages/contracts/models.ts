import type { RouteBilling } from './generated/typescript/RouteBilling'
import type { ServiceTier } from './generated/typescript/ServiceTier'

export type { CodingAgentModel } from './generated/typescript/CodingAgentModel'
export type { ModelCatalog } from './generated/typescript/ModelCatalog'
export type { ModelRoute } from './generated/typescript/ModelRoute'
export type { RouteBilling } from './generated/typescript/RouteBilling'
export type { ServiceTier } from './generated/typescript/ServiceTier'

export const billingLabels: Record<RouteBilling, string> = {
  subscription: 'Included',
  credits: 'Credits',
  key: 'Your key',
}

export function effortLabel(value: string) {
  if (value === 'xhigh')
    return 'Extra high'
  return value ? value[0].toUpperCase() + value.slice(1).replaceAll('_', ' ') : 'Default'
}

/** Codex's standard speed, chosen explicitly. An empty speed inherits. */
export const STANDARD_SPEED = 'standard'
/** Where OpenAI documents how each speed uses plan limits and credits. */
export const speedUsageUrl = 'https://learn.chatgpt.com/docs/agent-configuration/speed'
export function speedLabel(value: string, tiers: ServiceTier[] = []) {
  if (value === STANDARD_SPEED)
    return 'Standard'
  return tiers.find(tier => tier.id === value)?.name || (value ? value[0].toUpperCase() + value.slice(1).replaceAll('_', ' ') : 'Default')
}
