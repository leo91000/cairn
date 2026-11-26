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
  if (!questions.length) {
    return canAsk
      ? {
          kind: 'idle',
          label: 'Ask a side question',
          badge: null,
          badgeTone: null,
        }
      : null
  }

  const pending = questions.some(question => question.status === 'pending')
  let answers = 0
  let failures = 0
  for (const question of questions) {
    if (seen.has(question.id))
      continue
    if (question.status === 'answered')
      answers++
    if (question.status === 'failed')
      failures++
  }

  const engine = provider ? providers[provider].label : 'The agent'
  const noun = questions.length === 1 ? 'question' : 'questions'
  const status = [
    pending ? `${engine} is answering` : '',
    answers ? `Answer ready (${answers})` : '',
    failures ? `Not answered (${failures})` : '',
  ].filter(Boolean).join(' · ')

  if (answers) {
    return {
      kind: pending ? 'pending' : 'ready',
      label: `Side ${noun} · ${status}`,
      badge: formatBadgeCount(answers),
      badgeTone: 'accent',
    }
  }

  if (failures) {
    return {
      kind: pending ? 'pending' : 'failed',
      label: `Side ${noun} · ${status}`,
      badge: '!',
      badgeTone: 'danger',
    }
  }

  if (pending) {
    return {
      kind: 'pending',
      label: `Side ${noun} · ${status}`,
      badge: null,
      badgeTone: null,
    }
  }

  return {
    kind: 'saved',
    label: `Side ${noun} · Saved (${questions.length})`,
    badge: formatBadgeCount(questions.length),
    badgeTone: 'neutral',
  }
}
