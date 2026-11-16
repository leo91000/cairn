import type { HookRuleDefinition } from '../../../packages/contracts/generated/typescript/HookRuleDefinition'
import type { RuleNode } from '../../../packages/contracts/generated/typescript/RuleNode'
import { describe, expect, it } from 'vitest'
import { reactive } from 'vue'
import {
  authorVariable,
  comparisonExpects,
  comparisonListSchema,
  newStep,
  nodeAt,
  optionalRuleText,
  pathOfNode,
  ruleConditionDepth,
  sameRuleDefinition,
  schemaFields,
  typedComparisonRight,
  variablesAt,
  withInserted,
  withMoved,
  withRemoved,
  withReplaced,
} from '../src/hook-rules'
import { resolveRuleSchema, ruleLiteralDefault } from '../src/rule-schema'

it('clears optional text values without dropping meaningful literal data', () => {
  expect(optionalRuleText({ type: 'template', text: '' })).toBeNull()
  expect(optionalRuleText({ type: 'literal', value: '' })).toBeNull()
  expect(optionalRuleText({ type: 'literal', value: null })).toBeNull()
  for (const value of [
    { type: 'template', text: ' ' },
    { type: 'literal', value: 'alice' },
    { type: 'literal', value: ' ' },
    { type: 'literal', value: 0 },
    { type: 'formula', expression: 'payload.author.id' },
  ] as const)
    expect(optionalRuleText(value)).toEqual(value)
})

const action: RuleNode = {
  type: 'action',
  id: 'remember',
  action: {
    type: 'storeValue',
    scope: 'rule',
    name: 'last',
    value: { type: 'literal', value: { text: 'équipe {{name}}', flags: [true, false], missing: null } },
  },
}

function definition(): HookRuleDefinition {
  return {
    name: 'Support',
    description: '',
    trigger: { type: 'cairn', hook: 'replyReady' },
    executionAgentId: '00000000-0000-4000-8000-000000000001',
    limits: { triggeringsPerHour: 60, conversationsPerHour: 5 },
    steps: [{
      type: 'if',
      id: 'trusted',
      condition: { type: 'all', conditions: [] },
      then: [],
      otherwise: [],
    }],
  }
}

describe('visual rule editing', () => {
  it('inserts and edits a Vue reactive branch while preserving the original and wire values', () => {
    const original = reactive(definition())
    const inserted = withInserted(original, '/steps/0/then', 0, action)
    expect(original.steps[0].type === 'if' && original.steps[0].then).toEqual([])
    expect(nodeAt(inserted, '/steps/0/then/0')).toEqual(action)
    const renamed = withReplaced(reactive(inserted), '/steps/0/then/0', { ...action, id: 'renamed' })
    expect(nodeAt(renamed, '/steps/0/then/0')?.id).toBe('renamed')
    expect(nodeAt(inserted, '/steps/0/then/0')?.id).toBe('remember')
    expect(nodeAt(renamed, '/steps/0/then/0')?.type === 'action' && (nodeAt(renamed, '/steps/0/then/0') as Extract<RuleNode, { type: 'action' }>).action).toEqual(action.action)
  })
  it('moves and removes a nested step without flattening either branch', () => {
    const first = withInserted(reactive(definition()), '/steps/0/then', 0, action)
    const second = withInserted(reactive(first), '/steps/0/then', 1, { ...action, id: 'second' })
    const moved = withMoved(reactive(second), '/steps/0/then/1', -1)
    expect(nodeAt(moved, '/steps/0/then/0')?.id).toBe('second')
    expect(pathOfNode(moved, 'remember')).toBe('/steps/0/then/1')
    expect(nodeAt(moved, '/steps/0/then/1')?.id).toBe('remember')
    const removed = withRemoved(reactive(moved), '/steps/0/then/0')
    expect(nodeAt(removed, '/steps/0/then/0')?.id).toBe('remember')
    expect(removed.steps[0].type === 'if' && removed.steps[0].otherwise).toEqual([])
  })
  it('offers scoped results from earlier steps without leaking the sibling branch', () => {
    const tree = withInserted(withInserted(definition(), '/steps/0/then', 0, action), '/steps/0/otherwise', 0, { ...action, id: 'fallback' })
    const after = withInserted(tree, '/steps', 1, { ...action, id: 'after' })
    expect(variablesAt(after, '/steps/0/otherwise/0', null).map(value => value.expression)).not.toContain('steps.remember')
    expect(variablesAt(after, '/steps/1', null).map(value => value.expression)).toEqual(expect.arrayContaining(['steps.remember', 'steps.fallback']))
  })
  it('gives nested loops distinct item names and offers each scoped variable once', () => {
    const first = withInserted(definition(), '/steps', 1, newStep(definition(), 'forEach', null))
    const inner = newStep(first, 'forEach', null)
    const nested = withInserted(first, '/steps/1/body', 0, inner)
    expect(first.steps[1].type === 'forEach' && first.steps[1].itemName).toBe('item')
    expect(inner.type === 'forEach' && inner.itemName).toBe('item2')
    const legacy = structuredClone(nested)
    if (legacy.steps[1].type === 'forEach' && legacy.steps[1].body[0].type === 'forEach')
      legacy.steps[1].body[0].itemName = 'item'
    expect(variablesAt(legacy, '/steps/1/body/0/body/0', null).filter(value => value.expression === 'item')).toHaveLength(1)
  })
  it('uses numbers for ordering a result or formula whose type is unknown', () => {
    for (const expression of ['steps.fetch.count', 'size(payload.items)']) {
      for (const operator of ['greaterThan', 'lessThan'] as const)
        expect(comparisonExpects({ type: 'formula', expression }, operator, [])).toBe('number')
    }
  })
  it('resolves input definitions and typed enum defaults for catalog-driven forms', () => {
    const root = { $defs: { options: { type: ['object', 'null'], properties: { notify: { type: 'boolean' } } } } }
    expect(resolveRuleSchema({ $ref: '#/$defs/options' }, root)).toEqual({ type: 'object', properties: { notify: { type: 'boolean' } } })
    expect(ruleLiteralDefault({ type: 'integer', enum: [5, 10] })).toBe(5)
    expect(ruleLiteralDefault({ type: 'array' })).toEqual([])
    expect(ruleLiteralDefault({ type: 'object', required: ['options'], properties: { options: { type: 'object', required: ['notify'], properties: { notify: { type: 'boolean' } } } } })).toEqual({ options: { notify: false } })
  })
  it('types fixed comparison values from the chosen payload field', () => {
    const variables = [{
      group: 'Event' as const,
      expression: 'payload.count',
      label: 'count',
      type: 'integer',
      description: '',
    }, {
      group: 'Event' as const,
      expression: 'payload.approved',
      label: 'approved',
      type: 'boolean',
      description: '',
    }]
    expect(comparisonExpects({ type: 'formula', expression: 'payload.count' }, 'greaterThan', variables)).toBe('number')
    expect(comparisonExpects({ type: 'formula', expression: 'approved' }, 'equals', variables)).toBe('boolean')
    expect(typedComparisonRight({ type: 'template', text: '5' }, 'number')).toEqual({ type: 'literal', value: 5 })
    expect(typedComparisonRight({ type: 'template', text: 'false' }, 'boolean')).toEqual({ type: 'literal', value: false })
    expect(typedComparisonRight({ type: 'template', text: '{{payload.count}}' }, 'number')).toEqual({ type: 'formula', expression: 'payload.count' })
  })
})

describe('rule definition recovery and condition depth', () => {
  it('converts old numeric, boolean and list literals when a comparison becomes text', () => {
    for (const [value, text] of [[5, '5'], [true, 'true'], [['alice', 'bob'], '["alice","bob"]']] as const)
      expect(typedComparisonRight({ type: 'literal', value }, 'text')).toEqual({ type: 'literal', value: text })
  })

  it('compares client drafts without depending on property order and retains null data', () => {
    const original: HookRuleDefinition = { ...definition(), steps: [action] }
    const reordered = Object.fromEntries(Object.entries(original).reverse()) as HookRuleDefinition
    expect(sameRuleDefinition(original, reordered)).toBe(true)
    const optional = JSON.parse(JSON.stringify(original)) as HookRuleDefinition
    if (optional.steps[0].type === 'action' && optional.steps[0].action.type === 'storeValue')
      optional.steps[0].action.key = null
    optional.steps[0] = { ...optional.steps[0], label: '' } as RuleNode
    expect(sameRuleDefinition(original, optional)).toBe(true)
    const changed = JSON.parse(JSON.stringify(original)) as HookRuleDefinition
    if (changed.steps[0].type === 'action' && changed.steps[0].action.type === 'storeValue' && changed.steps[0].action.value.type === 'literal')
      delete (changed.steps[0].action.value.value as any).missing
    expect(sameRuleDefinition(original, changed)).toBe(false)
  })

  it('counts negation separately from each group and leaf', () => {
    let condition: any = { type: 'formula', expression: 'true' }
    for (let depth = 0; depth < 3; depth++)
      condition = { type: 'all', conditions: [condition] }
    expect(ruleConditionDepth({ type: 'all', conditions: [{ type: 'not', condition }] })).toBe(6)
  })
})

it('selects an author string identifier instead of the enclosing object', () => {
  const fields = schemaFields({ type: 'object', properties: { author: { type: 'object', properties: { id: { type: 'string' }, name: { type: 'string' } } } } }, 'payload')
  expect(authorVariable(fields)).toBe('payload.author.id')
  expect(authorVariable([...fields, {
    group: 'Event',
    expression: 'payload.sender',
    label: 'sender',
    type: 'string',
    description: '',
    personId: true,
  }])).toBe('payload.sender')
  expect(authorVariable(fields.filter(field => field.type === 'object'))).toBe('')
})

it('preserves numeric boolean and object list members in contains comparisons', () => {
  const fields = schemaFields({ type: 'object', properties: { counts: { type: 'array', items: { type: 'integer' } }, flags: { type: 'array', items: { type: 'boolean' } }, records: { type: 'array', items: { type: 'object' } } } }, 'payload')
  for (const expression of ['payload.counts', 'counts'])
    expect(comparisonExpects({ type: 'formula', expression }, 'contains', fields)).toBe('number')
  expect(comparisonExpects({ type: 'literal', value: [5] }, 'contains', [])).toBe('number')
  expect(comparisonExpects({ type: 'formula', expression: 'payload.flags' }, 'contains', fields)).toBe('boolean')
  expect(comparisonExpects({ type: 'formula', expression: 'payload.records' }, 'contains', fields)).toBe('json')
  expect(typedComparisonRight({ type: 'literal', value: 5 }, 'json', false)).toEqual({ type: 'literal', value: 5 })
  expect(typedComparisonRight({ type: 'literal', value: { id: 5 } }, 'json', false)).toEqual({ type: 'literal', value: { id: 5 } })
})

it('keeps equality operands and membership elements structured', () => {
  const fields = schemaFields({ type: 'object', properties: { items: { type: 'array' }, meta: { type: 'object' } } }, 'payload')
  expect(comparisonListSchema({ type: 'formula', expression: 'payload.meta' }, fields)).toEqual({ type: 'array', items: { type: 'object' } })
  expect(comparisonListSchema({ type: 'formula', expression: 'payload.items' }, fields)).toEqual({ type: 'array', items: { type: 'array' } })
  for (const [name, value] of [['items', [1, 2]], ['meta', { ok: true }]] as const) {
    for (const operator of ['equals', 'notEquals'] as const) {
      const expects = comparisonExpects({ type: 'formula', expression: `payload.${name}` }, operator, fields)
      expect(expects).toBe('json')
      expect(typedComparisonRight({ type: 'literal', value }, expects, false)).toEqual({ type: 'literal', value })
    }
  }
})

it('keeps unknown formula equality operands typed and retains nested membership schemas', () => {
  for (const expression of ['size(payload.items)', 'steps.fetch.count']) {
    for (const operator of ['equals', 'notEquals'] as const) {
      const expects = comparisonExpects({ type: 'formula', expression }, operator, [])
      expect(expects).toBe('json')
      expect(typedComparisonRight({ type: 'literal', value: 3 }, expects, false)).toEqual({ type: 'literal', value: 3 })
    }
  }

  const counts = { type: 'array', items: { type: 'integer' } }
  const fields = schemaFields({ type: 'object', properties: { counts } }, 'payload')
  expect(comparisonListSchema({ type: 'formula', expression: 'payload.counts' }, fields)).toEqual({ type: 'array', items: counts })
  expect(comparisonListSchema({ type: 'literal', value: [1, 2] }, [])).toEqual({ type: 'array', items: { type: 'array', items: { type: 'number' } } })
})

it('preserves right templates while editing an equality field or its operator', () => {
  const fields = schemaFields({ properties: { channel: { type: 'string' } } }, 'payload')
  for (const text of ['support-{{payload.author}}', '{{payload.author}}', '3']) {
    let right: any = { type: 'template', text }
    for (const expression of ['payload.channel', 'payload.channe', 'payload.channel', '"support-" + payload.author']) {
      for (const operator of ['equals', 'notEquals'] as const) {
        right = typedComparisonRight(right, comparisonExpects({ type: 'formula', expression }, operator, fields), false)
        expect(right).toEqual({ type: 'template', text })
      }
    }
  }
})
