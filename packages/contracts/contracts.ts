import { z } from 'zod'
import { LOCAL_NODE_ID } from './nodes'

export const name = z.string().trim().min(1).max(100)
export const id = z.string().uuid()
// Codex exposes effort names as strings, so new catalog levels need no app release.
export const reasoningEffort = z.string().max(40).regex(/^(?:[a-z][a-z0-9_-]*)?$/)
export { MAIN_AGENT_ID } from './constants'
export const accessPolicy = z.object({
  backend: z.enum(['firecracker', 'native']).default('firecracker'),
  nodes: z.array(id).max(100).nullable().default(() => [LOCAL_NODE_ID]),
  projects: z.array(id).max(100).nullable().default(null),
  skills: z.array(z.string().max(160)).max(100).nullable().default(null),
  mcps: z.array(id).max(100).nullable().default(null),
  mcpTools: z.record(id, z.array(z.string().min(1).max(200)).max(500)).default({}),
  installation: z.boolean().default(false),
  sandbox: z.enum(['yolo', 'workspace-write', 'read-only']).default('yolo'),
})
export const agentInput = z.object({
  name,
  provider: z.enum(['codex', 'claude', 'opencode']).default('codex'),
  description: z.string().max(500).default(''),
  model: z.string().max(100).default(''),
  reasoning: reasoningEffort.default('high'),
  instructions: z.string().max(20000).default(''),
  // Zero means no time limit; positive values are an optional execution budget.
  timeoutMinutes: z.number().int().min(0).max(720).default(0),
  access: accessPolicy.default(() => accessPolicy.parse({})),
  // Live grants, checked on every use rather than captured with a run, and kept out of
  // `access` so earlier releases compare the same policy: nodes this agent may reserve
  // exclusively (suspending other conversations) and plugin instances such as 1Password
  // service accounts.
  exclusiveNodes: z.array(id).max(100).default([]),
  plugins: z.array(id).max(100).default([]),
  // An acknowledgement for this save, removed before persistence; never a profile mode.
  nativeNodeReviews: z.array(id).max(100).optional().describe('Owner confirmation of specifically reviewed administrator node IDs already covered by access.nodes, including wildcard access. Reviews only these nodes while preserving the placement selection. These grant unrestricted passwordless sudo on the real host, including access to locally present credentials and services. Never infer confirmation from an all-node grant or conversion. Not saved on the agent; delegated installation agents cannot approve it.'),
})
export const agentUpdate = z.object({
  provider: agentInput.shape.provider.removeDefault().optional(),
  name: agentInput.shape.name.optional(),
  description: agentInput.shape.description.removeDefault().optional(),
  model: agentInput.shape.model.removeDefault().optional(),
  reasoning: agentInput.shape.reasoning.removeDefault().optional(),
  instructions: agentInput.shape.instructions.removeDefault().optional(),
  timeoutMinutes: agentInput.shape.timeoutMinutes.removeDefault().optional(),
  access: accessPolicy.extend({
    // A partial update from an older client must not restore local-runner access.
    nodes: accessPolicy.shape.nodes.removeDefault().optional(),
    backend: accessPolicy.shape.backend.removeDefault().optional(),
    installation: accessPolicy.shape.installation.removeDefault().optional(),
  }).optional(),
  exclusiveNodes: agentInput.shape.exclusiveNodes.removeDefault().optional(),
  plugins: agentInput.shape.plugins.removeDefault().optional(),
  nativeNodeReviews: agentInput.shape.nativeNodeReviews,
})
export const projectInput = z.object({
  name,
  path: z.string().min(1).max(2000),
  description: z.string().max(500).default(''),
  sourceMode: z.enum(['remote', 'local']).default('remote'),
  baseBranch: z
    .string()
    .regex(/^[a-z0-9][\w/.-]*$/i)
    .default('main'),
})
export const taskInput = z.object({
  name,
  prompt: z.string().trim().min(1).max(50000),
  agentId: id,
  projectId: id.nullable().default(null),
  skills: z.array(z.string().max(160)).max(100).nullable().default(null),
  tags: z.array(z.string().max(30)).max(10).default([]),
  cron: z.string().max(100).nullable().default(null),
  timezone: z.string().max(100).default('Europe/Paris'),
  enabled: z.boolean().default(true),
  archived: z.boolean().default(false),
  worktree: z.boolean().default(true),
})
export interface AgentAvatar {
  status: 'generating' | 'ready' | 'failed'
  revision: string
  url?: string | null
  error?: string
}
export type Agent = z.infer<typeof agentInput> & {
  avatar?: AgentAvatar
  id: string
  createdAt: number
  // Changes on every save; sent back so a stale editor cannot overwrite newer access.
  revision?: string
}
export type Project = z.infer<typeof projectInput> & {
  origin?: string
  id: string
  createdAt: number
}
export type Task = z.infer<typeof taskInput> & {
  id: string
  createdAt: number
  nextRun: number | null
  authorId?: string | null
  authorRemoved?: boolean | null
  scheduleWaitReason?: string | null
  /** The latest scheduled occurrence that could not start; cleared by the next admitted one. */
  scheduleFailure?: { message: string, scheduledFor: number, at: number } | null
}
export type RunStatus
  = 'queued' | 'running' | 'waiting' | 'succeeded' | 'failed' | 'cancelled' | 'interrupted'
/** Who must act next on work that is blocked or needs input; the user when absent. */
export type OutcomeWaitingOn = 'user' | 'requester' | 'external'
export interface TaskOutcome {
  status: 'completed' | 'blocked' | 'needs_input'
  reason: string
  waitingOn?: OutcomeWaitingOn
  evidence: string[]
  reportedAt: number
  messageId?: string | null
}
export interface Run {
  /** A durable mission wait; the run keeps its active task slot. */
  wakeup?: import('./chats').ChatWakeup | null
  executionBackend?: 'firecracker' | 'native' | null
  suspendedByExclusive?: import('./nodes').ExclusiveReservation | null
  retry?: {
    attempt: number
    limit: number
    nextAttemptAt: number | null
    cause: { kind: 'connection' | 'timeout' | 'rate-limit' | 'server', message: string }
  } | null
  nodeId?: string | null
  storage?: {
    mode: string
    waitingFor?: string | null
    dirtyBytes?: number
    localBytes?: number
    dirtySince?: number | null
    freeBytes?: number
    reserveBytes?: number
  }
  nativeMode?: import('./nodes').NativeModeRequest | null
  nodeState?: string | null
  pinnedNodeId?: string | null
  preferredNodeId?: string | null
  resources?: import('./nodes').NodeResources
  capacityWaitUntil?: number | null
  restoredAt?: number | null
  movementError?: string | null
  backup?: {
    id?: string
    capturedAt?: number
    status: string
    error?: string
    uploadedBytes?: number
  }

  outcome?: TaskOutcome | null
  /** The conversation or mission whose agent started this run. */
  requestedBy?: { runId: string, chatId?: string, taskId?: string } | null
  chatExecution?: import('./chats').ChatExecution
  recoveryPending?: boolean
  resumeAvailable?: boolean
  resumeCount?: number
  cancelRequestedAt?: number | null
  accountId?: string | null
  accountName?: string | null
  accountWaitReason?: string | null
  /** The coding agent whose accounts need the user in Connections before this run can start. */
  accountRequired?: import('./accounts').Provider | null
  id: string
  taskId: string
  /** Original mission when its execution was transferred to a chat. */
  sourceTaskId?: string
  projectId: string | null
  status: RunStatus
  trigger: string
  createdAt: number
  startedAt: number | null
  finishedAt: number | null
  summary: string
  error?: string | null
  sessionId: string | null
  workspace: string | null
  workspaces?: {
    projectId: string
    path: string
    revision?: string | null
    kind: 'worktree' | 'clone' | 'copy' | 'direct'
  }[]
  isolated?: boolean
  workspaceCleanedAt?: number
  snapshot: {
    task: Task
    agent: Agent
    project: Project | null
    projects?: Project[]
    skills: {
      name: string
      path: string
      content: string
      bundle?: string
    }[]
  }
  usage: Record<string, number> | null
}
export interface Skill {
  name: string
  description: string
  scope: string
  content: string
  path: string
  valid: boolean
  error?: string
  files?: number
  bytes?: number
  executables?: number
  source?: SkillSource | null
}
// Multi-file skill bundles; see docs/SKILLS.md.
/** Where a skill was imported from: a plugin's skill source, at a version. */
export interface SkillSource {
  pluginId: string
  /** The plugin's skill source name. */
  source: string
  instance?: string | null
  /** What the plugin shows for the location, such as a repository. */
  label: string
  /** The location as the plugin resolved it, such as a repository and a ref. */
  location: Record<string, unknown>
  url?: string | null
  /** The skill folder within the location. */
  folder: string
  version: string
  importedAt: number
  modified: boolean
}
export interface SkillFile {
  path: string
  size: number
  executable: boolean
  text: boolean
}
export interface SkillLimits {
  fileBytes: number
  bundleBytes: number
  files: number
  uploadBytes: number
}
export interface SkillBundle {
  scope: string
  name: string
  revision: string
  files: SkillFile[]
  limits: SkillLimits
  source: SkillSource | null
  description?: string | null
}
export interface SkillFileContent {
  content: string
  encoding: 'utf-8' | 'base64'
  size: number
  executable: boolean
}
export type SkillChange
  = | {
    op: 'write'
    path: string
    content?: string
    encoding?: 'utf-8' | 'base64'
    upload?: string
    executable?: boolean
  }
  | { op: 'delete', path: string }
  | { op: 'move', from: string, to: string }
  | { op: 'mode', path: string, executable: boolean }
export interface SkillImport {
  imported: SkillBundle | null
  candidates: string[]
}
export interface SkillArchiveChunk {
  filename: string
  revision: string
  size: number
  offset: number
  content: string
}
export interface SkillDiffLine {
  kind: 'context' | 'added' | 'removed' | 'gap'
  text: string
}
export interface SkillSourceChange {
  path: string
  status: 'added' | 'removed' | 'modified'
  binary: boolean
  lines?: SkillDiffLine[]
}
export interface SkillSourceStatus {
  source: SkillSource
  latest: { version: string, message: string, date: string }
  current: boolean
  changes: SkillSourceChange[]
}
export interface RunEvent {
  id: number
  runId: string
  createdAt: number
  type: string
  text: string
  payload?: Record<string, unknown>
}

export type RunListItem = Omit<Run, 'snapshot' | 'summary'> & {
  taskName: string
  agentName: string
  provider?: import('./accounts').Provider
  /** A mission run that finished after the signed-in account last opened its mission. */
  unread?: boolean
  /** The opening of the agent's final message, while unread. */
  preview?: string | null
  /** The signed-in account set this run's outcome aside: dismissed, or snoozed `until`. */
  dismissal?: { until: number | null } | null
}

/** A project a plugin's project source offers for import. */
export interface PluginProject {
  id: string
  name: string
  /** The name a project imported from this item takes. */
  projectName?: string
  description: string
  defaultBranch: string
  badges: string[]
  updatedAt?: string | null
  url?: string | null
  imported: boolean
}
export interface PluginProjectPage {
  items: PluginProject[]
  nextPage: number | null
}
