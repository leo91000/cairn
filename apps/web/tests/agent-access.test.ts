import type { Agent } from '../../../packages/contracts/contracts'
import type { ExecutionNode } from '../../../packages/contracts/nodes'
import type { Context } from '../src/agent-access'
import type { PluginConnection, PluginEntry } from '../src/plugins'
import { expect, it } from 'vitest'
import { LOCAL_NODE_ID } from '../../../packages/contracts/nodes'
import {
  accessSummary,
  backendOf,
  changes,
  draftOf,
  effectiveExclusive,
  errorSection,
  grantableConnection,
  grantablePackages,
  issues,
  mergeList,
  nativeReviewNodes,
  needsReview,
  newAgentDraft,
  nodesFor,
  rebaseDraft,
  reviewLines,
  sandboxOptionsFor,
  savedDraft,
  selectionSummary,
  skillLabel,
  unknownNode,
  withBackend,
  withRun,
} from '../src/agent-access'
import { agentPluginPackage } from '../src/agent-plugin-directory'

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
  packages: [agentPluginPackage(onePassword)],
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
  // Two instances of a package whose runs use its credentials: GitHub accounts are plugin grants.
  expect(errorSection('Grant one GitHub instance per agent: its runs use that instance\'s credentials.')).toBe('plugins')
  expect(errorSection('A revoked node cannot be reserved.')).toBe('nodes')
})

it('starts new agents restricted until the owner widens their access', () => {
  expect(newAgentDraft().access).toEqual({
    backend: 'firecracker',
    nodes: [LOCAL_NODE_ID],
    projects: [],
    skills: [],
    mcps: [],
    mcpTools: {},
    installation: false,
    delegation: false,
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
      // An older installation may still send the removed shared GitHub setting.
      github: true,
      installation: false,
      sandbox: 'yolo',
    },
  } as unknown as Agent
  const loaded = draftOf(agent)
  expect(loaded.access).toMatchObject({ mcpTools: {} })
  expect(loaded.access).not.toHaveProperty('github')
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
    packages: [onePassword, review, broken, withdrawn].map(agentPluginPackage),
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
  expect(found[1]!.text).toBe('broken “old” is unavailable: its plugin needs review or no longer gives agents access.')
})

it('lists every reduction the Android review lists', () => {
  const mcpContext: Context = { ...context, mcps: [{ id: 'm1', name: 'Design tools' }, { id: 'm2', name: 'Docs' }] as Context['mcps'] }
  const saved = newAgentDraft()
  saved.access.mcps = ['m1', 'm2']
  saved.access.mcpTools = { m2: ['read', 'write'] }
  saved.access.projects = null
  const draft = structuredClone(saved)
  // Stricter execution mode.
  draft.access.sandbox = 'workspace-write'
  // From all enabled tools to a chosen few, and a tool removed from a chosen list.
  draft.access.mcpTools = { m1: ['echo'], m2: ['read'] }
  draft.access.projects = ['p1']
  const lines = reviewLines(saved, draft, mcpContext)
  expect(lines.map(line => line.kind)).toEqual(['reduction'])
  expect(lines[0]!.text).toBe('Removed: all projects (now SheetOM), MCP tools on Design tools (now 1 chosen), MCP tools on Docs (now 1 chosen), execution mode (now Workspace write).')

  // A looser mode, an added tool or a connection granted with chosen tools reduces nothing.
  const looser = structuredClone(draft)
  const wider = structuredClone(looser)
  wider.access.sandbox = 'yolo'
  wider.access.mcpTools = { m1: ['echo', 'search'], m2: ['read', 'write'], m3: [] }
  wider.access.mcps = ['m1', 'm2', 'm3']
  expect(reviewLines(looser, wider, mcpContext)).toEqual([])
  const readOnly = structuredClone(wider)
  readOnly.access.sandbox = 'read-only'
  expect(reviewLines(wider, readOnly, mcpContext)[0]!.text).toBe('Removed: execution mode (now Read only).')
})

it('lists every addition and removal of powers and grants', () => {
  const saved = newAgentDraft()
  saved.access.nodes = [LOCAL_NODE_ID, 'gpu']
  const granted = structuredClone(saved)
  granted.exclusiveNodes = ['gpu']
  granted.plugins = ['prod']
  granted.access.installation = true
  expect(reviewLines(saved, granted, context).map(line => line.text)).toEqual([
    'Added: exclusive reservation on gpu-box. This agent will be able to suspend other agents’ conversations on this machine.',
    'Added: installation management. This agent will be able to create and change agents, projects, skills, MCP connections, missions and runs on your behalf.',
    'Added: 1Password “Production” — effective immediately, including in running conversations.',
  ])
  const revoked = structuredClone(granted)
  revoked.exclusiveNodes = []
  revoked.plugins = []
  revoked.access.installation = false
  revoked.access.nodes = [LOCAL_NODE_ID]
  expect(reviewLines(granted, revoked, context).map(line => line.text)).toEqual([
    'Removed: node gpu-box, exclusive reservation on gpu-box, 1Password “Production”, installation management.',
  ])
})

it('reviews task delegation as a power and lists its removal', () => {
  const saved = newAgentDraft()
  const granted = structuredClone(saved)
  granted.access.delegation = true
  expect(changes(saved, granted)).toEqual(['powers'])
  const added = reviewLines(saved, granted, context)
  expect(added).toEqual([expect.objectContaining({ kind: 'power', section: 'powers' })])
  expect(added[0]!.text).toMatch(/^Added: task delegation\./)
  expect(needsReview(added, null)).toBe(true)
  const revoked = structuredClone(granted)
  revoked.access.delegation = false
  expect(reviewLines(granted, revoked, context).map(line => line.text)).toEqual(['Removed: task delegation.'])
})

it('treats an exclusive reservation on a runtime without support as inactive', () => {
  const old = {
    ...gpu,
    id: 'old',
    name: 'old-box',
    exclusiveReservations: false,
  } as ExecutionNode
  const nodes = [gpu, old]
  const draft = newAgentDraft()
  draft.access.nodes = ['gpu', 'old']
  draft.exclusiveNodes = ['gpu', 'old']
  expect(effectiveExclusive(draft, nodes)).toEqual(['gpu'])
  expect(issues(draft, { ...context, nodes }, false).map(issue => issue.text)).toEqual([
    'The exclusive reservation on old-box is inactive: its runtime must be updated for exclusive reservations.',
  ])
  draft.exclusiveNodes = ['old']
  expect(effectiveExclusive(draft, nodes)).toEqual([])
  const agent = {
    id: 'a',
    name: 'Agent',
    access: draft.access,
    exclusiveNodes: ['old'],
  } as unknown as Agent
  expect(accessSummary(agent, { ...context, nodes })).toBe('No projects · 2 nodes')
})

it('raises no plugin issue while the plugin catalogue is not loaded', () => {
  const draft = newAgentDraft()
  draft.plugins = ['prod', 'gone']
  expect(issues(draft, {
    ...context,
    connections: [],
    packages: [],
    pluginsUnavailable: true,
  }, false)).toEqual([])
  expect(issues(draft, context, false).map(issue => issue.text)).toEqual(['A granted plugin instance no longer exists.'])
})

it('lists a delegation answering as the agent that waits for reapproval, as Android does', () => {
  const bot = {
    ...production,
    id: 'bot',
    pluginId: 'discord',
    name: '#support on Acme',
    agentId: 'a',
    status: { state: 'needs_review' },
  } as PluginConnection
  const withBot = { ...context, connections: [production, bot] }
  expect(issues(newAgentDraft(), withBot, false, 'a').map(issue => issue.text)).toEqual(['#support on Acme is suspended until it is reapproved.'])
  expect(issues(newAgentDraft(), withBot, false, 'other')).toEqual([])
  expect(issues(newAgentDraft(), { ...withBot, connections: [{ ...bot, enabled: false }] }, false, 'a')).toEqual([])
})

it('compares MCP tool choices as sets', () => {
  const saved = newAgentDraft()
  saved.access.mcps = ['m1']
  saved.access.mcpTools = { m1: ['echo', 'search'] }
  const draft = structuredClone(saved)
  draft.access.mcpTools = { m1: ['search', 'echo'] }
  expect(changes(saved, draft)).toEqual([])
  draft.access.mcpTools = { m1: ['search'] }
  expect(changes(saved, draft)).toEqual(['mcps'])
})

it('opens the review only for what this edit does', () => {
  const reduction = [{ kind: 'reduction' as const, section: 'projects' as const, text: 'Removed: project SheetOM.' }]
  const delegation = {
    id: 'bot',
    pluginId: 'discord',
    name: 'Bot',
    enabled: true,
    suspended: true,
    blocked: null,
    alreadySuspended: true,
  }
  const preview = { runs: { queued: 0, running: 0 }, delegations: [delegation] }
  expect(needsReview(reduction, null)).toBe(false)
  // Already suspended by an earlier change: listed if the review opens, but it does not open it.
  expect(needsReview(reduction, preview)).toBe(false)
  expect(needsReview(reduction, { ...preview, delegations: [{ ...delegation, alreadySuspended: false }] })).toBe(true)
  // Blocked or not, as on Android.
  expect(needsReview(reduction, { ...preview, delegations: [{ ...delegation, blocked: 'Too broad' }] })).toBe(false)
  expect(needsReview(reduction, { ...preview, delegations: [{ ...delegation, blocked: 'Too broad', alreadySuspended: false }] })).toBe(true)
  expect(needsReview(reduction, { ...preview, runs: { queued: 1, running: 0 } })).toBe(true)
  expect(needsReview([{ ...reduction[0]!, kind: 'secret' }], null)).toBe(true)
})

it('raises no node issue while the nodes are not loaded', () => {
  const draft = newAgentDraft()
  draft.exclusiveNodes = ['gone']
  expect(issues(draft, { ...context, nodes: [], nodesUnavailable: true }, false)).toEqual([])
  expect(issues(draft, { ...context, nodes: [] }, false).map(issue => issue.section)).toEqual(['nodes'])
})

it('saves tool choices only for granted MCP connections', () => {
  const draft = newAgentDraft()
  draft.access.mcps = null
  draft.access.mcpTools = { m1: ['echo'], m2: [] }
  expect(savedDraft(draft)).toBe(draft)
  // "All" → "Choose" keeps the choices while editing, but the server only accepts granted ones.
  draft.access.mcps = []
  expect(savedDraft(draft).access.mcpTools).toEqual({})
  draft.access.mcps = ['m2']
  expect(savedDraft(draft).access.mcpTools).toEqual({ m2: [] })
  // The draft itself is unchanged.
  expect(draft.access.mcpTools).toEqual({ m1: ['echo'], m2: [] })
})

it('runs agents in Firecracker VMs unless native execution is chosen, and only on nodes of their backend', () => {
  expect(backendOf(draftOf({ id: 'a', name: 'Agent', access: {} } as unknown as Agent).access)).toBe('firecracker')
  const local = {
    ...gpu,
    id: LOCAL_NODE_ID,
    name: 'local',
    local: true,
  } as ExecutionNode
  const desktop = {
    ...gpu,
    id: 'desktop',
    name: 'Native desktop',
    capabilities: { ...gpu.capabilities, backend: 'native' },
  } as ExecutionNode
  const revoked = { ...desktop, id: 'old', revoked: true } as ExecutionNode
  const nodes = [local, gpu, desktop, revoked]
  expect(nodesFor(nodes, 'firecracker').map(node => node.id)).toEqual([LOCAL_NODE_ID, 'gpu'])
  // The current runner is a VM runner: native agents choose native nodes one by one.
  expect(nodesFor(nodes, 'native').map(node => node.id)).toEqual(['desktop'])

  const saved = newAgentDraft()
  const native = structuredClone(saved)
  native.access.backend = 'native'
  native.access.nodes = ['desktop']
  const withNodes = { ...context, nodes }
  // The backend, the current runner removed and the native node added.
  expect(changes(saved, native)).toEqual(['nodes', 'nodes', 'nodes'])
  // Weaker isolation is reviewed like a power; going back to VMs is a reduction.
  expect(reviewLines(saved, native, withNodes)[0]).toEqual({
    kind: 'power',
    section: 'nodes',
    text: 'Added: native execution. Uses the host GPU and OS tools with weaker isolation. Each native node must enable execution and authorize the people directing this agent.',
  })
  expect(reviewLines(native, saved, withNodes).map(line => line.text)).toEqual(['Removed: node Native desktop, native execution.'])
  const agent = { id: 'a', name: 'Agent', access: { projects: [], backend: 'native', nodes: ['desktop'] } } as unknown as Agent
  expect(accessSummary(agent, context)).toBe('No projects · 1 node · Native process')
})

it('always offers the current runner to Firecracker agents, with an unknown state when unreported', () => {
  const desktop = {
    ...gpu,
    id: 'desktop',
    name: 'Native desktop',
    capabilities: { ...gpu.capabilities, backend: 'native' },
  } as ExecutionNode
  const offered = nodesFor([gpu, desktop], 'firecracker')
  expect(offered.map(node => node.id)).toEqual([LOCAL_NODE_ID, 'gpu'])
  expect(offered[0]).toMatchObject({ name: 'Current runner', local: true })
  expect(unknownNode(offered[0]!)).toBe(true)
  expect(unknownNode(gpu)).toBe(false)
  // A reported runner is used as is; native agents never get it.
  const reported = {
    ...gpu,
    id: LOCAL_NODE_ID,
    name: 'local',
    local: true,
  } as ExecutionNode
  expect(nodesFor([reported, gpu], 'firecracker')).toEqual([reported, gpu])
  expect(nodesFor([gpu, desktop], 'native').map(node => node.id)).toEqual(['desktop'])
})

it('merges list edits with grants made elsewhere, entry by entry', () => {
  // Unchanged by the owner: the newer list.
  expect(mergeList(['a'], ['a'], ['a', 'c'])).toEqual(['a', 'c'])
  // Added and removed entries apply to the newer list, keeping a concurrent grant.
  expect(mergeList(['a', 'b'], ['b', 'd'], ['a', 'b', 'c'])).toEqual(['b', 'c', 'd'])
  // A removal elsewhere stays removed unless the owner added the entry.
  expect(mergeList(['a', 'b'], ['a', 'b', 'd'], ['b'])).toEqual(['b', 'd'])
  // "All" ↔ a list is one change: the owner's wins, otherwise the newer one.
  expect(mergeList(null, ['a'], null)).toEqual(['a'])
  expect(mergeList(['a'], null, ['a', 'c'])).toBeNull()
  expect(mergeList(['a'], ['a', 'b'], null)).toBeNull()
  expect(mergeList(null, null, ['c'])).toEqual(['c'])
})

it('rebases every list of the agent, tool choices included', () => {
  const base = newAgentDraft()
  base.access.projects = ['p1']
  base.access.nodes = [LOCAL_NODE_ID, 'gpu']
  base.access.mcps = ['m1', 'm2']
  base.access.mcpTools = { m1: ['echo'] }
  base.plugins = ['prod']
  const mine = structuredClone(base)
  mine.access.nodes = [LOCAL_NODE_ID, 'gpu', 'box']
  mine.access.mcpTools = { m1: ['echo', 'search'] }
  mine.plugins = []
  mine.exclusiveNodes = ['gpu']
  const theirs = structuredClone(base)
  theirs.access.projects = ['p1', 'p2']
  theirs.access.nodes = ['gpu', 'other']
  theirs.access.mcpTools = { m1: ['echo', 'read'], m2: ['list'] }
  theirs.plugins = ['prod', 'staging']
  const rebased = rebaseDraft(base, mine, theirs)
  expect(rebased.access.projects).toEqual(['p1', 'p2'])
  expect(rebased.access.nodes).toEqual(['gpu', 'other', 'box'])
  expect(rebased.access.mcpTools).toEqual({ m1: ['echo', 'read', 'search'], m2: ['list'] })
  expect(rebased.plugins).toEqual(['staging'])
  expect(rebased.exclusiveNodes).toEqual(['gpu'])
})

it('flags run permissions on removed nodes or nodes of the other backend', () => {
  const desktop = {
    ...gpu,
    id: 'desktop',
    name: 'Native desktop',
    capabilities: { ...gpu.capabilities, backend: 'native' },
  } as ExecutionNode
  const draft = newAgentDraft()
  draft.access.nodes = [LOCAL_NODE_ID, 'gpu', 'gone', 'desktop']
  expect(issues(draft, { ...context, nodes: [gpu, desktop] }, false).map(issue => issue.text)).toEqual([
    'A node this agent can run on no longer exists.',
    'Native desktop uses another execution backend: this agent cannot run there.',
  ])
  draft.access.backend = 'native'
  draft.access.nodes = [LOCAL_NODE_ID, 'desktop']
  expect(issues(draft, { ...context, nodes: [gpu, desktop] }, false).map(issue => issue.text)).toEqual([
    'Current runner uses another execution backend: this agent cannot run there.',
  ])
  expect(issues(draft, { ...context, nodes: [], nodesUnavailable: true }, false)).toEqual([])
})

it('describes execution modes for what is known', () => {
  expect(sandboxOptionsFor('firecracker')[0]!.description).toBe('Private VM with sudo and Docker. Default.')
  expect(sandboxOptionsFor('native')[0]!.description).toBe('Native process under a managed account. Host privileges follow the node’s confirmed protected or administrator mode. Default.')
})

it('never sends tool choices of deleted MCP connections', () => {
  const draft = newAgentDraft()
  draft.access.mcps = null
  draft.access.mcpTools = { m1: ['echo'], gone: ['x'] }
  expect(savedDraft(draft, ['m1']).access.mcpTools).toEqual({ m1: ['echo'] })
})

it('never merges node grants across execution backends', () => {
  const base = newAgentDraft()
  base.access.nodes = ['gpu']
  base.exclusiveNodes = ['gpu']
  const mine = structuredClone(base)
  mine.access.backend = 'native'
  mine.access.nodes = ['desktop']
  mine.exclusiveNodes = []
  const theirs = structuredClone(base)
  theirs.access.nodes = ['gpu', 'other']
  theirs.exclusiveNodes = ['gpu', 'other']
  expect(rebaseDraft(base, mine, theirs)).toMatchObject({ access: { backend: 'native', nodes: ['desktop'] }, exclusiveNodes: [] })
  // Changed elsewhere: their backend's lists, without the owner's VM additions.
  const vm = structuredClone(base)
  vm.access.nodes = ['gpu', 'box']
  const native = structuredClone(base)
  native.access.backend = 'native'
  native.access.nodes = ['desktop']
  native.exclusiveNodes = []
  expect(rebaseDraft(base, vm, native)).toMatchObject({ access: { backend: 'native', nodes: ['desktop'] }, exclusiveNodes: [] })
})

it('reports no missing project or skill until the workspace lists load', () => {
  const draft = newAgentDraft()
  draft.access.projects = ['gone']
  draft.access.skills = ['global/gone']
  expect(issues(draft, { ...context, projects: [], workspaceUnavailable: true }, false)).toEqual([])
  expect(issues(draft, { ...context, projects: [] }, false).map(issue => issue.section)).toEqual(['projects', 'skills'])
})

it('names plugin grants it cannot resolve by count, and claims reservations only when effective', () => {
  const agent = {
    id: 'a',
    name: 'Agent',
    access: { projects: [], nodes: ['gpu'] },
    plugins: ['prod', 'x'],
    exclusiveNodes: ['gpu'],
  } as unknown as Agent
  // A member sees no catalogue nor nodes.
  expect(accessSummary(agent, { packages: [], connections: [] })).toBe('No projects · 1 node · 2 plugins')
  expect(accessSummary(agent, context)).toBe('No projects · 1 node · 1Password · 1 other plugin · Exclusive reservation')
})

it('never reports the current runner missing when the node list omits it', () => {
  const draft = newAgentDraft()
  draft.exclusiveNodes = [LOCAL_NODE_ID]
  expect(issues(draft, { ...context, nodes: [gpu] }, false)).toEqual([])
})

it('keeps read-only exclusive reservations when run permissions or the backend change', () => {
  const access = { ...newAgentDraft().access, nodes: ['gpu', 'box'] }
  expect(withRun(access, ['gpu'], 'gpu', false)).toEqual({ nodes: ['box'], exclusiveNodes: [] })
  // An older installation keeps them on its nodes and never receives them: nothing to remove.
  expect(withRun(access, ['gpu'], 'other', true)).toEqual({ nodes: ['gpu', 'box', 'other'], exclusiveNodes: ['gpu'] })
  expect(withBackend()).toEqual({ nodes: [], exclusiveNodes: [] })
})

it('keeps scoped resource rules visible when no whole instance is granted', () => {
  const agent = { id: 'scoped', plugins: [], access: { projects: [], nodes: ['local'] } } as unknown as Agent
  expect(accessSummary(agent, { packages: [], connections: [], resourceGrantCounts: { scoped: 2 } })).toContain('2 saved resource rules')
  expect(agent.plugins).toEqual([])
})

it('reviews administrator node admission as a power even after converting a wildcard grant', () => {
  const administrator: ExecutionNode = {
    ...gpu,
    nativeModeRequest: { mode: 'administrator', generation: 'generation' },
    nativeReviewedAgents: [],
    capabilities: { ...gpu.capabilities, backend: 'native' },
  }
  const ownContext = { ...context, agentId: 'future', nodes: [administrator] }
  const before = newAgentDraft()
  before.access.backend = 'native'
  before.access.nodes = null
  const draft = structuredClone(before)
  draft.access.nodes = [administrator.id]
  expect(nativeReviewNodes(draft, ownContext)).toEqual([administrator.id])
  const lines = reviewLines(before, draft, ownContext)
  expect(lines.some(line => line.kind === 'power' && line.text.includes('unrestricted passwordless sudo'))).toBe(true)
  expect(needsReview(lines, null)).toBe(true)
  administrator.nativeReviewedAgents = ['future']
  expect(nativeReviewNodes(draft, ownContext)).toEqual([])
})

it('edits the Codex speed with the model and preserves it when rebasing', () => {
  const saved = draftOf({
    ...newAgentDraft(),
    id: '00000000-0000-4000-8000-000000000003',
    name: 'Reviewer',
    revision: 'initial',
    createdAt: 0,
  })
  expect(saved.speed).toBe('')
  const mine = structuredClone(saved)
  mine.speed = 'priority'
  expect(changes(saved, mine)).toEqual(['model'])
  const theirs = structuredClone(saved)
  theirs.reasoning = 'low'
  const rebased = rebaseDraft(saved, mine, theirs)
  expect(rebased).toMatchObject({ speed: 'priority', reasoning: 'low' })
  expect(errorSection('Speed applies only to Codex agents. Remove it or choose Codex.')).toBe('model')
})
