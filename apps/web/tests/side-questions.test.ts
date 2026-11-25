import type { ChatSideQuestion } from '../../../packages/contracts/chats'
import { describe, expect, it } from 'vitest'
import { sideQuestionIndicator } from '../src/side-questions'

function question(id: string, status: ChatSideQuestion['status']): ChatSideQuestion {
  return {
    id,
    chatId: 'chat-1',
    runId: 'run-1',
    text: `Question ${id}`,
    quote: '',
    itemId: '',
    replyEventId: '',
    provider: 'claude',
    status,
    answer: status === 'answered' ? 'Answer' : null,
    error: status === 'failed' ? 'Ended before answering' : null,
    createdAt: 1,
    settledAt: status === 'pending' ? null : 2,
  }
}

describe('sideQuestionIndicator', () => {
  it('hides when no side questions exist and asking is unavailable', () => {
    expect(sideQuestionIndicator({
      questions: [],
      seen: new Set(),
      canAsk: false,
      provider: 'claude',
    })).toBeNull()
  })

  it('offers an idle icon button when asking is available with no questions yet', () => {
    expect(sideQuestionIndicator({
      questions: [],
      seen: new Set(),
      canAsk: true,
      provider: 'claude',
    })).toEqual({
      kind: 'idle',
      label: 'Ask a side question',
      badge: null,
      badgeTone: null,
      hasQuestions: false,
    })
  })

  it('shows the pending state while answering and carries any unread badge alongside it', () => {
    expect(sideQuestionIndicator({
      questions: [question('q1', 'pending')],
      seen: new Set(),
      canAsk: true,
      provider: 'claude',
    })).toEqual({
      kind: 'pending',
      label: 'Side question · Claude Code is answering',
      badge: null,
      badgeTone: null,
      hasQuestions: true,
    })

    expect(sideQuestionIndicator({
      questions: [question('q1', 'answered'), question('q2', 'pending')],
      seen: new Set(),
      canAsk: true,
      provider: 'antigravity',
    })).toEqual({
      kind: 'pending',
      label: 'Side questions · Antigravity is answering',
      badge: '1',
      badgeTone: 'accent',
      hasQuestions: true,
    })
  })

  it('highlights unread answers and failures before falling back to saved count', () => {
    const failed = question('q1', 'failed')
    const answered = question('q2', 'answered')
    const cancelled = question('q3', 'cancelled')

    expect(sideQuestionIndicator({
      questions: [failed],
      seen: new Set(),
      canAsk: false,
      provider: 'claude',
    })).toEqual({
      kind: 'failed',
      label: 'Side question · Not answered',
      badge: '!',
      badgeTone: 'danger',
      hasQuestions: true,
    })

    expect(sideQuestionIndicator({
      questions: [failed, answered],
      seen: new Set(),
      canAsk: false,
      provider: 'claude',
    })).toEqual({
      kind: 'ready',
      label: 'Side questions · Answer ready',
      badge: '2',
      badgeTone: 'accent',
      hasQuestions: true,
    })

    expect(sideQuestionIndicator({
      questions: [failed, answered, cancelled],
      seen: new Set(['q1', 'q2']),
      canAsk: false,
      provider: 'claude',
    })).toEqual({
      kind: 'saved',
      label: 'Side questions · Saved',
      badge: '3',
      badgeTone: 'neutral',
      hasQuestions: true,
    })
  })

  it('caps badge counts at 99+', () => {
    const questions = Array.from({ length: 105 }, (_, i) => question(`q${i}`, 'answered'))
    const seen = new Set(questions.map(item => item.id))

    expect(sideQuestionIndicator({
      questions,
      seen,
      canAsk: false,
      provider: 'claude',
    })?.badge).toBe('99+')
  })
})
