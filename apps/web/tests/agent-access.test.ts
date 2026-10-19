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
  effectiveExclusive,
  errorSection,
  githubTokenChange,
  grantableConnection,
  grantablePackages,
  issues,
  needsReview,
  newAgentDraft,
  rebaseDraft,
  restrictedGithubMode,
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

it('lists every reduction the Android review lists', () => {
  const mcpContext: Context = { ...context, mcps: [{ id: 'm1', name: 'Design tools' }, { id: 'm2', name: 'Docs' }] as Context['mcps'] }
  const saved = newAgentDraft()
  saved.access.mcps = ['m1', 'm2']
  saved.access.mcpTools = { m2: ['read', 'write'] }
  saved.access.github = true
  saved.access.projects = null
  const draft = structuredClone(saved)
  // Stricter execution mode.
  draft.access.sandbox = 'workspace-write'
  // From all enabled tools to a chosen few, and a tool removed from a chosen list.
  draft.access.mcpTools = { m1: ['echo'], m2: ['read'] }
  draft.access.github = false
  draft.access.projects = ['p1']
  const lines = reviewLines(saved, draft, mcpContext, undefined, 'remove')
  expect(lines.map(line => line.kind)).toEqual(['reduction', 'reduction'])
  expect(lines[0]!.text).toBe('Removed: all projects (now SheetOM), MCP tools on Design tools (now 1 chosen), MCP tools on Docs (now 1 chosen), shared GitHub connection, dedicated GitHub token, execution mode (now Workspace write).')
  expect(lines[1]!.text).toBe('Shared GitHub connection turned off: selected projects require a dedicated token.')

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

it('removes a saved GitHub token only when the owner asks for it', () => {
  const kept = {
    main: false,
    mode: 'none' as const,
    token: '',
    configured: true,
    removeRequested: false,
  }
  // Restricting projects switches the shared connection to a saved token, narrower, or to no GitHub.
  expect(restrictedGithubMode('shared', true)).toBe('dedicated')
  expect(restrictedGithubMode('shared', false)).toBe('none')
  expect(restrictedGithubMode('none', true)).toBe('none')
  expect(githubTokenChange({ ...kept, mode: restrictedGithubMode('shared', true) })).toBeNull()
  // A saved token is removed only on the owner's explicit choice.
  expect(githubTokenChange(kept)).toBeNull()
  expect(githubTokenChange({ ...kept, removeRequested: true })).toBe('remove')
  expect(githubTokenChange({ ...kept, configured: false, removeRequested: true })).toBeNull()
  expect(githubTokenChange({ ...kept, mode: 'dedicated', token: ' new ' })).toBe('set')
  expect(githubTokenChange({ ...kept, mode: 'dedicated' })).toBeNull()
  expect(githubTokenChange({ ...kept, main: true, removeRequested: true })).toBeNull()
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

it('says when restricted projects move the agent to its saved GitHub token', () => {
  const saved = newAgentDraft()
  saved.access.projects = null
  saved.access.github = true
  const draft = structuredClone(saved)
  draft.access.projects = ['p1']
  draft.access.github = false
  expect(reviewLines(saved, draft, context, undefined, null, true).at(-1)!.text).toBe('Shared GitHub connection turned off: the agent will use its saved dedicated token.')
  expect(reviewLines(saved, draft, context).at(-1)!.text).toBe('Shared GitHub connection turned off: selected projects require a dedicated token.')
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
  // Moving to the saved GitHub token changes the agent's credentials.
  expect(needsReview(reduction, null, true)).toBe(true)
})

it('raises no node issue while the nodes are not loaded', () => {
  const draft = newAgentDraft()
  draft.exclusiveNodes = ['gone']
  expect(issues(draft, { ...context, nodes: [], nodesUnavailable: true }, false)).toEqual([])
  expect(issues(draft, { ...context, nodes: [] }, false).map(issue => issue.section)).toEqual(['nodes'])
})
