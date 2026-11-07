import type { Router } from 'vue-router'
import type { PerformanceScreen, WebBuild } from './client-performance'
import { ref, watch } from 'vue'
import { state } from './api'
import { rememberPreference, sharedPreference } from './beacon-location'
import { clientPerformance } from './client-performance'
import { monitorPerformance } from './client-performance-monitor'
import { performancePreference } from './client-performance-preference'
import { performanceUploader } from './client-performance-upload'

declare const __CAIRN_WEB_BUILD_ID__: string

const preference = 'cairn_performance_upload'

function storedPreference() {
  let value: string | null | undefined
  try {
    value = sharedPreference(preference)
  }
  catch { /* Cookie access may be unavailable even when local storage works. */ }

  if (value !== 'true' && value !== 'false') {
    try {
      value = localStorage.getItem(preference)
    }
    catch { /* The current tab's in-memory choice remains authoritative. */ }
  }

  return value === 'true' ? true : value === 'false' ? false : undefined
}

function browser(): WebBuild['browser'] {
  // Only a fixed browser family is retained; never the user agent or its version.
  const agent = navigator.userAgent
  if (/Edg\//.test(agent))
    return 'EDGE'
  if (/Firefox\/|FxiOS\//.test(agent))
    return 'FIREFOX'
  if (/Chrome\/|Chromium\/|CriOS\//.test(agent))
    return 'CHROME'
  if (/Safari\//.test(agent))
    return 'SAFARI'
  return 'OTHER'
}

/** Receives router record templates, never a concrete path, query, resource ID or title. */
export function performanceScreen(records: readonly { path: string }[]): PerformanceScreen {
  const template = records.at(-1)?.path || ''
  if (template === '/' || template === '/fil')
    return 'FIL'
  if (template.startsWith('/chats'))
    return 'CHAT'
  if (template === '/tasks')
    return 'MISSIONS'
  if (template.startsWith('/runs') || template.startsWith('/fil/runs'))
    return 'RUN'
  if (template.startsWith('/settings'))
    return 'SETTINGS'
  if (['/atelier', '/agents', '/projects', '/skills', '/nodes', '/plugins', '/connections', '/mcps'].some(path => template === path || template.startsWith(`${path}/`)))
    return 'ATELIER'
  return 'OTHER'
}

export function startClientPerformance(router: Router) {
  const preferenceState = performancePreference({
    read: storedPreference,
    write: (value) => {
      try {
        localStorage.setItem(preference, value ? 'true' : 'false')
      }
      catch { /* Try the shared cookie independently. */ }

      rememberPreference(preference, value ? 'true' : 'false')
    },
  })
  const enabled = ref(preferenceState.refresh())
  const build: WebBuild = { buildId: __CAIRN_WEB_BUILD_ID__, buildType: import.meta.env.PROD ? 'release' : 'debug', browser: browser() }
  let foreground = false

  function snapshot() {
    const connection = !state.installationId ? 'NONE' : state.connectionLost || state.installationOnline === false ? 'OFFLINE' : state.transportRoute === 'direct' ? 'DIRECT' : 'RELAY'
    return clientPerformance.report(build, connection)
  }

  const uploader = performanceUploader({
    now: () => performance.now(),
    schedule: (callback, milliseconds) => {
      const timer = window.setTimeout(callback, milliseconds)
      return () => window.clearTimeout(timer)
    },
    report: snapshot,
    enabled: () => preferenceState.refresh(),
    send: async (report, csrf, signal) => {
      const body = JSON.stringify(report)
      if (new TextEncoder().encode(body).byteLength > 65_536)
        return false
      const response = await fetch('/api/account/telemetry/performance', {
        method: 'POST',
        credentials: 'same-origin',
        redirect: 'error',
        signal,
        headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
        body,
      })
      return response.status === 204
    },
  })

  function synchronizeUpload() {
    uploader.update({
      accountId: state.authenticated && !state.signingOut ? state.accountId : '',
      csrf: state.csrf,
      foreground,
      enabled: enabled.value,
    })
  }

  const monitor = monitorPerformance(clientPerformance, undefined, (visible) => {
    foreground = visible
    synchronizeUpload()
  })
  const stopAccount = watch(() => state.accountId, () => monitor.clear(), { flush: 'sync' })
  const stopUpload = watch(() => [state.authenticated, state.accountId, state.csrf, state.signingOut, enabled.value], synchronizeUpload, { immediate: true, flush: 'sync' })
  const stopRoute = watch(router.currentRoute, (route) => {
    clientPerformance.setScreen(performanceScreen(route.matched))
    clientPerformance.probe('NAVIGATION')
  }, { immediate: true, flush: 'sync' })

  function synchronizePreference() {
    enabled.value = preferenceState.refresh()
    synchronizeUpload()
  }

  window.addEventListener('storage', synchronizePreference)
  window.addEventListener('focus', synchronizePreference)

  return {
    enabled,
    snapshot,
    clear: () => monitor.clear(),
    setEnabled(value: boolean) {
      enabled.value = preferenceState.set(value)
      synchronizeUpload()
    },
    stop() {
      stopAccount()
      stopUpload()
      stopRoute()
      window.removeEventListener('storage', synchronizePreference)
      window.removeEventListener('focus', synchronizePreference)
      uploader.stop()
      monitor.stop()
      clientPerformance.clear()
    },
  }
}

export type ClientPerformanceRuntime = ReturnType<typeof startClientPerformance>
