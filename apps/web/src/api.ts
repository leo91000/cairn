import type {
  Agent,
  Project,
  Skill,
  Task,
} from '../../../packages/contracts/contracts'
import type { McpView } from '../../../packages/contracts/mcp'
import { reactive, watch } from 'vue'
import { clearHistoryCache } from './history-cache'
import { InstallationTransport } from './installation-transport'

// Resolve the beacon context before any shared view reads its local cache.
export const beaconEntry = typeof document !== 'undefined' && document.getElementById('app')?.hasAttribute('data-beacon')
const installationId = beaconEntry ? /^\/installations\/([\w-]+)(?:\/|$)/.exec(window.location.pathname)?.[1] || '' : ''

export const state = reactive({
  transportRoute: 'relay' as 'direct' | 'relay',
  ready: false,
  authenticated: false,
  signingOut: false,
  redirecting: false,
  csrf: '',
  installationId,
  installationOnline: undefined as boolean | undefined,
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

async function requestApi<T = any>(
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
  const response = path && !/^\/tokens(?:\/|$)/.test(path)
    ? await installationTransport.request(path, url, requestOptions)
    : await fetch(url, requestOptions)
  const data = await response.json()
  if (!response.ok) {
    if (response.status === 401)
      state.authenticated = false
    throw new ApiError(data.error || 'Request failed. Please try again.', response.status)
  }

  return data
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
    if (url.origin === window.location.origin && url.pathname.startsWith('/api/') && !url.pathname.startsWith('/api/installations/') && !url.pathname.startsWith('/api/public/'))
      return apiUrl(`${url.pathname.slice(4)}${url.search}${url.hash}`)
  }
  catch {}

  return value
}

export function ownerPage(path: string) {
  return ['/mcps', '/connections', '/nodes', '/authorize'].includes(path)
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
    window.location.assign('/')
  }
  catch (e) {
    notify((e as Error).message)
  }
  finally {
    state.signingOut = false
  }
}
