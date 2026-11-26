import type { Agent } from '../../../packages/contracts/contracts'
import type { SetupStep } from './plugin-setup'
import type { PluginConnection, SettingField } from './plugins'
import { stepFields } from './plugin-setup'

export const ACTIVITY_TONES = {
  working: ['bg-soft text-accent', 'Working'],
  replied: ['bg-[light-dark(#dff3ee,#163a33)] text-success', 'Replied'],
  failed: ['bg-coral-soft text-coral', 'Not delivered'],
  stopped: ['bg-hover text-muted', 'Stopped'],
} as const

export const CONNECTION_TONES = {
  ok: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  muted: 'bg-hover text-muted',
  coral: 'bg-coral-soft text-coral',
}

export function displayPluginValue(value: unknown) {
  if (Array.isArray(value))
    return value.length ? value.join(', ') : '—'
  if (typeof value === 'boolean')
    return value ? 'On' : 'Off'
  return value === undefined || value === null || value === '' ? '—' : String(value)
}

export function agentDisplayNames(connection: PluginConnection, agents: Agent[]) {
  return (connection.agentIds ?? []).map(agent => agents.find(item => item.id === agent)?.name ?? 'Removed agent')
}

/** Fields the current step shows besides its presets' switches. */
export function pluginStepShown(item: SetupStep, fields: SettingField[]) {
  const shown = stepFields(item, fields)
  return item.presets?.length ? shown.filter(field => field.type !== 'boolean') : shown
}

export function pluginStepSwitches(item: SetupStep, fields: SettingField[]) {
  return stepFields(item, fields).filter(field => field.type === 'boolean')
}
