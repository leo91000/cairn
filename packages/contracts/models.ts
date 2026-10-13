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
