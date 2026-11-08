import { execFileSync, spawnSync } from 'node:child_process'
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, it } from 'vitest'
import { parse } from 'yaml'

it('validates native reference tool preparation and private cache admission', () => {
  const script = fileURLToPath(new URL('./native_reference_tool_cache_test.py', import.meta.url))
  const output = execFileSync('python3', ['-B', script], { encoding: 'utf8', stdio: 'pipe' })
  expect(output).toContain('coldReferenceColdCandidate')
})

it('stops the actual reference CI shell before runtime when mandatory cache setup fails', () => {
  const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  const step = workflow.jobs['native-runtime'].steps.find(step => step.name === 'Qualify the exact reference on the current disposable host')
  const fixture = mkdtempSync(path.join(os.tmpdir(), 'cairn-reference-setup-test-'))
  try {
    const bin = path.join(fixture, 'bin')
    const called = path.join(fixture, 'setup-called')
    const runtime = path.join(fixture, 'runtime-started')
    mkdirSync(bin)
    mkdirSync(path.join(fixture, 'native-storage-evidence/prerequisite'), { recursive: true })
    for (const name of ['git', 'tar', 'chmod'])
      writeFileSync(path.join(bin, name), '#!/bin/sh\nexit 0\n', { mode: 0o700 })
    writeFileSync(path.join(bin, 'python3'), `#!/bin/sh
if test "$2" = scripts/native-reference-tool-cache.py; then
  : > "$SETUP_CALLED"
  exit 73
fi
exit 0
`, { mode: 0o700 })
    writeFileSync(path.join(bin, 'sudo'), '#!/bin/sh\n: > "$RUNTIME_STARTED"\nexit 0\n', { mode: 0o700 })
    // Match GitHub's unspecified Linux shell: bash -e, without implicit pipefail.
    // Every external setup/runtime boundary is synthetic; no git, native process
    // or host service is invoked by this exact workflow fragment.
    const result = spawnSync('/bin/bash', ['-e', '-c', step.run], {
      cwd: fixture,
      env: {
        PATH: `${bin}:/usr/bin:/bin`,
        RUNNER_TEMP: fixture,
        REFERENCE_SHA: 'b'.repeat(40),
        CI_DIAGNOSTIC_SOURCE: '',
        SETUP_CALLED: called,
        RUNTIME_STARTED: runtime,
      },
      encoding: 'utf8',
      timeout: 10000,
    })
    expect(readFileSync(called, 'utf8')).toBe('')
    expect(result.status, result.stderr).toBe(73)
    expect(() => readFileSync(runtime)).toThrow()
  }
  finally {
    rmSync(fixture, { recursive: true, force: true })
  }
})
