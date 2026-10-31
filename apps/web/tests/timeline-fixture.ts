import type { ChatWakeup } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import type { TimelineNotice } from '../../../packages/contracts/generated/typescript/TimelineNotice'
import type { TimelineEntry } from '../../../packages/contracts/live'
import type { ActivityGroups } from '../src/activity'
import { activityEntries } from '../src/activity'

/**
 * Test fixture for families the timeline does not convert yet (plans, subagents, waits, tools):
 * it stands in for the server with the cards those tests need as boundaries — messages and
 * plain lifecycle notices. The server's own folding is qualified by the Rust reducer tests and
 * the shared `timeline-corpus.json`, never re-derived here.
 */
export function withTimeline(input: RunEvent[]): { events: RunEvent[], cards: TimelineEntry[] } {
  const events: RunEvent[] = []
  const cards = new Map<string, TimelineEntry>()
  let turn = 0
  const card = (event: RunEvent, id: string, content: TimelineEntry['content']): TimelineEntry => ({
    id,
    runId: event.runId,
    position: event.id,
    revision: event.id,
    source: event.id,
    turn,
    attempt: 0,
    createdAt: event.createdAt,
    updatedAt: event.createdAt,
    content,
  })
  const notice = (event: RunEvent, value: Partial<TimelineNotice>) => {
    cards.set(`notice:${event.id}`, card(event, `notice:${event.id}`, {
      kind: 'notice',
      notice: {
        code: 'runStatus',
        state: 'done',
        prominent: false,
        ...value,
      },
    }))
  }

  for (const event of input) {
    const payload = (event.type === 'diagnostic' ? {} : event.payload ?? {}) as Record<string, unknown>
    const item = (payload.item ?? {}) as Record<string, unknown>
    if (event.type === 'turn.started')
      turn++
    if (event.type.startsWith('item.') && item.type === 'agent_message') {
      const id = typeof item.id === 'string' && item.id ? `message:${turn}:${item.id}` : `message:${event.id}`
      const text = typeof item.text === 'string' && item.text ? item.text : event.text
      const previous = cards.get(id)
      cards.set(id, {
        ...(previous ?? card(event, id, {
          kind: 'agentMessage',
          itemId: String(item.id ?? ''),
          text,
          state: 'running',
        })),
        revision: event.id,
        source: event.id,
        content: {
          kind: 'agentMessage',
          itemId: String(item.id ?? ''),
          text,
          state: event.type === 'item.completed' ? 'done' : 'running',
        },
      })
      continue
    }

    const covered = event.type === 'chat.user' || event.type === 'status' || event.type === 'diagnostic' || event.type === 'error' || event.type.startsWith('turn.') || event.type === 'thread.started' || event.type === 'runner.failed'
    events.push(covered ? { ...event, timeline: true } : event)
    if (event.type === 'chat.user') {
      const sentAt = typeof payload.createdAt === 'number' && payload.createdAt > 0 && payload.createdAt <= event.createdAt ? payload.createdAt : event.createdAt
      cards.set(`user:${event.id}`, card(event, `user:${event.id}`, {
        kind: 'userMessage',
        text: typeof payload.text === 'string' ? payload.text : event.text,
        ...(typeof payload.messageId === 'string' ? { messageId: payload.messageId } : {}),
        ...(typeof payload.questionId === 'string' ? { questionId: payload.questionId } : {}),
        sentAt,
        attachments: [],
      }))
    }
    else if (event.type === 'turn.waiting') {
      const source = Array.isArray(payload.tasks) ? payload.tasks : []
      const tasks = source.map(task => String((task as Record<string, unknown>).description ?? '').trim())
      const taskIds = source.map(task => String((task as Record<string, unknown>).id ?? ''))
      notice(event, {
        code: 'backgroundTasks',
        state: 'info',
        tasks,
        taskIds,
        ...(tasks.length ? { waitingSince: event.createdAt } : {}),
      })
    }
    else if (['turn.failed', 'runner.failed', 'error'].includes(event.type) || (event.type === 'status' && event.text === 'failed')) {
      const error = (payload.error ?? {}) as Record<string, unknown>
      const detail = String(error.message ?? payload.message ?? (event.type === 'status' ? '' : event.text))
      notice(event, {
        code: 'failure',
        state: 'error',
        prominent: true,
        origin: event.type === 'status' ? 'run' : event.type === 'runner.failed' ? 'runner' : event.type === 'error' ? 'engine' : 'turn',
        ...(detail ? { detail } : {}),
      })
    }
    else if (event.type === 'status') {
      notice(event, {
        code: 'runStatus',
        status: event.text,
        prominent: ['cancelled', 'interrupted'].includes(event.text),
        state: ['cancelled', 'interrupted'].includes(event.text) ? 'stopped' : 'done',
      })
    }
    else if (covered) {
      notice(event, { code: event.type === 'turn.started' ? 'turnStarted' : event.type === 'turn.completed' ? 'turnCompleted' : event.type === 'thread.started' ? 'sessionStarted' : 'diagnostic', ...(event.type === 'diagnostic' ? { detail: event.text, state: 'info' } : {}) })
    }
  }

  return { events, cards: [...cards.values()].sort((a, b) => a.position - b.position) }
}

/** `activityEntries` over raw test events, with the stand-in cards above. */
export function fixtureEntries(input: RunEvent[], chat = false, groups?: ActivityGroups, pendingWakeup?: ChatWakeup | null) {
  const { events, cards } = withTimeline(input)
  return activityEntries(events, chat, groups, pendingWakeup, cards)
}
