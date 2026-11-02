import type { RunEvent } from '../../../packages/contracts/contracts'
import type { TimelineChange, TimelineEntry } from '../../../packages/contracts/live'

/**
 * A stream's events and timeline cards, with its replay cursor. Cards arrive already
 * interpreted by the server: a newer revision replaces an older one, and a streaming
 * reply may only send the text it appends to the copy this stream delivered.
 */
export class LiveTimeline {
  cursor = 0
  events: RunEvent[] = []
  cards = new Map<string, TimelineEntry>()
  private streamCards = new Map<string, TimelineEntry>()

  copy() {
    const next = new LiveTimeline()
    next.cursor = this.cursor
    next.events = this.events.slice()
    next.cards = new Map(this.cards)
    next.streamCards = new Map(this.streamCards)
    return next
  }

  restore(events: RunEvent[], cursor: number, cards: TimelineEntry[]) {
    this.cursor = cursor
    this.events = events.slice()
    this.cards = new Map(cards.map(card => [card.id, card]))
    this.streamCards = new Map(this.cards)
  }

  /** Applies a live batch: events after the cursor, then the cards they changed. */
  apply(events: RunEvent[], changes: TimelineChange[] = [], cursor = this.cursor) {
    for (const event of events) {
      if (event.id <= this.cursor)
        continue
      this.events.push(event)
      this.cursor = event.id
    }

    for (const change of changes) {
      if (change.kind === 'upsert') {
        const previous = this.streamCards.get(change.entry.id)
        if (previous && previous.revision >= change.entry.revision)
          continue
        this.streamCards.set(change.entry.id, change.entry)
        this.upsert(change.entry)
        continue
      }

      const card = this.streamCards.get(change.id)
      if (card?.content.kind !== 'agentMessage')
        throw new Error('Missing streamed message baseline')
      if (change.revision <= card.revision)
        continue
      const updated = {
        ...card,
        revision: change.revision,
        source: change.source,
        updatedAt: change.updatedAt,
        content: { ...card.content, text: card.content.text + change.text },
      }
      this.streamCards.set(change.id, updated)
      this.upsert(updated)
    }

    this.cursor = Math.max(this.cursor, cursor)
  }

  /** Joins an older history page; neither its events nor its cards replace newer copies. */
  merge(events: RunEvent[], cards: TimelineEntry[]) {
    this.events = [...new Map([...events, ...this.events].map(event => [event.id, event])).values()].sort((a, b) => a.id - b.id)
    for (const card of cards)
      this.upsert(card)
  }

  upsert(card: TimelineEntry) {
    const current = this.cards.get(card.id)
    if (!current || current.revision <= card.revision)
      this.cards.set(card.id, card)
  }

  /** Cards in the order they first appeared. */
  entries() {
    return [...this.cards.values()].sort((a, b) => a.position - b.position || a.id.localeCompare(b.id))
  }
}
