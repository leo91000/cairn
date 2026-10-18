import type { Agent } from '../../../packages/contracts/contracts'
import type { ExecutionNode } from '../../../packages/contracts/nodes'
import type { Context } from '../src/agent-access'
import type { PluginConnection, PluginEntry } from '../src/plugins'
import { expect, it } from 'vitest'
import { LOCAL_NODE_ID } from '../../../packages/contracts/nodes'
import {
  accessSummary,
  changes,
  draftOf,
  errorSection,
  issues,
  newAgentDraft,
  reviewLines,
  selectionSummary,
  skillLabel,
} from '../src/agent-access'

const gpu = {
  id: 'gpu',
  name: 'gpu-box',
  local: false,
  revoked: false,
  status: 'online',
  accepting: true,
  exclusiveReservations: true,
  tags: [],
  capabilities: {
    cpu: 8,
    memoryMiB: 1024,
    diskMiB: 1024,
    os: 'linux',
    arch: 'x86_64',
    kvm: true,
  },
  limits: { cpu: 8, memoryMiB: 1024, diskMiB: 1024 },
  slots: 2,
  runtimeId: 'r',
  lastSeen: 0,
} as ExecutionNode
const onePassword = {
  id: 'onepassword',
  builtin: true,
  status: 'active',
  active: '1',
  versions: [],
  manifest: { id: 'onepassword', name: '1Password', agentAccess: {} },
} as unknown as PluginEntry
const production = {
  id: 'prod',
  pluginId: 'onepassword',
  name: 'Production',
  enabled: true,
  status: { state: 'ready' },
} as PluginConnection
const context: Context = {
  projects: [{ id: 'p1', name: 'SheetOM' }, { id: 'p2', name: 'cairn' }],
  skills: [],
  mcps: [],
  nodes: [gpu],
  packages: [onePassword],
  connections: [production],
}

it('summarizes selections the same way everywhere', () => {
  expect(selectionSummary(null, 12)).toBe('All, including future')
  expect(selectionSummary([], 12)).toBe('None')
  expect(selectionSummary(['a', 'b', 'c'], 12)).toBe('3 of 12')
})

it('labels skills by name and origin, never by their key', () => {
  expect(skillLabel('global/review', context.projects)).toEqual({ name: 'review', origin: 'Global' })
  expect(skillLabel('p2/lint', context.projects)).toEqual({ name: 'lint', origin: 'cairn' })
})

it('fills the live grants older agents omit and counts every changed value', () => {
  const saved = draftOf({ id: 'a', name: 'Agent', access: { projects: null } } as unknown as Agent)
  expect(saved.access).toMatchObject({ nodes: [LOCAL_NODE_ID], exclusiveNodes: [], plugins: [] })
  const draft = structuredClone(saved)
  draft.name = 'Renamed'
  draft.access.projects = ['p1']
  draft.access.exclusiveNodes = ['gpu']
  draft.access.plugins = ['prod']
  expect(changes(saved, draft)).toEqual(['identity', 'projects', 'nodes', 'plugins'])
})

it('asks for a review when a power or a secret is added, and explains reductions', () => {
  const saved = newAgentDraft()
  saved.access.projects = ['p1', 'p2']
  saved.access.nodes = [LOCAL_NODE_ID, 'gpu']
  const draft = structuredClone(saved)
  draft.access.exclusiveNodes = ['gpu']
  draft.access.plugins = ['prod']
  draft.access.projects = ['p2']
  const lines = reviewLines(saved, draft, context, { queued: 2, running: 1 })
  expect(lines.map(line => line.kind)).toEqual(['power', 'secret', 'reduction'])
  expect(lines[0]!.text).toContain('exclusive reservation on gpu-box')
  expect(lines[1]!.text).toBe('Added: 1Password “Production” — effective immediately, including in running conversations.')
  expect(lines[2]!.text).toBe('Removed: project SheetOM. 2 queued runs and 1 running conversation will not continue with the old access.')
})

it('reports grants that cannot be used', () => {
  const draft = newAgentDraft()
  draft.access.exclusiveNodes = ['gpu']
  draft.access.plugins = ['gone']
  expect(issues(draft, context, false).map(issue => issue.section)).toEqual(['nodes', 'plugins'])
})

it('summarizes an agent for the list and places server errors in their section', () => {
  const agent = { id: 'a', name: 'Agent', access: { projects: ['p1', 'p2', 'p3'], nodes: [LOCAL_NODE_ID, 'gpu'], plugins: ['prod'] } } as unknown as Agent
  expect(accessSummary(agent, context)).toBe('3 projects · 2 nodes · 1Password')
  expect(errorSection('A selected plugin instance no longer exists.')).toBe('plugins')
  expect(errorSection('A revoked node cannot be reserved.')).toBe('nodes')
})
