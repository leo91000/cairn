import { execFileSync, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import {
  cpus,
  loadavg,
  platform,
  release,
  totalmem,
} from 'node:os'
import { dirname, resolve } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

// Opt-in synthetic diagnostics. Runs on the JVM; never contacts a real installation.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const android = resolve(root, 'apps/android')
const classes = [
  'build.cairn.app.data.HistoryCacheCostDiagnosisTest',
  'build.cairn.app.ui.CacheRefreshTest',
]
const args = [':app:testDebugUnitTest', '--rerun']
for (const name of classes)
  args.push('--tests', name)
args.push('--no-daemon')

const startedAt = Date.now()
const result = spawnSync('./gradlew', args, {
  cwd: android,
  env: { ...process.env, CAIRN_MEASURE_ANDROID_CACHE: '1' },
  stdio: 'inherit',
})
if (result.error)
  throw result.error
if (result.signal)
  throw new Error(`Gradle stopped by ${result.signal}; no measurement recorded`)

const reports = []
for (const name of classes) {
  const report = resolve(android, `app/build/test-results/testDebugUnitTest/TEST-${name}.xml`)
  if (statSync(report).mtimeMs < startedAt)
    throw new Error(`Gradle did not produce a fresh report for ${name}; refusing stale measurements`)
  const xml = readFileSync(report, 'utf8')
  const stdout = xml.match(/<system-out><!\[CDATA\[([\s\S]*?)\]\]><\/system-out>/)?.[1] ?? ''
  reports.push({
    test: name,
    suite: xml.match(/<testsuite[^>]+>/)?.[0],
    stdout: stdout.trim().split('\n'),
  })
}

const measurement = {
  reference: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  observedAt: new Date().toISOString(),
  exitCode: result.status,
  expectResponsive: true,
  sourceHashes: Object.fromEntries([
    'apps/android/app/src/main/java/build/cairn/app/data/HistoryCache.kt',
    'apps/android/app/src/main/java/build/cairn/app/data/HistoryCacheWriter.kt',
    'apps/android/app/src/main/java/build/cairn/app/ui/LiveUi.kt',
    'apps/android/app/src/scrollTest/java/build/cairn/app/ui/CacheRefreshCases.kt',
    'apps/android/app/src/test/java/build/cairn/app/data/HistoryCacheCostDiagnosisTest.kt',
  ].map(file => [file, createHash('sha256').update(readFileSync(resolve(root, file))).digest('hex')])),
  host: {
    platform: platform(),
    release: release(),
    cpu: cpus()[0].model,
    logicalCpus: cpus().length,
    totalMemoryBytes: totalmem(),
    loadAverage: loadavg(),
  },
  limits: [
    'Synthetic JVM/Robolectric fixtures; no affected-phone FPS claim',
    'Identity seal/unseal excludes Android Keystore latency',
    'Cold read means new HistoryCache instance, with warm host filesystem page cache',
    'Controlled write gate demonstrates scheduling dependency, not phone storage time',
  ],
  reports,
}
if (process.argv[2]) {
  const output = resolve(process.argv[2])
  mkdirSync(dirname(output), { recursive: true })
  writeFileSync(output, `${JSON.stringify(measurement, null, 2)}\n`)
}

console.log(JSON.stringify(measurement, null, 2))
process.exitCode = result.status ?? 1
