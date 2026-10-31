import type { RunContentOutput } from '../../../packages/contracts/generated/typescript/RunContentOutput'
import type { TimelineEntry } from '../../../packages/contracts/generated/typescript/TimelineEntry'
import type { TimelinePreview } from '../../../packages/contracts/generated/typescript/TimelinePreview'
import { api } from './api'

/** Resolve complete cards before committing a page or a stream cursor. */
export async function readLargeTimeline(path: string, previews: TimelinePreview[], signal?: AbortSignal): Promise<TimelineEntry[]> {
  const cards: TimelineEntry[] = []
  for (const preview of previews) {
    const chunks: string[] = []
    let offset = 0
    let sha256: string | undefined
    let total: number | undefined
    for (;;) {
      const params = new URLSearchParams({ entryId: preview.id, offset: String(offset) })
      if (sha256)
        params.set('sha256', sha256)
      const part = await api<RunContentOutput>(`${path.replace(/\/stream$/, '')}/timeline-content?${params}`, { signal })
      if (part.entryId !== preview.id || part.offset !== offset || (sha256 && part.sha256 !== sha256) || (total !== undefined && part.totalBytes !== total))
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
    if (card.id !== preview.id || card.position !== preview.position || card.revision < preview.revision || (card.revision === preview.revision && card.source !== preview.source))
      throw new Error('Timeline card changed while loading')
    cards.push(card)
  }

  return cards
}
