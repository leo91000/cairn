import { describe, expect, it } from 'vitest'
import {
  junitXml,
  parseArguments,
  parseInstrumentation,
  planShards,
} from '../android-device-tests.mjs'

function output(text, step = 1000) {
  return text.trim().split('\n').map((line, index) => ({ text: line, time: index * step }))
}

function status(test, code, extra = '') {
  return `
INSTRUMENTATION_STATUS: class=build.cairn.app.ui.FooTest
INSTRUMENTATION_STATUS: current=1
INSTRUMENTATION_STATUS: id=AndroidJUnitRunner
INSTRUMENTATION_STATUS: numtests=2${extra}
INSTRUMENTATION_STATUS: stream=
INSTRUMENTATION_STATUS: test=${test}
INSTRUMENTATION_STATUS_CODE: ${code}`
}

describe('instrumentation output', () => {
  it('times passing tests and accepts a successful run', () => {
    const report = parseInstrumentation(output(`
${status('first', 1)}
${status('first', 0)}
${status('second', 1)}
${status('second', 0)}
INSTRUMENTATION_RESULT: stream=

Time: 4.2

OK (2 tests)


INSTRUMENTATION_CODE: -1`))
    expect(report.ok).toBe(true)
    expect(report.tests.map(test => [test.name, test.status, test.seconds])).toEqual([['first', 'passed', 8], ['second', 'passed', 8]])
  })

  it('keeps multi-line stack traces of failed tests', () => {
    const report = parseInstrumentation(output(`
${status('first', 1)}
${status('first', -2, `
INSTRUMENTATION_STATUS: stack=java.lang.AssertionError: expected the header
${'\t'}at build.cairn.app.ui.FooTest.first(FooTest.kt:12)`)}
${status('second', 1)}
${status('second', -3)}
INSTRUMENTATION_RESULT: stream=
FAILURES!!!
INSTRUMENTATION_CODE: -1`))
    expect(report.ok).toBe(false)
    expect(report.tests[0]).toMatchObject({ status: 'failure', stack: 'java.lang.AssertionError: expected the header\n\tat build.cairn.app.ui.FooTest.first(FooTest.kt:12)' })
    expect(report.tests[1].status).toBe('skipped')
    expect(report.problems).toEqual(['build.cairn.app.ui.FooTest#first failed'])
  })

  it('reports the test running when the app process crashes', () => {
    const report = parseInstrumentation(output(`
${status('first', 1)}
INSTRUMENTATION_RESULT: shortMsg=Process crashed.
INSTRUMENTATION_CODE: 0`))
    expect(report.ok).toBe(false)
    expect(report.tests).toEqual([expect.objectContaining({ name: 'first', status: 'error', stack: 'Process crashed.' })])
    expect(report.problems).toContain('Instrumentation exited with code 0')
  })

  it('fails output that stops early or skips announced tests', () => {
    expect(parseInstrumentation(output(status('first', 1))).problems).toContain('Instrumentation output ended early')
    const report = parseInstrumentation(output(`
${status('first', 1)}
${status('first', 0)}
INSTRUMENTATION_CODE: -1`))
    expect(report.problems).toEqual(['1 of 2 tests did not report'])
  })

  it('fails when the runner cannot start', () => {
    const report = parseInstrumentation(output(`
INSTRUMENTATION_FAILED: build.cairn.app.test/androidx.test.runner.AndroidJUnitRunner
INSTRUMENTATION_CODE: 1`))
    expect(report.ok).toBe(false)
  })
})

describe('shard planning', () => {
  const tests = ['A#slow', 'A#fast', 'B#medium', 'B#new', 'C#tiny']
  const timings = {
    'A#slow': 40,
    'A#fast': 2,
    'B#medium': 20,
    'C#tiny': 1,
  }

  it('runs every test exactly once, whatever the shard count', () => {
    for (const count of [1, 2, 3, 7]) {
      const shards = planShards(tests, count, timings)
      expect(shards).toHaveLength(count)
      expect(shards.flat().sort()).toEqual([...tests].sort())
    }
  })

  it('balances by recorded duration and weighs unknown tests as the median', () => {
    expect(planShards(tests, 2, timings)).toEqual([['A#fast', 'A#slow'], ['B#medium', 'B#new', 'C#tiny']])
  })

  it('gives every shard the same plan for the same input order or timings', () => {
    expect(planShards([...tests].reverse(), 3, timings)).toEqual(planShards(tests, 3, timings))
    expect(planShards(tests, 3)).toEqual(planShards([...tests].reverse(), 3))
  })
})

describe('reports and options', () => {
  it('writes escaped JUnit XML that CI tools can read', () => {
    const xml = junitXml([
      {
        className: 'A',
        name: 'ok',
        status: 'passed',
        seconds: 1.25,
      },
      {
        className: 'A',
        name: 'broken',
        status: 'failure',
        seconds: 2,
        stack: 'Expected <a> & "b"\n\tat A',
      },
    ], 'shard')
    expect(xml).toContain('<testsuite name="shard" tests="2" failures="1" errors="0" skipped="0" time="3.250">')
    expect(xml).toContain('<failure message="Expected &lt;a&gt; &amp; &quot;b&quot;">Expected &lt;a&gt; &amp; &quot;b&quot;\n\tat A</failure>')
  })

  it('parses shard, filter and capture options', () => {
    expect(parseArguments(['--shard', '2/3', '--not-class', 'A,B', '--require', 'x/a.png', '--test-apk', 't.apk', 'dir'])).toEqual({
      shard: { index: 2, count: 3 },
      notClass: ['A', 'B'],
      notAnnotation: [],
      require: ['x/a.png'],
      testApk: 't.apk',
      positional: ['dir'],
    })
    for (const shard of ['0/3', '4/3', 'two'])
      expect(() => parseArguments(['--shard', shard])).toThrow('--shard')
    expect(() => parseArguments(['--shards', '2'])).toThrow('Unknown option')
  })
})
