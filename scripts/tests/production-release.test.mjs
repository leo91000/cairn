import { Buffer } from 'node:buffer'
import { readFileSync } from 'node:fs'
import {
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { parse } from 'yaml'
import { verifyInstallation } from '../production-release.mjs'

const image = `ghcr.io/owner/cairn@sha256:${'b'.repeat(64)}`
const now = 1000000

describe('production release', () => {
  it('waits through an updater replacement and requires the manager, runner, and execution health', async () => {
    const ssh = vi.fn().mockResolvedValueOnce(Buffer.from('{"ready":false}')).mockResolvedValueOnce(Buffer.from('{"ready":true,"manager":true,"runner":true,"health":true}'))
    const sleep = vi.fn()
    expect(await verifyInstallation({ installationImage: image }, { ssh, sleep, now: () => now })).toMatchObject({ ready: true })
    expect(sleep).toHaveBeenCalledOnce()
  })

  it('does not accept an unhealthy or mismatched installation after replacement', async () => {
    for (const missing of ['ready', 'manager', 'runner', 'health']) {
      let clock = 0
      const result = {
        ready: true,
        manager: true,
        runner: true,
        health: true,
        [missing]: false,
      }
      await expect(verifyInstallation({ installationImage: image }, {
        ssh: vi.fn().mockResolvedValue(Buffer.from(JSON.stringify(result))),
        sleep: vi.fn(),
        now: () => clock++,
        timeoutMs: 2,
      })).rejects.toThrow('did not become healthy')
    }
  })

  it('checks installation readiness after deployment', () => {
    const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
    const steps = workflow.jobs.deploy.steps
    const upload = steps.findIndex(step => step.uses?.startsWith('actions/upload-artifact@'))
    const deploy = steps.findIndex(step => step.run === 'node scripts/deploy-coolify.mjs')
    const verify = steps.findIndex(step => step.run === 'node scripts/production-release.mjs verify')
    expect(upload).toBe(-1)
    expect(deploy).toBeGreaterThan(-1)
    expect(verify).toBeGreaterThan(deploy)
    expect(steps[deploy].env.COOLIFY_TOKEN).toContain('secrets.CAIRN_RELEASE_COOLIFY_TOKEN')
    expect(workflow.jobs.deploy.if).toContain('startsWith(github.ref, \'refs/tags/v\')')
    expect(workflow.jobs.deploy.concurrency['cancel-in-progress']).toBe(false)
    const quality = workflow.jobs.backend.steps.map(step => step.run ?? '').join('\n')
    expect(quality).toContain('set -o pipefail')
    expect(quality).toContain('node scripts/test-backend.mjs --nextest --test-threads 8 --no-fail-fast --color never --status-level all 2>&1 | tee backend-tests.log')
  })
})
