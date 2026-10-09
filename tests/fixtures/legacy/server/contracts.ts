// The oracle predates accounts shared by Codex and Claude Code: its runs still name
// their Codex account with these fields.
import type { Run as ApplicationRun } from '../../../../packages/contracts/contracts.ts'

export type * from '../../../../packages/contracts/contracts.ts'
export interface Run extends ApplicationRun {
  codexAccountId?: string | null
  codexAccountName?: string | null
}
