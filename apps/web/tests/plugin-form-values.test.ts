import type { SettingField } from '../src/plugins'
import { describe, expect, it } from 'vitest'
import corpus from '../../../tests/fixtures/plugin-form-values.json'
import {
  pluginFieldAnswer,
  pluginFormSecrets,
  pluginFormValues,
  pluginSecrets,
  pluginSettings,
} from '../src/plugin-form-values'

describe('declarative plugin forms (shared Android corpus)', () => {
  it('keeps secret fields empty for accepted prototype property names', () => {
    const secrets = pluginFormSecrets()
    const keys = ['constructor', 'toString', 'valueOf']
    for (const key of keys) {
      expect(secrets[key] ?? '').toBe('')
      secrets[key] = 'synthetic secret'
    }

    expect(pluginSecrets(secrets)).toEqual({ constructor: 'synthetic secret', toString: 'synthetic secret', valueOf: 'synthetic secret' })
    const cleared = pluginFormSecrets()
    for (const key of keys)
      expect(cleared[key] ?? '').toBe('')
  })

  it('reads package answers only for the field\'s own key', () => {
    const answers = JSON.parse('{"title":"Already used","__proto__":"Own entry"}') as Record<string, string>
    expect(pluginFieldAnswer(answers, 'title')).toBe('Already used')
    expect(pluginFieldAnswer(answers, '__proto__')).toBe('Own entry')
    for (const key of ['constructor', 'toString', 'valueOf', 'hasOwnProperty'])
      expect(pluginFieldAnswer(answers, key)).toBeUndefined()
    expect(pluginFieldAnswer(undefined, 'title')).toBeUndefined()
  })

  for (const scenario of corpus) {
    it(scenario.name, () => {
      const fields = scenario.fields as SettingField[]
      const secrets = scenario.secrets as Record<string, string>
      const before = structuredClone(scenario)
      const opened = pluginFormValues(fields, scenario.initial)
      expect(pluginSettings(fields, opened)).toEqual(scenario.opened)
      expect(Object.keys(opened)).not.toContain('password')
      const edited = { ...opened, ...scenario.edits }
      expect(pluginSettings(fields, edited)).toEqual(scenario.settings)
      expect(pluginSecrets(secrets)).toEqual(scenario.instanceSecrets)
      expect({ ...pluginSettings(fields, edited), ...pluginSecrets(secrets, false) })
        .toEqual({ ...scenario.settings, ...scenario.actionSecrets })
      expect(scenario).toEqual(before)
    })
  }

  it('edits cannot mutate a package default or saved array', () => {
    const fields: SettingField[] = [{
      key: 'scopes',
      title: 'Scopes',
      type: 'multiselect',
      default: ['read'],
    }]
    const defaults = pluginFormValues(fields)
    defaults.scopes.push('write')
    expect(fields[0].default).toEqual(['read'])
    const saved = { scopes: ['read'] }
    const opened = pluginFormValues(fields, saved)
    opened.scopes.push('write')
    expect(saved.scopes).toEqual(['read'])
    const submitted = pluginSettings(fields, opened)
    ;(submitted.scopes as string[]).push('admin')
    expect(opened.scopes).toEqual(['read', 'write'])
  })
})
