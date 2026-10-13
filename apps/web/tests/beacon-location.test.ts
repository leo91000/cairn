import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'

async function location(origin: string, current: string, enabled = true) {
  vi.resetModules()
  vi.stubGlobal('window', { location: new URL(current) })
  vi.stubGlobal('document', { getElementById: () => ({ getAttribute: (name: string) => name === 'data-beacon-origin' ? origin : name === 'data-beacon-subdomains' ? String(enabled) : null }) })
  return import('../src/beacon-location')
}

afterEach(() => vi.unstubAllGlobals())

it('uses a subdomain and preserves a deep path, query and fragment', async () => {
  const links = await location('https://cairn.test', 'https://quiet-cedar-1234abcd.cairn.test/chats/bright-otter')
  expect(links.installationHost).toBe('quiet-cedar-1234abcd')
  expect(links.centralAccount).toBe(false)
  expect(links.installationUrl('quiet-cedar-1234abcd', '/chats/bright-otter?draft=hello#reply')).toBe('https://quiet-cedar-1234abcd.cairn.test/chats/bright-otter?draft=hello#reply')
  expect(() => links.installationUrl('quiet-cedar-1234abcd', '//evil.test')).toThrow('Invalid installation path')
})

it('keeps path routing available before wildcard DNS and TLS are enabled', async () => {
  const links = await location('https://cairn.test', 'https://cairn.test/', false)
  expect(links.installationHost).toBe('')
  expect(links.installationUrl('quiet-cedar-1234abcd', '/chats/test')).toBe('https://cairn.test/installations/quiet-cedar-1234abcd/chats/test')
})

it('only restores account return links to accessible installations', async () => {
  const links = await location('https://cairn.test', 'https://cairn.test/')
  const installations = [{ id: 'uuid', slug: 'quiet-cedar-1234abcd' }]
  expect(links.workspaceReturn('https://quiet-cedar-1234abcd.cairn.test/chats/test', installations)).toBe('https://quiet-cedar-1234abcd.cairn.test/chats/test')
  expect(links.workspaceReturn('/installations/uuid/chats/test', installations)).toBe('https://cairn.test/installations/uuid/chats/test')
  for (const target of ['https://evil.test', 'https://quiet-cedar-1234abcd.cairn.test.evil.test', 'https://another.cairn.test', 'https://quiet-cedar-1234abcd.cairn.test:8443/', 'https://user:password@quiet-cedar-1234abcd.cairn.test', '/installations/unknown/'])
    expect(links.workspaceReturn(target, installations)).toBeUndefined()
})

it('keeps account settings and confirmation on the central origin', async () => {
  const links = await location('https://cairn.test', 'https://quiet-cedar-1234abcd.cairn.test/settings/sensitive')
  const url = new URL(links.accountSettingsUrl('quiet-cedar-1234abcd', true))
  expect(url.origin).toBe('https://cairn.test')
  expect(url.pathname).toBe('/installations/quiet-cedar-1234abcd/settings/account')
  expect(url.searchParams.get('confirm_identity')).toBe('1')
  expect(url.searchParams.get('return_to')).toBe('https://quiet-cedar-1234abcd.cairn.test/settings/sensitive')
})
