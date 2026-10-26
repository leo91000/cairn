/** Who pays for a route: a subscription's included usage, prepaid credits or the user's own key. */
export type RouteBilling = 'subscription' | 'credits' | 'key'
export const billingLabels: Record<RouteBilling, string> = {
  subscription: 'Included',
  credits: 'Credits',
  key: 'Your key',
}
/** One connected provider offering a model, as `provider/model`. */
export interface ModelRoute {
  model: string
  provider: string
  providerName: string
  billing: RouteBilling
  hidden: boolean
  supportedReasoningEfforts: { reasoningEffort: string, description: string }[]
}
/** A speed Codex offers for a model. `description` is Codex's own wording of its usage. */
export interface ServiceTier {
  id: string
  name: string
  description: string
  /** Offered by some enabled accounts only; runs asking for it prefer those, otherwise run at Standard. */
  limited?: boolean
}
export interface CodingAgentModel {
  model: string
  /** Models offered by several providers. `model` then runs on the first available route. */
  routes?: ModelRoute[]
  displayName: string
  description: string
  hidden: boolean
  isDefault: boolean
  defaultReasoningEffort: string
  supportedReasoningEfforts: { reasoningEffort: string, description: string }[]
  /** Codex speeds besides standard. Absent from catalogs cached before speeds were listed. */
  serviceTiers?: ServiceTier[]
  /** Codex's catalog default tier, applied only by Codex's own terminal client. Cairn runs Standard when no speed is chosen. */
  defaultServiceTier?: string | null
}
export interface ModelCatalog {
  models: CodingAgentModel[]
  checkedAt: number | null
  stale: boolean
  error: string
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
