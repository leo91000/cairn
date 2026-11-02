import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'
import { reloadWebApp, startWebUpdates, webUpdate } from '../src/web-update'

const buildId = 'a'.repeat(64)
let browser: EventTarget & {
  location: {
    pathname: string
    search: string
    hash: string
    assign: ReturnType<typeof vi.fn>
    reload: ReturnType<typeof vi.fn>
  }
}
let document: EventTarget & { hidden: boolean }
let fetchBuild: ReturnType<typeof vi.fn>
let stop: (() => void) | undefined

beforeEach(() => {
  vi.useFakeTimers()
  vi.stubEnv('PROD', true)
  vi.stubGlobal('__CAIRN_WEB_BUILD_ID__', buildId)
  browser = Object.assign(new EventTarget(), {
    location: {
      pathname: '/installations/demo/',
      search: '?filter=mine',
      hash: '#reply',
      assign: vi.fn(),
      reload: vi.fn(),
    },
  })
  document = Object.assign(new EventTarget(), { hidden: false })
  vi.stubGlobal('window', browser)
  vi.stubGlobal('document', document)
  fetchBuild = vi.fn(async () => Response.json({ buildId }))
  vi.stubGlobal('fetch', fetchBuild)
  Object.assign(webUpdate, { available: false, loadFailed: false, destination: '' })
})

afterEach(() => {
  stop?.()
  stop = undefined
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.unstubAllEnvs()
  vi.restoreAllMocks()
})

function routerFor(failure?: Error) {
  const router = createRouter({
    history: createMemoryHistory('/installations/demo/'),
    routes: [
      { path: '/', component: { render: () => null } },
      { path: '/tasks', component: () => failure ? Promise.reject(failure) : Promise.resolve({ render: () => null }) },
    ],
  })
  stop = startWebUpdates(router)
  return router
}

it('checks the served build without caching and announces a change without navigating', async () => {
  routerFor()
  await vi.advanceTimersByTimeAsync(0)
  expect(fetchBuild).toHaveBeenCalledWith('/web-build.json', { cache: 'no-store', signal: expect.any(AbortSignal) })
  expect(webUpdate.available).toBe(false)
  fetchBuild.mockImplementation(async () => Response.json({ buildId: 'b'.repeat(64) }))
  browser.dispatchEvent(new Event('focus'))
  await vi.advanceTimersByTimeAsync(0)
  expect(webUpdate.available).toBe(true)
  expect(browser.location.assign).not.toHaveBeenCalled()
  expect(browser.location.reload).not.toHaveBeenCalled()
})

it('checks periodically only while visible and checks again on return', async () => {
  routerFor()
  await vi.advanceTimersByTimeAsync(0)
  document.hidden = true
  await vi.advanceTimersByTimeAsync(120000)
  expect(fetchBuild).toHaveBeenCalledTimes(1)
  document.hidden = false
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(0)
  expect(fetchBuild).toHaveBeenCalledTimes(2)
  await vi.advanceTimersByTimeAsync(60000)
  expect(fetchBuild).toHaveBeenCalledTimes(3)
})

it.each([
  async () => { throw new TypeError('Failed to fetch') },
  async () => new Response('offline', { status: 503 }),
  async () => new Response('<!doctype html>'),
  async () => Response.json({ buildId: 'invalid' }),
])('keeps the current app usable when a build check fails', async (response) => {
  fetchBuild.mockImplementation(response)
  routerFor()
  await vi.advanceTimersByTimeAsync(0)
  expect(webUpdate.available).toBe(false)
  fetchBuild.mockImplementation(async () => Response.json({ buildId: 'b'.repeat(64) }))
  browser.dispatchEvent(new Event('focus'))
  await vi.advanceTimersByTimeAsync(0)
  expect(webUpdate.available).toBe(true)
})

it.each([
  'Failed to fetch dynamically imported module: https://cairn.test/assets/Tasks-old.js',
  'Importing a module script failed.',
  '\'text/html\' is not a valid JavaScript MIME type for module script \'https://cairn.test/assets/Tasks-old.js\'.',
  'error loading dynamically imported module: https://cairn.test/assets/Tasks-old.js',
  'Unable to preload CSS for /assets/Tasks-old.css',
])('preserves the failed route including its base, query and hash: %s', async (message) => {
  const failure = new TypeError(message)
  const router = routerFor(failure)
  await router.push('/')
  await expect(router.push('/tasks?view=all#missions')).rejects.toBe(failure)
  expect(webUpdate.loadFailed).toBe(true)
  expect(webUpdate.destination).toBe('/installations/demo/tasks?view=all#missions')
  expect(router.currentRoute.value.path).toBe('/')
  reloadWebApp()
  expect(browser.location.assign).toHaveBeenCalledWith('/installations/demo/tasks?view=all#missions')
  expect(browser.location.reload).not.toHaveBeenCalled()
})

it('does not describe application errors as an update', async () => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {})
  const failure = new Error('Invalid mission state')
  const router = routerFor(failure)
  await expect(router.push('/tasks')).rejects.toBe(failure)
  expect(webUpdate.loadFailed).toBe(false)
  expect(errorLog).toHaveBeenCalledWith(failure)
})

it('keeps preload errors rejected and clears recovery after successful navigation', async () => {
  const router = routerFor()
  await router.push('/')
  const preload = new Event('vite:preloadError', { cancelable: true })
  browser.dispatchEvent(preload)
  expect(preload.defaultPrevented).toBe(false)
  expect(webUpdate.loadFailed).toBe(true)
  await router.push('/tasks')
  expect(webUpdate.loadFailed).toBe(false)
  expect(webUpdate.destination).toBe('')
})

it('reloads the current URL only when requested, without automatic retry state', () => {
  routerFor()
  reloadWebApp()
  expect(browser.location.reload).toHaveBeenCalledOnce()
  expect(browser.location.assign).not.toHaveBeenCalled()
})

it('stops background checks and ignores an in-flight result after disposal', async () => {
  let finish: (response: Response) => void = () => {}
  fetchBuild.mockImplementation(() => new Promise<Response>(resolve => finish = resolve))
  routerFor()
  stop!()
  finish(Response.json({ buildId: 'b'.repeat(64) }))
  browser.dispatchEvent(new Event('focus'))
  document.dispatchEvent(new Event('visibilitychange'))
  browser.dispatchEvent(new Event('vite:preloadError'))
  await vi.advanceTimersByTimeAsync(120000)
  expect(fetchBuild).toHaveBeenCalledOnce()
  expect(webUpdate.available).toBe(false)
  expect(webUpdate.loadFailed).toBe(false)
})
