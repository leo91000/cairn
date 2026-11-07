import type { Deliverable } from './artifacts'
import type { ChatDetail, ChatView } from './chats'
import type { Run, RunEvent } from './contracts'
import type { LargeTimelineReference } from './generated/typescript/LargeTimelineReference'
import type { TimelineChange } from './generated/typescript/TimelineChange'

export interface LiveState {
  cacheRevision?: string
  run: Run | null
  chat: ChatDetail | null
  chats?: ChatView[]
  artifacts: Deliverable[]
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
  state?: LiveState
  /** Replaces the loaded history: an invalid cursor, another run, or a `recent=1` resume past a long backlog. */
  reset: boolean
  more: boolean
}

export type { HistoryPage } from './generated/typescript/HistoryPage'
export type { TimelineChange } from './generated/typescript/TimelineChange'
export type { TimelineEntry } from './generated/typescript/TimelineEntry'
