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

// Resolve the beacon context before any shared view reads its local cache.
export const beaconEntry = typeof document !== 'undefined' && document.getElementById('app')?.hasAttribute('data-beacon')
export const installationRoute = beaconEntry ? installationHost || /^\/installations\/([\w-]+)(?:\/|$)/.exec(window.location.pathname)?.[1] || '' : ''

export const state = reactive({
  transportRoute: 'relay' as 'direct' | 'relay',
  connectionLost: false,
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
  if (!message)
    return
  state.toast = message
  if (toastTimer)
    clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (state.toast = ''), 4500)
}

export class ApiError extends Error {
  constructor(message: string, readonly status: number) { super(message) }
}

class ConnectionError extends Error {
  constructor(readonly original: unknown) { super(original instanceof Error ? original.message : String(original)) }
}

const relayUnavailable = new Set([
  'Installation unavailable',
  'Installation busy',
  'Installation busy or unavailable',
  'Installation request timed out',
  'Installation response timed out',
  'Installation connection lost',
])

function connectionScope() {
  return JSON.stringify([state.installationId, state.accountId, state.csrf])
}

const recoveries = new Map<string, {
  scope: string
  controller: AbortController
  promise: Promise<void>
  reconcile: boolean
}>()

watch(() => [connectionScope(), state.authenticated, state.signingOut], () => {
  for (const [probe, recovery] of recoveries) {
    if (recovery.scope !== connectionScope() || !state.authenticated || state.signingOut) {
      recoveries.delete(probe)
      recovery.controller.abort()
    }
  }

  state.connectionLost = recoveries.size > 0
}, { flush: 'sync' })

function recoveryDelay(milliseconds: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout>
    const abort = () => {
      clearTimeout(timer)
      reject(new ApiError('', 0))
    }

    timer = setTimeout(() => {
      signal.removeEventListener('abort', abort)
      resolve()
    }, milliseconds)
    signal.addEventListener('abort', abort, { once: true })
    if (signal.aborted)
      abort()
  })
}

function awaitRecovery(pending: Promise<void>, signal?: AbortSignal | null) {
  if (!signal)
    return pending
  return new Promise<void>((resolve, reject) => {
    const abort = () => {
      signal.removeEventListener('abort', abort)
      reject(signal.reason)
    }

    signal.addEventListener('abort', abort, { once: true })
    void pending.then(() => {
      signal.removeEventListener('abort', abort)
      resolve()
    }, (error) => {
      signal.removeEventListener('abort', abort)
      reject(error)
    })
    if (signal.aborted)
      abort()
  })
}

function recoverConnection(scope: string, reconcile: boolean, probe: string) {
  const recovery = recoveries.get(probe)
  if (recovery?.scope === scope) {
    recovery.reconcile ||= reconcile
    return recovery.promise
  }

  const controller = new AbortController()
  state.connectionLost = true
  const promise = (async () => {
    let delay = 1000
    while (true) {
      await recoveryDelay(delay, controller.signal)
      try {
        await requestOnce(probe, { signal: controller.signal })
        return
      }
      catch (error) {
        if (!(error instanceof ConnectionError))
          throw error
      }

      delay = Math.min(delay * 2, 10000)
    }
  })().finally(() => {
    if (recoveries.get(probe)?.controller === controller) {
      recoveries.delete(probe)
      state.connectionLost = recoveries.size > 0
    }
  })
  const attempt = {
    scope,
    controller,
    promise,
    reconcile,
  }
  recoveries.set(probe, attempt)
  // Mutations are never replayed; reload lists to reconcile an uncertain result.
  void promise.then(async () => {
    if (attempt.reconcile && connectionScope() === scope && state.authenticated && !state.signingOut)
      await refresh()
  }).catch((error) => {
    if (connectionScope() === scope && state.authenticated && !state.signingOut)
      state.error = (error as Error).message
  })
  return promise
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

async function requestOnce<T = any>(
  url: string,
  options: RequestInit = {},
): Promise<T> {
  const requestOptions = {
    ...options,
    headers: {
      ...(options.body === undefined
        ? {}
        : { 'Content-Type': 'application/json' }),
      'X-CSRF-Token': state.csrf,
      ...options.headers,
    },
  }
  const path = state.installationId && url.startsWith(apiUrl('/')) ? url.slice(apiUrl('').length) : undefined
  let response: Response
  let text: string
  try {
    response = path && !/^\/tokens(?:\/|$)/.test(path)
      ? await installationTransport.request(path, url, requestOptions)
      : await fetch(url, requestOptions)
    text = await response.text()
  }
  catch (error) {
    if (options.signal?.aborted)
      throw error
    throw new ConnectionError(error)
  }

  let data: any
  try {
    data = JSON.parse(text)
  }
  catch (error) {
    if (response.status >= 502 && response.status <= 504)
      throw new ConnectionError(error)
    throw error
  }

  if (!response.ok) {
    if (response.status === 401)
      state.authenticated = false
    const message = typeof data?.error === 'string' ? data.error : ''
    const failure = new ApiError(message || 'Request failed. Please try again.', response.status)
    if (response.status >= 502 && response.status <= 504 && (!message || relayUnavailable.has(message.replace(/\.$/, ''))))
      throw new ConnectionError(failure)
    throw failure
  }

  return data
}

async function requestApi<T = any>(url: string, options: RequestInit = {}, account = false): Promise<T> {
  const scope = connectionScope()
  while (true) {
    try {
      return await requestOnce<T>(url, options)
    }
    catch (error) {
      const active = !!state.installationId && connectionScope() === scope && state.authenticated && !state.signingOut
      if (!(error instanceof ConnectionError))
        throw error
      if (!active)
        throw error.original

      const method = (options.method || 'GET').toUpperCase()
      const mutation = method !== 'GET' && method !== 'HEAD'
      const probe = account ? '/api/account/session' : apiUrl('/overview')
      const pending = recoverConnection(scope, mutation && !account, probe)
      if (mutation) {
        notify('Connection unavailable. Please try again.')
        throw new ApiError('', 0)
      }

      await awaitRecovery(pending, options.signal)
      if (options.signal?.aborted)
        throw options.signal.reason
      if (connectionScope() !== scope || !state.authenticated || state.signingOut)
        throw new ApiError('', 0)
    }
  }
}

export function api<T = any>(url: string, options: RequestInit = {}): Promise<T> {
  return requestApi<T>(apiUrl(url), options)
}

export function accountApi<T = any>(url: string, options: RequestInit = {}): Promise<T> {
  return requestApi<T>(`/api/account${url}`, options, true)
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
  const scope = connectionScope()
  const [agents, projects, tasks, skills, mcps] = await Promise.all([
    api<Agent[]>('/agents'),
    api<Project[]>('/projects'),
    api<Task[]>('/tasks'),
    api<Skill[]>('/skills'),
    state.installationRole === 'owner' ? api<McpView[]>('/mcps') : Promise.resolve([]),
  ])
  if (connectionScope() !== scope || !state.authenticated || state.signingOut)
    return
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
