import {
  beaconOrigin,
  centralAccount,
  installationHost,
  installationSubdomains,
  installationUrl,
} from './beacon-location'

const transferPrefix = 'cairn-draft-transfer:'
const transferParameter = 'cairn_draft_transfer'
const transferLifetime = 5 * 60 * 1000
let pendingTransfer = ''

function validTransferId(value: unknown): value is string {
  return typeof value === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(value)
}

function draftEntries(drafts: unknown, installationId: string) {
  if (!drafts || typeof drafts !== 'object' || Array.isArray(drafts))
    throw new Error('Invalid draft transfer')
  const entries = Object.entries(drafts)
  const prefix = `cairn-chat-draft:${installationId}:`
  const draftKeys = new Set(entries.map(([key]) => key.endsWith(':plan') ? key.slice(0, -5) : key))
  if (draftKeys.size > 1000 || entries.length > 2000 || entries.some(([key, value]) => !key.startsWith(prefix) || typeof value !== 'string' || value.length > 1000000 || (key.endsWith(':plan') && value !== 'true' && value !== 'false')))
    throw new Error('Invalid draft transfer')
  return entries as [string, string][]
}

function removeExpiredTransfers() {
  for (let index = localStorage.length - 1; index >= 0; index--) {
    const key = localStorage.key(index)
    if (!key?.startsWith(transferPrefix))
      continue
    try {
      const batch = JSON.parse(localStorage.getItem(key) || 'null')
      if (!batch || typeof batch.expiresAt !== 'number' || batch.expiresAt <= Date.now())
        localStorage.removeItem(key)
    }
    catch { localStorage.removeItem(key) }
  }
}

// Remove the opaque handoff marker before the router sees it. It contains no draft or credential.
export function prepareDraftTransfer() {
  if (!installationHost || window.parent !== window)
    return
  const url = new URL(window.location.href)
  const id = url.searchParams.get(transferParameter)
  if (id !== null) {
    pendingTransfer = validTransferId(id) ? id : ''
    url.searchParams.delete(transferParameter)
    window.history.replaceState(window.history.state, '', url.href)
  }
}

// WebKit partitions iframe sessionStorage; a short-lived target-origin mailbox survives navigation.
// Consume only this tab's account/installation-bound batch before the composer is mounted.
export function restoreInstallationDrafts(installationId: string, accountId: string) {
  if (!pendingTransfer) {
    try {
      removeExpiredTransfers()
    }
    catch {}

    return
  }

  removeExpiredTransfers()
  const key = `${transferPrefix}${pendingTransfer}`
  const raw = localStorage.getItem(key)
  if (!raw)
    return
  const batch = JSON.parse(raw)
  if (batch.id !== installationId || batch.accountId !== accountId || batch.origin !== beaconOrigin || batch.expiresAt <= Date.now()) {
    localStorage.removeItem(key)
    pendingTransfer = ''
    return
  }

  const entries = draftEntries(batch.drafts, installationId)
  const existingDrafts = new Set(entries.map(([key]) => key.endsWith(':plan') ? key.slice(0, -5) : key)
    .filter(key => sessionStorage.getItem(key) !== null || sessionStorage.getItem(`${key}:plan`) !== null))
  for (const [key, value] of entries) {
    // Text and Plan belong together. Empty text is a newer deliberate clear,
    // including legacy drafts whose absent Plan field means ordinary execution.
    const draftKey = key.endsWith(':plan') ? key.slice(0, -5) : key
    if (!existingDrafts.has(draftKey))
      sessionStorage.setItem(key, value)
  }

  localStorage.removeItem(key)
  pendingTransfer = ''
}

// Transfer only this installation's drafts, directly between browser frames.
// The central server never stores or receives their contents.
export async function transferInstallationDrafts(id: string, slug: string, accountId: string) {
  if (!installationSubdomains || !centralAccount)
    return
  const drafts: Record<string, string> = {}
  const prefix = `cairn-chat-draft:${id}:`
  for (let i = 0; i < sessionStorage.length; i++) {
    const key = sessionStorage.key(i)
    if (key?.startsWith(prefix))
      drafts[key] = sessionStorage.getItem(key) || ''
  }

  if (!Object.values(drafts).some(Boolean))
    return

  const transferId = crypto.randomUUID()
  const target = new URL('/browser/drafts', installationUrl(slug))
  target.searchParams.set(transferParameter, transferId)
  const frame = document.createElement('iframe')
  frame.hidden = true
  frame.title = 'Restore conversation drafts'
  frame.src = target.href
  await new Promise<void>((resolve, reject) => {
    let timeout: ReturnType<typeof setTimeout>
    const finish = (error?: Error) => {
      clearTimeout(timeout)
      window.removeEventListener('message', receive)
      frame.remove()
      error ? reject(error) : resolve()
    }

    function receive(event: MessageEvent) {
      if (event.origin !== target.origin || event.source !== frame.contentWindow)
        return
      if (event.data?.kind === 'cairn-drafts-ready') {
        frame.contentWindow?.postMessage({
          kind: 'cairn-drafts',
          transferId,
          id,
          accountId,
          drafts,
        }, target.origin)
      }

      if (event.data?.kind === 'cairn-drafts-saved' && event.data.transferId === transferId)
        finish(event.data.ok ? undefined : new Error('Unable to restore your drafts. They remain saved here; please try again.'))
    }

    timeout = setTimeout(() => finish(new Error('Unable to restore your drafts. They remain saved here; please try again.')), 10000)
    window.addEventListener('message', receive)
    document.body.append(frame)
  })
  return transferId
}

export async function receiveInstallationDrafts(slug: string) {
  const transferId = new URL(window.location.href).searchParams.get(transferParameter)
  if (!validTransferId(transferId))
    return
  const response = await fetch('/api/account/session', { credentials: 'same-origin' })
  if (!response.ok)
    return
  const session = await response.json()
  const installation = session.installations?.find((item: { slug?: string }) => item.slug === slug)
  if (!session.authenticated || !installation)
    return

  window.addEventListener('message', (event: MessageEvent) => {
    if (event.origin !== beaconOrigin || event.source !== window.parent || event.data?.kind !== 'cairn-drafts')
      return
    const data = event.data
    if (data.transferId !== transferId || data.id !== installation.id || data.accountId !== session.account.id)
      return
    let ok = false
    try {
      draftEntries(data.drafts, installation.id)
      removeExpiredTransfers()
      localStorage.setItem(`${transferPrefix}${transferId}`, JSON.stringify({
        id: installation.id,
        accountId: session.account.id,
        origin: beaconOrigin,
        expiresAt: Date.now() + transferLifetime,
        drafts: data.drafts,
      }))
      ok = true
    }
    catch {}

    window.parent.postMessage({ kind: 'cairn-drafts-saved', transferId, ok }, beaconOrigin)
  })
  window.parent.postMessage({ kind: 'cairn-drafts-ready' }, beaconOrigin)
}
