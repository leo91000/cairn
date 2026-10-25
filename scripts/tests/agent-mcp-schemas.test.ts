import type { CreateAgentInput } from '../../packages/contracts/generated/typescript/CreateAgentInput'
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
    speed: '',
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

it('publishes resource, skill and MCP tool bounds from the Rust contract', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const create = ajv.compile(createSchema)
  const id = '00000000-0000-4000-8000-000000000003'

  for (const field of ['nodes', 'projects', 'mcps', 'skills'] as const) {
    const input = structuredClone(wire.creates[0]) as CreateAgentInput
    input.settings.access[field] = Array.from<string>({ length: 100 }).fill(field === 'skills' ? 'global/review' : id)
    expect(create(input), JSON.stringify(create.errors)).toBe(true)
    input.settings.access[field]!.push(field === 'skills' ? 'global/extra' : id)
    expect(create(input)).toBe(false)
  }

  for (const field of ['plugins', 'exclusiveNodes'] as const) {
    const input = structuredClone(wire.creates[0]) as CreateAgentInput
    input.settings[field] = Array.from<string>({ length: 100 }).fill(id)
    expect(create(input), JSON.stringify(create.errors)).toBe(true)
    input.settings[field].push(id)
    expect(create(input)).toBe(false)
    input.settings[field] = ['not-a-uuid']
    expect(create(input)).toBe(false)
  }

  for (const field of ['nativeNodeReviews', 'reapproveDelegations'] as const) {
    const input = structuredClone(wire.creates[0]) as CreateAgentInput
    input.review[field] = Array.from<string>({ length: 100 }).fill(id)
    expect(create(input), JSON.stringify(create.errors)).toBe(true)
    input.review[field]!.push(id)
    expect(create(input)).toBe(false)
    input.review[field] = ['not-a-uuid']
    expect(create(input)).toBe(false)
  }

  const input = structuredClone(wire.creates[0]) as CreateAgentInput
  input.settings.access.skills = ['é'.repeat(160)]
  input.settings.access.mcpTools = { [id]: Array.from({ length: 500 }, () => 'r'.repeat(200)) }
  expect(create(input), JSON.stringify(create.errors)).toBe(true)
  for (const invalid of [Array.from<string>({ length: 501 }).fill('read'), [''], ['r'.repeat(201)]]) {
    input.settings.access.mcpTools = { [id]: invalid }
    expect(create(input)).toBe(false)
  }

  input.settings.access.mcpTools = { 'not-a-uuid': ['read'] }
  expect(create(input)).toBe(false)
})
