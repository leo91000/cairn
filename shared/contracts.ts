import { z } from 'zod'
import { LOCAL_NODE_ID } from './nodes'

export const name = z.string().trim().min(1).max(100)
export const id = z.string().uuid()
// Codex exposes effort names as strings, so new catalog levels need no app release.
export const reasoningEffort = z.string().max(40).regex(/^(?:[a-z][a-z0-9_-]*)?$/)
export { MAIN_AGENT_ID } from './constants'
export const accessPolicy = z.object({
  nodes: z.array(id).max(100).nullable().default(() => [LOCAL_NODE_ID]),
  projects: z.array(id).max(100).nullable().default(null),
  skills: z.array(z.string().max(160)).max(100).nullable().default(null),
  mcps: z.array(id).max(100).nullable().default(null),
  mcpTools: z.record(id, z.array(z.string().min(1).max(200)).max(500)).default({}),
  github: z.boolean().default(true),
  sandbox: z.enum(['yolo', 'workspace-write', 'read-only']).default('yolo'),
})
export const agentInput = z.object({
  name,
  provider: z.enum(['codex', 'claude']).default('codex'),
  description: z.string().max(500).default(''),
  model: z.string().max(100).default(''),
  reasoning: reasoningEffort.default('high'),
  instructions: z.string().max(20000).default(''),
  // Zero means no time limit; positive values are an optional execution budget.
  timeoutMinutes: z.number().int().min(0).max(720).default(0),
  access: accessPolicy.default(() => accessPolicy.parse({})),
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
  }).optional(),
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
}
export type RunStatus
  = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'interrupted'
export interface TaskOutcome {
  status: 'completed' | 'blocked' | 'needs_input'
  reason: string
  evidence: string[]
  reportedAt: number
  messageId?: string | null
}
export interface Run {
  nodeId?: string | null
  nodeState?: string | null
  pinnedNodeId?: string | null
  preferredNodeId?: string | null
  resources?: import('./nodes').NodeResources
  capacityWaitUntil?: number | null
  restoredAt?: number | null
  movementError?: string | null
  backup?: { id?: string, capturedAt?: number, status: string, error?: string, uploadedBytes?: number }

  outcome?: TaskOutcome | null
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
  workspaces?: { projectId: string, path: string, revision?: string | null, kind: 'worktree' | 'clone' | 'copy' | 'direct' }[]
  isolated?: boolean
  workspaceCleanedAt?: number
  snapshot: {
    task: Task
    agent: Agent
    project: Project | null
    projects?: Project[]
    skills: { name: string, path: string, content: string }[]
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
}

export interface GithubRepository {
  fullName: string
  name: string
  description: string
  defaultBranch: string
  private: boolean
  archived: boolean
  fork: boolean
  owner: string
  language: string
  stars: number
  pushedAt: string
  imported: boolean
}
export interface GithubRepositoryPage {
  repositories: GithubRepository[]
  nextPage: number | null
}
