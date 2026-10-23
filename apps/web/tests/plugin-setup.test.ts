import type { SetupStep } from '../src/plugin-setup'
import type { Manifest, PluginConnection, SettingField } from '../src/plugins'
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
  it('derives the installation’s default steps when an older one sends only the manifest', () => {
    for (const item of corpus.derivation) {
      const setup = pluginSetup({ manifest: item.manifest as unknown as Manifest })
      expect(setup.effort ?? null, item.name).toBe(item.effort)
      expect(setup.steps, item.name).toEqual(item.steps)
    }
  })

  it('prefers the steps the installation resolved', () => {
    const setup = pluginSetup({
      manifest: corpus.derivation[0].manifest as unknown as Manifest,
      setup: { effort: 'app', summary: 'Create an app once.', steps: [{ id: 'connect', kind: 'credential', title: 'Sign in with Google' }] },
    })
    expect(setup.summary).toBe('Create an app once.')
    expect(setup.steps.map(step => step.title)).toEqual(['Sign in with Google'])
    expect(pluginSetup({ manifest: null })).toEqual({ steps: [] })

    // Without the resolved setup, the manifest's own steps still win over the default ones.
    const declared = { ...corpus.derivation[0].manifest, setup: { summary: 'Once.', steps: [{ id: 'connect', kind: 'credential', title: 'Sign in with Google' }] } }
    const fromManifest = pluginSetup({ manifest: declared as unknown as Manifest })
    expect(fromManifest).toMatchObject({ effort: 'app', summary: 'Once.' })
    expect(fromManifest.steps.map(step => step.id)).toEqual(['connect'])
    const summaryOnly = { ...corpus.derivation[0].manifest, setup: { summary: 'Once.' } }
    expect(pluginSetup({ manifest: summaryOnly as unknown as Manifest }).steps).toEqual(corpus.derivation[0].steps)
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
