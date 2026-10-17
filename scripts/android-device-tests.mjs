import { Buffer } from 'node:buffer'
import { execFile, spawn } from 'node:child_process'
import {
  mkdir,
  readdir,
  readFile,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'
import { promisify } from 'node:util'
import { waitForDeviceReady } from './android-device-ready.mjs'

const exec = promisify(execFile)

export const appPackage = 'build.cairn.app'
export const runner = `${appPackage}.test/androidx.test.runner.AndroidJUnitRunner`

// AndroidJUnitRunner status codes (InstrumentationResultPrinter).
const started = 1
const outcomes = {
  0: 'passed',
  [-1]: 'error',
  [-2]: 'failure',
  [-3]: 'skipped',
  [-4]: 'skipped',
}
const pngSignature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])

/**
 * Reads `am instrument -r` output. Each line is `{ text, time }`, so the host
 * clock times each test. Values such as stack traces continue on the lines
 * that do not start with an INSTRUMENTATION_ prefix.
 */
export function parseInstrumentation(lines) {
  const tests = []
  const running = new Map()
  let bundle = {}
  let key = null
  const result = {}
  let resultKey = null
  let code = null
  let failure = null
  let expected = null

  const assign = (target, line, prefix) => {
    const entry = line.slice(prefix.length)
    const separator = entry.indexOf('=')
    const name = separator < 0 ? entry : entry.slice(0, separator)
    target[name] = separator < 0 ? '' : entry.slice(separator + 1)
    return name
  }

  for (const { text, time } of lines) {
    if (text.startsWith('INSTRUMENTATION_STATUS: ')) {
      key = assign(bundle, text, 'INSTRUMENTATION_STATUS: ')
      resultKey = null
    }
    else if (text.startsWith('INSTRUMENTATION_STATUS_CODE: ')) {
      const status = Number(text.slice('INSTRUMENTATION_STATUS_CODE: '.length))
      const id = `${bundle.class}#${bundle.test}`
      if (bundle.numtests)
        expected = Number(bundle.numtests)
      if (status === started) {
        running.set(id, time)
      }
      else if (bundle.test && Object.hasOwn(outcomes, status)) {
        const startedAt = running.get(id) ?? time
        running.delete(id)
        tests.push({
          className: bundle.class,
          name: bundle.test,
          status: outcomes[status],
          seconds: Math.max(0, (time - startedAt) / 1000),
          stack: bundle.stack?.trim() || null,
        })
      }

      bundle = {}
      key = null
    }
    else if (text.startsWith('INSTRUMENTATION_RESULT: ')) {
      resultKey = assign(result, text, 'INSTRUMENTATION_RESULT: ')
      key = null
    }
    else if (text.startsWith('INSTRUMENTATION_CODE: ')) {
      code = Number(text.slice('INSTRUMENTATION_CODE: '.length))
      key = null
      resultKey = null
    }
    else if (text.startsWith('INSTRUMENTATION_FAILED: ')) {
      failure = text.slice('INSTRUMENTATION_FAILED: '.length)
    }
    else if (key) {
      bundle[key] += `\n${text}`
    }
    else if (resultKey) {
      result[resultKey] += `\n${text}`
    }
  }

  const crash = result.shortMsg || failure || (code === null ? 'Instrumentation output ended early' : null)
  for (const [id, startedAt] of running) {
    const [className, name] = id.split('#')
    const last = lines.at(-1)?.time ?? startedAt
    tests.push({
      className,
      name,
      status: 'error',
      seconds: (last - startedAt) / 1000,
      stack: crash || 'The test did not finish',
    })
  }

  const failed = tests.filter(test => test.status === 'failure' || test.status === 'error')
  const problems = [
    ...(crash ? [crash] : []),
    ...(code !== null && code !== -1 ? [`Instrumentation exited with code ${code}`] : []),
    ...(expected !== null && tests.length < expected ? [`${expected - tests.length} of ${expected} tests did not report`] : []),
    ...failed.map(test => `${test.className}#${test.name} ${test.status === 'error' ? 'errored' : 'failed'}`),
  ]
  return { tests, ok: problems.length === 0, problems }
}

/** Splits tests into shards of similar duration; unknown tests weigh the median. */
export function planShards(tests, count, timings = {}) {
  const known = Object.values(timings).sort((a, b) => a - b)
  const fallback = known.length ? known[Math.floor(known.length / 2)] : 1
  const weighted = tests
    .map(id => ({ id, seconds: timings[id] ?? fallback }))
    .sort((a, b) => b.seconds - a.seconds || a.id.localeCompare(b.id))
  const shards = Array.from({ length: count }, () => ({ seconds: 0, tests: [] }))
  for (const test of weighted) {
    const lightest = shards.reduce((best, shard) => shard.seconds < best.seconds ? shard : best)
    lightest.seconds += test.seconds
    lightest.tests.push(test.id)
  }

  return shards.map(shard => shard.tests.sort())
}

function escapeXml(value) {
  return String(value).replace(/[<>&"]/g, character => ({
    '<': '&lt;',
    '>': '&gt;',
    '&': '&amp;',
    '"': '&quot;',
  })[character])
}

export function junitXml(tests, name) {
  const total = tests.reduce((sum, test) => sum + test.seconds, 0)
  const count = status => tests.filter(test => test.status === status).length
  const cases = tests.map((test) => {
    const attributes = `classname="${escapeXml(test.className)}" name="${escapeXml(test.name)}" time="${test.seconds.toFixed(3)}"`
    if (test.status === 'passed')
      return `  <testcase ${attributes}/>`
    if (test.status === 'skipped')
      return `  <testcase ${attributes}><skipped/></testcase>`
    const message = escapeXml((test.stack || test.status).split('\n')[0])
    return `  <testcase ${attributes}><${test.status} message="${message}">${escapeXml(test.stack || '')}</${test.status}></testcase>`
  })
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    `<testsuite name="${escapeXml(name)}" tests="${tests.length}" failures="${count('failure')}" errors="${count('error')}" skipped="${count('skipped')}" time="${total.toFixed(3)}">`,
    ...cases,
    '</testsuite>',
    '',
  ].join('\n')
}

function adb(args, options = {}) {
  const binary = process.env.ANDROID_HOME ? path.join(process.env.ANDROID_HOME, 'platform-tools/adb') : 'adb'
  return exec(binary, args, { maxBuffer: 64 * 1024 * 1024, encoding: options.encoding ?? 'utf8', timeout: options.timeout ?? 120000 })
}

export function instrument(args, onLine) {
  const binary = process.env.ANDROID_HOME ? path.join(process.env.ANDROID_HOME, 'platform-tools/adb') : 'adb'
  const child = spawn(binary, ['shell', 'am', 'instrument', '-r', '-w', ...args, runner], { stdio: ['ignore', 'pipe', 'inherit'] })
  const lines = []
  const reader = createInterface({ input: child.stdout })
  reader.on('line', (text) => {
    const line = { text: text.replace(/\r$/, ''), time: Date.now() }
    lines.push(line)
    onLine?.(line)
  })
  return new Promise((resolve, reject) => {
    child.once('error', reject)
    child.once('close', () => resolve(lines))
  })
}

function filterArguments({ notClass = [], notAnnotation = [] }) {
  return [
    ...(notClass.length ? ['-e', 'notClass', notClass.join(',')] : []),
    ...(notAnnotation.length ? ['-e', 'notAnnotation', notAnnotation.join(',')] : []),
  ]
}

/** Lists the selected tests without running them (`-e log true`). */
async function listTests(filters) {
  const { tests, ok, problems } = parseInstrumentation(await instrument(['-e', 'log', 'true', ...filterArguments(filters)]))
  if (!ok)
    throw new Error(`Could not list device tests: ${problems.join('; ')}`)
  return tests.map(test => `${test.className}#${test.name}`)
}

async function pullCaptures(output) {
  const { stdout } = await adb(['shell', 'run-as', appPackage, 'find', 'files', '-name', '\'*.png\''])
  const files = stdout.split('\n').map(line => line.trim()).filter(line => line.endsWith('.png'))
  for (const file of files) {
    const target = path.join(output, 'captures', path.relative('files', file))
    await mkdir(path.dirname(target), { recursive: true })
    const { stdout: bytes } = await adb(['exec-out', 'run-as', appPackage, 'cat', file], { encoding: 'buffer' })
    await writeFile(target, bytes)
  }

  return files.length
}

export function parseArguments(argv) {
  const options = { notClass: [], notAnnotation: [], require: [] }
  for (let index = 0; index < argv.length; index++) {
    const [name, value] = [argv[index], argv[index + 1]]
    if (name === '--shard') {
      const match = /^(\d+)\/(\d+)$/.exec(value || '')
      if (!match || Number(match[1]) < 1 || Number(match[1]) > Number(match[2]))
        throw new Error('Use --shard <index>/<count>, starting at 1')
      options.shard = { index: Number(match[1]), count: Number(match[2]) }
    }
    else if (['--apk', '--test-apk', '--output', '--timings', '--write-timings', '--job'].includes(name)) {
      options[name.slice(2).replace(/-(\w)/g, (_, letter) => letter.toUpperCase())] = value
    }
    else if (name === '--not-class') {
      options.notClass.push(...value.split(','))
    }
    else if (name === '--not-annotation') {
      options.notAnnotation.push(...value.split(','))
    }
    else if (name === '--require') {
      options.require.push(...value.split(','))
    }
    else {
      if (name.startsWith('--'))
        throw new Error(`Unknown option ${name}`)
      options.positional = [...options.positional || [], name]
      continue
    }

    index++
  }

  return options
}

/**
 * Device jobs boot their emulator while the build job compiles, then wait here
 * for its artifact. A build that ends without uploading it stops the wait.
 */
export async function waitForArtifact({
  repository,
  runId,
  attempt,
  artifact,
  job,
}, {
  gh,
  sleep = delay => new Promise(resolve => setTimeout(resolve, delay)),
  now = Date.now,
  timeoutMs = 20 * 60 * 1000,
}) {
  const deadline = now() + timeoutMs
  while (now() < deadline) {
    const artifacts = JSON.parse(await gh(['api', `repos/${repository}/actions/runs/${runId}/artifacts?name=${artifact}`]))
    if (artifacts.artifacts?.some(entry => entry.name === artifact && !entry.expired))
      return

    const { jobs } = JSON.parse(await gh(['api', `repos/${repository}/actions/runs/${runId}/attempts/${attempt}/jobs?per_page=100`]))
    const producer = jobs.find(entry => entry.name === job)
    if (producer?.status === 'completed') {
      // The upload is the build job's last step; re-read once to close the race.
      const latest = JSON.parse(await gh(['api', `repos/${repository}/actions/runs/${runId}/artifacts?name=${artifact}`]))
      if (latest.artifacts?.some(entry => entry.name === artifact && !entry.expired))
        return
      throw new Error(`The ${job} job finished (${producer.conclusion}) without uploading ${artifact}`)
    }

    await sleep(5000)
  }

  throw new Error(`Timed out waiting for the ${artifact} artifact`)
}

async function fetchArtifact(options) {
  const artifact = options.positional?.[0]
  if (!artifact || !options.output)
    throw new Error('fetch needs the artifact name and --output')
  const gh = async args => (await exec('gh', args, { maxBuffer: 16 * 1024 * 1024, timeout: 300000 })).stdout
  const started = Date.now()
  const run = {
    repository: process.env.GITHUB_REPOSITORY,
    runId: process.env.GITHUB_RUN_ID,
    attempt: process.env.GITHUB_RUN_ATTEMPT || '1',
    artifact,
    job: options.job || 'build',
  }
  await waitForArtifact(run, { gh })
  await gh(['run', 'download', run.runId, '--repo', run.repository, '--name', artifact, '--dir', options.output])
  console.log(`Fetched ${artifact} after ${((Date.now() - started) / 1000).toFixed(0)}s`)
}

async function run(options) {
  if (!options.apk || !options.testApk || !options.output)
    throw new Error('run needs --apk, --test-apk and --output')
  const shard = options.shard || { index: 1, count: 1 }
  await mkdir(options.output, { recursive: true })

  // -g grants runtime permissions as Gradle's connected tests do.
  const installStarted = Date.now()
  await adb(['install', '-r', '-t', '-g', options.apk], { timeout: 300000 })
  await adb(['install', '-r', '-t', '-g', options.testApk], { timeout: 300000 })
  console.log(`Installed both APKs in ${((Date.now() - installStarted) / 1000).toFixed(1)}s`)

  await waitForDeviceReady({ adb, output: path.join(options.output, 'readiness.json') })

  const all = await listTests(options)
  const timings = options.timings ? JSON.parse(await readFile(options.timings, 'utf8')) : {}
  const selected = planShards(all, shard.count, timings)[shard.index - 1]
  console.log(`Shard ${shard.index}/${shard.count}: ${selected.length} of ${all.length} tests`)
  await writeFile(path.join(options.output, 'selection.txt'), `${selected.join('\n')}\n`)
  if (!selected.length)
    throw new Error('This shard selected no tests')

  const lines = await instrument(['-e', 'class', selected.join(',')], ({ text }) => {
    if (text.startsWith('INSTRUMENTATION_STATUS_CODE: ') || text.startsWith('INSTRUMENTATION_RESULT: ') || text.startsWith('INSTRUMENTATION_CODE: '))
      return
    if (/^(?:Time: |OK \(|FAILURES!!!|Tests run:)/.test(text))
      console.log(text)
  })
  const report = parseInstrumentation(lines)
  const name = `device-shard-${shard.index}-of-${shard.count}`
  await writeFile(path.join(options.output, 'instrumentation.txt'), `${lines.map(line => line.text).join('\n')}\n`)
  await writeFile(path.join(options.output, `TEST-${name}.xml`), junitXml(report.tests, name))
  await writeFile(path.join(options.output, 'results.json'), `${JSON.stringify({ shard, ...report }, null, 2)}\n`)
  for (const test of report.tests)
    console.log(`${test.status.padEnd(7)} ${test.seconds.toFixed(1).padStart(6)}s  ${test.className.split('.').at(-1)}#${test.name}`)

  const missing = selected.filter(id => !report.tests.some(test => `${test.className}#${test.name}` === id))
  if (missing.length)
    report.problems.push(`Not run: ${missing.join(', ')}`)

  console.log(`Pulled ${await pullCaptures(options.output).catch(() => 0)} captures`)
  if (report.problems.length) {
    for (const problem of report.problems)
      console.error(`::error::${problem}`)
    process.exitCode = 1
  }
}

async function results(directory) {
  const found = []
  for (const entry of await readdir(directory, { withFileTypes: true, recursive: true })) {
    if (entry.isFile() && entry.name === 'results.json')
      found.push(JSON.parse(await readFile(path.join(entry.parentPath, entry.name), 'utf8')))
  }

  return found
}

async function isPng(file) {
  return (await readFile(file).catch(() => Buffer.alloc(0))).subarray(0, 8).equals(pngSignature)
}

/** Combines shard results, checks required captures and summarises timings. */
async function report(options) {
  const directory = options.positional?.[0]
  if (!directory)
    throw new Error('report needs the directory holding the shard outputs')
  const shards = await results(directory)
  const tests = shards.flatMap(shard => shard.tests)
  const problems = shards.flatMap(shard => shard.problems)
  const expectedShards = Math.max(0, ...shards.map(shard => shard.shard.count))
  const reported = new Set(shards.map(shard => shard.shard.index)).size
  if (reported < expectedShards)
    problems.push(`Only ${reported} of ${expectedShards} shards reported results`)

  const captures = []
  for (const entry of await readdir(directory, { withFileTypes: true, recursive: true })) {
    if (entry.isFile() && entry.parentPath.split(path.sep).includes('captures'))
      captures.push(path.join(entry.parentPath, entry.name))
  }

  for (const required of options.require) {
    const matches = captures.filter(file => file.endsWith(`${path.sep}captures${path.sep}${required}`))
    if (!matches.length)
      problems.push(`Missing capture ${required}`)
    for (const file of matches) {
      if (!await isPng(file))
        problems.push(`Invalid PNG capture ${required}`)
    }
  }

  const seconds = test => test.seconds
  const lines = [
    `### Android device tests`,
    '',
    `${tests.length} tests in ${shards.length} shard(s), ${tests.filter(test => test.status === 'passed').length} passed, ${problems.length} problem(s).`,
    '',
    '| Shard | Tests | Test time |',
    '| --- | ---: | ---: |',
    ...shards
      .sort((a, b) => a.shard.index - b.shard.index)
      .map(shard => `| ${shard.shard.index}/${shard.shard.count} | ${shard.tests.length} | ${shard.tests.map(seconds).reduce((a, b) => a + b, 0).toFixed(0)} s |`),
    '',
    '| Slowest tests | Time |',
    '| --- | ---: |',
    ...tests.toSorted((a, b) => b.seconds - a.seconds).slice(0, 10).map(test => `| ${test.className.split('.').at(-1)}#${test.name} | ${test.seconds.toFixed(1)} s |`),
    ...problems.length ? ['', '**Problems**', '', ...problems.map(problem => `- ${problem}`)] : [],
    '',
  ]
  console.log(lines.join('\n'))
  if (process.env.GITHUB_STEP_SUMMARY)
    await writeFile(process.env.GITHUB_STEP_SUMMARY, lines.join('\n'), { flag: 'a' })

  if (options.writeTimings) {
    const timings = Object.fromEntries(tests
      .filter(test => test.status === 'passed')
      .map(test => [`${test.className}#${test.name}`, Math.round(test.seconds * 10) / 10])
      .sort(([a], [b]) => a.localeCompare(b)))
    await writeFile(options.writeTimings, `${JSON.stringify(timings, null, 2)}\n`)
  }

  if (!shards.length)
    problems.push('No shard results found')
  if (problems.length)
    process.exitCode = 1
}

if (import.meta.main) {
  const [command, ...rest] = process.argv.slice(2)
  const options = parseArguments(rest)
  const commands = { fetch: fetchArtifact, run, report }
  if (!commands[command]) {
    console.error('Usage: android-device-tests.mjs fetch <artifact> --output <dir> [--job build]\n       android-device-tests.mjs run --apk <apk> --test-apk <apk> --output <dir> [--shard i/n] [--timings file] [--not-class a,b] [--not-annotation a]\n       android-device-tests.mjs report <dir> [--require path.png,...] [--write-timings file]')
    process.exitCode = 2
  }
  else {
    commands[command](options).catch((error) => {
      console.error(`::error::${error.message}`)
      process.exitCode = 1
    })
  }
}
