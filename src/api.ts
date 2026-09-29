import type { Agent, Project, Skill, Task } from '../shared/contracts'
import type { McpView } from '../shared/mcp'
import { reactive, watch } from 'vue'
import { clearHistoryCache } from './history-cache'

export const state = reactive({
  ready: false,
  authenticated: false,
  signingOut: false,
  redirecting: false,
  setupRequired: false,
  csrf: '',
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
export async function api<T = any>(
  url: string,
  options: RequestInit = {},
): Promise<T> {
  const response = await fetch(`/api${url}`, {
    ...options,
    headers: {
      ...(options.body === undefined
        ? {}
        : { 'Content-Type': 'application/json' }),
      'X-CSRF-Token': state.csrf,
      ...options.headers,
    },
  })
  const data = await response.json()
  if (!response.ok) {
    if (response.status === 401)
      state.authenticated = false
    throw new ApiError(data.error || 'Request failed. Please try again.', response.status)
  }
  return data
}
export async function session() {
  Object.assign(state, await api('/session'))
  state.ready = true
}
export async function refresh() {
  const [agents, projects, tasks, skills, mcps] = await Promise.all([
    api<Agent[]>('/agents'),
    api<Project[]>('/projects'),
    api<Task[]>('/tasks'),
    api<Skill[]>('/skills'),
    api<McpView[]>('/mcps'),
  ])
  Object.assign(state, { agents, projects, tasks, skills, mcps })
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
export async function signOut() {
  if (state.signingOut)
    return
  state.signingOut = true
  try {
    await api('/logout', { method: 'POST' })
    state.authenticated = false
    state.csrf = ''
    notify('Signed out')
  }
  catch (e) {
    notify((e as Error).message)
  }
  finally {
    state.signingOut = false
  }
}
