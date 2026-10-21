import type { Agent, Project, Skill } from '../../../packages/contracts/contracts'
import type { McpView } from '../../../packages/contracts/mcp'
import type { ExecutionNode } from '../../../packages/contracts/nodes'
import type { PluginConnection, PluginEntry } from './plugins'
import { MAIN_AGENT_ID } from '../../../packages/contracts/constants'
import { LOCAL_NODE_ID } from '../../../packages/contracts/nodes'

// Everything an agent can reach or do is edited on its page, as one draft.
export type AgentAccess = Agent['access']
export interface AgentDraft {
  name: string
  description: string
  instructions: string
  provider: Agent['provider']
  model: string
  reasoning: string
  timeoutMinutes: number
  access: AgentAccess
  // Live grants, kept beside `access` rather than inside it: nodes the agent may reserve
  // exclusively and plugin instances it may use.
  exclusiveNodes: string[]
  plugins: string[]
}

export type SectionId
  = | 'identity'
    | 'model'
    | 'projects'
    | 'skills'
    | 'mcps'
    | 'plugins'
    | 'nodes'
    | 'sandbox'
    | 'powers'
    | 'advanced'

/** Granted / None / Unavailable / Not found / Fixed (main agent) / Power. */
export type ChipTone = 'granted' | 'none' | 'unavailable' | 'missing' | 'fixed' | 'power'

/** One choosable resource in a section's "Choose" list. */
export interface SelectionItem {
  id: string
  name: string
  // Accessible name of its checkbox when the visible name is ambiguous (e.g. "review (Global)").
  label?: string
  detail?: string
  // Why it cannot be used right now, e.g. "Disabled on the installation".
  unavailable?: string
}

/** Sections the server keeps unrestricted for the main agent. */
export const mainLockedSections: SectionId[] = ['projects', 'skills', 'mcps']

export const sandboxOptions = [
  { value: 'yolo', label: 'YOLO', description: 'Private VM with sudo and Docker. Default.' },
  { value: 'workspace-write', label: 'Workspace write', description: 'Writes limited to task workspaces, no sudo.' },
  { value: 'read-only', label: 'Read only', description: 'Project files are read-only; nothing can be changed.' },
] as const

/**
 * The execution modes as they apply to the agent's backend: a native process runs under a managed
 * account on the host, without sudo or Docker (see docs/NATIVE-NODES.md).
 */
export function sandboxOptionsFor(backend: 'firecracker' | 'native') {
  if (backend !== 'native')
    return sandboxOptions
  return sandboxOptions.map(option => option.value === 'yolo'
    ? { ...option, description: 'Native process under a managed account on the host: GPU and system tools, no sudo or Docker. Default.' }
    : option)
}

/** Where the agent's conversations run, for the execution mode section. */
export function isolationNote(backend: 'firecracker' | 'native') {
  return backend === 'native'
    ? 'This agent runs as a native process on its native nodes, with weaker isolation than a VM. Sandboxed runs never bypass denied operations or wait for unattended approvals.'
    : 'Every agent runs in a private VM. Sandboxed runs never bypass denied operations or wait for unattended approvals.'
}

export const alwaysAvailable = 'Publish deliverables (including public links), wait for a delay or a condition, see its nodes and move the conversation between them, report its outcome.'

/**
 * A new agent starts restricted: no project, skill, MCP connection or plugin, on the
 * current runner only. The owner widens each section, e.g. with "All, including future".
 */
export function newAgentDraft(): AgentDraft {
  return {
    name: '',
    description: '',
    instructions: '',
    provider: 'codex',
    model: '',
    reasoning: '',
    timeoutMinutes: 0,
    access: {
      backend: 'firecracker',
      nodes: [LOCAL_NODE_ID],
      projects: [],
      skills: [],
      mcps: [],
      mcpTools: {},
      installation: false,
      sandbox: 'yolo',
    },
    exclusiveNodes: [],
    plugins: [],
  }
}

/** The editable part of a saved agent, with the defaults older records omit. */
export function draftOf(agent: Agent): AgentDraft {
  const access = (agent.access ?? {}) as Partial<AgentAccess>
  return {
    name: agent.name,
    description: agent.description ?? '',
    instructions: agent.instructions ?? '',
    provider: agent.provider ?? 'codex',
    model: agent.model ?? '',
    reasoning: agent.reasoning ?? '',
    timeoutMinutes: agent.timeoutMinutes ?? 0,
    access: {
      backend: access.backend ?? 'firecracker',
      nodes: access.nodes === undefined ? [LOCAL_NODE_ID] : access.nodes === null ? null : [...access.nodes],
      projects: access.projects ? [...access.projects] : null,
      skills: access.skills ? [...access.skills] : null,
      mcps: access.mcps ? [...access.mcps] : null,
      mcpTools: structuredClone(JSON.parse(JSON.stringify(access.mcpTools ?? {}))),
      installation: access.installation ?? false,
      sandbox: access.sandbox ?? 'yolo',
    },
    // Responses omit empty live grants: absent means none.
    exclusiveNodes: [...(agent.exclusiveNodes ?? [])],
    plugins: [...(agent.plugins ?? [])],
  }
}

function same(left: unknown, right: unknown) {
  return JSON.stringify(left) === JSON.stringify(right)
}

/**
 * Three-way merge of a list where null means "all": entries the owner added or removed apply to
 * the newer list, so a grant made elsewhere meanwhile is kept. Switching between "all" and a list
 * is one change: the owner's wins, otherwise the newer one.
 */
export function mergeList(base: string[] | null, mine: string[] | null, theirs: string[] | null): string[] | null {
  if (same(base, mine))
    return theirs
  if (base === null || mine === null || theirs === null)
    return base === null || mine === null ? mine : theirs
  const added = mine.filter(item => !base.includes(item))
  const removed = base.filter(item => !mine.includes(item))
  return [...new Set([...theirs.filter(item => !removed.includes(item)), ...added])]
}

/** Tool choices per connection, merged like lists: an absent connection means all its tools. */
function mergeTools(base: Record<string, string[]>, mine: Record<string, string[]>, theirs: Record<string, string[]>) {
  const merged: Record<string, string[]> = {}
  for (const id of new Set([...Object.keys(base), ...Object.keys(mine), ...Object.keys(theirs)])) {
    const tools = mergeList(base[id] ?? null, mine[id] ?? null, theirs[id] ?? null)
    if (tools !== null)
      merged[id] = tools
  }

  return merged
}

const listAccess = ['nodes', 'projects', 'skills', 'mcps'] as const

/**
 * Applies the owner's edits onto a newer saved agent, as Android does: a value the owner
 * changed from `base` keeps the draft value, every other value takes the newer one. Lists merge
 * entry by entry, so neither the owner's edits nor grants made elsewhere are lost.
 */
export function rebaseDraft(base: AgentDraft, mine: AgentDraft, theirs: AgentDraft): AgentDraft {
  const pick = <T>(old: T, edited: T, newer: T): T => same(old, edited) ? newer : edited
  const access: Record<string, unknown> = Object.fromEntries((Object.keys(theirs.access) as (keyof AgentAccess)[])
    .map(key => [key, pick(base.access[key], mine.access[key], theirs.access[key])]))
  for (const key of listAccess)
    access[key] = mergeList(base.access[key], mine.access[key], theirs.access[key])
  // Nodes belong to one backend: when either side changed it, its node lists are taken whole,
  // the owner's if they changed it, never merged across backends.
  const backendChanged = backendOf(mine.access) !== backendOf(base.access) || backendOf(theirs.access) !== backendOf(base.access)
  const ownBackend = backendOf(mine.access) !== backendOf(base.access)
  if (backendChanged)
    access.nodes = ownBackend ? mine.access.nodes : theirs.access.nodes
  access.mcpTools = mergeTools(base.access.mcpTools ?? {}, mine.access.mcpTools ?? {}, theirs.access.mcpTools ?? {})
  const rebased: AgentDraft = {
    name: pick(base.name, mine.name, theirs.name),
    description: pick(base.description, mine.description, theirs.description),
    instructions: pick(base.instructions, mine.instructions, theirs.instructions),
    provider: pick(base.provider, mine.provider, theirs.provider),
    model: pick(base.model, mine.model, theirs.model),
    reasoning: pick(base.reasoning, mine.reasoning, theirs.reasoning),
    timeoutMinutes: pick(base.timeoutMinutes, mine.timeoutMinutes, theirs.timeoutMinutes),
    access: access as unknown as AgentAccess,
    exclusiveNodes: backendChanged
      ? (ownBackend ? mine.exclusiveNodes : theirs.exclusiveNodes)
      : mergeList(base.exclusiveNodes, mine.exclusiveNodes, theirs.exclusiveNodes) ?? [],
    plugins: mergeList(base.plugins, mine.plugins, theirs.plugins) ?? [],
  }
  return JSON.parse(JSON.stringify(rebased))
}

/**
 * The draft as it is saved: tool choices only for granted MCP connections, as the server requires.
 * The page keeps the others while editing, so switching back to "All" restores them.
 */
export function savedDraft(draft: AgentDraft, existingMcps?: string[]): AgentDraft {
  const { mcps, mcpTools } = draft.access
  // Choices for deleted connections are never sent either.
  const granted = Object.fromEntries(Object.entries(mcpTools ?? {})
    .filter(([id]) => (mcps === null || mcps.includes(id)) && (!existingMcps || existingMcps.includes(id))))
  if (same(granted, mcpTools ?? {}))
    return draft
  return { ...draft, access: { ...draft.access, mcpTools: granted } }
}

/**
 * An installation that predates live grants on the agent ignores top-level `exclusiveNodes` and
 * `plugins` and keeps them on the resources, as Android handles it. Current installations mark
 * every listed agent with the computed `grantsOnAgent` capability; revisions alone are not proof,
 * since agents keep them after a rollback.
 */
export function olderInstallation(agents: object[]) {
  return agents.length > 0 && !agents.some(agent => (agent as { grantsOnAgent?: unknown }).grantsOnAgent === true)
}

/** The draft as the server applies it: an older installation never receives live grant changes. */
export function sentDraft(saved: AgentDraft, draft: AgentDraft, older: boolean): AgentDraft {
  return older ? { ...draft, exclusiveNodes: saved.exclusiveNodes, plugins: saved.plugins } : draft
}

/** On an older installation, the agent's live grants as its resources record them. */
export function withResourceGrants(draft: AgentDraft, agentId: string, connections: Pick<PluginConnection, 'id' | 'agentIds'>[], nodes: Pick<ExecutionNode, 'id' | 'local' | 'exclusiveAgents'>[]): AgentDraft {
  return {
    ...draft,
    plugins: connections.filter(connection => connection.agentIds?.includes(agentId)).map(connection => connection.id),
    exclusiveNodes: nodes.filter(node => node.exclusiveAgents?.includes(agentId)).map(node => node.local ? LOCAL_NODE_ID : node.id),
  }
}

/**
 * The agents an instance save must send back unchanged on an older installation (or one not known
 * yet), which replaces the list when it is omitted; undefined when it should be omitted.
 */
export function keptAgentIds(agents: object[], entry: Pick<PluginEntry, 'manifest'> | undefined, connection: Pick<PluginConnection, 'agentIds'> | null | undefined) {
  // Main always exists: no agent at all means the list did not load, and the installation is unknown.
  const unknown = agents.length === 0
  if (!(unknown || olderInstallation(agents)) || !entry?.manifest?.agentAccess || !connection?.agentIds?.length)
    return undefined
  return [...connection.agentIds]
}

/**
 * `keptAgentIds` from the instance as it is now, re-read right before saving: an agent revoked
 * meanwhile from its page is not granted back. Undefined when nothing has to be sent.
 */
export async function currentAgentIds(
  agents: object[],
  entry: Pick<PluginEntry, 'manifest'> | undefined,
  connection: Pick<PluginConnection, 'id' | 'agentIds'> | null | undefined,
  connections: () => Promise<Pick<PluginConnection, 'id' | 'agentIds'>[]>,
) {
  if (!connection || keptAgentIds(agents, entry, { agentIds: ['?'] }) === undefined)
    return undefined
  const fresh = (await connections()).find(item => item.id === connection.id)
  return keptAgentIds(agents, entry, fresh ?? connection)
}

export const updateForGrants = 'Update the installation to grant these from the agent page.'

/** "All, including future" | "3 of 12" | "None". */
export function selectionSummary(selected: string[] | null, total: number) {
  if (selected === null)
    return 'All, including future'
  if (!selected.length)
    return 'None'
  return `${selected.length} of ${Math.max(total, selected.length)}`
}

export function skillKey(skill: Pick<Skill, 'scope' | 'name'>) {
  return `${skill.scope}/${skill.name}`
}

/** "review · Global", "lint · cairn": the skill's name and where it comes from. */
export function skillLabel(key: string, projects: Pick<Project, 'id' | 'name'>[]) {
  const separator = key.indexOf('/')
  const scope = separator < 0 ? '' : key.slice(0, separator)
  const name = separator < 0 ? key : key.slice(separator + 1)
  const origin = scope === 'global' ? 'Global' : projects.find(project => project.id === scope)?.name ?? 'Removed project'
  return { name, origin }
}

export function nodeName(node: Pick<ExecutionNode, 'id' | 'name' | 'local'> | undefined, id: string) {
  if (id === LOCAL_NODE_ID || node?.local)
    return 'Current runner'
  return node?.name ?? 'Removed node'
}

export type Backend = 'firecracker' | 'native'

export const backendOptions = [
  { value: 'firecracker', label: 'Firecracker VM' },
  { value: 'native', label: 'Native process · trusted people only' },
] as const

export const nativeHelp = 'Uses the host GPU and OS tools with weaker isolation. Choose native nodes explicitly; each node must also authorize the people directing this agent.'

/** The agent's execution backend; records from before native execution run in Firecracker VMs. */
export function backendOf(access: Pick<AgentAccess, 'backend'> | undefined): Backend {
  return access?.backend === 'native' ? 'native' : 'firecracker'
}

export function nodeBackend(node: Pick<ExecutionNode, 'capabilities'>): Backend {
  return node.capabilities.backend === 'native' ? 'native' : 'firecracker'
}

const placeholders = new WeakSet<ExecutionNode>()

/** The current runner when the node list does not report it: its state is unknown. */
function currentRunner(): ExecutionNode {
  const node: ExecutionNode = {
    id: LOCAL_NODE_ID,
    name: 'Current runner',
    local: true,
    accepting: true,
    revoked: false,
    status: 'local',
    tags: [],
    capabilities: {
      cpu: 0,
      memoryMiB: 0,
      diskMiB: 0,
      os: '',
      arch: '',
      kvm: false,
    },
    limits: { cpu: 0, memoryMiB: 0, diskMiB: 0 },
    slots: 0,
    runtimeId: '',
    lastSeen: null,
  }
  placeholders.add(node)
  return node
}

/** A node only known by its grant, such as the current runner missing from the node list. */
export function unknownNode(node: ExecutionNode) {
  return placeholders.has(node)
}

/**
 * Nodes an agent of `backend` can be granted: those of the same backend. The current runner is a
 * VM runner, always offered to Firecracker agents as Android does, even when the node list omits
 * it. A native agent never runs on "all nodes": its nodes are chosen one by one.
 */
export function nodesFor(nodes: ExecutionNode[], backend: Backend) {
  const matching = nodes.filter(node => !node.revoked && nodeBackend(node) === backend && !(backend === 'native' && (node.local || node.id === LOCAL_NODE_ID)))
  if (backend === 'firecracker' && !nodes.some(node => node.local || node.id === LOCAL_NODE_ID))
    return [currentRunner(), ...matching]
  return matching
}

/**
 * Why a node the agent may run on cannot be used: it was revoked or deleted, or it belongs to the
 * other execution backend. The current runner counts as present even when unreported.
 */
export function runProblem(id: string, access: AgentAccess, nodes: ExecutionNode[]): 'missing' | 'backend' | '' {
  const node = nodes.find(item => item.id === id)
  const local = id === LOCAL_NODE_ID || node?.local === true
  if (local)
    return backendOf(access) === 'native' ? 'backend' : ''
  if (!node || node.revoked)
    return 'missing'
  return nodeBackend(node) === backendOf(access) ? '' : 'backend'
}

/**
 * Node access after allowing or forbidding a node: forbidding it also drops its exclusive
 * reservation, which could never be used, unless reservations are read-only (an older installation
 * keeps them on its nodes and never receives them from here).
 */
export function withRun(access: AgentAccess, exclusiveNodes: string[], node: string, on: boolean, readOnlyGrants = false) {
  const nodes = (access.nodes ?? []).filter(id => id !== node)
  return {
    nodes: on ? [...nodes, node] : nodes,
    exclusiveNodes: on || readOnlyGrants ? exclusiveNodes : exclusiveNodes.filter(id => id !== node),
  }
}

/** Node access for another execution backend: the choices start over, read-only reservations stay. */
export function withBackend(exclusiveNodes: string[], readOnlyGrants = false) {
  return { nodes: [] as string[], exclusiveNodes: readOnlyGrants ? exclusiveNodes : [] }
}

/** Whether the agent may run on a node: listed, or every node allowed. */
export function runsOn(access: AgentAccess, node: string) {
  return access.nodes === null || access.nodes.includes(node)
}

/** The runtime can suspend other conversations for an exclusive reservation. */
export function supportsExclusive(node: ExecutionNode) {
  return node.exclusiveReservations === true
}

/** Exclusive grants that take effect: the node exists, the agent may run there and its runtime supports it. */
export function effectiveExclusive(draft: Pick<AgentDraft, 'access' | 'exclusiveNodes'>, nodes: ExecutionNode[]) {
  return (draft.exclusiveNodes ?? []).filter((id) => {
    const node = nodes.find(item => item.id === id)
    return node !== undefined && !node.revoked && runsOn(draft.access, id) && supportsExclusive(node)
  })
}

export type NodeState = 'online' | 'offline' | 'paused' | 'update'
export function nodeState(node: ExecutionNode): { state: NodeState, label: string } {
  if (!node.local && node.status === 'offline')
    return { state: 'offline', label: 'Offline' }
  if (!node.accepting)
    return { state: 'paused', label: 'Not accepting work' }
  if (!node.local && node.executionReady === false)
    return { state: 'update', label: 'Runtime update needed' }
  return { state: 'online', label: 'Online' }
}

/**
 * Packages whose instances give agents something (commands, tools or instructions) with an
 * approved active version, as the server requires to use a grant.
 */
export function grantablePackages(packages: PluginEntry[]) {
  return packages.filter(entry => (entry.status === 'active' || entry.status === 'update') && entry.manifest?.agentAccess)
}

/**
 * Whether an agent can use a grant on this instance. A grant on any other instance is kept
 * but unusable, e.g. until its package is reviewed again.
 */
export function grantableConnection(connection: Pick<PluginConnection, 'pluginId'>, packages: PluginEntry[]) {
  return grantablePackages(packages).some(entry => entry.id === connection.pluginId)
}

export function instanceState(connection: PluginConnection): { tone: 'granted' | 'unavailable', label: string } {
  if (!connection.enabled)
    return { tone: 'unavailable', label: connection.pluginId === 'onepassword' ? 'Account disabled' : 'Disabled' }
  if (connection.status.state === 'needs_review')
    return { tone: 'unavailable', label: 'Needs review' }
  if (['error', 'failed', 'invalid'].includes(connection.status.state))
    return { tone: 'unavailable', label: 'Test failed' }
  return { tone: 'granted', label: 'Active' }
}

export interface Context {
  projects: Pick<Project, 'id' | 'name'>[]
  skills: Skill[]
  mcps: McpView[]
  nodes: ExecutionNode[]
  packages: PluginEntry[]
  connections: PluginConnection[]
  // The plugin catalogue could not be loaded: plugin grants cannot be checked.
  pluginsUnavailable?: boolean
  // The nodes could not be loaded: node grants cannot be checked.
  nodesUnavailable?: boolean
  // Projects, skills and MCP connections are not loaded (yet, or the refresh failed).
  workspaceUnavailable?: boolean
}

function listChanges(before: string[] | null, after: string[] | null) {
  if (before === null || after === null) {
    if (before === after)
      return { added: [] as string[], removed: [] as string[], scope: 0 }
    return { added: [], removed: [], scope: 1 }
  }

  return {
    added: after.filter(item => !before.includes(item)),
    removed: before.filter(item => !after.includes(item)),
    scope: 0,
  }
}

function listCount(before: string[] | null, after: string[] | null) {
  const change = listChanges(before, after)
  return change.scope + change.added.length + change.removed.length
}

/** The sections whose saved values differ, one entry per changed value. */
export function changes(saved: AgentDraft, draft: AgentDraft) {
  const changed: SectionId[] = []
  const add = (section: SectionId, count: number) => {
    for (let index = 0; index < count; index++)
      changed.push(section)
  }

  for (const key of ['name', 'description', 'instructions'] as const)
    add('identity', Number(saved[key] !== draft[key]))
  for (const key of ['provider', 'model', 'reasoning'] as const)
    add('model', Number(saved[key] !== draft[key]))
  add('advanced', Number(saved.timeoutMinutes !== draft.timeoutMinutes))
  add('projects', listCount(saved.access.projects, draft.access.projects))
  add('skills', listCount(saved.access.skills, draft.access.skills))
  add('mcps', listCount(saved.access.mcps, draft.access.mcps))
  const savedTools = saved.access.mcpTools ?? {}
  const draftTools = draft.access.mcpTools ?? {}
  const tools = new Set([...Object.keys(savedTools), ...Object.keys(draftTools)])
  // Tool choices are sets: unticking and reticking a tool changes nothing.
  const asSet = (chosen: string[] | undefined) => chosen ? JSON.stringify([...new Set(chosen)].sort()) : null
  for (const connection of tools)
    add('mcps', Number(asSet(savedTools[connection]) !== asSet(draftTools[connection])))
  add('sandbox', Number(saved.access.sandbox !== draft.access.sandbox))
  add('powers', Number(saved.access.installation !== draft.access.installation))
  add('nodes', Number(backendOf(saved.access) !== backendOf(draft.access)))
  add('nodes', listCount(saved.access.nodes, draft.access.nodes))
  // Responses omit empty live grants: absent means none.
  add('nodes', listCount(saved.exclusiveNodes ?? [], draft.exclusiveNodes ?? []))
  add('plugins', listCount(saved.plugins ?? [], draft.plugins ?? []))
  return changed
}

export interface ReviewLine {
  kind: 'power' | 'secret' | 'reduction'
  text: string
  section: SectionId
}

function connectionLabel(id: string, context: Context) {
  const connection = context.connections.find(item => item.id === id)
  const pack = context.packages.find(entry => entry.id === connection?.pluginId)
  return `${pack?.manifest?.name ?? 'Plugin'} “${connection?.name ?? 'Removed instance'}”`
}

const sandboxStrictness: Record<AgentAccess['sandbox'], number> = { 'yolo': 0, 'workspace-write': 1, 'read-only': 2 }

/** MCP connections granted before and after whose tools the draft narrows, from all enabled tools or by removing one. */
function restrictedTools(saved: AgentAccess, draft: AgentAccess) {
  const before = saved.mcpTools ?? {}
  const after = draft.mcpTools ?? {}
  return Object.entries(after).filter(([id, tools]) => {
    // A connection granted just now, or removed, is reported as such.
    const kept = (saved.mcps === null || saved.mcps.includes(id)) && (draft.mcps === null || draft.mcps.includes(id))
    const previous = before[id]
    return kept && (previous === undefined || previous.some(tool => !tools.includes(tool)))
  }).map(([id]) => id)
}

/**
 * What saving would grant or take away that deserves a look first: new powers and
 * secrets, and removed access when `runs` says work is affected.
 */
export function reviewLines(saved: AgentDraft, draft: AgentDraft, context: Context, runs = { queued: 0, running: 0 }) {
  const lines: ReviewLine[] = []
  const node = (id: string) => nodeName(context.nodes.find(item => item.id === id), id)

  for (const id of listChanges(saved.exclusiveNodes ?? [], draft.exclusiveNodes ?? []).added)
    lines.push({ kind: 'power', section: 'nodes', text: `Added: exclusive reservation on ${node(id)}. This agent will be able to suspend other agents’ conversations on this machine.` })
  if (backendOf(draft.access) === 'native' && backendOf(saved.access) !== 'native')
    lines.push({ kind: 'power', section: 'nodes', text: `Added: native execution. ${nativeHelp}` })
  if (draft.access.installation && !saved.access.installation)
    lines.push({ kind: 'power', section: 'powers', text: 'Added: installation management. This agent will be able to create and change agents, projects, skills, MCP connections, missions and runs on your behalf.' })
  for (const id of listChanges(saved.plugins ?? [], draft.plugins ?? []).added)
    lines.push({ kind: 'secret', section: 'plugins', text: `Added: ${connectionLabel(id, context)} — effective immediately, including in running conversations.` })

  const removed: string[] = []
  const project = (id: string) => context.projects.find(item => item.id === id)?.name ?? 'removed project'
  const mcp = (id: string) => context.mcps.find(item => item.id === id)?.name ?? 'removed connection'
  const described = (label: string, before: string[] | null, after: string[] | null, name: (id: string) => string) => {
    const change = listChanges(before, after)
    if (change.scope && after !== null)
      removed.push(`all ${label}s (now ${after.length ? after.map(name).join(', ') : 'none'})`)
    for (const id of change.removed)
      removed.push(`${label} ${name(id)}`)
  }

  described('project', saved.access.projects, draft.access.projects, project)
  described('skill', saved.access.skills, draft.access.skills, key => skillLabel(key, context.projects).name)
  described('MCP connection', saved.access.mcps, draft.access.mcps, mcp)
  for (const id of restrictedTools(saved.access, draft.access))
    removed.push(`MCP tools on ${mcp(id)} (now ${draft.access.mcpTools[id]!.length || 'none'} chosen)`)
  described('node', saved.access.nodes, draft.access.nodes, node)
  for (const id of listChanges(saved.exclusiveNodes ?? [], draft.exclusiveNodes ?? []).removed)
    removed.push(`exclusive reservation on ${node(id)}`)
  for (const id of listChanges(saved.plugins ?? [], draft.plugins ?? []).removed)
    removed.push(connectionLabel(id, context))
  if (saved.access.installation && !draft.access.installation)
    removed.push('installation management')
  if (backendOf(saved.access) === 'native' && backendOf(draft.access) !== 'native')
    removed.push('native execution')
  if (sandboxStrictness[draft.access.sandbox] > sandboxStrictness[saved.access.sandbox]) {
    const mode = sandboxOptions.find(option => option.value === draft.access.sandbox)?.label ?? draft.access.sandbox
    removed.push(`execution mode (now ${mode})`)
  }

  const affected = runs.queued + runs.running
  const work = [
    runs.queued ? `${runs.queued} queued ${runs.queued === 1 ? 'run' : 'runs'}` : '',
    runs.running ? `${runs.running} running ${runs.running === 1 ? 'conversation' : 'conversations'}` : '',
  ].filter(Boolean).join(' and ')
  const consequence = affected ? ` ${work} will not continue with the old access.` : ''
  if (removed.length)
    lines.push({ kind: 'reduction', section: 'projects', text: `Removed: ${removed.join(', ')}.${consequence}` })
  else if (affected)
    lines.push({ kind: 'reduction', section: 'projects', text: `Changed access.${consequence}` })
  return lines
}

/** A plugin delegation answering as the agent, as `POST /agents/{id}/preview` reports it. */
export interface DelegationEffect {
  id: string
  pluginId: string
  name: string
  enabled: boolean
  suspended: boolean
  // The saved agent already no longer matches the delegation's approval, before this edit.
  alreadySuspended?: boolean
  // Why the new access cannot be reapproved, when it cannot.
  blocked: string | null
}

export interface Preview {
  runs: { queued: number, running: number }
  delegations: DelegationEffect[]
}

/**
 * Whether saving needs the review first: a power or a secret added, interrupted work, or a
 * delegation this edit suspends. A delegation already suspended by an earlier change, blocked or
 * not, is listed when the review opens, but never opens it.
 */
export function needsReview(lines: ReviewLine[], preview: Preview | null) {
  const runs = preview ? preview.runs.queued + preview.runs.running : 0
  const delegations = preview?.delegations.some(item => item.suspended && !item.alreadySuspended) ?? false
  return lines.some(line => line.kind !== 'reduction') || runs > 0 || delegations
}

export interface Issue {
  section: SectionId
  text: string
}

/**
 * What the owner should check: grants that exist but cannot be used, targets that are gone, and
 * delegations answering as the agent (`agentId`) that wait for reapproval.
 */
export function issues(draft: AgentDraft, context: Context, isMain: boolean, agentId: string | null = null) {
  const found: Issue[] = []
  const access = draft.access
  // Until the workspace lists load, a grant cannot be said to be gone.
  if (!isMain && !context.workspaceUnavailable) {
    for (const id of access.projects ?? []) {
      if (!context.projects.some(project => project.id === id))
        found.push({ section: 'projects', text: 'A selected project no longer exists.' })
    }

    for (const key of access.skills ?? []) {
      if (!context.skills.some(skill => skillKey(skill) === key))
        found.push({ section: 'skills', text: `The skill “${skillLabel(key, context.projects).name}” no longer exists.` })
    }

    for (const connection of context.mcps) {
      if (!connection.enabled && (access.mcps === null || access.mcps.includes(connection.id)))
        found.push({ section: 'mcps', text: `The MCP connection “${connection.name}” is disabled on the installation. The agent keeps it but cannot use it until it is enabled again.` })
    }
  }

  for (const id of context.nodesUnavailable ? [] : access.nodes ?? []) {
    const problem = runProblem(id, access, context.nodes)
    if (problem === 'missing')
      found.push({ section: 'nodes', text: 'A node this agent can run on no longer exists.' })
    else if (problem === 'backend')
      found.push({ section: 'nodes', text: `${nodeName(context.nodes.find(item => item.id === id), id)} uses another execution backend: this agent cannot run there.` })
  }

  for (const id of context.nodesUnavailable ? [] : draft.exclusiveNodes ?? []) {
    const node = context.nodes.find(item => item.id === id)
    // The current runner exists even when the node list omits it; its support is then unknown.
    if (!node && id === LOCAL_NODE_ID)
      continue
    if (!node || node.revoked)
      found.push({ section: 'nodes', text: 'An exclusive reservation targets a node that no longer exists.' })
    else if (!runsOn(access, id))
      found.push({ section: 'nodes', text: `The exclusive reservation on ${nodeName(node, id)} is inactive: this agent cannot run there.` })
    else if (!supportsExclusive(node))
      found.push({ section: 'nodes', text: `The exclusive reservation on ${nodeName(node, id)} is inactive: its runtime must be updated for exclusive reservations.` })
  }

  if (context.pluginsUnavailable)
    return found

  for (const id of draft.plugins ?? []) {
    const connection = context.connections.find(item => item.id === id)
    if (!connection)
      found.push({ section: 'plugins', text: 'A granted plugin instance no longer exists.' })
    else if (!grantableConnection(connection, context.packages))
      found.push({ section: 'plugins', text: `${connectionLabel(id, context)} is unavailable: its plugin needs review or no longer gives agents access.` })
    else if (!connection.enabled)
      found.push({ section: 'plugins', text: `${connectionLabel(id, context)} is disabled: the access is kept but cannot be used.` })
  }

  for (const connection of context.connections) {
    if (agentId && connection.agentId === agentId && connection.enabled && connection.status.state === 'needs_review')
      found.push({ section: 'plugins', text: `${connection.name} is suspended until it is reapproved.` })
  }

  return found
}

/** "3 projects · 2 nodes · 1Password" for the agents list. */
export function accessSummary(agent: Agent, context: Pick<Context, 'packages' | 'connections'> & { nodes?: ExecutionNode[] }, older = false) {
  const access = agent.access ?? {} as AgentAccess
  // An older installation keeps plugin grants on its instances (unknown to members, who get none).
  const plugins = older
    ? context.connections.filter(connection => connection.agentIds?.includes(agent.id)).map(connection => connection.id)
    : agent.plugins ?? []
  const count = (value: string[] | null | undefined, noun: string, all: string) => value === null || value === undefined
    ? all
    : `${value.length || 'No'} ${noun}${value.length === 1 ? '' : 's'}`
  const nodes = access.nodes === undefined ? [LOCAL_NODE_ID] : access.nodes
  const parts = [
    agent.id === MAIN_AGENT_ID ? 'All projects' : count(access.projects, 'project', 'All projects'),
    count(nodes, 'node', 'All nodes'),
    ...(backendOf(access) === 'native' ? ['Native process'] : []),
  ]
  // Named by package when the catalogue says which; members, who do not see plugins, get a count.
  const packages = new Set<string>()
  let unnamed = 0
  for (const id of plugins) {
    const connection = context.connections.find(item => item.id === id)
    const name = context.packages.find(entry => entry.id === connection?.pluginId)?.manifest?.name
    if (name)
      packages.add(name)
    else
      unnamed++
  }

  parts.push(...packages)
  if (unnamed)
    parts.push(`${unnamed} ${packages.size ? 'other ' : ''}plugin${unnamed === 1 ? '' : 's'}`)
  // Only an effective reservation, which the node list says; without it, none is claimed.
  if (!older && agent.exclusiveNodes?.length && context.nodes && effectiveExclusive({ access, exclusiveNodes: agent.exclusiveNodes }, context.nodes).length)
    parts.push('Exclusive reservation')
  if (access.installation)
    parts.push('Installation management')
  return parts.join(' · ')
}

/** Guesses which section a server error belongs to, so the page can point at it. */
export function errorSection(message: string): SectionId | undefined {
  const text = message.toLowerCase()
  const rules: [SectionId, RegExp][] = [
    // "Grant one GitHub instance per agent: its runs use that instance's credentials."
    ['plugins', /plugin|1password|delegation|instance|credential/],
    ['nodes', /node|exclusive|reserv/],
    ['mcps', /mcp|tool/],
    ['skills', /skill/],
    ['projects', /project/],
    ['model', /model|provider|reasoning|codex|claude|opencode/],
    ['advanced', /time limit|timeout/],
    ['identity', /name|description|instruction/],
  ]
  return rules.find(([, pattern]) => pattern.test(text))?.[0]
}
