import type { Deliverable } from './artifacts'
import type { ChatDetail, ChatView } from './chats'
import type { Run, RunEvent } from './contracts'
import type { LargeTimelineReference } from './generated/typescript/LargeTimelineReference'
import type { TimelineChange } from './generated/typescript/TimelineChange'

export interface LiveState {
  planTurnsEnabled?: boolean
  /** The run's conversation has a pending question, queued input or coding agent change. */
  workspaceCleanupBlockedByInput?: boolean
  /** The trash revision clients without `compact=1` follow. */
  cacheRevision?: string
  /** Compact streams only: the saved-history revision, which trashing leaves unchanged. */
  historyRevision?: string
  run: Run | null
  chat: ChatDetail | null
  chats?: ChatView[]
  artifacts: Deliverable[]
}
/** List changes on a compact stream (`compact=1`), applied to the list it sent first. */
export interface ChatChanges {
  /** New conversations and conversations whose summary changed, in full. */
  upserts: ChatView[]
  /** Conversations that left the list. */
  removed: string[]
  /** Every listed id in order, when the order changed beyond the removals. */
  order?: string[]
}
export interface LiveBatch {
  oldest?: number
  hasOlder?: boolean
  /** Append-only history identity and retention revision for conversation streams. */
  history?: string
  events: RunEvent[]
  /** Timeline cards: the window's snapshot, or the cards these events changed. */
  entries?: TimelineChange[]
  largeEntries?: LargeTimelineReference[]
  /**
   * On a compact stream the state omits an unchanged list, which these changes update,
   * and its chat omits `run`, which is the state's own `run`.
   */
  state?: LiveState
  chatChanges?: ChatChanges
  /** Replaces the loaded history: an invalid cursor, another run, or a `recent=1` resume past a long backlog. */
  reset: boolean
  more: boolean
}

export type { HistoryPage } from './generated/typescript/HistoryPage'
export type { TimelineChange } from './generated/typescript/TimelineChange'
export type { TimelineEntry } from './generated/typescript/TimelineEntry'
