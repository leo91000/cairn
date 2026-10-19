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
    | 'github'
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
export const mainLockedSections: SectionId[] = ['projects', 'skills', 'mcps', 'github']

export const sandboxOptions = [
  { value: 'yolo', label: 'YOLO', description: 'Private VM with sudo and Docker. Default.' },
  { value: 'workspace-write', label: 'Workspace write', description: 'Writes limited to task workspaces, no sudo.' },
  { value: 'read-only', label: 'Read only', description: 'Project files are read-only; nothing can be changed.' },
] as const

export const alwaysAvailable = 'Publish deliverables (including public links), wait for a delay or a condition, see its nodes and move the conversation between them, report its outcome.'

/**
 * A new agent starts restricted: no project, skill, MCP connection or GitHub, on the
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
      nodes: [LOCAL_NODE_ID],
      projects: [],
      skills: [],
      mcps: [],
      mcpTools: {},
      installation: false,
      github: false,
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
      nodes: access.nodes === undefined ? [LOCAL_NODE_ID] : access.nodes === null ? null : [...access.nodes],
      projects: access.projects ? [...access.projects] : null,
      skills: access.skills ? [...access.skills] : null,
      mcps: access.mcps ? [...access.mcps] : null,
      mcpTools: structuredClone(JSON.parse(JSON.stringify(access.mcpTools ?? {}))),
      installation: access.installation ?? false,
      github: access.github ?? true,
      sandbox: access.sandbox ?? 'yolo',
    },
    // Responses omit empty live grants: absent means none.
    exclusiveNodes: [...(agent.exclusiveNodes ?? [])],
    plugins: [...(agent.plugins ?? [])],
  }
}

/**
 * Applies the owner's edits onto a newer saved agent, as Android does: a value the owner
 * changed from `base` keeps the draft value, every other value takes the newer one.
 */
export function rebaseDraft(base: AgentDraft, mine: AgentDraft, theirs: AgentDraft): AgentDraft {
  const same = (left: unknown, right: unknown) => JSON.stringify(left) === JSON.stringify(right)
  const pick = <T>(old: T, edited: T, newer: T): T => same(old, edited) ? newer : edited
  const access = Object.fromEntries((Object.keys(theirs.access) as (keyof AgentAccess)[])
    .map(key => [key, pick(base.access[key], mine.access[key], theirs.access[key])]))
  const rebased: AgentDraft = {
    name: pick(base.name, mine.name, theirs.name),
    description: pick(base.description, mine.description, theirs.description),
    instructions: pick(base.instructions, mine.instructions, theirs.instructions),
    provider: pick(base.provider, mine.provider, theirs.provider),
    model: pick(base.model, mine.model, theirs.model),
    reasoning: pick(base.reasoning, mine.reasoning, theirs.reasoning),
    timeoutMinutes: pick(base.timeoutMinutes, mine.timeoutMinutes, theirs.timeoutMinutes),
    access: access as AgentAccess,
    exclusiveNodes: pick(base.exclusiveNodes, mine.exclusiveNodes, theirs.exclusiveNodes),
    plugins: pick(base.plugins, mine.plugins, theirs.plugins),
  }
  return JSON.parse(JSON.stringify(rebased))
}

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

/** Whether the agent may run on a node: listed, or every node allowed. */
export function runsOn(access: AgentAccess, node: string) {
  return access.nodes === null || access.nodes.includes(node)
}

/** The runtime can suspend other conversations for an exclusive reservation. */
export function supportsExclusive(node: ExecutionNode) {
  return node.exclusiveReservations === true
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
  for (const connection of tools)
    add('mcps', Number(JSON.stringify(savedTools[connection] ?? null) !== JSON.stringify(draftTools[connection] ?? null)))
  add('github', Number(saved.access.github !== draft.access.github))
  add('sandbox', Number(saved.access.sandbox !== draft.access.sandbox))
  add('powers', Number(saved.access.installation !== draft.access.installation))
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
 * secrets, and removed access when `runs` says work is affected. `githubToken` is the
 * pending change of the dedicated GitHub token, saved with the page.
 */
export function reviewLines(saved: AgentDraft, draft: AgentDraft, context: Context, runs = { queued: 0, running: 0 }, githubToken: 'set' | 'remove' | null = null) {
  const lines: ReviewLine[] = []
  const node = (id: string) => nodeName(context.nodes.find(item => item.id === id), id)

  for (const id of listChanges(saved.exclusiveNodes ?? [], draft.exclusiveNodes ?? []).added)
    lines.push({ kind: 'power', section: 'nodes', text: `Added: exclusive reservation on ${node(id)}. This agent will be able to suspend other agents’ conversations on this machine.` })
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
  if (saved.access.github && !draft.access.github)
    removed.push('shared GitHub connection')
  if (githubToken === 'remove')
    removed.push('dedicated GitHub token')
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
  if (saved.access.github && !draft.access.github && saved.access.projects === null && draft.access.projects !== null)
    lines.push({ kind: 'reduction', section: 'github', text: 'Shared GitHub connection turned off: selected projects require a dedicated token.' })
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

export interface Issue {
  section: SectionId
  text: string
}

/** What the owner should check: grants that exist but cannot be used, or targets that are gone. */
export function issues(draft: AgentDraft, context: Context, isMain: boolean) {
  const found: Issue[] = []
  const access = draft.access
  if (!isMain) {
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

  for (const id of draft.exclusiveNodes ?? []) {
    const node = context.nodes.find(item => item.id === id)
    if (!node || node.revoked)
      found.push({ section: 'nodes', text: 'An exclusive reservation targets a node that no longer exists.' })
    else if (!runsOn(access, id))
      found.push({ section: 'nodes', text: `The exclusive reservation on ${nodeName(node, id)} is inactive: this agent cannot run there.` })
  }

  for (const id of draft.plugins ?? []) {
    const connection = context.connections.find(item => item.id === id)
    if (!connection)
      found.push({ section: 'plugins', text: 'A granted plugin instance no longer exists.' })
    else if (!grantableConnection(connection, context.packages))
      found.push({ section: 'plugins', text: `${connectionLabel(id, context)} is unavailable: its plugin needs review or no longer gives agents access.` })
    else if (!connection.enabled)
      found.push({ section: 'plugins', text: `${connectionLabel(id, context)} is disabled: the access is kept but cannot be used.` })
  }

  return found
}

/** "3 projects · 2 nodes · 1Password" for the agents list. */
export function accessSummary(agent: Agent, context: Pick<Context, 'packages' | 'connections'>) {
  const access = agent.access ?? {} as AgentAccess
  const count = (value: string[] | null | undefined, noun: string, all: string) => value === null || value === undefined
    ? all
    : `${value.length || 'No'} ${noun}${value.length === 1 ? '' : 's'}`
  const nodes = access.nodes === undefined ? [LOCAL_NODE_ID] : access.nodes
  const parts = [
    agent.id === MAIN_AGENT_ID ? 'All projects' : count(access.projects, 'project', 'All projects'),
    count(nodes, 'node', 'All nodes'),
  ]
  const packages = new Set<string>()
  for (const id of agent.plugins ?? []) {
    const connection = context.connections.find(item => item.id === id)
    const name = context.packages.find(entry => entry.id === connection?.pluginId)?.manifest?.name
    packages.add(name ?? 'Plugin')
  }

  parts.push(...packages)
  if (agent.exclusiveNodes?.length)
    parts.push('Exclusive reservation')
  if (access.installation)
    parts.push('Installation management')
  return parts.join(' · ')
}

/** Guesses which section a server error belongs to, so the page can point at it. */
export function errorSection(message: string): SectionId | undefined {
  const text = message.toLowerCase()
  const rules: [SectionId, RegExp][] = [
    ['plugins', /plugin|1password|delegation/],
    ['nodes', /node|exclusive|reserv/],
    ['mcps', /mcp|tool/],
    ['skills', /skill/],
    ['github', /github|token/],
    ['projects', /project/],
    ['model', /model|provider|reasoning|codex|claude|opencode/],
    ['advanced', /time limit|timeout/],
    ['identity', /name|description|instruction/],
  ]
  return rules.find(([, pattern]) => pattern.test(text))?.[0]
}
