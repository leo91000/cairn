import type {
  Agent,
  Project,
  Skill,
  Task,
} from '../../../packages/contracts/contracts'
import type { McpView } from '../../../packages/contracts/mcp'
import { reactive, watch } from 'vue'
import { beaconOrigin, installationHost } from './beacon-location'
import { clearHistoryCache } from './history-cache'
import { InstallationTransport } from './installation-transport'
import { observeTransport } from './transport-observation'

// Resolve the beacon context before any shared view reads its local cache.
export const beaconEntry = typeof document !== 'undefined' && document.getElementById('app')?.hasAttribute('data-beacon')
export const installationRoute = beaconEntry ? installationHost || /^\/installations\/([\w-]+)(?:\/|$)/.exec(window.location.pathname)?.[1] || '' : ''

export const state = reactive({
  transportRoute: 'relay' as 'direct' | 'relay',
  ready: false,
  authenticated: false,
  signingOut: false,
  redirecting: false,
  csrf: '',
  installationId: '',
  installationOnline: undefined as boolean | undefined,
  installationUpdateRequired: false,
  accountId: '',
  installationRole: 'owner' as 'owner' | 'member',
  agents: [] as Agent[],
  projects: [] as Project[],
  tasks: [] as Task[],
  skills: [] as Skill[],
  mcps: [] as McpView[],
  toast: '',
  error: '',
})
watch(() => state.signingOut || (state.ready && !state.authenticated), (clear) => {
  if (clear)
    void clearHistoryCache()
}, { flush: 'sync' })
export const installationTransport = new InstallationTransport(state)
watch(() => [state.ready, state.authenticated, state.installationId, state.signingOut, state.installationOnline], () => {
  installationTransport.availabilityChanged(state.installationOnline)
  if (beaconEntry && state.ready && state.authenticated && !state.signingOut)
    installationTransport.start()
  else if (typeof window !== 'undefined')
    installationTransport.stop()
}, { flush: 'sync' })

let toastTimer: ReturnType<typeof setTimeout> | undefined
export function notify(message: string) {
  state.toast = message
  if (toastTimer)
    clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (state.toast = ''), 4500)
}

export class ApiError extends Error {
  constructor(message: string, readonly status: number) { super(message) }
}
export function redirect(url: string) {
  const resume = () => {
    state.redirecting = false
  }

  state.redirecting = true
  // Restore polling if Back returns this document from the browser's page cache.
  window.addEventListener('pageshow', resume, { once: true })
  try {
    window.location.assign(url)
  }
  catch (error) {
    window.removeEventListener('pageshow', resume)
    resume()
    throw error
  }
}

function apiHeaders(options: RequestInit) {
  return {
    ...(options.body === undefined
      ? {}
      : { 'Content-Type': 'application/json' }),
    'X-CSRF-Token': state.csrf,
    ...options.headers,
  } as Record<string, string>
}

// Installation API paths go through the installation transport (direct or relay).
function transportPath(url: string) {
  const path = state.installationId && url.startsWith(apiUrl('/')) ? url.slice(apiUrl('').length) : undefined
  return path && !/^\/tokens(?:\/|$)/.test(path) ? path : undefined
}

async function apiResponse(url: string, options: RequestInit) {
  const requestOptions = { ...options, headers: apiHeaders(options) }
  const path = transportPath(url)
  return path
    ? await installationTransport.request(path, url, requestOptions)
    : await fetch(url, requestOptions)
}

function failure(status: number, data: any) {
  if (status === 401)
    state.authenticated = false
  return new ApiError(data?.error || 'Request failed. Please try again.', status)
}

async function requestApi<T = any>(
  url: string,
  options: RequestInit = {},
): Promise<T> {
  const response = await apiResponse(url, options)
  const data = await response.json()
  if (!response.ok)
    throw failure(response.status, data)

  return data
}

/** A binary installation resource, through the same transport and credentials as `api()`. */
export async function apiBlob(url: string, options: RequestInit = {}): Promise<Blob> {
  const response = await apiResponse(apiUrl(url), options)
  if (!response.ok)
    throw failure(response.status, await response.json().catch(() => null))
  return response.blob()
}

/**
 * Uploads a file body like `api()` and reports progress. File bodies never use the direct channel
 * (it only carries string bodies), so `api()` would use `fetch` with these same headers.
 */
export function apiUpload<T = any>(url: string, body: Blob, options: { method?: string, headers?: Record<string, string>, onProgress?: (fraction: number) => void } = {}): Promise<T> {
  const target = apiUrl(url)
  const method = options.method ?? 'PUT'
  const headers = apiHeaders({ body, headers: options.headers })
  if (typeof XMLHttpRequest === 'undefined')
    return requestApi<T>(target, { method, headers, body })
  return new Promise<T>((resolve, reject) => {
    const request = new XMLHttpRequest()
    request.open(method, target)
    for (const [name, value] of Object.entries(headers))
      request.setRequestHeader(name, value)
    request.upload.onprogress = (event) => {
      if (event.lengthComputable && event.total > 0)
        options.onProgress?.(event.loaded / event.total)
    }

    request.onerror = () => reject(new TypeError('Upload failed. Check your connection and try again.'))
    request.onabort = () => reject(new DOMException('Upload cancelled.', 'AbortError'))
    request.onload = () => {
      let data: any = null
      try {
        data = JSON.parse(request.responseText)
      }
      catch {}

      if (request.status < 200 || request.status >= 300) {
        reject(failure(request.status, data))
        return
      }

      const route = request.getResponseHeader('x-cairn-transport')
      const path = transportPath(target)
      if (path && (route === 'direct' || route === 'relay'))
        observeTransport(route, path, method)
      resolve(data)
    }

    request.send(body)
  })
}

export function api<T = any>(url: string, options: RequestInit = {}): Promise<T> {
  return requestApi<T>(apiUrl(url), options)
}

export function accountApi<T = any>(url: string, options: RequestInit = {}): Promise<T> {
  return requestApi<T>(`/api/account${url}`, options)
}

export function apiUrl(path: string) {
  if (beaconEntry && state.installationId && /^\/tokens(?:\/|$)/.test(path))
    return `/api/installations/${encodeURIComponent(state.installationId)}${path}`

  const prefix = state.installationId
    ? `/api/installations/${encodeURIComponent(state.installationId)}/api`
    : '/api'
  return `${prefix}${path}`
}

// Installation responses describe assets relative to their own API. Keep
// external links intact and route these assets through the current relay.
export function apiResourceUrl(value: string) {
  if (!state.installationId || !value)
    return value
  try {
    const url = new URL(value, window.location.origin)
    if ((url.origin === window.location.origin || url.origin === beaconOrigin) && url.pathname.startsWith('/api/') && !url.pathname.startsWith('/api/installations/') && !url.pathname.startsWith('/api/public/'))
      return apiUrl(`${url.pathname.slice(4)}${url.search}${url.hash}`)
  }
  catch {}

  return value
}

export function ownerPage(path: string) {
  return ['/plugins', '/mcps', '/connections', '/nodes', '/authorize'].includes(path) || path.startsWith('/plugins/')
}

export async function refresh() {
  const [agents, projects, tasks, skills, mcps] = await Promise.all([
    api<Agent[]>('/agents'),
    api<Project[]>('/projects'),
    api<Task[]>('/tasks'),
    api<Skill[]>('/skills'),
    state.installationRole === 'owner' ? api<McpView[]>('/mcps') : Promise.resolve([]),
  ])
  Object.assign(state, {
    agents,
    projects,
    tasks,
    skills,
    mcps,
  })
}

export function date(value: number | null | undefined) {
  return value
    ? new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
      }).format(value)
    : '—'
}

export function relative(value: number) {
  const mins = Math.round((Date.now() - value) / 60000)
  if (mins < 1)
    return 'Just now'
  if (mins < 60)
    return `${mins}m ago`
  if (mins < 1440)
    return `${Math.floor(mins / 60)}h ago`
  return date(value)
}

export function duration(start: number | null, end: number | null) {
  if (!start)
    return '—'
  const seconds = Math.floor(((end ?? Date.now()) - start) / 1000)
  return seconds < 60
    ? `${seconds}s`
    : `${Math.floor(seconds / 60)}m ${seconds % 60}s`
}

export async function logoutAccount() {
  const alreadySigningOut = state.signingOut
  state.signingOut = true
  try {
    const response = await fetch('/api/account/logout', {
      method: 'POST',
      headers: { 'X-CSRF-Token': state.csrf },
    })
    if (!response.ok && response.status !== 401)
      throw new Error('Unable to sign out. Please try again.')

    state.authenticated = false
    state.csrf = ''
  }
  finally {
    state.signingOut = alreadySigningOut
  }
}

export async function signOut() {
  if (state.signingOut)
    return
  state.signingOut = true
  try {
    await logoutAccount()
    window.location.assign(`${beaconOrigin}/`)
  }
  catch (e) {
    notify((e as Error).message)
  }
  finally {
    state.signingOut = false
  }
}
