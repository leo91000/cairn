export interface Deliverable {
  id: string
  runId: string
  messageId: string | null
  key: string
  version: number
  title: string
  name: string
  group: string
  kind: 'image' | 'video' | 'audio' | 'pdf' | 'markdown' | 'code' | 'file'
  mediaType: string
  size: number
  createdAt: number
  url: string
  visibility?: 'private' | 'public'
  publicUrl?: string | null
  previewStatus: 'pending' | 'ready' | 'unavailable' | 'none'
  width?: number
  height?: number
  duration?: number
  excerpt?: string
}

export function artifactUrl(item: Deliverable, action?: 'preview' | 'download') {
  // Construct from scoped identifiers, never trust URLs returned by a tool event.
  return `/api/runs/${encodeURIComponent(item.runId)}/artifacts/${encodeURIComponent(item.id)}${action ? `?${action}=1` : ''}`
}

/** Resolve only artifact endpoints on the application's own origin. */
export function artifactLink(link: string, origin: string) {
  try {
    const url = new URL(link, origin)
    if (url.origin !== new URL(origin).origin || url.username || url.password)
      return null
    const match = /^\/api\/runs\/([\w-]+)\/artifacts\/([\w-]+)$/.exec(url.pathname)
    return match ? { runId: match[1]!, id: match[2]! } : null
  }
  catch {
    return null
  }
}

export function latestArtifacts(items: Deliverable[]) {
  const latest = new Map<string, Deliverable>()
  for (const item of items) {
    const key = `${item.runId}:${item.key}`
    if ((latest.get(key)?.version ?? 0) < item.version)
      latest.set(key, item)
  }

  return [...latest.values()]
}

export function fileSize(bytes: number) {
  if (bytes < 1024)
    return `${bytes} B`
  if (bytes < 1024 * 1024)
    return `${Math.ceil(bytes / 1024)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}
