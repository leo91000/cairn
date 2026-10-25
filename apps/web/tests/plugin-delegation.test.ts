import type { Agent } from '../../../packages/contracts/contracts'
import { expect, it } from 'vitest'
import { delegationRisks, listed } from '../src/plugins'

function agent(access: Partial<Agent['access']>): Agent {
  return {
    id: 'helper',
    name: 'Helper',
    access: {
      projects: [],
      mcps: [],
      installation: false,
      sandbox: 'read-only',
      ...access,
    },
  } as Agent
}

it('warns about broad agents for listed people without refusing any', () => {
  expect(delegationRisks(agent({}), [], false)).toEqual([])
  expect(delegationRisks(agent({ sandbox: 'yolo', mcps: null }), [], false)).toEqual([])
  expect(delegationRisks(agent({ installation: true, projects: null }), [], false))
    .toEqual(['manage your whole installation', 'open every project'])
  expect(delegationRisks(agent({ projects: ['a', 'b'] }), [], false)).toEqual(['open 2 projects'])
})

it('also warns about writing, MCP and plugin access for anyone in a channel', () => {
  expect(delegationRisks(agent({}), [], true)).toEqual([])
  expect(delegationRisks(agent({ sandbox: 'yolo', mcps: null }), [{ agentIds: ['helper'] }], true)).toEqual([
    'change files and run commands',
    'use its MCP connections',
    'use its plugins, such as GitHub or 1Password',
  ])
})

it('lists risks as a sentence', () => {
  expect(listed([])).toBe('')
  expect(listed(['a'])).toBe('a')
  expect(listed(['a', 'b', 'c'])).toBe('a, b and c')
})

it('warns listed and public audiences about delegation to other coding agents', () => {
  for (const everyone of [false, true]) {
    expect(delegationRisks(agent({ delegation: true }), [], everyone))
      .toEqual(['delegate work to other coding agents and models'])
  }
})

it('warns about grants to the edited instance and unsaved instance', () => {
  const helper = agent({ projects: ['project'] })
  expect(delegationRisks(helper, [{ agentIds: ['helper'] }], true))
    .toEqual(['use its plugins, such as GitHub or 1Password'])
  expect(delegationRisks(helper, [], true, ['helper']))
    .toEqual(['use its plugins, such as GitHub or 1Password'])
  expect(delegationRisks(helper, [], true, ['other'])).toEqual([])
})

it('includes scoped resource grants from compact access without an instance grant', () => {
  const helper = agent({})
  expect(delegationRisks(helper, [{ agentIds: [], resourceAgentIds: ['helper'] }], true))
    .toEqual(['use its plugins, such as GitHub or 1Password'])
  expect(delegationRisks(helper, [{ agentIds: [], resourceAgentIds: [] }], true)).toEqual([])
  expect(delegationRisks(helper, [{ agentIds: [], resourceAgentIds: ['helper'] }], false)).toEqual([])
})
