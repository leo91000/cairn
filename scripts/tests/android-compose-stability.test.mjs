import { describe, expect, it } from 'vitest'
import { measureComposeStability, stabilityTests } from '../android-compose-stability.mjs'

function result({ failed = false, skipped = false, missing = false } = {}) {
  const tests = missing ? stabilityTests.slice(0, 2) : stabilityTests
  const text = tests.flatMap((test, index) => {
    const [className, name] = test.split('#')
    const status = `INSTRUMENTATION_STATUS: class=${className}\nINSTRUMENTATION_STATUS: test=${name}\nINSTRUMENTATION_STATUS: numtests=${tests.length}\n`
    return [`${status}INSTRUMENTATION_STATUS_CODE: 1`, `${status}INSTRUMENTATION_STATUS_CODE: ${index === 0 && failed ? -2 : index === 0 && skipped ? -3 : 0}`]
  }).join('\n')
  return `${text}\nINSTRUMENTATION_CODE: -1`.split('\n').map((text, index) => ({ text, time: index * 100 }))
}

function fixture(results) {
  let calls = 0
  const files = new Map()
  return {
    files,
    calls: () => calls,
    options: {
      run: async () => results[calls++],
      save: async (path, text) => files.set(path, text),
      prepare: async () => {},
      log: () => {},
    },
  }
}

describe('compose stability measurement', () => {
  it('records every requested trial and keeps their timings and JUnit evidence', async () => {
    const f = fixture([result(), result()])
    const trials = await measureComposeStability(2, 'proof', f.options)
    expect(f.calls()).toBe(2)
    expect(trials.every(trial => trial.tests.length === 3 && trial.ok)).toBe(true)
    expect(JSON.parse(f.files.get('proof/stability.json'))).toMatchObject({ requested: 2, completed: 2, ok: true })
    expect(f.files.get('proof/TEST-compose-trial-2.xml')).toContain('tests="3"')
  })

  it.each([{ failed: true }, { skipped: true }, { missing: true }])('stops at the first incomplete or failed trial without retrying: %j', async (condition) => {
    const f = fixture([result(condition), result()])
    await expect(measureComposeStability(2, 'proof', f.options)).rejects.toThrow('No retry')
    expect(f.calls()).toBe(1)
    expect(JSON.parse(f.files.get('proof/stability.json'))).toMatchObject({ requested: 2, completed: 1, ok: false })
  })

  it.each(['0', '-1', '21', '1.5', '2; echo injected'])('rejects an invalid trial count: %s', async (count) => {
    const f = fixture([])
    await expect(measureComposeStability(count, 'proof', f.options)).rejects.toThrow('Usage')
    expect(f.calls()).toBe(0)
  })
})
