import type { CodingAgentModel } from '../../../packages/contracts/models'
import { describe, expect, it } from 'vitest'
import { speedLabel } from '../../../packages/contracts/models'
import {
  effectiveSpeed,
  offersSpeed,
  showsSpeed,
  speedNote,
  speedTier,
} from '../src/speeds'

function model(fields: Partial<CodingAgentModel>): CodingAgentModel {
  return {
    model: 'gpt-6-astra',
    displayName: 'GPT-6 Astra',
    description: '',
    hidden: false,
    isDefault: true,
    defaultReasoningEffort: 'medium',
    supportedReasoningEfforts: [],
    ...fields,
  }
}

const fast = { id: 'priority', name: 'Fast', description: '2x speed, increased usage' }
const astra = model({ serviceTiers: [fast], defaultServiceTier: null })

describe('codex speeds', () => {
  // Codex's app server ignores a model's defaultServiceTier: without a choice, turns run at Standard.
  it('uses the chosen speed, then the inherited one, then standard', () => {
    expect(effectiveSpeed('', '')).toBe('standard')
    expect(effectiveSpeed('', 'priority')).toBe('priority')
    expect(effectiveSpeed('standard', 'priority')).toBe('standard')
  })

  it('names a queued speed as its model runs it', () => {
    const deep = model({ model: 'deep', serviceTiers: [] })
    const fast = model({ model: 'fast', serviceTiers: [{ id: 'priority', name: 'Fast', description: '' }] })
    // A faster speed is a request: each turn says how it runs.
    expect(speedNote('priority', 'fast', [fast, deep])).toBe('Fast speed requested')
    expect(speedNote('priority', 'deep', [fast, deep])).toBe('Fast speed requested (not listed for this model)')
    // An empty model is the catalog's default.
    expect(speedNote('priority', '', [{ ...fast, isDefault: false }, { ...deep, isDefault: true }])).toBe('Fast speed requested (not listed for this model)')
    // Without the model's speeds, the speed is only requested.
    expect(speedNote('priority', 'custom', [fast, deep])).toBe('Fast speed requested')
    expect(speedNote('priority', 'fast', [])).toBe('Priority speed requested')
    expect(speedNote('standard', 'deep', [fast, deep])).toBe('Standard speed')
    expect(speedNote('', 'deep', [fast, deep])).toBe('')
    // A speed some account cannot confirm is only requested.
    const limited = model({
      model: 'fast',
      serviceTiers: [{
        id: 'priority',
        name: 'Fast',
        description: '',
        limited: true,
      }],
    })
    expect(speedNote('priority', 'fast', [limited])).toBe('Fast speed requested')
  })

  it('treats catalogs without speeds as unknown rather than unsupported', () => {
    expect(offersSpeed(astra, 'priority')).toBe(true)
    expect(offersSpeed(astra, 'ultrafast')).toBe(false)
    expect(offersSpeed(model({ serviceTiers: [] }), 'priority')).toBe(false)
    expect(offersSpeed(model({}), 'priority')).toBe(true)
    expect(offersSpeed(undefined, 'priority')).toBe(true)
    expect(offersSpeed(model({ serviceTiers: [] }), 'standard')).toBe(true)
  })

  it('offers speeds only for Codex models listing them, or to keep a saved choice', () => {
    expect(showsSpeed('codex', astra, '')).toBe(true)
    expect(showsSpeed('codex', model({ serviceTiers: [] }), '')).toBe(false)
    expect(showsSpeed('codex', model({}), 'priority')).toBe(true)
    expect(showsSpeed('claude', astra, 'priority')).toBe(false)
    expect(showsSpeed('opencode', astra, '')).toBe(false)
  })

  it('names speeds with Codex\'s own words', () => {
    expect(speedLabel('priority', [fast])).toBe('Fast')
    expect(speedLabel('standard')).toBe('Standard')
    expect(speedLabel('ultra_fast')).toBe('Ultra fast')
    expect(speedLabel('')).toBe('Default')
    expect(speedTier(astra, 'priority')?.description).toBe('2x speed, increased usage')
    expect(speedTier(astra, 'standard')).toBeUndefined()
  })
})
