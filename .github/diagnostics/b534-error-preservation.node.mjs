import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { resolve } from 'node:path'
import { setImmediate as nextTurn } from 'node:timers/promises'
import { runInNewContext } from 'node:vm'
const path = '.github/diagnostics/b534-navigation-probe.ts'
const raw = process.argv.includes('--baseline') ? execFileSync('git', ['-c', `safe.directory=${process.cwd()}`, 'show', `87590feb14088429c2869b24a8138c29e6b7d3b3:${path}`], { encoding: 'utf8' }) : readFileSync(new URL('./b534-navigation-probe.ts', import.meta.url), 'utf8')
const source = stripTypeScriptTypes(raw).replace(/^import .*\n/gm, '')
const tests = []
const expect = value => ({
  toBe: expected => assert.equal(value, expected),
  toEqual: expected => assert.deepEqual(Array.from(value), Array.from(expected)),
  toBeVisible: async () => {}, toBeAttached: async () => {}, toHaveAttribute: async () => {},
})
expect.poll = callback => ({ async toBe(expected) {
  let result
  for (let i = 0; i < 30; i++) { result = await callback(); if (result === expected) return; await nextTurn() }
  assert.equal(result, expected)
} })
let closeProvider
runInNewContext(source, {
  test: (name, body) => tests.push(body), expect, Date, Promise, URL, Error, AggregateError, setTimeout, clearTimeout,
  process: { env: { CAIRN_B534_CONTROL: 'native-cancel' } },
  saveB534Journal: writeFileSync, workspacePath: path => path,
  authenticateWorkspace: async () => {},
  mcpProvider: async () => ({ origin: 'http://fixture.test', exchanges: 1, close: async () => closeProvider() }),
})
let gaps = 0
for (const scenario of ['healthy-boundary', 'late-http403-after-drain', 'late-pending-after-drain', 'startup-http500', 'startup-fetch-rejection', 'workload-cleanup-error', 'workload-attach-provider-error', 'workload-error-cleanup-ok']) {
  const workloadError = new Error('controlled first workload error')
  const cleanupError = new Error('controlled cleanup error')
  const attachError = new Error('controlled attach error')
  const providerError = new Error('controlled provider error')
  const listeners = new Map(), routes = new Map(), active = []
  const frame = {}
  const locator = { or() { return this }, first() { return this }, filter() { return this }, getByRole() { return this }, getByText() { return this }, click: async () => {} }
  let document = 'old', pending = 0, record, target
  const request = (path, navigation = false) => ({ url: () => 'http://fixture.test' + path, method: () => 'GET', resourceType: () => navigation ? 'document' : 'fetch', isNavigationRequest: () => navigation, failure: () => ({ errorText: 'Load request cancelled' }) })
  const invoke = (pattern, navigation = false) => {
    const req = request(navigation ? '/agents' : '/api/installations', navigation)
    listeners.get('request')?.(req)
    if (!navigation) { target = req; pending = 1 }
    else { listeners.get('requestfailed')(target); record('poll-rejected-signal-active', 1, 'old', 4); pending = 0 }
    const operation = routes.get(pattern)({
      request: () => req, fetch: async () => ({ status: () => 200 }), abort: async () => {},
      fulfill: async () => {
        if (navigation) { document = 'new'; listeners.get('framenavigated')(frame) }
      },
    })
    active.push(operation)
    return operation
  }
  const page = {
    on: (name, callback) => listeners.set(name, callback), mainFrame: () => frame,
    exposeFunction: async (_, callback) => { record = callback }, addInitScript: async () => {},
    setViewportSize: async () => {}, emulateMedia: async () => {}, waitForURL: async () => {},
    getByRole: () => locator, getByLabel: () => locator, locator: () => locator,
    route: async (pattern, callback) => routes.set(pattern, callback),
    clock: { install: async () => {}, pauseAt: async () => {}, runFor: async () => { void invoke('**/api/installations'); await nextTurn() } },
    goto: async path => {
      if (path === '/agents' && scenario.startsWith('workload-')) {
        listeners.get('requestfailed')(target)
        record('poll-rejected-signal-active', 1, 'old', 4)
        pending = 0
        throw workloadError
      }
      if (path === '/agents') return invoke('**/agents', true)
      listeners.get('framenavigated')(frame)
      if (scenario === 'startup-http500') {
        const req = request('/api/projects')
        listeners.get('request')(req)
        listeners.get('response')({ request: () => req, status: () => 500 })
        listeners.get('requestfinished')(req)
      }
      if (scenario === 'startup-fetch-rejection') {
        record('http-start', 0, 'old', 99, { method: 'GET', path: '/api/projects' })
        record('http-rejected-signal-active', 1, 'old', 99)
      }
    },
    evaluate: async callback => {
      const text = callback.toString()
      if (text.includes('shellPending.size')) return pending
      if (text.includes('targetController')) return { active: true, poll: 4, document }
      if (text.includes('return observation.documentId') || text.includes('b534Observation.documentId')) return document
      if (text.includes('directChatAccepted') || text.includes('chatAccepted') || text.includes('armedSources')) return true
    },
    unrouteAll: async () => { if (scenario === 'workload-cleanup-error') throw cleanupError; await Promise.all(active) },
  }
  closeProvider = () => {
    if (scenario === 'workload-attach-provider-error') throw providerError
    if (scenario !== 'late-http403-after-drain' && scenario !== 'late-pending-after-drain') return
    const req = request('/api/projects')
    listeners.get('request')(req)
    if (scenario === 'late-http403-after-drain') {
      listeners.get('response')({ request: () => req, status: () => 403 })
      listeners.get('requestfinished')(req)
    }
  }
  const dir = resolve('test-results/source-only-b534-errors', scenario)
  mkdirSync(dir, { recursive: true })
  const info = { outputPath: name => resolve(dir, name), attach: async () => { if (scenario === 'workload-attach-provider-error') throw attachError } }
  let failure
  try { await tests[0]({ page, workspace: { api: async () => {} } }, info) }
  catch (error) { failure = error }
  const shouldFail = scenario !== 'healthy-boundary'
  if (!!failure !== shouldFail) { gaps++; console.log(scenario + ': final-boundary gap') }
  else console.log(scenario + ': PASS')
  if (failure && !shouldFail) console.log(String(failure))
  if (scenario.startsWith('workload-')) {
    const flatten = error => [error, ...(Array.isArray(error?.errors) ? error.errors.flatMap(flatten) : [])]
    const errors = flatten(failure)
    const observation = JSON.parse(readFileSync(resolve(dir, 'b534-navigation.json'), 'utf8'))
    const retained = errors.includes(workloadError)
      && (scenario !== 'workload-cleanup-error' || errors.includes(cleanupError))
      && (scenario !== 'workload-attach-provider-error' || (errors.includes(attachError) && errors.includes(providerError)))
    const audited = observation.journal.some(event => event.kind === 'outcome-validation-entered')
    if (!retained || !audited) { gaps++; console.log(scenario + ': first failure or final audit LOST') }
    else console.log(scenario + ': exact first and cleanup errors retained; final audits invoked')
  }

}
if (gaps) process.exitCode = 1
else console.log('Actual archived callback source checks PASS; controlled adapters only, no native browser.')
