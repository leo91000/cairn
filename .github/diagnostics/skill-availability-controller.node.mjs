import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { setImmediate } from 'node:timers/promises'
import test from 'node:test'

const source = readFileSync(new URL('./skill-availability-probe.ts', import.meta.url), 'utf8')
const declarations = source.slice(source.indexOf('export async function skillProbeBound'), source.indexOf("for (const boundary of ['pending-poll'"))
assert.ok(declarations.includes('export async function commitSkillReload'))
assert.ok(declarations.includes('export async function closeAndAuditSkillProbe'))
const factory = new Function('setTimeout', 'clearTimeout', `${stripTypeScriptTypes(declarations).replace(/^export /gm, '')}\nreturn { commitSkillReload, closeAndAuditSkillProbe }`)
const actual = factory(setTimeout, clearTimeout)

function fixture(change = {}) {
  const terminal = { kind: 'poll-rejected', document: 'old-document', poll: 3 }
  const state = {
    document: 'old-document',
    pending: 0,
    terminal,
    held: true,
    advanced: 0,
    drains: 0,
  }
  const control = {
    document: 'old-document',
    read: 3,
    held: () => state.held,
    snapshot: async () => ({ document: state.document, pending: state.pending }),
    terminals: async () => [state.terminal],
    drain: async () => {
      state.drains++
    },
    advance: async () => {
      state.advanced++
    },
    ...change,
  }
  return { state, control }
}

if (process.argv.includes('--baseline')) {
  const before = execFileSync('git', ['show', 'bc1b48316f033460e5c5ab40e128583260f8c386:.github/diagnostics/skill-availability-probe.ts'], { encoding: 'utf8' })
  const start = before.indexOf('      // Both stimuli must reach the old document')
  const stop = before.indexOf("      await expect(page.getByRole('heading'", start)
  assert.ok(start > 0 && stop > start)
  const commit = new Function('expect', 'pollHeld', 'releaseReload', 'reload', 'releasePoll', `return (async () => {${before.slice(start, stop)}})()`)
  test('baseline must not release reload with missing old terminal (expected RED)', async () => {
    const f = fixture({ terminals: async () => [] })
    await commit(value => ({ toBe: expected => assert.equal(value, expected) }), true, { resolve: () => f.state.advanced++ }, Promise.resolve(), { resolve() {} })
    assert.equal(f.state.advanced, 0)
  })
  const auditStart = before.indexOf('        await reload?.catch')
  const auditStop = before.indexOf('        cleanupCompleted = true', auditStart)
  assert.ok(auditStart > 0 && auditStop > auditStart)
  const cleanup = new Function('reload', 'page', 'expect', 'requests', 'statuses', 'targetPoll', 'record', 'deliveryFailure', 'browserSettled', `return (async () => {${before.slice(auditStart, auditStop)}})()`)
  test('baseline must reject HTTP503 arriving during JavaScript drain (expected RED)', async () => {
    const initial = {}
    const late = {}
    const requests = new Map([[initial, 'finished']])
    const statuses = new Map([[initial, 200]])
    const expect = { poll: read => ({ toBe: async value => assert.equal(await read(), value) }) }
    await assert.rejects(cleanup(Promise.resolve(), { unrouteAll: async () => {} }, expect, requests, statuses, undefined, () => {}, undefined, async () => {
      requests.set(late, 'finished')
      statuses.set(late, 503)
    }))
  })
}
else {
  test('real old-document receipt and JavaScript closure permit commit exactly once', async () => {
    const f = fixture()
    await actual.commitSkillReload(f.control)
    assert.equal(f.state.drains, 1)
    assert.equal(f.state.advanced, 1)
    assert.equal(f.state.held, true)
  })
  const negative = {
    missing: (f) => {
      f.control.terminals = async () => []
    },
    duplicate: (f) => {
      f.control.terminals = async () => [f.state.terminal, f.state.terminal]
    },
    'wrong-document': (f) => {
      f.state.document = 'replacement'
    },
    'wrong-read': (f) => {
      f.state.terminal.poll = 4
    },
    'headers-only': (f) => {
      f.state.terminal.kind = 'poll-headers'
    },
    'released-body': (f) => {
      f.state.held = false
    },
    'pending-JS': (f) => {
      f.state.pending = 1
    },
    'replacement-during-drain': (f) => {
      f.control.drain = async () => {
        f.state.document = 'replacement'
      }
    },
    'late-duplicate': (f) => {
      f.control.drain = async () => {
        f.control.terminals = async () => [f.state.terminal, f.state.terminal]
      }
    },
  }
  for (const [name, change] of Object.entries(negative)) {
    test(`real commit guard refuses ${name} before release`, async () => {
      const f = fixture()
      change(f)
      await assert.rejects(actual.commitSkillReload(f.control))
      assert.equal(f.state.advanced, 0)
    })
  }

  for (const phase of ['drain', 'close']) {
    test(`real final audit rejects HTTP503 introduced during ${phase}`, async () => {
      const events = []
      const statuses = [200]
      const insert = () => {
        statuses.push(503)
      }
      await assert.rejects(actual.closeAndAuditSkillProbe({
        drain: async () => {
          events.push('drain')
          if (phase === 'drain')
            insert()
        },
        close: [async () => {
          events.push('close')
          if (phase === 'close')
            insert()
        }],
        finished: async () => {
          events.push('finished')
        },
        audit: () => {
          events.push('audit')
          assert.deepEqual(statuses, [200])
        },
      }))
      assert.deepEqual(events, ['drain', 'close', 'finished', 'audit'])
    })
  }
  test('real final audit passes HTTP200 only after closure', async () => {
    const events = []
    await actual.closeAndAuditSkillProbe({
      drain: async () => {
        events.push('drain')
      },
      close: [async () => {
        events.push('close')
      }],
      finished: async () => {
        events.push('finished')
      },
      audit: () => {
        events.push('audit')
      },
    })
    assert.deepEqual(events, ['drain', 'close', 'finished', 'audit'])
  })
  test('an unknown owner closes independently of its peer and remains RED', async () => {
    const timers = new Map()
    let id = 0
    const controller = factory((callback) => {
      timers.set(++id, callback)
      return id
    }, key => timers.delete(key))
    let peerClosed = false
    let audited = false
    const done = controller.closeAndAuditSkillProbe({
      drain: async () => {},
      close: [() => new Promise(() => {}), async () => {
        peerClosed = true
      }],
      finished: async () => {},
      audit: () => {
        audited = true
      },
    })
    const rejected = assert.rejects(done, error => error instanceof AggregateError && error.errors.some(cause => cause.message.includes('deadline')))
    await setImmediate()
    assert.equal(peerClosed, true)
    assert.equal(timers.size, 1)
    for (const callback of [...timers.values()])
      callback()
    await rejected
    assert.equal(audited, true)
    assert.equal(timers.size, 0)
  })
  test('unknown final finite HTTP closure remains RED and still audits', async () => {
    const failure = new Error('finite closure unknown')
    let audited = false
    await assert.rejects(actual.closeAndAuditSkillProbe({
      drain: async () => {},
      close: [async () => {}],
      finished: async () => {
        throw failure
      },
      audit: () => {
        audited = true
      },
    }), error => error instanceof AggregateError && error.errors.includes(failure))
    assert.equal(audited, true)
  })
}
