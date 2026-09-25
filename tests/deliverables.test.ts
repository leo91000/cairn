import type { Deliverable } from '../shared/artifacts'
import type { ActivityEntry } from '../src/activity'
import { describe, expect, it } from 'vitest'
import { artifactLink, artifactUrl, latestArtifacts } from '../shared/artifacts'
import { deliveryEntries } from '../src/deliverables'

function image(id: string, version: number, createdAt: number, messageId = 'turn1'): Deliverable {
  return {
    id,
    runId: 'run',
    messageId,
    key: 'mobile',
    version,
    title: 'Mobile',
    name: 'mobile.png',
    kind: 'image',
    mediaType: 'image/png',
    size: 12,
    createdAt,
    url: 'javascript:alert(1)',
    group: '',
    previewStatus: 'none',
  }
}

describe('published deliverables', () => {
  it('keeps versions with their answer while the library selects the latest revision', () => {
    const entries: ActivityEntry[] = [{
      kind: 'message',
      role: 'user',
      id: 'a',
      time: 1,
      text: 'Create',
    }, {
      kind: 'message',
      id: 'b',
      time: 5,
      text: 'Done',
    }, {
      kind: 'message',
      role: 'user',
      id: 'c',
      time: 6,
      text: 'Improve',
    }, {
      kind: 'message',
      id: 'd',
      time: 10,
      text: 'Improved',
    }]
    const files = [image('old', 1, 3), image('new', 2, 8, 'turn2')]
    const result = deliveryEntries(entries, files)
    expect(result.map(entry => entry.id)).toEqual(['a', 'b', 'deliverables:1', 'c', 'd', 'deliverables:3'])
    expect(latestArtifacts(files).map(item => item.id)).toEqual(['new'])
  })
  it('shows published files before an unfinished answer and never crosses a later user message', () => {
    const entries: ActivityEntry[] = [{
      kind: 'message',
      id: 'a',
      time: 1,
      text: 'Working',
    }, {
      kind: 'message',
      role: 'user',
      id: 'b',
      time: 5,
      text: 'Next',
    }]
    expect(deliveryEntries(entries, [image('file', 1, 3)]).map(entry => entry.kind)).toEqual(['message', 'deliverables', 'message'])
    expect(deliveryEntries([], [image('file', 1, 3)])[0].kind).toBe('deliverables')
    expect(artifactUrl(image('file', 1, 3))).toBe('/api/runs/run/artifacts/file')
  })
})

it('resolves artifact message links only against the connected origin', () => {
  const origin = 'https://leo.example'
  for (const link of ['/api/runs/run/artifacts/file', 'https://leo.example/api/runs/run/artifacts/file?download=1#top'])
    expect(artifactLink(link, origin)).toEqual({ runId: 'run', id: 'file' })
  for (const link of ['https://other.example/api/runs/run/artifacts/file', '//other.example/api/runs/run/artifacts/file', 'https://user@leo.example/api/runs/run/artifacts/file', 'https://leo.example:444/api/runs/run/artifacts/file', '/api/runs/run/artifacts', '/api/runs/run/artifacts/file/preview', 'javascript:alert(1)', 'file:///api/runs/run/artifacts/file'])
    expect(artifactLink(link, origin)).toBeNull()
})
