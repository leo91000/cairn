import type { ChatDetail, ChatMessage } from '../../../packages/contracts/chats'
import type { Run } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import { providers } from '../../../packages/contracts/accounts'

export interface SendingMessage {
  message: ChatMessage
  label: string
}

/** The wait a finished turn scheduled; the header counts it down, as `waitStatus` words it. */
export function scheduledWakeup(chat: ChatDetail | null | undefined) {
  if (chat?.paused || (chat?.lifecycle && chat.lifecycle !== 'active') || chat?.run?.status !== 'succeeded' || !chat.wakeup)
    return null
  return chat.wakeup
}

export function chatWaitNotice(run: Run | null | undefined) {
  const reason = run?.status === 'queued' ? run.accountWaitReason?.trim() : ''
  if (!reason)
    return null
  // Set when the user must connect, reconnect or resume an account of the run's coding agent.
  const provider = run?.accountRequired ?? null
  return {
    account: provider && providers[provider].label,
    message: provider ? `${reason} Your message is saved and will be sent then.` : reason,
  }
}

/** The dispatch queue is durable storage; only waiting follow-ups belong in the UI queue. */
export function chatDelivery(chat: ChatDetail | null, cards: TimelineEntry[], outgoing: ChatMessage | null = null) {
  const acknowledged = new Set(cards.flatMap(card => card.content.kind === 'userMessage' && card.content.messageId ? [card.content.messageId] : []))
  const messages = (chat?.messages ?? []).filter(message => message.status === 'queued' || message.status === 'sending')
  if (outgoing && !messages.some(message => message.id === outgoing.id))
    messages.push(outgoing)
  // As on Android, a run waiting on its background work is still active.
  const active = !!chat?.run && ['queued', 'running', 'waiting'].includes(chat.run.status)
  // A scheduled wait resumes the task before queued follow-ups start. After a turn that failed
  // or stopped, the next message starts a new one: what it never delivered came back unsent.
  const canStart = !chat?.paused && !active && !scheduledWakeup(chat)
  const first = messages[0]
  const sending: SendingMessage[] = []
  const queued: ChatMessage[] = []
  for (const message of messages) {
    if (acknowledged.has(message.id))
      continue
    const local = message.id === outgoing?.id && !chat?.messages.some(saved => saved.id === message.id)
    const starting = active && chat?.run?.chatExecution?.messageId === message.id
    const steering = active && message.mode === 'steer' && (!chat?.paused || !!message.questionId)
    if (local || starting || steering || (canStart && message === first)) {
      // Delivered question answers get their transcript text (including privacy
      // redaction) from the server; preserve genuinely waiting answers below.
      if (message.questionId)
        continue
      const waiting = chat?.run?.status === 'queued'
      const account = waiting ? chatWaitNotice(chat.run)?.account : undefined
      sending.push({ message, label: waiting ? (account ? `Waiting for a ${account} account` : 'Waiting for the agent…') : starting ? 'Starting agent…' : steering ? 'Sending to agent…' : 'Sending…' })
    }
    else {
      queued.push(message)
    }
  }

  return { sending, queued }
}

/** Messages a failed or stopped turn returned to this person, oldest first, not yet restored. */
export function returnedMessages(chat: ChatDetail | null | undefined, account: string, restored: ReadonlySet<string> = new Set()) {
  if (!chat || !account)
    return []
  return chat.messages.filter(message => message.status === 'cancelled' && message.returnedTo === account && !restored.has(message.id))
}

/**
 * The composer state a set of returned messages restores: their text ahead of the draft, and the
 * choices of the oldest one. A plan request stays one, so a returned plan never changes files.
 */
export function restoredComposer(returned: ChatMessage[], draft: string) {
  const first = returned[0]
  // A restore repeated after a reload does not add a block the draft already holds whole.
  const blocks = new Set(draft.split('\n\n').map(block => block.trim()))
  const texts = returned.map(message => message.text.trim()).filter(text => text && !blocks.has(text))
  return {
    text: [...texts, draft.trim()].filter(Boolean).join('\n\n'),
    attachments: returned.flatMap(message => message.attachments ?? []),
    provider: first?.provider ?? '',
    model: first?.model ?? '',
    reasoning: first?.reasoning ?? '',
    speed: first?.speed ?? '',
    plan: returned.some(message => message.plan === true),
  }
}

/** After a Stop, even one still settling, or a failed turn, the next message resumes an empty paused queue. */
export function resumesOnSend(chat: ChatDetail | null | undefined, queued: number) {
  const stopped = ['failed', 'interrupted', 'cancelled'].includes(chat?.run?.status ?? '') || !!chat?.run?.cancelRequestedAt
  // A turn message still unacknowledged when it stopped, or steering already claimed by it,
  // may be held for a replay first.
  const turn = chat?.run?.chatExecution?.messageId
  const starting = chat?.messages.some(message => message.status === 'sending' || (message.id === turn && message.status === 'queued'))
  return !!chat?.paused && !queued && stopped && !starting
}

/**
 * A failed conversation's reason, worded for what continues it: a held queue is resumed, as a
 * message written now would wait behind it (ADR-0091).
 */
export function failureReason(chat: ChatDetail | null | undefined, reason: string, readOnly = false) {
  // Someone without access to the agent cannot resume: they are told nothing to do.
  const held = !readOnly && !!chat?.paused && chat.messages.some(message => message.status === 'queued')
  return held ? `${reason.replace(/\s*Send a message to continue\.$/, '')} Resume the queue to continue.` : reason
}
