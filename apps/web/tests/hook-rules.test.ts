import type { HookRuleDefinition } from '../../../packages/contracts/generated/typescript/HookRuleDefinition'
import type { RuleNode } from '../../../packages/contracts/generated/typescript/RuleNode'
import { describe, expect, it } from 'vitest'
import { reactive } from 'vue'
import {
  nodeAt,
  pathOfNode,
  variablesAt,
  withInserted,
  withMoved,
  withRemoved,
  withReplaced,
} from '../src/hook-rules'
import { resolveRuleSchema, ruleLiteralDefault } from '../src/rule-schema'

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
  it('resolves input definitions and typed enum defaults for catalog-driven forms', () => {
    const root = { $defs: { options: { type: ['object', 'null'], properties: { notify: { type: 'boolean' } } } } }
    expect(resolveRuleSchema({ $ref: '#/$defs/options' }, root)).toEqual({ type: 'object', properties: { notify: { type: 'boolean' } } })
    expect(ruleLiteralDefault({ type: 'integer', enum: [5, 10] })).toBe(5)
    expect(ruleLiteralDefault({ type: 'array' })).toEqual([])
    expect(ruleLiteralDefault({ type: 'object', required: ['options'], properties: { options: { type: 'object', required: ['notify'], properties: { notify: { type: 'boolean' } } } } })).toEqual({ options: { notify: false } })
  })
})
