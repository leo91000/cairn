import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import { createContext, runInContext } from 'node:vm'
import { MessageChannel as NativeMessageChannel } from 'node:worker_threads'
import { setImmediate } from 'node:timers/promises'

const reference = process.argv.includes('--reference')
const referenceHead = process.env.C40_ARCHIVE_REFERENCE || '589e704e90bc451a181bd7fdcf667993e518522e'
function fixture(t, name) {
  const raw = reference
    ? execFileSync('git', ['-c', `safe.directory=${process.cwd()}`, 'show', `${referenceHead}:.github/diagnostics/${name}`], { encoding: 'utf8' })
    : readFileSync(new URL('./' + name, import.meta.url), 'utf8')
  const source = stripTypeScriptTypes(raw, { mode: 'transform' })
  const start = source.indexOf('await page.addInitScript(') + 'await page.addInitScript('.length
  const next = source.indexOf(name.startsWith('skill') ? 'await page.clock.install()' : 'await page.setViewportSize', start)
  assert(start > 30 && next > start)
  const bootstrap = source.slice(start, next).trim().replace(/\);$/, '')
  const originals = new Map()
  const bodyCalls = []
  for (const method of ['json', 'text', 'arrayBuffer', 'blob', 'formData']) {
    const original = Response.prototype[method]
    originals.set(method, original)
    Response.prototype[method] = function (...args) {
      const promise = Reflect.apply(original, this, args)
      bodyCalls.push({ method, receiver: this, args, promise })
      return promise
    }
  }
  const channels = []
  t.after(() => {
    for (const [method, original] of originals)
      Response.prototype[method] = original
    for (const channel of channels) {
      channel.port1.close()
      channel.port2.close()
    }
  })
  const queue = []
  const calls = []
  const events = []
  const closeResult = Symbol('actual original native-close stand-in result')
  class NativeSource extends EventTarget {
    static CLOSED = 2
    readyState = 0
    closes = 0
    constructor(url) { super(); this.url = url }
    close() { this.readyState = 2; this.closes++; return closeResult }
  }
  const context = createContext({
    URL, Request, Response, AbortController, AbortSignal, Object, EventSource: NativeSource, crypto,
    Event, MessageEvent, Promise, performance,
    MessageChannel: class {
      constructor() {
        const channel = new NativeMessageChannel()
        channels.push(channel)
        return channel
      }
    },
    fetch: function (...args) {
      calls.push({ receiver: this, args })
      const next = queue.shift()
      assert(next)
      return typeof next === 'function' ? Reflect.apply(next, this, args) : next
    },
    availabilityProbe: (...args) => { events.push(args); return Promise.resolve() },
    b534Record: (...args) => { events.push(args); return Promise.resolve() },
    addEventListener() {},
    location: { href: 'http://localhost:4422/fixture', origin: 'http://localhost:4422' },
  })
  runInContext('window=globalThis;', context)
  runInContext(`(${bootstrap})();`, context)
  const ownWindow = runInContext('window', context)
  const fetch = (...args) => Reflect.apply(context.fetch, ownWindow, args)
  return { context, fetch, queue, calls, bodyCalls, events, closeResult }
}
for (const name of ['skill-availability-probe.ts', 'b534-navigation-probe.ts']) {
  test(`${name}: actual bootstrap preserves original fetch promise, arguments and receiver`, async (t) => {
    const f = fixture(t, name)
    const original = Promise.withResolvers()
    f.queue.push(original.promise)
    const receiver = {}
    const options = { method: 'GET' }
    const result = Reflect.apply(f.context.fetch, receiver, ['/api/installations', options])
    original.resolve(Response.json({ value: 'original' }))
    await result
    assert.equal(result, original.promise)
    assert.equal(f.calls[0].receiver, receiver)
    assert.equal(f.calls[0].args[1], options)
  })
  test(`${name}: actual body observation returns the original native JSON promise`, async (t) => {
    const f = fixture(t, name)
    const response = Response.json({ value: 'original' })
    f.queue.push(Promise.resolve(response))
    const returned = await f.fetch('/api/installations', { method: 'GET' })
    const body = returned.json()
    await body
    assert.equal(body, f.bodyCalls.at(-1).promise)
    assert.equal(f.bodyCalls.at(-1).receiver, returned)
    assert.equal(Object.hasOwn(returned, 'json'), false)
  })
  test(`${name}: borrowing a body method keeps the ACTUAL receiver and original promise`, async (t) => {
    const f = fixture(t, name)
    const a = Response.json({ value: 'A' })
    const b = Response.json({ value: 'B' })
    f.queue.push(Promise.resolve(a), Promise.resolve(b))
    const first = await f.fetch('/api/installations', { method: 'GET' })
    const second = await f.fetch('/api/installations', { method: 'GET' })
    const body = Reflect.apply(first.json, second, [])
    assert.deepEqual(await body, { value: 'B' })
    assert.equal(f.bodyCalls.at(-1).receiver, second)
    assert.equal(body, f.bodyCalls.at(-1).promise)
    assert.equal(first.bodyUsed, false)
  })
}
test('B534 actual bootstrap source.close preserves actual receiver and original return', async (t) => {
  const f = fixture(t, 'b534-navigation-probe.ts')
  const first = new f.context.EventSource('/api/chats/stream')
  const second = new f.context.EventSource('/api/chats/stream')
  const returned = Reflect.apply(first.close, second, [])
  assert.equal(returned, f.closeResult)
  assert.equal(first.closes, 0)
  assert.equal(second.closes, 1)
  await setImmediate()
})


for (const name of ['skill-availability-probe.ts', 'b534-navigation-probe.ts']) {
  test(`H76 ${name}: malformed URL preserves the ORIGINAL rejected promise and cause`, async (t) => {
    const f = fixture(t, name)
    const cause = new TypeError('same native malformed URL rejection')
    const original = Promise.reject(cause)
    // Attach immediately so a synchronous observer throw cannot create an
    // unrelated unhandled-rejection error in the reference counterexample.
    void original.catch(() => {})
    f.queue.push(original)
    const result = f.fetch('http://[')
    assert.equal(result, original)
    await assert.rejects(result, value => value === cause)
    assert.equal(f.calls.length, 1)
  })
  test(`H76 ${name}: coercible inputs are consumed ONLY by original fetch`, async (t) => {
    const f = fixture(t, name)
    let reads = 0
    const input = { get url() { reads++; return '/api/installations' } }
    const original = Promise.resolve(Response.json({ value: 'original' }))
    f.queue.push((value) => { void value.url; return original })
    const result = f.fetch(input)
    assert.equal(result, original)
    await result
    assert.equal(reads, 1)
  })
  test(`H76 ${name}: accessor options are read ONLY by original fetch`, async (t) => {
    const f = fixture(t, name)
    let methodReads = 0
    let signalReads = 0
    const controller = new AbortController()
    const options = { get method() { methodReads++; return 'GET' }, get signal() { signalReads++; return controller.signal } }
    const original = Promise.resolve(Response.json({ value: 'original' }))
    f.queue.push((_, init) => { void init.method; void init.signal; return original })
    const result = f.fetch('/api/installations', options)
    assert.equal(result, original)
    await result
    assert.equal(methodReads, 1)
    assert.equal(signalReads, 1)
  })
}
