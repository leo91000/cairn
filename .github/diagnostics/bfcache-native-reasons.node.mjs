import assert from 'node:assert/strict'
import { webcrypto } from 'node:crypto'
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { execFileSync } from 'node:child_process'
import { stripTypeScriptTypes } from 'node:module'
import { setImmediate } from 'node:timers/promises'
import { createContext, runInContext } from 'node:vm'
import test from 'node:test'
import { armNativeHistoryIdentity, readNativeHistoryReasons, validateClosedProjection, acquireNativeReasonDiagnostic, finalizeNativeReasonDiagnostic } from './bfcache-native-reasons.mjs'

function realm(raw = { reasons: [{ reason: 'response-cache-control-no-store' }], children: [], url: 'PRIVATE_URL', id: 'PRIVATE_ID', name: 'PRIVATE_NAME', src: 'PRIVATE_SRC' }) {
  const storage = new Map()
  const state = { type: 'navigate', raw, timingHref: 'http://fixture.invalid/mcps' }
  class NavigationEntry {
    get type() { return state.type }
    get name() { return state.timingHref }
    get notRestoredReasons() { return state.raw }
  }
  class PerformanceClass {
    getEntriesByType() { return [entry] }
  }
  const entry = new NavigationEntry()
  const sandbox = {
    crypto: webcrypto, TextEncoder, Uint8Array,
    isSecureContext: true,
    Performance: PerformanceClass,
    PerformanceNavigationTiming: NavigationEntry,
    performance: new PerformanceClass(),
    navigation: { currentEntry: { key: 'original-entry' } },
    cairnAvailabilityDocument: 'original-document',
    location: { href: state.timingHref, origin: 'http://fixture.invalid' },
    document: {},
    sessionStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value), removeItem: key => storage.delete(key) },
  }
  const context = createContext(sandbox)
  runInContext('window = globalThis; top = globalThis; document.defaultView = globalThis', context)
  return {
    sandbox, state, storage,
    arm: () => runInContext(`(${armNativeHistoryIdentity.toString()})()`, context),
    read: () => runInContext(`(${readNativeHistoryReasons.toString()})()`, context),
    back() {
      state.type = 'back_forward'
      sandbox.cairnAvailabilityDocument = 'new-document-after-cache-miss'
    },
  }
}

async function projection(raw) {
  const f = realm(raw)
  assert.equal((await f.arm()).armed, true)
  f.back()
  return f.read()
}

test('serialized projection in a controlled source realm is closed and anonymized', async () => {
  const result = await projection()
  assert.equal(result.projectionComplete, true)
  assert.equal(result.historyEntryMatched, true)
  assert.equal(result.originalDocumentSame, false)
  assert.equal(validateClosedProjection(result), true)
  assert.equal(JSON.stringify(result).includes('PRIVATE_'), false)
  assert.deepEqual(Object.keys(result.tree).sort(), ['children', 'reasonCodes'])
})

for (const [name, raw] of [
  ['null', null], ['undefined', undefined],
  ['masked', { reasons: [{ reason: 'masked' }], children: [] }],
  ['cross-origin masked', { reasons: [], children: [{ reasons: null, children: [] }] }],
  ['empty reasons', { reasons: [], children: [] }],
  ['non-code payload', { reasons: [{ reason: 'https://PRIVATE_URL/?secret=PRIVATE_VALUE' }], children: [] }],
]) {
  test(`actual projection makes ${name} inconclusive without raw leakage`, async () => {
    const f = realm()
    f.state.raw = raw
    assert.equal((await f.arm()).armed, true)
    f.back()
    const result = await f.read()
    assert.equal(result.projectionComplete, false)
    assert.equal(JSON.stringify(result).includes('PRIVATE_'), false)
  })
}

for (const kind of ['wrong-entry', 'wrong-location', 'wrong-origin', 'foreign-frame', 'foreign-navigation', 'unknown-api', 'replayed-read']) {
  test(`actual identity gate rejects ${kind}`, async () => {
    const f = realm()
    assert.equal((await f.arm()).armed, true)
    f.back()
    if (kind === 'wrong-entry')
      f.sandbox.navigation.currentEntry.key = 'foreign'
    if (kind === 'wrong-location')
      f.sandbox.location.href = 'http://fixture.invalid/foreign'
    if (kind === 'wrong-origin')
      f.sandbox.location.origin = 'http://foreign.invalid'
    if (kind === 'foreign-frame')
      f.sandbox.top = {}
    if (kind === 'foreign-navigation')
      f.state.type = 'navigate'
    if (kind === 'unknown-api')
      f.sandbox.PerformanceNavigationTiming = undefined
    if (kind === 'replayed-read')
      await f.read()
    const result = await f.read()
    assert.equal(result.projectionComplete, false)
  })
}

test('reason wording is not a fixed cause allowlist', async () => {
  const result = await projection({ reasons: [{ reason: 'Future native reason v9' }], children: [] })
  assert.equal(result.projectionComplete, true)
  assert.equal(result.tree.reasonCodes[0], 'Future native reason v9')
})

const closure = {
  arm: {
    armed: true,
    mainFrame: true,
    nativeHistoryPresent: true,
    secureContext: true,
  },
  cleanupFailureCount: 0,
  auditFailureCount: 0,
  pageClosed: true,
  httpClosed: true,
  journalAcknowledged: true,
}
async function acquisition(extra = {}) {
  const first = new Error('original strict persisted=true checkpoint failed')
  const calls = { back: 0, harvest: 0, observe: 0 }
  const result = await acquireNativeReasonDiagnostic({
    checkpoint: async () => {
      throw first
    },
    checkpointFlags: () => [{ persisted: false, trusted: true }],
    browserVersion: () => '155.0.0.0',
    history: async () => {
      calls.back++
    },
    harvest: async () => {
      calls.harvest++
    },
    observe: async () => {
      calls.observe++
      return projection()
    },
    ...extra,
  })
  return { ...result, calls, first }
}

test('real controller keeps original checkpoint red while acquiring once', async () => {
  const result = await acquisition()
  assert.equal(result.failures[0], result.first)
  assert.deepEqual(result.calls, { back: 1, harvest: 1, observe: 1 })
  assert.equal(result.receipt.nativeReadUsable, true)
  assert.equal(result.receipt.productQualified, false)
  assert.equal(finalizeNativeReasonDiagnostic(result.receipt, closure).observationComplete, true)
})

test('wrong or missing checkpoint never admits history', async () => {
  const result = await acquisition({ checkpointFlags: () => [] })
  assert.equal(result.calls.back, 0)
  assert.equal(result.receipt.nativeReadUsable, false)
  assert.equal(result.failures[0], result.first)
})

test('later failure never replaces the original failed checkpoint', async () => {
  const late = new Error('history failed')
  const result = await acquisition({ history: async () => {
    throw late
  } })
  assert.equal(result.failures[0], result.first)
  assert.equal(result.failures.includes(late), true)
  assert.equal(result.calls.observe, 0)
})

for (const key of ['pageClosed', 'httpClosed', 'journalAcknowledged', 'cleanupFailureCount', 'auditFailureCount', 'arm']) {
  test(`final gate keeps ${key} unknown/failure red`, async () => {
    const result = await acquisition()
    const broken = { ...closure, [key]: key.endsWith('Count') ? 1 : false }
    assert.equal(finalizeNativeReasonDiagnostic(result.receipt, broken).observationComplete, false)
  })
}

test('publication guard drops unexpected outer data and refuses completeness', async () => {
  const result = await acquisition()
  const final = finalizeNativeReasonDiagnostic({ ...result.receipt, url: 'PRIVATE_URL' }, closure)
  assert.equal(final.observationComplete, false)
  assert.equal(JSON.stringify(final).includes('PRIVATE_'), false)
})

test('projection schema refuses hidden data and inconsistent masked/empty claims', async () => {
  const value = await projection()
  assert.equal(validateClosedProjection({ ...value, url: 'PRIVATE_URL' }), false)
  assert.equal(validateClosedProjection({ ...value, tree: { ...value.tree, name: 'PRIVATE_NAME' } }), false)
  assert.equal(validateClosedProjection({ ...value, tree: { reasonCodes: ['masked'], children: [] } }), false)
  assert.equal(validateClosedProjection({ ...value, tree: { reasonCodes: [], children: [] }, reasonCount: 0 }), false)
})

test('existing bounded helper returns a stalled history as red with no replay', { timeout: 10000 }, async () => {
  let count = 0
  const result = await acquisition({ history: () => {
    count++
    return new Promise(() => {})
  } })
  assert.equal(count, 1)
  assert.equal(result.receipt.historyCompleted, false)
  assert.equal(result.failures[0], result.first)
  assert.equal(result.failures.length, 2)
})

// Source-only fixtures above are not native API evidence. The CI path calls
// these exact helpers through ordinary page.evaluate and the original fixture.

function patchedSource() {
  const source = execFileSync('git', ['-c', `safe.directory=${process.cwd()}`, 'show', 'b1863f5601e483000d96eaab121ce3ec0ae44c7b:tests/e2e/availability-bfcache.spec.ts'])
  const directory = mkdtempSync(join(tmpdir(), 'c40-nrr-source-unit-'))
  const target = join(directory, 'tests/e2e/availability-bfcache.spec.ts')
  mkdirSync(join(directory, 'tests/e2e'), { recursive: true })
  writeFileSync(target, source)
  try {
    const overlay = readFileSync(new URL('./bfcache-native-reasons.overlay.ts', import.meta.url))
    writeFileSync(target, overlay)
    const diagnostic = readFileSync(target, 'utf8')
    assert.ok(diagnostic.includes('.toEqual([{ persisted: true, trusted: true }])'))
    assert.ok(diagnostic.includes('diagnosticFailures.push(...outcome.failures)'))
    assert.equal(diagnostic.includes('await page.goBack()'), false)
    assert.equal(diagnostic.split('page.goBack()').length - 1, 1)
    writeFileSync(target, source)
    assert.deepEqual(readFileSync(target), source)
    return diagnostic
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

test('versioned exact overlay preserves strict checkpoint and restores original bytes', () => {
  patchedSource()
})

test('actual patched cleanup starts HTTP closure despite stalled page and drains navigation boundedly', async () => {
  const source = patchedSource()
  const start = source.indexOf('    poll?.release()')
  const end = source.indexOf('\n  }\n\n  cleanupFailureCount', start)
  assert.ok(start > 0 && end > start)
  const block = stripTypeScriptTypes(source.slice(start, end))
  const finish = new Function('page', 'responseGate', 'navigation', 'setTimeout', 'clearTimeout', `return (async () => {
    const failures = []; const poll = undefined; const destination = undefined; let httpClosed = false;
    ${block}
    return { failures, httpClosed };
  })()`)
  const timers = new Map()
  let next = 0
  let pageCalled = false
  let httpCalled = false
  const done = finish({ close: () => { pageCalled = true; return new Promise(() => {}) } }, { close: async () => { httpCalled = true } }, new Promise(() => {}), callback => { timers.set(++next, callback); return next }, id => timers.delete(id))
  await setImmediate()
  assert.equal(pageCalled, true)
  assert.equal(httpCalled, true)
  for (const callback of [...timers.values()]) callback()
  await setImmediate()
  for (const callback of [...timers.values()]) callback()
  const result = await done
  assert.equal(result.httpClosed, true)
  assert.equal(result.failures.length, 2)
  assert.equal(timers.size, 0)
  const acquisitionResult = await acquisition()
  assert.equal(finalizeNativeReasonDiagnostic(acquisitionResult.receipt, { ...closure, cleanupFailureCount: result.failures.length, pageClosed: false }).observationComplete, false)
})
