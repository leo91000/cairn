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
export type { PluginProject } from './generated/typescript/PluginProject'
export type { PluginProjectPage } from './generated/typescript/PluginProjectPage'
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
export type { Project } from './generated/typescript/Project'
export type { RunEvent } from './generated/typescript/RunEvent'
export type { RunStatus } from './generated/typescript/RunStatus'
export type { Skill } from './generated/typescript/Skill'
export type { SkillArchiveChunk } from './generated/typescript/SkillArchiveChunk'
export type { SkillBundle } from './generated/typescript/SkillBundle'
export type { SkillChange } from './generated/typescript/SkillChange'
export type { SkillDiffLine } from './generated/typescript/SkillDiffLine'
export type { SkillFile } from './generated/typescript/SkillFile'
export type { SkillFileContent } from './generated/typescript/SkillFileContent'
export type { SkillImport } from './generated/typescript/SkillImport'
export type { SkillLimits } from './generated/typescript/SkillLimits'

export type { SkillSource } from './generated/typescript/SkillSource'

export type RunListItem = Omit<import('./generated/typescript/Run').Run, 'snapshot' | 'summary'>

export type { SkillSourceChange } from './generated/typescript/SkillSourceChange'
export type { SkillSourceStatus } from './generated/typescript/SkillSourceStatus'
