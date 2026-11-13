import { existsSync, readFileSync } from 'node:fs'
import { runInNewContext } from 'node:vm'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'
import {
  AVD_HOST_HEADROOM_BYTES,
  AVD_USERDATA_BYTES_REQUIRED,
  capacityPlan,
  sdkPackagesToRemove,
} from '../android-emulator-sdk.mjs'
import { nextestArtifact } from '../install-nextest-ci.mjs'

const quality = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const android = parse(readFileSync(new URL('../../.github/workflows/android.yaml', import.meta.url), 'utf8'))
const pr = parse(readFileSync(new URL('../../.github/workflows/pr.yaml', import.meta.url), 'utf8'))
const chatJourneySource = readFileSync(new URL('../../tests/e2e/chats.spec.ts', import.meta.url), 'utf8')

describe('parallel CI coverage', () => {
  it('runs the native controller preflight in the full workspace graph with the same installation test selection', () => {
    const steps = quality.jobs.backend.steps
    const preflight = steps.find(step => step.name === 'Qualify native controller concurrency before building every test binary')
    const suite = steps.find(step => step.name === 'Run backend tests across binaries with isolated credentials')

    expect(preflight.run).toBe('node scripts/test-backend.mjs --nextest --workspace --lib -E \'package(=cairn-installation) & (test(native::service::tests::) | test(nodes::files::tests::))\' --no-capture')
    expect(preflight.if).toBe(suite.if)
    expect(preflight['continue-on-error']).toBeUndefined()
    expect(steps.indexOf(preflight)).toBeLessThan(steps.indexOf(suite))
  })

  it('keeps both post-suite measurements in the workspace test graph with exact case selection', () => {
    for (const [name, selection] of [
      ['Measure the bounded bootstrap observer without filesystem work', 'native::bootstrap::tests::observation_hot_path_measurement -- --exact --nocapture'],
      ['Measure bounded Native terminal-reason metadata', 'native::service::terminal_tests::terminal_reason_metadata_measurement -- --exact --ignored --nocapture'],
    ]) {
      expect(quality.jobs.backend.steps.find(step => step.name === name).run)
        .toBe(`node scripts/test-backend.mjs --workspace --lib ${selection}`)
    }
  })

  it('bounds nextest acquisition and refuses an unsupported version instead of installing fallback tools', () => {
    const backend = quality.jobs.backend
    const installers = backend.steps.filter(step => step.run === 'node scripts/install-nextest-ci.mjs')
    expect(installers).toHaveLength(1)

    const installer = installers[0]
    expect(backend.steps.some(step => step.uses?.startsWith('taiki-e/install-action@'))).toBe(false)
    expect(nextestArtifact.version).toBe('0.9.149')
    expect(nextestArtifact.url).toBe('https://api.github.com/repos/nextest-rs/nextest/releases/assets/660814555')
    expect(nextestArtifact.sha256).toBe('32d1d8c0567b7a80bee2bad8bb09125c846da96bc009b2de11ec08557290c770')
    expect(installer['timeout-minutes']).toBeGreaterThan(0)
    expect(installer['timeout-minutes']).toBeLessThanOrEqual(3)
    // eslint-disable-next-line no-template-curly-in-string -- exact step-scoped GitHub expression
    expect(installer.env).toEqual({ GITHUB_TOKEN: '${{ github.token }}' })
    expect(backend.env.GITHUB_TOKEN).toBeUndefined()
    expect(backend.permissions.contents).toBe('read')
    expect(installer['continue-on-error']).toBeUndefined()
    expect(installer.if).toBeUndefined()
    expect(backend['timeout-minutes']).toBe(30)
  })

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
    expect(pr.jobs.required.needs).toEqual(['scope', 'quality', 'contracts', 'android', 'browser'])
    expect(pr.jobs.quality.steps.some(step => step.run === 'pnpm validate:pr quality')).toBe(true)
    expect(pr.jobs.contracts.steps.some(step => step.run === 'pnpm validate:pr contracts')).toBe(true)
    expect(pr.jobs.android.steps.some(step => step.run === 'pnpm validate:pr android')).toBe(true)
    expect(quality.jobs.quality.steps.some(step => step.run === 'pnpm validate:pr quality')).toBe(true)
    expect(quality.jobs.quality.steps.some(step => step.run === 'pnpm validate:pr contracts')).toBe(true)
  })

  it('runs the selected confirmed backend-change case in a separate bounded job after the backend suite', () => {
    const input = quality.on.workflow_dispatch.inputs['backend-qualification']
    expect(input).toEqual({
      description: 'Run confirmed backend change regression in a separate job after the backend suite',
      type: 'choice',
      options: ['none', 'backend-migration'],
      default: 'none',
    })

    const backend = quality.jobs.backend
    expect(backend['timeout-minutes']).toBe(30)
    expect(backend.steps.some(step => step.name === 'Qualify confirmed backend changes from this commit')).toBe(false)
    expect(backend.steps.some(step => step.run?.includes('--nextest --test-threads'))).toBe(true)
    expect(backend.steps.some(step => step.name === 'Measure the bounded bootstrap observer without filesystem work')).toBe(true)
    expect(backend.steps.some(step => step.name === 'Qualify native backend namespace isolation with ordinary private bind mounts')).toBe(true)
    expect(backend.steps.some(step => step.name === 'Check Rust documentation examples')).toBe(true)
    expect(backend.steps.some(step => step.name === 'Qualify the resolved OpenCode release against its native adapter')).toBe(false)
    expect(backend.steps.some(step => step.name === 'Qualify the resolved Claude Code release against its native adapter')).toBe(false)
    expect(backend.steps.some(step => step.run?.includes('--test native_plan_turns'))).toBe(false)
    expect(backend.steps.some(step => step.name === 'Check all Rust targets for a backend-only qualification')).toBe(false)
    expect(backend.steps.some(step => step.uses === './.github/actions/observe-rust-build' && step.with?.phase === 'before-build')).toBe(true)
    expect(backend.steps.some(step => step.uses === './.github/actions/observe-rust-build' && step.with?.phase === 'after-build')).toBe(true)

    const rustCheck = quality.jobs['rust-check']
    expect(rustCheck.needs).toBe('resolve')
    expect(rustCheck.if).toContain('inputs.backend-only == true')
    const rustCondition = rustCheck.if
      .replaceAll('inputs.backend-only', 'backendOnly')
      .replaceAll('inputs.ci-diagnostic', 'diagnostic')
      .replace(/\bneeds\.([a-z0-9-]+)\b/g, (_match, name) => `needs[${JSON.stringify(name)}]`)
    const runsRustCheck = (backendOnly, resolveResult, reuse) => runInNewContext(rustCondition, {
      backendOnly,
      diagnostic: '',
      needs: { resolve: { result: resolveResult, outputs: { reuse } } },
      github: { ref: 'refs/heads/feature' },
      always: () => true,
      cancelled: () => false,
    }, { timeout: 1000 })
    expect(runsRustCheck(true, 'skipped', '')).toBe(true)
    expect(runsRustCheck(false, 'success', 'false')).toBe(true)
    expect(runsRustCheck(false, 'success', 'true')).toBe(false)

    const selected = quality.jobs['backend-migration-qualification']
    expect(selected.needs).toEqual(['backend', 'cli-qualification'])
    expect(selected['timeout-minutes']).toBe(30)
    expect(selected.if).not.toContain('needs.resolve')
    expect(backend.if).toContain('inputs.backend-only == true')
    const backendCondition = backend.if
      .replaceAll('inputs.backend-only', 'backendOnly')
      .replaceAll('inputs.ci-diagnostic', 'diagnostic')
      .replaceAll('inputs.backend-qualification', 'selection')
      .replace(/\bneeds\.([a-z0-9-]+)\b/g, (_match, name) => `needs[${JSON.stringify(name)}]`)
    const runsBackend = (backendOnly, selection, resolveResult, reuse) => runInNewContext(backendCondition, {
      backendOnly,
      diagnostic: '',
      selection,
      needs: { resolve: { result: resolveResult, outputs: { reuse } } },
      github: { ref: 'refs/heads/feature' },
      always: () => true,
      cancelled: () => false,
    }, { timeout: 1000 })
    expect(runsBackend(false, 'backend-migration', 'success', 'true')).toBe(true)
    expect(runsBackend(false, 'none', 'success', 'true')).toBe(false)
    expect(runsBackend(false, 'none', 'success', 'false')).toBe(true)
    expect(runsBackend(true, 'none', 'skipped', '')).toBe(true)
    expect(runsBackend(false, 'backend-migration', 'failure', 'true')).toBe(false)
    const enabled = selected.if
      .replaceAll('inputs.backend-qualification', 'selection')
      .replaceAll('inputs.backend-only', 'backendOnly')
      .replaceAll('needs.cli-qualification', 'needs.cliQualification')
    for (const [selection, backendResult, expected] of [
      ['backend-migration', 'success', true],
      ['backend-migration', 'failure', false],
      ['backend-migration', 'skipped', false],
      ['backend-migration', 'cancelled', false],
      ['none', 'success', false],
      [undefined, 'success', false],
    ]) {
      expect(runInNewContext(enabled, {
        selection,
        backendOnly: false,
        needs: { backend: { result: backendResult }, cliQualification: { result: 'success' } },
        always: () => true,
        cancelled: () => false,
      }, { timeout: 1000 })).toBe(expected)
    }

    expect(selected.steps[0].uses).toBe('actions/checkout@v7.0.1')
    expect(selected.steps[1]).toEqual({ uses: './.github/actions/setup-pnpm', with: { cache: true } })
    expect(selected.steps[2].run).toBe('rustup toolchain install 1.97.1 --profile minimal --component rustfmt,clippy && rustup default 1.97.1')
    expect(selected.steps[3].run).toBe('node scripts/install-native-ci-dependencies.mjs')
    expect(selected.steps[3]['timeout-minutes']).toBe(10)
    expect(selected.env).toEqual({ CARGO_PROFILE_DEV_DEBUG: '0', CARGO_INCREMENTAL: '0' })
    expect(selected.steps.some(step => step.uses?.startsWith('actions/download-artifact@'))).toBe(false)
    const step = selected.steps.find(step => step.name === 'Qualify confirmed backend changes from this commit')
    expect(step).toBeDefined()
    expect(step.run).toContain('set -o pipefail')
    expect(step.run).not.toContain('moto[server]')
    expect(step.run).toContain('pnpm test:backend -p cairn-installation --test native_nodes both_backend_change_routes_require_file_loss_confirmation_and_expire_without_conversion -- --exact')
    expect(step.run).toContain('1 passed; 0 failed; 0 ignored;')
    expect(step.run).not.toContain('${{')
    expect(step.run).not.toContain('CAIRN_TEST_REUSED_BUILD')
    expect(step.run).not.toContain('--archive-file')
  })

  it('qualifies the exact resolved CLIs in their own bounded job after the normal backend', () => {
    const job = quality.jobs['cli-qualification']
    expect(job).toBeDefined()
    expect(job.needs).toEqual(['resolve', 'backend'])
    expect(job['timeout-minutes']).toBe(30)
    expect(job.env).toEqual({ CARGO_PROFILE_DEV_DEBUG: '0', CARGO_INCREMENTAL: '0' })
    expect(job.steps[0]).toEqual({ uses: 'actions/checkout@v7.0.1' })
    expect(job.steps[1]).toEqual({ uses: './.github/actions/setup-pnpm', with: { cache: true } })
    expect(job.steps[2].run).toBe('rustup toolchain install 1.97.1 --profile minimal --component rustfmt,clippy && rustup default 1.97.1')
    expect(job.steps[3].run).toBe('node scripts/install-native-ci-dependencies.mjs')
    expect(job.steps[3]['timeout-minutes']).toBe(10)
    const qualifications = job.steps.filter(step => step.name?.startsWith('Qualify'))
    expect(qualifications.map(step => step.name)).toEqual([
      'Qualify the resolved OpenCode release against its native adapter',
      'Qualify the resolved Claude Code release against its native adapter',
      'Qualify plan turns of the resolved Claude Code and Codex releases',
    ])
    for (const step of qualifications) {
      expect(step.if).toBe('inputs.backend-only != true')
      expect(step.run).toContain('node scripts/test-backend.mjs -p cairn-installation --test ')
      expect(step.run).toContain('--ignored')
      expect(step.run).not.toContain('CAIRN_TEST_REUSED_BUILD')
    }

    /* eslint-disable no-template-curly-in-string -- exact GitHub expressions shared with the image build */
    expect(qualifications[0].env).toEqual({
      OPENCODE_VERSION: '${{ needs.resolve.outputs.opencode }}',
      SERVED_OPENCODE: '${{ needs.resolve.outputs.served-opencode }}',
    })
    expect(qualifications[1].env).toEqual({
      CLAUDE_VERSION: '${{ needs.resolve.outputs.claude }}',
      SERVED_CLAUDE: '${{ needs.resolve.outputs.served-claude }}',
    })
    expect(qualifications[2].env).toEqual({
      CLAUDE_VERSION: '${{ needs.resolve.outputs.claude }}',
      CODEX_VERSION: '${{ needs.resolve.outputs.codex }}',
    })
    /* eslint-enable no-template-curly-in-string */
    expect(qualifications[2].run).toContain('--test native_plan_turns -- --ignored --test-threads 1')
    expect(quality.jobs.backend.steps.find(step => step.run?.includes('--nextest --test-threads')).run).toContain('--nextest --test-threads 8 --no-fail-fast --color never --status-level all')
  })

  it('preserves CLI selection and combined migration prerequisites across full, reuse and isolated modes', () => {
    function runs(name, inputs, backendResult, cliResult, resolveResult = 'success', reuse = 'false', ref = 'refs/heads/feature', cancelled = false) {
      const job = quality.jobs[name]
      expect(job).toBeDefined()
      const expression = job.if.replace(/\b(inputs|needs)\.([a-z0-9-]+)\b/g, (_match, scope, property) => `${scope}[${JSON.stringify(property)}]`)
      const needs = {
        'resolve': { result: resolveResult, outputs: { reuse } },
        'backend': { result: backendResult },
        'cli-qualification': { result: cliResult },
      }
      // Match Actions' implicit success guard for any job without a status function.
      const dependencies = Array.isArray(job.needs) ? job.needs : [job.needs]
      if (!expression.includes('always()') && dependencies.some(name => needs[name].result !== 'success'))
        return false
      return Boolean(runInNewContext(expression, {
        inputs: { 'ci-diagnostic': '', ...inputs },
        needs,
        github: { ref },
        always: () => true,
        cancelled: () => cancelled,
      }, { timeout: 1000 }))
    }

    for (const cliResult of ['success', 'failure', 'cancelled', 'skipped']) {
      for (const backendResult of ['success', 'failure', 'cancelled', 'skipped']) {
        for (const reuse of ['false', 'true']) {
          for (const validationOnly of [false, true]) {
            const full = {
              'ci-diagnostic': 'none',
              'backend-only': false,
              'validation-only': validationOnly,
              'backend-qualification': 'backend-migration',
            }
            expect(runs('cli-qualification', full, backendResult, cliResult, 'success', reuse)).toBe(backendResult === 'success')
            expect(runs('backend-migration-qualification', full, backendResult, cliResult, 'success', reuse)).toBe(backendResult === 'success' && cliResult === 'success')
            const backendOnly = { ...full, 'backend-only': true }
            expect(runs('cli-qualification', backendOnly, backendResult, cliResult, 'skipped', reuse)).toBe(false)
            expect(runs('backend-migration-qualification', backendOnly, backendResult, cliResult, 'skipped', reuse)).toBe(backendResult === 'success')
          }
        }
      }
    }

    for (const diagnostic of ['backend', 'native']) {
      for (const backendOnly of [false, true]) {
        for (const cliResult of ['success', 'failure', 'cancelled', 'skipped']) {
          const inputs = { 'ci-diagnostic': diagnostic, 'backend-only': backendOnly }
          expect(runs('cli-qualification', inputs, 'success', cliResult)).toBe(false)
        }
      }
    }

    for (const diagnostic of ['', 'none']) {
      for (const ref of ['refs/heads/main', 'refs/heads/feature', 'refs/tags/v0.75.0']) {
        expect(runs('cli-qualification', { 'ci-diagnostic': diagnostic }, 'success', 'skipped', 'success', 'false', ref)).toBe(true)
      }
    }

    for (const [diagnostic, backendOnly, selection, reuse, ref, expected] of [
      ['none', false, 'none', 'false', 'refs/heads/feature', [true, true, false]],
      ['none', false, 'backend-migration', 'false', 'refs/heads/feature', [true, true, true]],
      ['', false, 'none', 'true', 'refs/heads/main', [true, true, false]],
      ['none', false, 'none', 'true', 'refs/heads/feature', [false, false, false]],
      ['none', false, 'backend-migration', 'true', 'refs/heads/feature', [true, true, true]],
      ['none', true, 'backend-migration', '', 'refs/heads/feature', [true, false, true]],
      ['backend', true, 'none', '', 'refs/heads/feature', [true, false, false]],
      ['native', true, 'none', '', 'refs/heads/feature', [false, false, false]],
    ]) {
      for (const validationOnly of [false, true]) {
        const inputs = {
          'ci-diagnostic': diagnostic,
          'backend-only': backendOnly,
          'backend-qualification': selection,
          'validation-only': validationOnly,
        }
        const resolveResult = backendOnly ? 'skipped' : 'success'
        const backend = runs('backend', inputs, 'skipped', 'skipped', resolveResult, reuse, ref)
        const backendResult = backend ? 'success' : 'skipped'
        const cli = runs('cli-qualification', inputs, backendResult, 'skipped', resolveResult, reuse, ref)
        const migration = runs('backend-migration-qualification', inputs, backendResult, cli ? 'success' : 'skipped', resolveResult, reuse, ref)
        expect([backend, cli, migration]).toEqual(expected)
      }
    }

    for (const resolveResult of ['failure', 'cancelled', 'skipped'])
      expect(runs('cli-qualification', {}, 'success', 'success', resolveResult)).toBe(false)
    expect(runs('cli-qualification', {}, 'success', 'success', 'success', 'false', undefined, true)).toBe(false)
    for (const selection of ['none', undefined])
      expect(runs('backend-migration-qualification', { 'backend-qualification': selection }, 'success', 'success')).toBe(false)
    expect(runs('backend-migration-qualification', { 'backend-qualification': 'backend-migration' }, 'success', 'success', 'success', 'false', undefined, true)).toBe(false)
  })

  it('requires the selected physical qualification for image publication and bypasses it when unselected', () => {
    const publish = quality.jobs.publish
    expect(publish.needs).toContain('backend-migration-qualification')
    expect(publish.if).toContain('inputs.backend-qualification != \'backend-migration\' || needs.backend-migration-qualification.result == \'success\'')

    const expression = publish.if
      .replaceAll('inputs.backend-qualification', 'selection')
      .replaceAll('inputs.validation-only', 'validationOnly')
      .replace(/\bneeds\.([a-z0-9-]+)\b/g, (_match, name) => `needs[${JSON.stringify(name)}]`)
    const baseNeeds = {
      'resolve': { outputs: { reuse: 'false' } },
      'build': { result: 'success' },
      'quality': { result: 'success' },
      'rust-check': { result: 'success' },
      'backend': { result: 'success' },
      'cli-qualification': { result: 'success' },
      'node-checks': { result: 'success' },
      'browser': { result: 'success' },
      'network-bench': { result: 'success' },
      'native-runtime': { result: 'success' },
      'android-direct-build': { result: 'success' },
      'android-direct': { result: 'success' },
      'image': { result: 'success', outputs: { published: 'true' } },
      'beacon-image': { result: 'success', outputs: { published: 'true' } },
    }
    const canPublish = (selection, migrationResult, backendResult = 'success', reuse = 'false', validationOnly = false, cliResult = 'success') => runInNewContext(expression, {
      selection,
      validationOnly,
      needs: {
        ...baseNeeds,
        'resolve': { outputs: { reuse } },
        'backend': { result: backendResult },
        'cli-qualification': { result: cliResult },
        'backend-migration-qualification': { result: migrationResult },
      },
      always: () => true,
      cancelled: () => false,
    }, { timeout: 1000 })

    expect(canPublish('backend-migration', 'success')).toBe(true)
    expect(canPublish('backend-migration', 'failure')).toBe(false)
    expect(canPublish('backend-migration', 'skipped')).toBe(false)
    expect(canPublish('backend-migration', 'cancelled')).toBe(false)
    for (const cliResult of ['success', 'failure', 'cancelled', 'skipped']) {
      for (const selection of ['none', 'backend-migration']) {
        expect(canPublish(selection, selection === 'none' ? 'skipped' : 'success', 'success', 'false', false, cliResult)).toBe(cliResult === 'success')
        expect(canPublish(selection, 'success', 'success', 'true', false, cliResult)).toBe(false)
        expect(canPublish(selection, 'success', 'success', 'false', true, cliResult)).toBe(false)
      }
    }

    expect(canPublish('none', 'skipped')).toBe(true)
    expect(canPublish(undefined, 'skipped')).toBe(true)
    expect(canPublish(undefined, 'skipped', 'success', 'true')).toBe(false)
    expect(canPublish(undefined, 'skipped', 'success', 'false', true)).toBe(false)
  })

  it('requires the selected physical qualification before re-promoting a reused image', () => {
    const promote = quality.jobs.promote
    expect(promote.needs).toContain('resolve')
    expect(promote.needs).toContain('backend-migration-qualification')
    expect(promote.if).toContain('needs.resolve.outputs.reuse == \'true\'')
    expect(promote.if).toContain('inputs.backend-qualification != \'backend-migration\' || needs.backend-migration-qualification.result == \'success\'')

    const expression = promote.if
      .replaceAll('inputs.backend-qualification', 'selection')
      .replaceAll('inputs.validation-only', 'validationOnly')
      .replace(/\bneeds\.([a-z0-9-]+)\b/g, (_match, name) => `needs[${JSON.stringify(name)}]`)
    const canPromote = (selection, migrationResult, reuse, validationOnly = false) => runInNewContext(expression, {
      selection,
      validationOnly,
      needs: {
        'resolve': { outputs: { reuse } },
        'backend-migration-qualification': { result: migrationResult },
      },
      always: () => true,
      cancelled: () => false,
    }, { timeout: 1000 })

    expect(canPromote('backend-migration', 'success', 'true')).toBe(true)
    expect(canPromote('backend-migration', 'failure', 'true')).toBe(false)
    expect(canPromote('backend-migration', 'skipped', 'true')).toBe(false)
    expect(canPromote('backend-migration', 'cancelled', 'true')).toBe(false)
    expect(canPromote('none', 'skipped', 'true')).toBe(true)
    expect(canPromote(undefined, 'skipped', 'true')).toBe(true)
    expect(canPromote('backend-migration', 'success', 'false')).toBe(false)
    expect(canPromote(undefined, 'skipped', 'true', true)).toBe(false)

    const migrationExpression = quality.jobs['backend-migration-qualification'].if
      .replace(/\b(inputs|needs)\.([a-z0-9-]+)\b/g, (_match, scope, property) => `${scope}[${JSON.stringify(property)}]`)
    for (const cliResult of ['success', 'failure', 'cancelled', 'skipped']) {
      const migrationRuns = runInNewContext(migrationExpression, {
        inputs: { 'backend-qualification': 'backend-migration', 'backend-only': false },
        needs: { 'backend': { result: 'success' }, 'cli-qualification': { result: cliResult } },
        always: () => true,
        cancelled: () => false,
      }, { timeout: 1000 })
      const migrationResult = migrationRuns ? 'success' : 'skipped'
      expect(canPromote('backend-migration', migrationResult, 'true')).toBe(cliResult === 'success')
      expect(canPromote('backend-migration', migrationResult, 'true', true)).toBe(false)
      // Without selected migration, reuse retains the trusted prior exact-tree proof.
      expect(canPromote('none', 'skipped', 'true')).toBe(true)
    }
  })

  it('keeps the PR browser lane conditional, read-only and limited to the saved-chat journey', () => {
    const browser = pr.jobs.browser
    expect(browser.needs).toBe('scope')
    expect(browser.if).toBe('needs.scope.outputs.browser == \'true\'')
    expect(browser.permissions).toEqual({ contents: 'read' })
    expect(browser['timeout-minutes']).toBe(25)
    const dependencies = browser.steps.find(step => step.name === 'Install the pinned Rust toolchain and build dependencies')
    expect(dependencies.run).toContain('node scripts/prepare-browser-build-dependencies.mjs')
    expect(dependencies.run).toContain('--default-toolchain 1.97.1')
    expect(dependencies.run).not.toContain('apt-get')
    expect(browser.services.postgres.image).toBe('postgres:17-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24')
    expect(browser.services.postgres.env).toEqual({
      POSTGRES_USER: 'cairn',
      POSTGRES_PASSWORD: 'test-only',
      POSTGRES_DB: 'cairn_beacon_test',
    })
    expect(browser.container.image).toBe('mcr.microsoft.com/playwright:v1.64.1-noble@sha256:2f05f8a734509a7b3f26a912639488ee08f58a7b292d14d62271bcff9038768f')

    const build = browser.steps.find(step => step.name === 'Build the manager fixtures and web assets')
    expect(build.run.trim().split('\n').map(line => line.trim())).toEqual([
      'cargo build --locked -p cairn-installation --bin cairn --example worker_fixture --example browser_fixture',
      'cargo build --locked -p cairn-beacon --bin cairn-beacon --example beacon_sql_fixture',
      'VITE_CAIRN_TEST_RESUME_DIAGNOSTICS=true pnpm exec vite build --config apps/web/vite.config.ts',
    ])
    const journey = browser.steps.find(step => step.name === 'Run the saved-chat resume journey')
    expect(journey['timeout-minutes']).toBe(10)
    expect(journey.run).toBe('pnpm test:e2e --project=journeys-chats tests/e2e/chats.spec.ts --grep \'same-provider quota resume\' --workers=1 --fail-on-flaky-tests')
    const grep = journey.run.match(/--grep '([^']+)'/)?.[1]
    const project = journey.run.match(/--project=([^ ]+)/)?.[1]
    const testFile = journey.run.match(/ (tests\/e2e\/chats\.spec\.ts) --grep/)?.[1]
    expect(grep).toBe('same-provider quota resume')
    expect(project).toBe('journeys-chats')
    expect(testFile).toBe('tests/e2e/chats.spec.ts')

    const resumeTitles = [...chatJourneySource.matchAll(/^test\('([^']+)'/gm)]
      .map(([, title]) => title)
      .filter(title => title.startsWith('same-provider quota resume'))
    expect(resumeTitles).toEqual([
      'same-provider quota resume submits the saved turn with the picker-applied speed',
      'same-provider quota resume retries a prelaunch turn without selection',
      'same-provider quota resume preserves an inherited blank model',
      'same-provider quota resume submits the changed model default and unchanged saved effort',
    ])

    const matches = new RegExp(grep)
    for (const title of resumeTitles) {
      const combinedTitle = `${project} ${testFile} ${title}`
      expect(matches.test(combinedTitle), title).toBe(true)
    }

    expect(matches.test(`${project} ${testFile} unrelated chat case`)).toBe(false)

    const uploads = browser.steps.filter(step => step.uses?.startsWith('actions/upload-artifact@'))
    expect(uploads).toHaveLength(1)
    expect(uploads[0].if).toBe('always()')
    expect(uploads[0].with.path.trim().split('\n').map(path => path.trim())).toEqual([
      'playwright-report/',
      'test-results/',
    ])
    expect(uploads[0].with['retention-days']).toBe(14)
    expect(browser.steps.some(step => step.uses?.startsWith('docker/build-push-action@'))).toBe(false)
    expect(browser.environment).toBeUndefined()
  })

  it('keeps every split validation in the image publication gate', () => {
    for (const name of ['build', 'quality', 'rust-check', 'backend', 'cli-qualification', 'node-checks', 'browser', 'network-bench', 'native-runtime', 'android-direct-build', 'android-direct', 'image', 'beacon-image']) {
      expect(quality.jobs.publish.needs).toContain(name)
      expect(quality.jobs.publish.if).toContain(`needs.${name}.result == 'success'`)
    }
  })

  it('keeps shared Rust preparation cold and preserves only exact native reference caching', () => {
    expect(existsSync(new URL('../../.github/actions/cache-rust/action.yaml', import.meta.url))).toBe(false)
    for (const name of ['build', 'rust-check', 'backend', 'cli-qualification', 'node-checks']) {
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
    for (const name of ['build', 'rust-check', 'backend', 'cli-qualification', 'node-checks']) {
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
    const candidate = steps.find(step => step.name === 'Qualify local disks, ublk, systemd ownership and crash recovery')
    // eslint-disable-next-line no-template-curly-in-string -- same local harness for both exact binary pairs
    expect(reference.run).toContain('git archive "${DIAGNOSTIC_CANDIDATE:-HEAD}" scripts/tests deploy/nodes/native-host.py')
    // eslint-disable-next-line no-template-curly-in-string -- literal shell fallback is the provenance contract
    expect(candidate.run).toContain('git archive "${DIAGNOSTIC_CANDIDATE:-HEAD}" scripts/tests deploy/nodes/native-host.py')
    // eslint-disable-next-line no-template-curly-in-string -- literal GitHub expression is the provenance contract
    expect(candidate.env.DIAGNOSTIC_CANDIDATE).toBe('${{ inputs.ci-diagnostic == \'native\' && inputs.diagnostic-candidate || \'\' }}')
    expect(reference.run).toContain('native_admission_overlay.py "$reference_fixture" reference')
    expect(candidate.run).toContain('native_admission_overlay.py "$candidate_fixture" candidate')
    expect(reference.run).toContain('--repo "$reference_fixture" --native-runtime reference-native/cairn --qualifier reference-native/examples/native_storage_qualifier')
    expect(candidate.run).toContain('--repo "$candidate_fixture" --native-runtime target/debug/cairn --qualifier target/debug/examples/native_storage_qualifier')
    expect(candidate.run).toContain('CAIRN_NATIVE_DOCKER_QUALIFY=1')
    expect(reference.run).not.toContain('CAIRN_NATIVE_DOCKER_QUALIFY=1')
    expect(steps.indexOf(candidate)).toBeGreaterThan(steps.indexOf(reference))
  })

  it('keeps all native preparation tests after their verified candidate producer', () => {
    const steps = quality.jobs['native-runtime'].steps
    const host = steps.findIndex(step => step.name === 'Prepare disposable host block storage')
    const toolchain = steps.findIndex(step => step.run === 'rustup toolchain install 1.97.1 --profile minimal')
    const pair = steps.findIndex(step => step.name === 'Build the immutable diagnostic pair')
    const verify = steps.findIndex(step => step.name === 'Verify the exact reference binaries')
    const consumers = steps.filter(step => step.run?.includes('-p test_native_local_runtime.py'))
    expect(consumers).toHaveLength(1)
    const consumer = consumers[0]
    const index = steps.indexOf(consumer)
    expect(host).toBeLessThan(toolchain)
    expect(toolchain).toBeLessThan(pair)
    expect(steps[pair].run.indexOf('apt-get install -y --no-install-recommends libclang-dev')).toBeLessThan(steps[pair].run.indexOf('scripts/ci-diagnostic-native-build.mjs'))
    expect(pair).toBeLessThan(verify)
    expect(verify).toBeLessThan(index)
    expect(index).toBeLessThan(steps.findIndex(step => step.name === 'Qualify the exact reference on the current disposable host'))
    expect(steps[host].run).not.toContain('test_native_local_runtime.py')
    expect(consumer.if).toBeUndefined()

    expect(consumer.run).toContain('scripts/ci-deadline.mjs "$PWD/native-diagnostics/preparation" 90 -- python3 -m unittest discover -s scripts/tests -p test_native_local_runtime.py -v')
    expect(consumer.run).not.toMatch(/(?:--filter|--ignore|\s-k\s)/)
    expect(steps.filter(step => step.run?.includes('node scripts/ci-diagnostic-native-build.mjs'))).toHaveLength(1)
    const memoryTests = readFileSync(new URL('./test_native_local_runtime.py', import.meta.url), 'utf8')
    expect(memoryTests.match(/^\s+def test_/gm)).toHaveLength(1)
  })

  it('isolates each diagnostic job even when legacy independent inputs are populated', () => {
    function runsWith(inputs, event = 'workflow_dispatch') {
      const results = {}

      function run(name) {
        if (Object.hasOwn(results, name))
          return results[name]
        const job = quality.jobs[name]
        const dependencies = Array.isArray(job.needs) ? job.needs : job.needs ? [job.needs] : []
        const needs = Object.fromEntries(dependencies.map(dependency => [dependency, {
          result: run(dependency) ? 'success' : 'skipped',
          outputs: { reuse: results[dependency] ? 'false' : '' },
        }]))
        const expression = (job.if ?? 'true')
          .replace(/\b(inputs|needs)\.([a-z0-9-]+)\b/g, (_match, scope, property) => `${scope}[${JSON.stringify(property)}]`)
        const failedDependency = dependencies.some(dependency => !results[dependency])
        const enabled = !failedDependency || expression.includes('always()')
        results[name] = enabled && Boolean(runInNewContext(expression, {
          inputs,
          needs,
          github: { event_name: event, ref: 'refs/heads/feature' },
          always: () => true,
          cancelled: () => false,
          startsWith: (value, prefix) => value.startsWith(prefix),
        }, { timeout: 1000 }))
        return results[name]
      }

      for (const name of Object.keys(quality.jobs))
        run(name)
      return Object.keys(results).filter(name => results[name]).sort()
    }

    const legacy = {
      'backend-only': true,
      'validation-only': true,
      'backend-qualification': 'none',
      'plan-storage-reference': '06829d1b5f3d7accb9791c09d49522e7e2d2ab25',
      'exclusive-android': true,
      'browser-workers': '2',
    }
    for (const mode of ['backend', 'native']) {
      expect(runsWith({ ...legacy, 'ci-diagnostic': mode })).toEqual([mode === 'native' ? 'native-runtime' : 'backend'])
    }

    for (const mode of ['', 'none']) {
      expect(runsWith({ ...legacy, 'ci-diagnostic': mode })).toEqual(['backend', 'plan-storage', 'rust-check'])
    }

    expect(quality.on.pull_request).toBeUndefined()
    expect(quality.on.push.tags).toEqual(['v*'])
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

  it('reclaims only unused Android SDK packages and checks userdata headroom before emulator boot', () => {
    const job = quality.jobs['android-direct']
    const steps = job.steps
    const prune = steps.findIndex(step => step.run === 'node scripts/android-emulator-sdk.mjs prune')
    const install = steps.findIndex(step => step.uses === './.github/actions/setup-android-emulator')
    const preflight = steps.findIndex(step => step.run === 'node scripts/android-emulator-sdk.mjs preflight')
    const emulatorIndex = steps.findIndex(step => step.uses?.startsWith('reactivecircus/android-emulator-runner@'))

    expect(prune).toBeGreaterThan(-1)
    expect(prune).toBeLessThan(install)
    expect(install).toBeLessThan(emulatorIndex)
    expect(install).toBeLessThan(preflight)
    expect(preflight).toBeLessThan(emulatorIndex)
    expect(job['timeout-minutes']).toBe(20)

    expect(steps[install].with['build-packages']).toBe('build-tools;37.0.0 platforms;android-36')

    const emulator = steps[emulatorIndex]
    expect(emulator.with).toMatchObject({
      'api-level': 36,
      'arch': 'x86_64',
      'target': 'google_apis',
      'profile': 'pixel_6',
    })
    expect(emulator.with['disk-size']).toBeUndefined()
    expect(emulator.with['pre-emulator-launch-script']).toBe('node scripts/android-emulator-sdk.mjs preflight')
    expect(emulator.with['working-directory']).toBeUndefined()
    expect(emulator.with['emulator-boot-timeout']).toBeUndefined()
    expect(emulator.with.script).toContain('python3 tests/network-bench.py')
    expect(steps[preflight].name).toBe('Check emulator userdata disk headroom')

    const packages = [
      'build-tools;34.0.0 | 34.0.0 | Android SDK Build-Tools 34',
      'build-tools;37.0.0 | 37.0.0 | Android SDK Build-Tools 37',
      'cmake;3.31.5 | 3.31.5 | CMake 3.31.5',
      'ndk;27.3.13750724 | 27.3.13750724 | NDK',
      'platforms;android-35 | 3 | Android SDK Platform 35',
      'platforms;android-36 | 2 | Android SDK Platform 36',
      'system-images;android-35;google_apis;x86_64 | 1 | Android image 35',
      'system-images;android-36;google_apis;x86_64 | 7 | Android image 36',
    ].join('\n')

    expect(sdkPackagesToRemove(packages)).toEqual([
      'build-tools;34.0.0',
      'cmake;3.31.5',
      'ndk;27.3.13750724',
      'platforms;android-35',
      'system-images;android-35;google_apis;x86_64',
    ])

    expect(capacityPlan(AVD_USERDATA_BYTES_REQUIRED + AVD_HOST_HEADROOM_BYTES).sufficient).toBe(true)
    expect(capacityPlan(AVD_USERDATA_BYTES_REQUIRED + AVD_HOST_HEADROOM_BYTES - 1).sufficient).toBe(false)
    expect(AVD_USERDATA_BYTES_REQUIRED).toBe(7_730_941_133)
    expect(AVD_USERDATA_BYTES_REQUIRED + AVD_HOST_HEADROOM_BYTES).toBe(8_267_812_045)
    expect(capacityPlan(7_909_670_912).sufficient).toBe(false)
    expect(capacityPlan(8_267_812_044).sufficient).toBe(false)
    expect(capacityPlan(8_267_812_045).sufficient).toBe(true)
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
