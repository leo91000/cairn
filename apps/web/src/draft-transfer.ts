import {
  beaconOrigin,
  centralAccount,
  installationSubdomains,
  installationUrl,
} from './beacon-location'

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

  const target = new URL('/browser/drafts', installationUrl(slug))
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
          id,
          accountId,
          drafts,
        }, target.origin)
      }

      if (event.data?.kind === 'cairn-drafts-saved')
        finish(event.data.ok ? undefined : new Error('Unable to restore your drafts. They remain saved here; please try again.'))
    }

    timeout = setTimeout(() => finish(new Error('Unable to restore your drafts. They remain saved here; please try again.')), 10000)
    window.addEventListener('message', receive)
    document.body.append(frame)
  })
}

export async function receiveInstallationDrafts(slug: string) {
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
    if (data.id !== installation.id || data.accountId !== session.account.id)
      return
    let ok = false
    try {
      const entries = Object.entries(data.drafts as Record<string, unknown>)
      const prefix = `cairn-chat-draft:${installation.id}:`
      const valid = entries.length <= 1000 && entries.every(([key, value]) => key.startsWith(prefix) && typeof value === 'string' && value.length <= 1000000)
      if (valid) {
        for (const [key, value] of entries) {
          // Returning to a legacy bookmark must not overwrite newer drafts on the subdomain.
          if (!sessionStorage.getItem(key))
            sessionStorage.setItem(key, value as string)
        }

        ok = true
      }
    }
    catch {}

    window.parent.postMessage({ kind: 'cairn-drafts-saved', ok }, beaconOrigin)
  })
  window.parent.postMessage({ kind: 'cairn-drafts-ready' }, beaconOrigin)
}
