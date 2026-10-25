import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import wire from '../../packages/contracts/generated/agent-wire.json'
import createSchema from '../../packages/contracts/generated/CreateAgentInput.json'
import replaceSchema from '../../packages/contracts/generated/ReplaceAgentInput.json'
import corpus from '../../tests/fixtures/agent-contracts.json'

it('requires explicit permissions and revisions while sharing scalar defaults', () => {
  const ajv = new Ajv({ useDefaults: true, strict: false })
  addFormats(ajv)
  const create = ajv.compile(createSchema)
  const replace = ajv.compile(replaceSchema)
  const settings = {
    name: 'Agent',
    exclusiveNodes: [],
    plugins: [],
    access: { mcpTools: {} },
  }
  const value = { settings, review: {} }
  expect(create(value), JSON.stringify(create.errors)).toBe(true)
  expect(value.settings).toMatchObject({
    provider: 'codex',
    reasoning: 'high',
    timeoutMinutes: 0,
    access: { nodes: ['00000000-0000-4000-8000-000000000002'], projects: null },
  })
  expect(replace({ settings, review: {} })).toBe(false)
  expect(create({ settings: { name: 'Agent' }, review: {} })).toBe(false)
  expect(create({ settings: { ...settings, provider: 'unknown' }, review: {} })).toBe(false)
})

it('qualifies the shared agent wire corpus using generated schemas', () => {
  const ajv = new Ajv({ useDefaults: true, strict: false })
  addFormats(ajv)
  const create = ajv.compile(createSchema)
  for (const [index, input] of corpus.creates.entries()) {
    const value = structuredClone(input)
    expect(create(value), JSON.stringify(create.errors)).toBe(true)
    expect(value).toEqual(wire.creates[index])
  }

  for (const [index, input] of corpus.invalidCreates.entries()) {
    if (index === 6)
      continue // UUID version 4 is an additional server invariant.
    expect(create(structuredClone(input)), JSON.stringify(input)).toBe(false)
  }
})
