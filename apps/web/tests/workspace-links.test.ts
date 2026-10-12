import { describe, expect, it } from 'vitest'
import { chatPath } from '../src/workspace-links'

describe('conversation links', () => {
  it('uses a stable alias regardless of the display title', () => {
    expect(chatPath({ id: 'internal-id', slug: 'quiet-cedar-1234abcd' })).toBe('/chats/quiet-cedar-1234abcd')
  })

  it('supports installations running the previous API version', () => {
    expect(chatPath({ id: 'legacy-id' })).toBe('/chats/legacy-id')
  })
})
