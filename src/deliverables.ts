import type { Deliverable } from '../shared/artifacts'
import type { ActivityEntry } from './activity'
import { artifactLink as sharedArtifactLink, artifactUrl as sharedArtifactUrl } from '../shared/artifacts'
import { apiUrl, state } from './api'

export function artifactUrl(item: Deliverable, action?: 'preview' | 'download') {
  return apiUrl(sharedArtifactUrl(item, action).slice(4))
}

export function artifactLink(link: string, origin: string) {
  try {
    const url = new URL(link, origin)
    const prefix = apiUrl('')
    if (state.installationId && url.pathname.startsWith(`${prefix}/`))
      url.pathname = `/api${url.pathname.slice(prefix.length)}`
    return sharedArtifactLink(url.href, origin)
  }
  catch { return null }
}

export function deliveryEntries(entries: ActivityEntry[], files: Deliverable[]) {
  const groups = new Map<number, Deliverable[]>()
  const versions = new Map<string, Deliverable>()
  for (const file of files) {
    const key = `${file.messageId ?? ''}:${file.key}`
    if ((versions.get(key)?.version ?? 0) < file.version)
      versions.set(key, file)
  }

  for (const file of versions.values()) {
    let position = entries.length - 1
    for (let index = 0; index < entries.length; index++) {
      const entry = entries[index]
      if (entry.kind !== 'message' || entry.time < file.createdAt)
        continue
      position = entry.role === 'user' ? index - 1 : index
      break
    }

    const group = groups.get(position) ?? []
    group.push(file)
    groups.set(position, group)
  }

  const result: (ActivityEntry | { kind: 'deliverables', id: string, files: Deliverable[] })[] = []

  function append(index: number) {
    const files = groups.get(index)
    if (files)
      result.push({ kind: 'deliverables', id: `deliverables:${index}`, files })
  }

  append(-1)
  for (const [index, entry] of entries.entries()) {
    result.push(entry)
    append(index)
  }

  return result
}
