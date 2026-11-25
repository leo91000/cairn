import type { Provider } from '../../../packages/contracts/accounts'
import type { ChatSideQuestion } from '../../../packages/contracts/chats'
import { providers } from '../../../packages/contracts/accounts'

export type SideQuestionIndicatorKind = 'idle' | 'pending' | 'ready' | 'failed' | 'saved'

export type SideQuestionBadgeTone = 'accent' | 'danger' | 'neutral'

export interface SideQuestionIndicator {
  kind: SideQuestionIndicatorKind
  label: string
  badge: string | null
  badgeTone: SideQuestionBadgeTone | null
  hasQuestions: boolean
}

function formatBadgeCount(count: number): string {
  return count > 99 ? '99+' : String(count)
}

/**
 * Summarizes the icon-only side-question trigger state, accessible label and badge from the
 * conversation's retained side questions and unread settled answers.
 */
export function sideQuestionIndicator(input: {
  questions: ChatSideQuestion[]
  seen: ReadonlySet<string>
  canAsk: boolean
  provider?: Provider | null
}): SideQuestionIndicator | null {
  const {
    questions,
    seen,
    canAsk,
    provider,
  } = input
  const hasQuestions = questions.length > 0
  if (!hasQuestions && !canAsk)
    return null

  const pending = questions.filter(question => question.status === 'pending')
  const unread = questions.filter(
    question => question.status !== 'pending' && question.status !== 'cancelled' && !seen.has(question.id),
  )
  const isPending = pending.length > 0
  const hasUnreadAnswer = unread.some(question => question.status === 'answered')
  const hasUnreadFailure = !hasUnreadAnswer && unread.length > 0
  const engine = provider ? providers[provider].label : 'The agent'
  const noun = questions.length > 1 ? 'questions' : 'question'

  if (!hasQuestions) {
    return {
      kind: 'idle',
      label: 'Ask a side question',
      badge: null,
      badgeTone: null,
      hasQuestions: false,
    }
  }

  const badge = hasUnreadAnswer
    ? formatBadgeCount(unread.length)
    : hasUnreadFailure
      ? '!'
      : !isPending
          ? formatBadgeCount(questions.length)
          : null
  const badgeTone: SideQuestionBadgeTone | null = hasUnreadAnswer
    ? 'accent'
    : hasUnreadFailure
      ? 'danger'
      : !isPending
          ? 'neutral'
          : null

  if (isPending) {
    return {
      kind: 'pending',
      label: `Side ${noun} · ${engine} is answering`,
      badge,
      badgeTone,
      hasQuestions: true,
    }
  }

  if (hasUnreadAnswer) {
    return {
      kind: 'ready',
      label: `Side ${noun} · Answer ready`,
      badge,
      badgeTone,
      hasQuestions: true,
    }
  }

  if (hasUnreadFailure) {
    return {
      kind: 'failed',
      label: `Side ${noun} · Not answered`,
      badge,
      badgeTone,
      hasQuestions: true,
    }
  }

  return {
    kind: 'saved',
    label: `Side ${noun} · Saved`,
    badge,
    badgeTone,
    hasQuestions: true,
  }
}
