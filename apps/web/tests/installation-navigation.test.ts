import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'

// Run the actual navigation and transport together, with only the draft handoff held.
async function navigationSetup() {
  vi.resetModules()
  const location = Object.assign(new URL('https://cairn.test/installations/install/mcps/callback?code=fixture-code#return'), { assign: vi.fn() })
  vi.stubGlobal('window', Object.assign(new EventTarget(), { location }))
  vi.stubGlobal('document', Object.assign(new EventTarget(), {
    getElementById: () => ({ hasAttribute: () => true }),
    hidden: false,
  }))
  vi.stubGlobal('navigator', { onLine: true })

  class Peer {
    createDataChannel() { return Object.assign(new EventTarget(), { close() {} }) }
    async createOffer() { return { sdp: `v=0\r\na=fingerprint:sha-256 ${'AA:'.repeat(31)}AA\r\n` } }
    close() {}
  }

  vi.stubGlobal('RTCPeerConnection', Peer)
  const installation = {
    id: 'install',
    slug: 'quiet-cedar',
    name: 'Installation',
    online: true,
    role: 'owner',
  }
  const account = {
    authenticated: true,
    account: { id: 'account' },
    csrf: 'fixture',
    installations: [installation],
  }
  const fetch = vi.fn(async (path: string) => {
    if (path === '/api/account/session')
      return Response.json(account)
    if (path === '/api/installations')
      return Response.json([installation])
    return Response.json({ available: false })
  })
  vi.stubGlobal('fetch', fetch)
  const api = await import('../src/api')
  const transfer = Promise.withResolvers<string | undefined>()
  const handoff = vi.fn(() => transfer.promise)
  const imports: Record<string, any> = {
    'vue': {
      ...vue,
      onMounted() {},
      provide() {},
    },
    'vue-router': { useRouter: () => ({}) },
    './api': api,
    './beacon-location': {
      beaconOrigin: location.origin,
      centralAccount: true,
      installationRoute: 'install',
      installationHost: '',
      installationSubdomains: true,
      installationUrl: (_slug: string, path: string) => new URL(path, 'https://quiet-cedar.cairn.test').href,
      workspaceReturn() {},
    },
    './client-performance': {
      clientPerformance: {
        capture() {},
        begin() {},
        probe() {},
        measure: (_name: string, read: () => unknown) => read(),
      },
    },
    './client-performance-runtime': { startClientPerformance: () => ({ stop() {} }) },
    './draft-transfer': { transferInstallationDrafts: handoff },
  }
  const source = readFileSync(new URL('../src/BeaconApp.vue', import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: 'installation-navigation-test' }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const exports: any = {}
  vm.runInNewContext(code, {
    exports,
    URL,
    URLSearchParams,
    Error,
    AbortController,
    window,
    document,
    fetch,
    setTimeout,
    clearTimeout,
    require: (path: string) => imports[path] ?? {},
  })
  const scope = vue.effectScope()
  const page = scope.run(() => exports.default.setup({}, { expose() {} }))
  await page.loadSession()
  const authorizations = () => fetch.mock.calls.filter(([path]) => path.endsWith('/direct/authorize'))
  const availability = () => fetch.mock.calls.filter(([path]) => path === '/api/installations')
  const destination = 'https://quiet-cedar.cairn.test/mcps/callback?code=fixture-code#return'
  return {
    ...api,
    page,
    fetch,
    authorizations,
    availability,
    handoff,
    transfer,
    destination,
    location,
    scope,
  }
}

afterEach(async () => {
  const { state } = await import('../src/api')
  state.authenticated = false
  vi.unstubAllGlobals()
})

it('keeps direct authorization stopped while canonical navigation waits for browser drafts', async () => {
  const fixture = await navigationSetup()
  try {
    await vue.nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(fixture.handoff).toHaveBeenCalledWith('install', 'quiet-cedar', 'account')
    expect(fixture.authorizations()).toEqual([])
    expect(fixture.state.redirecting).toBe(true)
    expect(fixture.location.assign).not.toHaveBeenCalled()
    fixture.transfer.resolve('opaque-handoff')
    const destination = new URL(fixture.destination)
    destination.searchParams.set('cairn_draft_transfer', 'opaque-handoff')
    await vi.waitFor(() => expect(fixture.location.assign).toHaveBeenCalledWith(destination.href))
    expect(fixture.authorizations()).toEqual([])
  }
  finally {
    fixture.transfer.resolve(undefined)
    await vue.nextTick()
    fixture.scope.stop()
  }
})

it('resumes direct authorization and availability polls if the draft handoff fails before navigation', async () => {
  const fixture = await navigationSetup()
  try {
    expect(fixture.availability()).toEqual([])
    fixture.transfer.reject(new Error('Draft handoff failed'))
    await vi.waitFor(() => expect(fixture.page.navigationPending.value).toBe(false))
    expect(fixture.state.redirecting).toBe(false)
    expect(fixture.page.error.value).toBe('Draft handoff failed')
    expect(fixture.location.assign).not.toHaveBeenCalled()
    await vi.waitFor(() => expect(fixture.authorizations()).toHaveLength(1))
    expect(fixture.availability()).toHaveLength(1)
  }
  finally {
    fixture.scope.stop()
  }
})
