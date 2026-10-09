export interface CodexModel {
  model: string
  displayName: string
  description: string
  hidden: boolean
  isDefault: boolean
  defaultReasoningEffort: string
  supportedReasoningEfforts: { reasoningEffort: string, description: string }[]
}
export interface ModelCatalog {
  models: CodexModel[]
  checkedAt: number | null
  stale: boolean
  error: string
}
export function effortLabel(value: string) {
  if (value === 'xhigh')
    return 'Extra high'
  return value ? value[0].toUpperCase() + value.slice(1).replaceAll('_', ' ') : 'Default'
}
