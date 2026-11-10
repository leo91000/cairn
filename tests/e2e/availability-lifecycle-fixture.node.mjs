import assert from 'node:assert/strict'
import { once } from 'node:events'
import {
  mkdir,
  mkdtemp,
  rm,
  symlink,
  writeFile,
} from 'node:fs/promises'
import { request } from 'node:http'
import path from 'node:path'
import { test } from 'node:test'
import { setImmediate } from 'node:timers/promises'
import { performancePreference } from '../../apps/web/src/client-performance-preference.ts'
import { performanceUploader } from '../../apps/web/src/client-performance-upload.ts'
import {
  availabilityLifecycleFixture,
  lifecycleAvailability,
  lifecycleStorageState,
  retireLifecycleOwners,
} from './availability-lifecycle-fixture.ts'
import { nativeResponseGate } from './native-response-gate.ts'

async function fixture(run) {
  await mkdir('test-results/source-only-lifecycle', { recursive: true })
  const directory = await mkdtemp(path.resolve('test-results/source-only-lifecycle/build-'))
  await mkdir(path.join(directory, 'assets'))
  await writeFile(path.join(directory, 'beacon.html'), '<div id="app" data-beacon></div><script src="/assets/beacon.js"></script>')
  await writeFile(path.join(directory, 'assets/beacon.js'), '// controlled source-only bytes')
  await writeFile(path.join(directory, 'web-build.json'), JSON.stringify({ buildId: 'a'.repeat(64) }))
  const server = await availabilityLifecycleFixture(directory)
  let failure
  try {
    await run(server, directory)
  }
  catch (cause) {
    failure = cause
  }
  finally {
    try {
      await server.close()
    }
    catch (cause) {
      failure ??= cause
    }

    await rm(directory, { recursive: true, force: true })
  }

  if (failure)
    throw failure
}

test('real fixture serves exact entry/assets and synthetic data without selecting an installation', async () => {
  await fixture(async (server) => {
    const entry = await fetch(`${server.url}/claim`)
    assert.equal(entry.headers.get('cache-control'), 'no-cache')
    assert.equal(entry.headers.has('set-cookie'), false)
    assert.equal(await entry.text(), '<div id="app" data-beacon></div><script src="/assets/beacon.js"></script>')
    assert.equal(await (await fetch(`${server.url}/assets/beacon.js`)).text(), '// controlled source-only bytes')
    const session = await (await fetch(`${server.url}/api/account/session`)).json()
    assert.equal(session.authenticated, true)
    assert.equal(session.installations.length, 1)
    assert.equal(new URL(`${server.url}/claim`).pathname.startsWith('/installations/'), false)
    assert.deepEqual(await (await fetch(`${server.url}/api/account/options`)).json(), { google: false, github: false, passkeys: false })
    assert.deepEqual(await (await fetch(`${server.url}/api/account/invitations`)).json(), [])
    assert.deepEqual(await (await fetch(`${server.url}/api/installations`)).json(), lifecycleAvailability(true))
    server.setAvailability(false)
    assert.deepEqual(await (await fetch(`${server.url}/api/installations`)).json(), lifecycleAvailability(false))
    assert.equal(server.availabilityRequests, 2)
    for (const page of ['inert', 'health'])
      assert.equal((await fetch(`${server.url}/${page}`)).status, 200)
    server.check()
  })
})

for (const [name, target, init] of [
  ['unplanned API/Direct', '/api/installations/lifecycle-installation/direct/authorize', undefined],
  ['mutation', '/api/account/session', { method: 'POST' }],
  ['query', '/claim?unexpected=true', undefined],
  ['missing asset', '/assets/missing.js', undefined],
]) {
  test(`real fixture refuses ${name} and retains the failure at close`, async () => {
    await assert.rejects(fixture(async (server) => {
      await assert.rejects(fetch(server.url + target, init))
      assert.throws(() => server.check(), AggregateError)
    }), AggregateError)
  })
}

test('real fixture refuses assets symlinked outside the immutable build', async () => {
  await assert.rejects(fixture(async (server, directory) => {
    await symlink(path.resolve('package.json'), path.join(directory, 'assets/escape.js'))
    await assert.rejects(fetch(`${server.url}/assets/escape.js`))
    assert.throws(() => server.check(), AggregateError)
  }), AggregateError)
})

test('existing response gate holds real HTTP body and observes client cancellation', async () => {
  await fixture(async (server) => {
    const gate = await nativeResponseGate(server.url)
    try {
      const held = gate.holdBody()
      const proxy = new URL(gate.server)
      const client = request({ hostname: proxy.hostname, port: proxy.port, path: `${server.url}/api/installations` })
      client.on('error', () => {})
      const response = once(client, 'response')
      client.end()
      const [incoming] = await response
      assert.equal(incoming.statusCode, 200)
      assert.equal(held.headersDelivered, true)
      assert.equal(held.bodyReleased, false)
      const closed = once(incoming, 'close')
      incoming.on('error', () => {})
      incoming.destroy()
      await closed
      for (let turn = 0; turn < 30 && !held.closed; turn++)
        await setImmediate()
      assert.equal(held.closed, true)
      assert.equal(held.terminal, 'cancelled')
      assert.equal(held.bodyReleased, false)
      gate.check()
    }
    finally {
      await gate.close()
    }
  })
})

test('real closure controller attempts every owner and preserves original errors', async () => {
  const original = new Error('controlled close failure')
  const called = []
  const failures = await retireLifecycleOwners([
    async () => {
      called.push('page')
      throw original
    },
    async () => {
      called.push('gate')
      return true
    },
    async () => {
      called.push('http')
      return true
    },
  ])
  assert.deepEqual(called, ['page', 'gate', 'http'])
  assert.deepEqual(failures, [original])
})

test('real closure controller refuses unknown closure', async () => {
  const failures = await retireLifecycleOwners([async () => false])
  assert.equal(failures.length, 1)
  assert.match(failures[0].message, /closure is unknown/)
})

test('real closure controller keeps the existing 7s deadline red and other owners independent', async (control) => {
  control.mock.timers.enable({ apis: ['setTimeout'] })
  let independent = false
  const retired = retireLifecycleOwners([
    () => new Promise(() => {}),
    async () => {
      independent = true
      return true
    },
  ])
  await setImmediate()
  assert.equal(independent, true)
  control.mock.timers.tick(7000)
  const failures = await retired
  assert.equal(failures.length, 1)
  assert.match(failures[0].message, /Lifecycle deadline: owner 0/)
})

test('HTTP retirement is idempotent and refuses subsequent data changes', async () => {
  await fixture(async (server) => {
    await Promise.all([server.close(), server.close()])
    assert.throws(() => server.setAvailability(false), /retired/)
    await assert.rejects(fetch(`${server.url}/api/installations`))
  })
})

test('existing persisted user choice prevents unrelated real uploader traffic beyond 10s', async () => {
  async function uploads(storage) {
    const values = storage?.origins[0].localStorage ?? []
    const preference = performancePreference({
      read: () => {
        const value = values.find(item => item.name === 'cairn_performance_upload')?.value
        return value === undefined ? undefined : value === 'true'
      },
      write() {},
    })
    let now = 0
    let sends = 0
    const tasks = new Map()
    const uploader = performanceUploader({
      now: () => now,
      schedule: (callback, milliseconds) => {
        tasks.set(callback, now + milliseconds)
        return () => tasks.delete(callback)
      },
      report: () => ({ measurements: { windowId: 'controlled', sequence: 1 } }),
      enabled: () => preference.refresh(),
      send: async () => {
        sends++
        return true
      },
    })
    uploader.update({
      accountId: 'synthetic',
      csrf: 'synthetic',
      foreground: true,
      enabled: preference.refresh(),
    })
    now = 10000
    for (const [callback, at] of tasks) {
      if (at <= now) {
        tasks.delete(callback)
        await callback()
      }
    }

    uploader.stop()
    assert.equal(tasks.size, 0)
    return sends
  }

  // Controlled old fixture precondition: the real default generates a POST.
  assert.equal(await uploads(undefined), 1)
  // Same real preference/uploader and task cadence, persisted user choice only.
  assert.equal(await uploads(lifecycleStorageState('http://localhost:4322')), 0)
})
