import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { resolve } from 'node:path'
import { setImmediate } from 'node:timers/promises'
import { runInNewContext } from 'node:vm'

const filename = '.github/diagnostics/skill-availability-probe.ts'
const raw = process.argv.includes('--baseline')
  ? execFileSync('git', ['-c', `safe.directory=${process.cwd()}`, 'show', `c5e4786c9f39eea5e3c51df2b32af1cda2b15006:${filename}`], { encoding: 'utf8' })
  : readFileSync(new URL('./skill-availability-probe.ts', import.meta.url), 'utf8')
const text = stripTypeScriptTypes(raw).replace(/^import .*\n/gm, '').replace(/^export /gm, '')
let gaps = 0
for (const boundary of ['pending-poll', 'departing-timer']) {
  for (const scenario of ['ordinary-error', 'stalled-attachment', 'journal-entry', 'journal-completion']) {
    const timers = new Map()
    let next = 0
    const tests = []
    const listeners = new Map()
    const routes = new Map()
    const active = []
    const workload = new Error('controlled original workload failure')
    const persistence = new Error('controlled journal persistence failure')
    let record
    let document = 'old'
    let pending = 0
    let target
    let attachCalls = 0
    let closeCalls = 0
    let routeDrainCalls = 0
    let done = false
    let failure
    let lastPayload
    const expect = value => ({ toBe: expected => assert.equal(value, expected), toEqual: expected => assert.deepEqual(Array.from(value), Array.from(expected)), toBeVisible: async () => {}, toBeAttached: async () => {} })
    expect.poll = callback => ({ toBe: async expected => {
      for (let i = 0; i < 30; i++) {
        const value = await callback()
        if (value === expected) return
        await setImmediate()
      }
      assert.equal(await callback(), expected)
    } })
    const locator = { or() { return this }, first() { return this } }
    const request = navigation => ({ url: () => 'http://fixture.invalid/' + (navigation ? 'skills' : 'api/installations'), resourceType: () => navigation ? 'document' : 'fetch', isNavigationRequest: () => navigation, failure: () => ({ errorText: 'Load request cancelled' }) })
    const poll = () => {
      const req = request(false)
      target = req
      listeners.get('request')(req)
      record('poll-start', 1, 'old', 1)
      pending = 1
      const operation = routes.get('**/api/installations')({ request: () => req, fetch: async () => ({ status: () => 200 }), fulfill: async () => {}, abort: async () => {} })
      active.push(operation)
    }
    const page = {
      on: (kind, fn) => listeners.set(kind, fn),
      once: (kind, fn) => listeners.set(kind, (...args) => { listeners.delete(kind); return fn(...args) }),
      off: (kind, fn) => { if (listeners.get(kind) === fn) listeners.delete(kind) },
      exposeFunction: async (_, fn) => { record = fn }, addInitScript: async () => {},
      getByRole: () => locator, getByLabel: () => locator,
      route: async (pattern, fn) => routes.set(pattern, fn),
      goto: async () => {},
      clock: { install: async () => {}, pauseAt: async () => {}, runFor: async () => { poll(); await setImmediate() } },
      reload: async () => {
        if (target) {
          listeners.get('requestfailed')(target)
          record('poll-rejected', 2, 'old', 1)
          pending = 0
        }
        throw workload
      },
      evaluate: async fn => {
        const text = fn.toString()
        if (text.includes('shellObserverFailed')) return false
        if (text.includes('({ document:')) return { document, pending }
        if (text.includes('shellObservation.document')) return document
        if (text.includes('shellPending.size')) return pending
      },
      close: async () => { closeCalls++; listeners.get('close')?.() },
      unrouteAll: async () => { routeDrainCalls++; await Promise.all(active) },
    }
    runInNewContext(text, {
      test: (name, fn) => { if (name.includes(boundary)) tests.push(fn) },
      expect, Date, Promise, URL, Error, AggregateError,
      setTimeout: (fn, delay) => { assert.equal(delay, 7000); timers.set(++next, fn); return next },
      clearTimeout: id => timers.delete(id),
      saveAvailabilityJournal: (path, content) => {
        lastPayload = JSON.parse(content)
        const kind = lastPayload.journal.at(-1)?.kind
        if (scenario === 'journal-entry' && kind === 'cleanup-entered') throw persistence
        if (scenario === 'journal-completion' && /^cleanup-(completed|failed)$/.test(kind)) throw persistence
        writeFileSync(path, content)
      },
      workspacePath: path => path, authenticateWorkspace: async () => {}, useRelayForHttpMocks: async () => {},
    })
    assert.equal(tests.length, 1)
    const dir = resolve('test-results/source-only-skill-errors', boundary, scenario, process.argv.includes('--baseline') ? 'baseline' : 'candidate')
    mkdirSync(dir, { recursive: true })
    const run = tests[0]({ page }, { outputPath: name => resolve(dir, name), attach: () => {
      attachCalls++
      return scenario === 'stalled-attachment' ? new Promise(() => {}) : Promise.resolve()
    } }).then(() => { done = true }, error => { done = true; failure = error })
    for (let turn = 0; turn < 120 && !done; turn++) {
      await setImmediate()
      for (const callback of [...timers.values()]) callback()
    }
    const flatten = value => [value, ...(Array.isArray(value?.errors) ? value.errors.flatMap(flatten) : [])]
    const errors = flatten(failure)
    const retained = done && errors.includes(workload)
      && (!scenario.startsWith('journal-') || errors.includes(persistence))
      && (scenario !== 'stalled-attachment' || errors.some(error => error?.message?.includes('deadline')))
    const owners = closeCalls === 1 && routeDrainCalls === 1 && attachCalls === 1
    if (!retained || !owners) {
      gaps++
      console.log(`${boundary}/${scenario}: first failure or owned cleanup LOST`)
    }
    else console.log(`${boundary}/${scenario}: PASS actual callback retained causes and all owner attempts`)
    assert.ok(lastPayload)
    // Pending baseline attachment has no timers/IO; never await it or call it native.
    void run
  }
}
console.log('Controlled actual-callback source fixtures only, no browser/native evidence.')
if (gaps) process.exitCode = 1
