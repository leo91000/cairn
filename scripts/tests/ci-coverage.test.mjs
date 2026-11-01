import { existsSync, readFileSync } from 'node:fs'
import { runInNewContext } from 'node:vm'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const quality = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const android = parse(readFileSync(new URL('../../.github/workflows/android.yaml', import.meta.url), 'utf8'))
const pr = parse(readFileSync(new URL('../../.github/workflows/pr.yaml', import.meta.url), 'utf8'))

describe('parallel CI coverage', () => {
  it.each(['image', 'beacon-image'])('exports %s layers for main and version tags without giving PR refs a cache writer', (name) => {
    const build = quality.jobs[name].steps.find(step => step.uses?.startsWith('docker/build-push-action@'))
    const expression = build.with['cache-to'].replace(/^\$\{\{\s*|\s*\}\}$/g, '')
    const image = name === 'image' ? 'cairn' : 'cairn-beacon'
    for (const [ref, writes] of [
      ['refs/heads/main', true],
      ['refs/tags/v0.74.1', true],
      ['refs/tags/nightly', false],
      ['refs/heads/fix/cache', false],
      ['refs/heads/v0.74.1', false],
      ['refs/pull/42/merge', false],
    ]) {
      // Evaluate the string/boolean expression used by the workflow, not a copied predicate.
      const destination = runInNewContext(expression, {
        github: { ref, repository_owner: 'cache-owner' },
        startsWith: (value, prefix) => value.startsWith(prefix),
        format: (template, value) => template.replaceAll('{0}', value),
      }, { timeout: 1000 })
      expect(destination, ref).toBe(writes
        ? `type=registry,ref=ghcr.io/cache-owner/${image}:buildcache,mode=max,compression=zstd,compression-level=3,ignore-error=true`
        : '')
    }
  })

  it('keeps full qualification off PRs and branch pushes while preserving nightly and tag checks', () => {
    for (const workflow of [quality, android]) {
      expect(workflow.on.pull_request).toBeUndefined()
      expect(workflow.on.push).toEqual({ tags: ['v*'] })
      expect(workflow.on.schedule).toHaveLength(1)
      expect(workflow.on.workflow_dispatch).toBeDefined()
    }

    expect(Object.keys(pr.on).sort()).toEqual(['pull_request', 'push'])
    expect(pr.on.push.branches).toEqual(['main'])
    expect(pr.jobs.required.if).toBe('always()')
    expect(pr.jobs.required.needs).toEqual(['scope', 'quality', 'contracts', 'android'])
    expect(pr.jobs.quality.steps.some(step => step.run === 'pnpm validate:pr quality')).toBe(true)
    expect(pr.jobs.contracts.steps.some(step => step.run === 'pnpm validate:pr contracts')).toBe(true)
    expect(pr.jobs.android.steps.some(step => step.run === 'pnpm validate:pr android')).toBe(true)
    expect(quality.jobs.quality.steps.some(step => step.run === 'pnpm validate:pr quality')).toBe(true)
    expect(quality.jobs.quality.steps.some(step => step.run === 'pnpm validate:pr contracts')).toBe(true)
  })

  it('opts into one physical migration case after the normal backend suite without changing ordinary CI', () => {
    const input = quality.on.workflow_dispatch.inputs['backend-qualification']
    expect(input).toEqual({
      description: 'Optional physical backend qualification after the backend suite',
      type: 'choice',
      options: ['none', 'backend-migration'],
      default: 'none',
    })

    const backend = quality.jobs.backend
    const normal = backend.steps.findIndex(step => step.run?.includes('--nextest --test-threads'))
    const selected = backend.steps.findIndex(step => step.name === 'Qualify physical backend migration with the same compiled source')
    expect(selected).toBeGreaterThan(normal)
    const step = backend.steps[selected]
    const enabled = step.if.replaceAll('inputs.backend-qualification', 'selection')
    for (const selection of [undefined, '', 'none', 'other', 'backend-migration']) {
      expect(runInNewContext(enabled, { selection }, { timeout: 1000 })).toBe(selection === 'backend-migration')
    }

    expect(step.run).toContain('set -o pipefail')
    expect(step.run).toContain('\'moto[server]==5.2.3\' awscli blake3==1.0.8')
    expect(step.run).toContain('scripts/tests/node_s3_test.py --case backend_migration_keeps_chat_and_files_both_directions_and_preserves_source_on_failure')
    expect(step.run).toContain('1 passed; 0 failed; 0 ignored;')
    expect(step.run).not.toContain('${{')
    expect(step.run).not.toContain('CAIRN_TEST_REUSED_BUILD')
    expect(step.run).not.toContain('--archive-file')
    expect(backend['timeout-minutes']).toBe(30)
  })

  it('keeps every split validation in the image publication gate', () => {
    for (const name of ['build', 'quality', 'rust-check', 'backend', 'node-checks', 'browser', 'network-bench', 'native-runtime', 'android-direct-build', 'android-direct', 'image', 'beacon-image']) {
      expect(quality.jobs.publish.needs).toContain(name)
      expect(quality.jobs.publish.if).toContain(`needs.${name}.result == 'success'`)
    }
  })

  it('keeps shared Rust preparation cold and preserves only exact native reference caching', () => {
    expect(existsSync(new URL('../../.github/actions/cache-rust/action.yaml', import.meta.url))).toBe(false)
    for (const name of ['build', 'rust-check', 'backend', 'node-checks']) {
      const steps = quality.jobs[name].steps
      expect(steps.some(step => step.uses === './.github/actions/cache-rust')).toBe(false)
      expect(steps.some(step => step.uses?.startsWith('actions/cache'))).toBe(false)
    }

    const referenceCaches = quality.jobs['native-runtime'].steps.filter(step => step.uses?.startsWith('actions/cache'))
    expect(referenceCaches).toHaveLength(2)
    for (const step of referenceCaches) {
      expect(step.with.path).toBe('reference-native/')
      expect(step.with.key).toContain('steps.native-reference-key.outputs.key')
    }

    expect(referenceCaches[0].with.key).toBe(referenceCaches[1].with.key)
  })

  it('observes capacity around actual cold builds without introducing a cache or writer mode', () => {
    const action = parse(readFileSync(new URL('../../.github/actions/observe-rust-build/action.yaml', import.meta.url), 'utf8'))
    expect(Object.keys(action.inputs)).toEqual(['phase'])
    expect(action.runs.steps.some(step => step.uses?.startsWith('actions/cache'))).toBe(false)
    const observation = action.runs.steps.find(step => step.run?.includes('ci-rust-build-observation.mjs'))
    expect(observation['continue-on-error']).toBe(true)
    const upload = action.runs.steps.find(step => step.uses?.startsWith('actions/upload-artifact'))
    expect(upload['continue-on-error']).toBe(true)
    expect(upload.with.path).toContain('runner.temp')
    for (const name of ['build', 'rust-check', 'backend', 'node-checks']) {
      const steps = quality.jobs[name].steps
      const before = steps.findIndex(step => step.uses === './.github/actions/observe-rust-build' && step.with?.phase === 'before-build')
      const after = steps.findIndex(step => step.uses === './.github/actions/observe-rust-build' && step.with?.phase === 'after-build')
      expect(before).toBeGreaterThan(-1)
      expect(after).toBeGreaterThan(before)
      expect(after).toBe(steps.length - 1)
      expect(steps[after].if).toBe('always()')
    }
  })

  it('observes exact archived native fixtures symmetrically while keeping original binaries and qualification flags', () => {
    const steps = quality.jobs['native-runtime'].steps
    const reference = steps.find(step => step.name === 'Qualify the exact reference on the current disposable host')
    const candidate = steps.find(step => step.name === 'Qualify real ublk, S3, eviction, systemd ownership and crash recovery')
    expect(reference.run).toContain('git archive "$REFERENCE_SHA" scripts/tests deploy/nodes/native-host.py')
    expect(candidate.run).toContain('git archive HEAD scripts/tests deploy/nodes/native-host.py')
    expect(reference.run).toContain('native_admission_overlay.py "$reference_fixture" reference')
    expect(candidate.run).toContain('native_admission_overlay.py "$candidate_fixture" candidate')
    expect(reference.run).toContain('--repo "$reference_fixture" --native-runtime reference-native/cairn --qualifier reference-native/examples/native_storage_qualifier')
    expect(candidate.run).toContain('--repo "$candidate_fixture" --native-runtime target/debug/cairn --qualifier target/debug/examples/native_storage_qualifier')
    expect(candidate.run).toContain('CAIRN_NATIVE_DOCKER_QUALIFY=1')
    expect(reference.run).not.toContain('CAIRN_NATIVE_DOCKER_QUALIFY=1')
    expect(steps.indexOf(candidate)).toBeGreaterThan(steps.indexOf(reference))
  })

  it('gives installer and update fixtures their own database and web assets', () => {
    const job = quality.jobs['node-checks']
    expect(job.services.postgres.env.POSTGRES_DB).toBe('cairn_beacon_test')
    expect(job.services.postgres.ports).toContain('5432:5432')
    expect(job.env.CAIRN_BEACON_TEST_DATABASE_URL).toBe('postgres://cairn:test-only@localhost:5432/cairn_beacon_test')
    const web = job.steps.findIndex(step => step.run?.includes('vite build --config apps/web/vite.config.ts'))
    const containers = job.steps.findIndex(step => step.run?.includes('python3 tests/installation_update_containers.py'))
    expect(web).toBeGreaterThan(-1)
    expect(containers).toBeGreaterThan(web)
  })

  it('runs every network topology exactly once on independent hosts', () => {
    const scenarios = quality.jobs['network-bench'].strategy.matrix.include.flatMap(group => group.scenarios.split(' '))
    expect(scenarios.sort()).toEqual(['same-lan', 'mdns-only-client', 'nat-client', 'nat-installation', 'nat-both', 'udp-blocked', 'symmetric-nat', 'symmetric-client', 'same-server', 'network-change', 'packet-loss', 'qualification'].sort())
    expect(quality.jobs['network-bench'].strategy['fail-fast']).toBe(false)
  })

  it('runs the actual missing-only network preparer before every unchanged topology suite', () => {
    for (const name of ['network-bench', 'android-direct']) {
      const steps = quality.jobs[name].steps
      expect(steps.some(step => step.run === `node scripts/prepare-network-tools.mjs ${name}`)).toBe(true)
      expect(steps.some(step => step.run?.startsWith('sudo apt-get update'))).toBe(false)
    }

    const network = quality.jobs['network-bench'].steps.find(step => step.name === 'Validate real network topology and authenticated sessions')
    expect(network.run).toContain('python3 tests/network_bench_test.py')
    expect(network.run).toContain('python3 tests/network_probe_test.py')
    expect(network.run).toContain('python3 tests/stun_docker_test.py')
  })

  it('delivers the pinned local package closure from this build before network preparation', () => {
    const build = quality.jobs.build.steps
    const acquire = build.findIndex(step => step.run === 'node scripts/ci-network-package-closure.mjs acquire target/ci-network-tools')
    const upload = build.findIndex(step => step.with?.name === 'network-tools-noble-amd64-v1')
    expect(acquire).toBeGreaterThan(-1)
    expect(upload).toBeGreaterThan(acquire)
    expect(build[upload].with).toMatchObject({ 'path': 'target/ci-network-tools/', 'retention-days': 1 })
    const consumer = quality.jobs['network-bench']
    expect(consumer.needs).toContain('build')
    const download = consumer.steps.findIndex(step => step.with?.name === 'network-tools-noble-amd64-v1')
    const prepare = consumer.steps.findIndex(step => step.run === 'node scripts/prepare-network-tools.mjs network-bench')
    expect(download).toBeGreaterThan(-1)
    expect(download).toBeLessThan(prepare)
    expect(consumer.steps[download].with).toEqual({ name: 'network-tools-noble-amd64-v1', path: 'target/ci-network-tools/' })
    expect(quality.jobs['android-direct'].steps.some(step => step.with?.name === 'network-tools-noble-amd64-v1')).toBe(false)
  })

  it('runs every native network scenario once with one shared APK producer', () => {
    const build = quality.jobs['android-direct-build']
    const device = quality.jobs['android-direct']
    expect(device.needs).toContain('android-direct-build')
    expect(device.strategy.matrix.scenario).toEqual(['same-lan', 'udp-blocked', 'network-change'])
    expect(device.strategy['fail-fast']).toBe(false)
    expect(device.steps.some(step => step.run?.includes('gradlew'))).toBe(false)
    const upload = build.steps.find(step => step.with?.name === 'android-direct-apks')
    expect(upload.with.path).toContain('apk/debug/app-debug.apk')
    expect(upload.with.path).toContain('apk/androidTest/debug/app-debug-androidTest.apk')
    const download = device.steps.find(step => step.with?.name === 'android-direct-apks')
    expect(download.with.path).toBe('apps/android/app/build/outputs/apk/')
    const observe = device.steps.find(step => step.name === 'Observe native responses, network restart and revocation')
    expect(observe.env.ANDROID_SCENARIO).toContain('matrix.scenario')
    expect(observe.with.script).toContain('"$ANDROID_SCENARIO" --android')
  })

  it('retains complete browser shard coverage and unique evidence names', () => {
    const groups = quality.jobs.browser.strategy.matrix.include
    expect(new Set(groups.map(group => group.group)).size).toBe(groups.length)
    for (const projects of ['journeys*', 'layout-webkit-*'])
      expect(groups.filter(group => group.projects === projects).map(group => group.shard).sort()).toEqual(['1/4', '2/4', '3/4', '4/4'])
    expect(groups.filter(group => group.projects === 'layout-chromium-*')).toHaveLength(1)
  })

  it('builds and delivers the SQL fixture before real Postgres and browser qualification without psql installation', () => {
    const build = quality.jobs.build.steps
    expect(build.find(step => step.name === 'Build the browser-test backend and frontend').run).toContain('--example beacon_sql_fixture')
    expect(build.find(step => step.with?.name === 'browser-backend').with.path).toContain('target/debug/examples/beacon_sql_fixture')
    for (const name of ['browser', 'network-bench', 'android-direct']) {
      const steps = quality.jobs[name].steps
      expect(steps.some(step => step.run?.includes('chmod +x') && step.run.includes('target/debug/examples/beacon_sql_fixture'))).toBe(true)
    }

    const steps = quality.jobs.browser.steps
    expect(steps.some(step => step.run?.includes('postgresql-client'))).toBe(false)
    const sql = steps.findIndex(step => step.run === 'pnpm exec tsx scripts/qualify-beacon-sql-fixture.ts')
    const journeys = steps.findIndex(step => step.run?.includes('pnpm test:e2e'))
    expect(sql).toBeGreaterThan(-1)
    expect(sql).toBeLessThan(journeys)
    expect(quality.jobs.browser.services.postgres.image).toContain('postgres:17-alpine@sha256:')
  })

  it('keeps the browser execution deadline independent of slow preparation and evidence upload', () => {
    const job = quality.jobs.browser
    const tests = job.steps.find(step => step.run?.includes('pnpm test:e2e'))
    const executionMinutes = tests['timeout-minutes']
    expect(Number.isInteger(executionMinutes)).toBe(true)
    expect(executionMinutes).toBeGreaterThan(0)
    expect(executionMinutes).toBeLessThanOrEqual(10)

    // Run 41079506538 reached its first test after this preparation interval.
    // Replaying that delay must leave the complete execution budget and time
    // to upload the failure evidence before the outer job deadline.
    const preparationSeconds = (Date.parse('2026-10-30T15:33:02Z') - Date.parse('2026-10-30T15:23:43Z')) / 1000
    const evidenceSeconds = 60
    const requiredSeconds = preparationSeconds + executionMinutes * 60 + evidenceSeconds
    expect(job['timeout-minutes'] * 60).toBeGreaterThan(requiredSeconds)
  })

  it('downloads the published image outside the runtime smoke deadline', () => {
    const steps = quality.jobs.image.steps
    const smokeIndex = steps.findIndex(step => step.run?.includes('node tests/container-smoke.mjs'))
    const pullIndex = steps.findIndex(step => step.run?.includes('docker --context default pull "$SMOKE_IMAGE"'))
    expect(pullIndex).toBeGreaterThan(-1)
    expect(pullIndex).toBeLessThan(smokeIndex)

    const pull = steps[pullIndex]
    // Locally built PR candidates already have the exact image and must not pull.
    expect(pull.if).toBe('steps.target.outputs.local == \'\'')
    expect(Number.isInteger(pull['timeout-minutes'])).toBe(true)
    // Run 41110883520 spent the complete three-minute docker-run budget pulling.
    expect(pull['timeout-minutes']).toBeGreaterThan(3)
    expect(pull['timeout-minutes']).toBeLessThanOrEqual(10)
  })

  it('keeps Android unit tests, lint and all device shards as publication prerequisites', () => {
    expect(android.jobs.check.strategy.matrix.task).toEqual(['testDebugUnitTest', 'lintDebug'])
    expect(android.jobs.device.strategy.matrix.shard).toContain('[1, 2, 3, 4]')
    for (const job of ['check', 'device', 'device-report', 'release']) {
      expect(android.jobs.publish.needs).toContain(job)
      expect(android.jobs.publish.if).toContain(`needs.${job}.result == 'success'`)
    }
  })
})
