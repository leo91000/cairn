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
  grantableConnection,
  grantablePackages,
  issues,
  newAgentDraft,
  rebaseDraft,
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
  expect(saved.access).toMatchObject({ nodes: [LOCAL_NODE_ID] })
  expect(saved).toMatchObject({ exclusiveNodes: [], plugins: [] })
  const draft = structuredClone(saved)
  draft.name = 'Renamed'
  draft.access.projects = ['p1']
  draft.exclusiveNodes = ['gpu']
  draft.plugins = ['prod']
  expect(changes(saved, draft)).toEqual(['identity', 'projects', 'nodes', 'plugins'])
})

it('asks for a review when a power or a secret is added, and explains reductions', () => {
  const saved = newAgentDraft()
  saved.access.projects = ['p1', 'p2']
  saved.access.nodes = [LOCAL_NODE_ID, 'gpu']
  const draft = structuredClone(saved)
  draft.exclusiveNodes = ['gpu']
  draft.plugins = ['prod']
  draft.access.projects = ['p2']
  const lines = reviewLines(saved, draft, context, { queued: 2, running: 1 })
  expect(lines.map(line => line.kind)).toEqual(['power', 'secret', 'reduction'])
  expect(lines[0]!.text).toContain('exclusive reservation on gpu-box')
  expect(lines[1]!.text).toBe('Added: 1Password “Production” — effective immediately, including in running conversations.')
  expect(lines[2]!.text).toBe('Removed: project SheetOM. 2 queued runs and 1 running conversation will not continue with the old access.')
})

it('reports grants that cannot be used', () => {
  const draft = newAgentDraft()
  draft.exclusiveNodes = ['gpu']
  draft.plugins = ['gone']
  expect(issues(draft, context, false).map(issue => issue.section)).toEqual(['nodes', 'plugins'])
})

it('summarizes an agent for the list and places server errors in their section', () => {
  const agent = {
    id: 'a',
    name: 'Agent',
    access: { projects: ['p1', 'p2', 'p3'], nodes: [LOCAL_NODE_ID, 'gpu'] },
    plugins: ['prod'],
  } as unknown as Agent
  expect(accessSummary(agent, context)).toBe('3 projects · 2 nodes · 1Password')
  expect(errorSection('A selected plugin instance no longer exists.')).toBe('plugins')
  expect(errorSection('A revoked node cannot be reserved.')).toBe('nodes')
})

it('starts new agents restricted until the owner widens their access', () => {
  expect(newAgentDraft().access).toEqual({
    nodes: [LOCAL_NODE_ID],
    projects: [],
    skills: [],
    mcps: [],
    mcpTools: {},
    installation: false,
    github: false,
    sandbox: 'yolo',
  })
  expect(newAgentDraft()).toMatchObject({ exclusiveNodes: [], plugins: [] })
  expect(issues(newAgentDraft(), context, false)).toEqual([])
  expect(reviewLines(newAgentDraft(), newAgentDraft(), context)).toEqual([])
})

it('treats live grants the server omits as none, so a loaded agent is not changed', () => {
  const agent = {
    id: 'a',
    name: 'Agent',
    access: {
      nodes: [LOCAL_NODE_ID],
      projects: null,
      skills: null,
      mcps: null,
      github: true,
      installation: false,
      sandbox: 'yolo',
    },
  } as unknown as Agent
  const loaded = draftOf(agent)
  expect(loaded.access).toMatchObject({ mcpTools: {} })
  expect(loaded).toMatchObject({ exclusiveNodes: [], plugins: [] })
  expect(changes(loaded, draftOf(agent))).toEqual([])
  // A draft compared against the raw response, keys absent, is unchanged too.
  const { exclusiveNodes: _nodes, plugins: _plugins, ...omitted } = loaded
  const raw = { ...omitted, access: agent.access } as typeof loaded
  expect(changes(raw, loaded)).toEqual([])
  expect(reviewLines(raw, loaded, context)).toEqual([])
  expect(issues(raw, context, false)).toEqual([])
  expect(accessSummary(agent, context)).toBe('All projects · 1 node')
})

it('rebases the owner\'s edits onto a newer agent, like Android', () => {
  const base = draftOf({ id: 'a', name: 'Docs writer', access: { projects: ['p1'], mcps: null, mcpTools: {} } } as unknown as Agent)
  const mine = structuredClone(base)
  mine.description = 'Keeps the documentation current'
  mine.access.mcpTools = { m1: ['echo'] }
  const theirs = structuredClone(base)
  theirs.name = 'Documentation writer'
  theirs.description = 'Changed elsewhere'
  theirs.access.projects = ['p1', 'p2']
  theirs.plugins = ['prod']
  const rebased = rebaseDraft(base, mine, theirs)
  // Edited values keep the draft; the others take the newer agent.
  expect(rebased.description).toBe('Keeps the documentation current')
  expect(rebased.access.mcpTools).toEqual({ m1: ['echo'] })
  expect(rebased.name).toBe('Documentation writer')
  expect(rebased.access.projects).toEqual(['p1', 'p2'])
  expect(rebased.plugins).toEqual(['prod'])
  expect(changes(theirs, rebased)).toEqual(['identity', 'mcps'])
  // The result is independent of its inputs.
  rebased.access.projects!.push('p3')
  expect(theirs.access.projects).toEqual(['p1', 'p2'])
})

it('keeps the live grants out of the access policy sent to the server', () => {
  const agent = {
    id: 'a',
    name: 'Agent',
    access: { nodes: [LOCAL_NODE_ID, 'gpu'], projects: null },
    exclusiveNodes: ['gpu'],
    plugins: ['prod'],
  } as unknown as Agent
  const draft = draftOf(agent)
  expect(draft).toMatchObject({ exclusiveNodes: ['gpu'], plugins: ['prod'] })
  expect(draft.access).not.toHaveProperty('exclusiveNodes')
  expect(draft.access).not.toHaveProperty('plugins')
  // A grant left inside an older response's access is not a live grant.
  const legacy = draftOf({ id: 'a', name: 'Agent', access: { exclusiveNodes: ['gpu'], plugins: ['prod'] } } as unknown as Agent)
  expect(legacy).toMatchObject({ exclusiveNodes: [], plugins: [] })
  expect(accessSummary(agent, context)).toBe('All projects · 2 nodes · 1Password · Exclusive reservation')

  // A grant on another agent's page or window, made meanwhile, is kept when rebasing.
  const mine = structuredClone(draft)
  mine.exclusiveNodes = []
  const theirs = structuredClone(draft)
  theirs.plugins = ['prod', 'staging']
  const rebased = rebaseDraft(draft, mine, theirs)
  expect(rebased).toMatchObject({ exclusiveNodes: [], plugins: ['prod', 'staging'] })
  expect(changes(theirs, rebased)).toEqual(['nodes'])
})

it('reports a held grant whose plugin cannot give agents access any more', () => {
  const review = {
    ...onePassword,
    id: 'tickets',
    builtin: false,
    status: 'needs_approval',
    manifest: { id: 'tickets', name: 'Tickets', agentAccess: {} },
  } as unknown as PluginEntry
  const broken = {
    ...onePassword,
    id: 'broken',
    builtin: false,
    status: 'invalid',
    manifest: null,
  } as unknown as PluginEntry
  const withdrawn = {
    ...onePassword,
    id: 'notes',
    builtin: false,
    manifest: { id: 'notes', name: 'Notes' },
  } as unknown as PluginEntry
  const instance = (id: string, pluginId: string) => ({
    ...production,
    id,
    pluginId,
    name: id,
  }) as PluginConnection
  const held: Context = {
    ...context,
    packages: [onePassword, review, broken, withdrawn],
    connections: [production, instance('queue', 'tickets'), instance('old', 'broken'), instance('memo', 'notes')],
  }
  expect(grantablePackages(held.packages).map(entry => entry.id)).toEqual(['onepassword'])
  expect(grantableConnection(production, held.packages)).toBe(true)
  expect(grantableConnection(instance('memo', 'notes'), held.packages)).toBe(false)

  const draft = newAgentDraft()
  draft.plugins = ['prod', 'queue', 'old', 'memo']
  const found = issues(draft, held, false)
  expect(found.map(issue => issue.section)).toEqual(['plugins', 'plugins', 'plugins'])
  expect(found[0]!.text).toBe('Tickets “queue” is unavailable: its plugin needs review or no longer gives agents access.')
  expect(found[1]!.text).toBe('Plugin “old” is unavailable: its plugin needs review or no longer gives agents access.')
})
