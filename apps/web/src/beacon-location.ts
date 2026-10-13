// Server-injected configuration avoids assuming a production hostname in the browser.
const root = typeof document !== 'undefined' ? document.getElementById('app') : null
export const beaconOrigin = root?.getAttribute?.('data-beacon-origin') || (typeof window !== 'undefined' ? window.location?.origin || '' : '')
export const installationSubdomains = root?.getAttribute?.('data-beacon-subdomains') === 'true'
export const installationHost = (() => {
  if (!installationSubdomains || typeof window === 'undefined')
    return ''
  const central = new URL(beaconOrigin)
  const suffix = `.${central.hostname}`
  const host = window.location.hostname
  if (!host.endsWith(suffix) || window.location.port !== central.port)
    return ''
  const slug = host.slice(0, -suffix.length)
  return /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug) ? slug : ''
})()
export const centralAccount = !installationHost

export function installationUrl(slug: string, path = '/') {
  const url = new URL(beaconOrigin)
  if (installationSubdomains) {
    url.hostname = `${slug}.${url.hostname}`
    const destination = new URL(path, url)
    if (destination.origin !== url.origin || destination.username || destination.password)
      throw new Error('Invalid installation path')
    return destination.href
  }

  return new URL(`/installations/${encodeURIComponent(slug)}${path}`, url).href
}

export function accountSettingsUrl(slug: string, confirm = false) {
  const url = new URL(`/installations/${encodeURIComponent(slug)}/settings/account`, beaconOrigin)
  url.searchParams.set('return_to', window.location.pathname === '/settings/account' ? installationUrl(slug, '/settings') : window.location.href)
  if (confirm)
    url.searchParams.set('confirm_identity', '1')
  return url.href
}

export function workspaceReturn(value: string | null, installations: Array<{ id: string, slug?: string }>) {
  if (!value)
    return undefined
  try {
    const url = new URL(value, beaconOrigin)
    if (url.username || url.password)
      return undefined
    const central = new URL(beaconOrigin)
    if (url.origin === central.origin) {
      const key = /^\/installations\/([\w-]+)(?:\/|$)/.exec(url.pathname)?.[1]
      return key && installations.some(item => item.id === key || item.slug === key) ? url.href : undefined
    }

    return installationSubdomains && installations.some(item => item.slug && new URL(installationUrl(item.slug)).origin === url.origin)
      ? url.href
      : undefined
  }
  catch { return undefined }
}

export function sharedPreference(name: string) {
  const prefix = `${encodeURIComponent(name)}=`
  const value = document.cookie.split(';').map(part => part.trim()).find(part => part.startsWith(prefix))?.slice(prefix.length)
  try {
    return value ? decodeURIComponent(value) : undefined
  }
  catch { return undefined }
}

export function rememberPreference(name: string, value: string) {
  if (!installationSubdomains)
    return
  const root = new URL(beaconOrigin)
  const secure = root.protocol === 'https:' ? '; Secure' : ''
  document.cookie = `${encodeURIComponent(name)}=${encodeURIComponent(value)}; Domain=${root.hostname}; Path=/; SameSite=Lax; Max-Age=31536000${secure}`
}
