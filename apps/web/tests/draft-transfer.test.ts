import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'

const origin = 'https://cairn.test'
const child = 'https://quiet-cedar-1234abcd.cairn.test'
const transferId = '12345678-1234-1234-1234-123456789abc'
const anotherTransfer = '87654321-4321-4321-4321-cba987654321'
const draftKey = 'cairn-chat-draft:installation-id:chat-id'

function storage() {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    key: (index: number) => [...values.keys()][index] ?? null,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  }
}

function mailbox(id = transferId, accountId = 'account-id', expiresAt = Date.now() + 60000) {
  const key = `cairn-draft-transfer:${id}`
  localStorage.setItem(key, JSON.stringify({
    id: 'installation-id',
    accountId,
    origin,
    expiresAt,
    drafts: { [draftKey]: 'An original draft' },
  }))
  return key
}

beforeEach(() => {
  vi.resetModules()
  const location = new URL(`${child}/chats/chat-slug?bookmark=yes&cairn_draft_transfer=${transferId}#reply`)
  const browser = { location, parent: {} as unknown, history: { state: null, replaceState: vi.fn() } }
  browser.parent = browser
  vi.stubGlobal('window', browser)
  vi.stubGlobal('document', { getElementById: () => ({ getAttribute: (name: string) => name === 'data-beacon-origin' ? origin : name === 'data-beacon-subdomains' ? 'true' : null }) })
  vi.stubGlobal('localStorage', storage())
  vi.stubGlobal('sessionStorage', storage())
})

afterEach(() => vi.unstubAllGlobals())

it('restores this tab’s mailbox before rendering and removes its opaque URL marker and stored contents', async () => {
  const transfer = await import('../src/draft-transfer')
  const key = mailbox()
  const unrelated = mailbox(anotherTransfer)
  transfer.prepareDraftTransfer()
  expect(window.history.replaceState).toHaveBeenCalledWith(null, '', `${child}/chats/chat-slug?bookmark=yes#reply`)
  transfer.restoreInstallationDrafts('installation-id', 'account-id')
  expect(sessionStorage.getItem(draftKey)).toBe('An original draft')
  expect(localStorage.getItem(key)).toBeNull()
  expect(localStorage.getItem(unrelated)).not.toBeNull()
})

it.each(['', 'A newer draft'])('preserves a newer target draft, including an explicitly cleared value %j', async (value) => {
  const transfer = await import('../src/draft-transfer')
  const key = mailbox()
  sessionStorage.setItem(draftKey, value)
  transfer.prepareDraftTransfer()
  transfer.restoreInstallationDrafts('installation-id', 'account-id')
  expect(sessionStorage.getItem(draftKey)).toBe(value)
  expect(localStorage.getItem(key)).toBeNull()
})

it.each(['foreign-account', 'expired'])('rejects an incompatible mailbox: %s', async (kind) => {
  const transfer = await import('../src/draft-transfer')
  const key = mailbox(transferId, kind === 'foreign-account' ? 'other-account' : 'account-id', kind === 'expired' ? Date.now() - 1 : Date.now() + 60000)
  transfer.prepareDraftTransfer()
  transfer.restoreInstallationDrafts('installation-id', 'account-id')
  expect(sessionStorage.getItem(draftKey)).toBeNull()
  expect(localStorage.getItem(key)).toBeNull()
})

it.each(['true', 'false'])('restores the Plan choice %s with its draft text', async (plan) => {
  const transfer = await import('../src/draft-transfer')
  const key = mailbox()
  const batch = JSON.parse(localStorage.getItem(key)!)
  batch.drafts[`${draftKey}:plan`] = plan
  localStorage.setItem(key, JSON.stringify(batch))
  transfer.prepareDraftTransfer()
  transfer.restoreInstallationDrafts('installation-id', 'account-id')
  expect(sessionStorage.getItem(draftKey)).toBe('An original draft')
  expect(sessionStorage.getItem(`${draftKey}:plan`)).toBe(plan)
})

it.each([null, 'false'])('keeps a newer ordinary draft with its own Plan choice %j', async (plan) => {
  const transfer = await import('../src/draft-transfer')
  const key = mailbox()
  const batch = JSON.parse(localStorage.getItem(key)!)
  batch.drafts[`${draftKey}:plan`] = 'true'
  localStorage.setItem(key, JSON.stringify(batch))
  sessionStorage.setItem(draftKey, 'A newer ordinary draft')
  if (plan !== null)
    sessionStorage.setItem(`${draftKey}:plan`, plan)
  transfer.prepareDraftTransfer()
  transfer.restoreInstallationDrafts('installation-id', 'account-id')
  expect(sessionStorage.getItem(draftKey)).toBe('A newer ordinary draft')
  expect(sessionStorage.getItem(`${draftKey}:plan`)).toBe(plan)
})
