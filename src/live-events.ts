import type { RunEvent } from '../shared/contracts'

/**
 * Fold intermediate text snapshots in place while retaining a separate replay
 * cursor. Completed messages and tool ordering survive refresh and reconnection.
 */
export class LiveEvents {
  cursor = 0
  private messages = new Map<string, number>()

  copy() {
    const next = new LiveEvents()
    next.cursor = this.cursor
    next.messages = new Map(this.messages)
    return next
  }

  restore(events: RunEvent[], cursor: number) {
    this.messages.clear()
    this.cursor = cursor
    events.forEach((event, index) => {
      if (event.type === 'turn.started')
        this.messages.clear()
      const item = event.payload?.item as { id?: string, type?: string } | undefined
      if (item?.type === 'agent_message' && item.id)
        this.messages.set(item.id, index)
    })
  }

  append(target: RunEvent[], incoming: RunEvent[]) {
    for (let event of incoming) {
      if (event.id <= this.cursor)
        continue
      if (event.type === 'turn.started')
        this.messages.clear()
      const item = event.payload?.item as { id?: string, type?: string, delta?: string } | undefined
      if (item?.type !== 'agent_message' || !item.id) {
        target.push(event)
        this.cursor = event.id
        continue
      }

      const index = this.messages.get(item.id)
      if (typeof item.delta === 'string') {
        if (index === undefined)
          throw new Error('Missing streamed message baseline')
        const previous = target[index]
        const original = previous.payload?.item as { text?: string } | undefined
        const text = (original?.text ?? previous.text) + item.delta
        event = { ...event, text, payload: { ...event.payload, item: { ...item, delta: undefined, text } } }
      }

      this.cursor = event.id
      if (index !== undefined) {
        // Preserve when and where the message first appeared, as ActivityFeed does.
        target[index] = { ...event, createdAt: target[index].createdAt }
        continue
      }

      this.messages.set(item.id, target.length)
      target.push(event)
    }
  }
}
