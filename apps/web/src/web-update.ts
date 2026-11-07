import type { Router } from 'vue-router'
import { reactive } from 'vue'

declare const __CAIRN_WEB_BUILD_ID__: string

export const webUpdate = reactive({ available: false, loadFailed: false, destination: '' })

function moduleLoadError(error: unknown) {
  return error instanceof Error && /Failed to fetch dynamically imported module|Importing a module script failed|error loading dynamically imported module|not a valid JavaScript MIME type for module script|Unable to preload CSS/i.test(error.message)
}

/** An open tab can outlive the deployment that supplied its lazy chunk URLs. */
export function startWebUpdates(router: Router) {
  let checking = false
  let stopped = false
  let pendingDestination = ''

  async function check() {
    if (document.hidden || checking || stopped || webUpdate.available || !import.meta.env.PROD)
      return
    checking = true
    try {
      const response = await fetch('/web-build.json', { cache: 'no-store', signal: AbortSignal.timeout(5000) })
      if (!response.ok)
        return
      const build: unknown = await response.json()
      if (stopped || !build || typeof build !== 'object' || !('buildId' in build))
        return
      const remoteBuildId = build.buildId
      if (typeof remoteBuildId !== 'string' || !/^[a-f0-9]{64}$/.test(remoteBuildId))
        return
      webUpdate.available = remoteBuildId !== __CAIRN_WEB_BUILD_ID__
    }
    catch {
      // Offline, or a deployment that does not expose its build yet. Try later.
    }
    finally {
      checking = false
    }
  }

  function preloadFailed() {
    webUpdate.loadFailed = true
    webUpdate.destination = pendingDestination
    void check()
    // Let the import reject so Vue Router aborts the failed navigation.
  }

  const removeBefore = router.beforeEach((to) => {
    pendingDestination = router.resolve(to.fullPath).href
  })
  const removeError = router.onError((error, to) => {
    pendingDestination = ''
    if (!moduleLoadError(error)) {
      console.error(error)
      return
    }

    webUpdate.loadFailed = true
    webUpdate.destination = router.resolve(to.fullPath).href
    void check()
  })
  const removeAfter = router.afterEach((_to, _from, failure) => {
    pendingDestination = ''
    if (failure)
      return
    webUpdate.loadFailed = false
    webUpdate.destination = ''
  })
  const checkVisible = () => {
    void check()
  }

  window.addEventListener('vite:preloadError', preloadFailed)
  window.addEventListener('focus', checkVisible)
  document.addEventListener('visibilitychange', checkVisible)
  const timer = setInterval(checkVisible, 60000)
  void check()

  return () => {
    stopped = true
    clearInterval(timer)
    window.removeEventListener('vite:preloadError', preloadFailed)
    window.removeEventListener('focus', checkVisible)
    document.removeEventListener('visibilitychange', checkVisible)
    removeError()
    removeBefore()
    removeAfter()
  }
}

export function reloadWebApp() {
  if (webUpdate.destination && webUpdate.destination !== `${window.location.pathname}${window.location.search}${window.location.hash}`) {
    window.location.assign(webUpdate.destination)
    return
  }

  window.location.reload()
}
