import type { LargeTimelineReference } from '../../../packages/contracts/generated/typescript/LargeTimelineReference'
import type { ReadLargeTimelineInput } from '../../../packages/contracts/generated/typescript/ReadLargeTimelineInput'
import type { RunContentOutput } from '../../../packages/contracts/generated/typescript/RunContentOutput'
import type { TimelineEntry } from '../../../packages/contracts/generated/typescript/TimelineEntry'
import { api } from './api'

/** Resolve complete cards before committing a page or a stream cursor. */
export async function readLargeTimeline(path: string, previews: LargeTimelineReference[], signal?: AbortSignal): Promise<TimelineEntry[]> {
  const cards: TimelineEntry[] = []
  for (const preview of previews) {
    const snapshot = preview.snapshot
    const chunks: string[] = []
    let offset = 0
    let sha256: string | undefined
    let total: number | undefined
    for (;;) {
      const request: ReadLargeTimelineInput = { snapshot, offset, sha256: sha256 ?? null }
      const part = await api<RunContentOutput>(`${path.replace(/\/stream$/, '')}/timeline-content`, {
        method: 'POST',
        body: JSON.stringify(request),
        signal,
      })
      if (part.entryId !== snapshot.id || part.runId !== snapshot.runId || part.offset !== offset || (sha256 && part.sha256 !== sha256) || (total !== undefined && part.totalBytes !== total))
        throw new Error('Timeline content changed while loading')
      sha256 = part.sha256
      total = part.totalBytes
      chunks.push(part.data)
      const end = offset + new TextEncoder().encode(part.data).length
      if (end > total || (part.nextOffset === null ? end !== total : part.nextOffset !== end))
        throw new Error('Invalid timeline content length')
      if (part.nextOffset === null)
        break
      if (part.nextOffset <= offset || part.nextOffset > total)
        throw new Error('Invalid timeline content cursor')
      offset = part.nextOffset
    }

    const card = JSON.parse(chunks.join('')) as TimelineEntry
    if (card.id !== snapshot.id || card.runId !== snapshot.runId || card.position !== snapshot.position || card.revision !== snapshot.revision || card.source !== snapshot.source)
      throw new Error('Timeline card changed while loading')
    cards.push(card)
  }

  return cards
}
