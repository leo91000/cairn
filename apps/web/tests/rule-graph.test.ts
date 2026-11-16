import type { RuleNode } from '../../../packages/contracts/generated/typescript/RuleNode'
import { expect, it } from 'vitest'
import { ruleGraphPorts, ruleGraphWidth } from '../src/rule-graph'

it('keeps one-sided nesting linear rather than doubling empty branch space', () => {
  let nodes: RuleNode[] = []
  for (let depth = 0; depth < 7; depth++) {
    nodes = [{
      type: 'if',
      id: `choice-${depth}`,
      condition: { type: 'all', conditions: [] },
      then: nodes,
      otherwise: [],
    }]
    expect(ruleGraphWidth(nodes)).toBe(240 + (depth + 1) * 304)
  }
})

it('connects asymmetric branches to their actual centers and reserves the branch gutter', () => {
  const left = 848
  const right = 240
  const ports = ruleGraphPorts(left, right)
  const total = left + 64 + right
  expect(ports.left * total / 100).toBeCloseTo(left / 2)
  expect(ports.right * total / 100).toBeCloseTo(left + 64 + right / 2)
  expect(ports.right * total / 100 - right / 2 - left).toBeCloseTo(64)
})
