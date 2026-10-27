import { z } from 'zod'

export const name = z.string().trim().min(1).max(100)
export const id = z.string().uuid()
// Codex exposes effort names as strings, so new catalog levels need no app release.
export const reasoningEffort = z.string().max(40).regex(/^(?:[a-z][a-z0-9_-]*)?$/)
// A Codex service tier id from the model catalog, `standard`, or empty to inherit.
// Tiers are catalog strings too, so a new speed needs no app release.
export const serviceSpeed = z.string().max(40).regex(/^(?:[a-z][a-z0-9_-]*)?$/)
export { MAIN_AGENT_ID } from './constants'
export type { Agent } from './generated/typescript/Agent'
export type { AgentAvatar } from './generated/typescript/AgentAvatar'
export type { Mission as Task } from './generated/typescript/Mission'
export type { Project } from './generated/typescript/Project'
export type { RunEvent } from './generated/typescript/RunEvent'
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
/** Complete run details have a snapshot; list projections use RunListItem. */
export type Run = import('./generated/typescript/Run').Run & { snapshot: import('./generated/typescript/RunSnapshot').RunSnapshot, summary: string }
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
export type { RunStatus } from './generated/typescript/RunStatus'

export type RunListItem = Omit<import('./generated/typescript/Run').Run, 'snapshot' | 'summary'>

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
