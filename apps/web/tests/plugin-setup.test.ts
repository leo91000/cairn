import type { SetupStep } from '../src/plugin-setup'
import type { PluginConnection, SettingField } from '../src/plugins'
import { describe, expect, it } from 'vitest'
import corpus from '../../../tests/fixtures/plugin-setup.json'
import {
  matchingPreset,
  pluginSetup,
  presetValues,
  sharedSources,
  shortValue,
} from '../src/plugin-setup'

describe('plugin setup (shared Rust and Android corpus)', () => {
  it('prefers the steps the installation resolved', () => {
    const setup = pluginSetup({
      setup: { effort: 'app', summary: 'Create an app once.', steps: [{ id: 'connect', kind: 'credential', title: 'Sign in with Google' }] },
    })
    expect(setup.summary).toBe('Create an app once.')
    expect(setup.steps.map(step => step.title)).toEqual(['Sign in with Google'])
    expect(pluginSetup({ setup: null })).toEqual({ steps: [] })
  })

  it('matches and applies presets over every switch of the step', () => {
    const step = corpus.presets.step as SetupStep
    const fields = corpus.presets.fields as SettingField[]
    for (const item of corpus.presets.cases)
      expect(matchingPreset(step, fields, item.values), JSON.stringify(item.values)).toBe(item.matching)
    for (const [id, values] of Object.entries(corpus.presets.applied))
      expect(presetValues(step, fields, step.presets!.find(preset => preset.id === id)!)).toEqual(values)
  })

  it('keeps secret-bearing sources selectable and permits absent optional secrets', () => {
    const { steps, fields, connections } = corpus.sources
    for (const item of corpus.sources.cases) {
      const sources = sharedSources((item.steps ?? steps) as SetupStep[], (item.fields ?? fields) as SettingField[], connections as unknown as PluginConnection[], item.except ?? undefined)
      expect(sources, String(item.except)).toEqual(item.expected)
    }

    expect(sharedSources([{ id: 'connect', kind: 'credential', title: 'Sign in' }], [], connections as unknown as PluginConnection[])).toEqual([])
  })

  it('shortens long identifiers around an ellipsis', () => {
    expect(shortValue('481203947512-k2d9qv0bm3h1.apps.googleusercontent.com', 6)).toBe('481203…nt.com')
    expect(shortValue('short')).toBe('short')
  })
})
