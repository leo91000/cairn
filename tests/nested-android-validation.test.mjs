import { describe, expect, it } from 'vitest'
import { evidenceStatus, waitForValidation } from '../scripts/nested-android-validation.mjs'

const config = {
  repository: 'leo91000/leo-agent-manager',
  commit: 'a'.repeat(40),
  digest: `sha256:${'b'.repeat(64)}`,
  validator: 'leo91000',
}
const evidence = {
  schema: 1,
  repository: config.repository,
  commit: config.commit,
  image: `ghcr.io/${config.repository}@${config.digest}`,
  cpuVendor: 'GenuineIntel',
  api: '34',
  system: 'aosp',
  results: [{ mode: 'first', status: 'passed' }, { mode: 'resume', status: 'passed' }],
}
const url = 'https://agents.example.test/evidence/1'
const status = { ...evidenceStatus(evidence, url), id: 12, creator: { login: config.validator } }

describe('external nested Android release qualification', () => {
  it('records only a complete Intel AOSP first boot and restart against an immutable image', () => {
    expect(status.state).toBe('success')
    expect(status.context).toContain(config.digest.slice(7))
    expect(status.target_url).toBe(url)
    for (const changed of [{ cpuVendor: 'AuthenticAMD' }, { system: 'google-apis' }, { api: '29' }, { commit: 'main' }, { image: 'ghcr.io/leo91000/leo-agent-manager:latest' }, { repository: 'elsewhere/repo' }, { results: [{ mode: 'first', status: 'passed' }] }, { results: [{ mode: 'first', status: 'passed' }, { mode: 'resume', status: 'failed' }] }])
      expect(() => evidenceStatus({ ...evidence, ...changed }, url)).toThrow()
    expect(() => evidenceStatus(evidence, 'http://insecure.test')).toThrow()
  })
  it('waits for this image and accepts evidence from the configured validator', async () => {
    let requests = 0
    const result = await waitForValidation(config, { statuses: async () => ++requests === 1 ? [{ ...status, context: 'nested-android/intel/old-image' }] : [status], sleep: async () => {} })
    expect(requests).toBe(2)
    expect(result).toEqual(status)
  })
  it('does not accept another author, a missing report, or an earlier success superseded by failure', async () => {
    for (const entries of [[{ ...status, creator: { login: 'github-actions[bot]' } }], [{ ...status, target_url: null }], [{ ...status, state: 'failure' }, status]])
      await expect(waitForValidation(config, { statuses: async () => entries })).rejects.toThrow()
  })
  it('fails closed at the deadline when no matching qualification exists', async () => {
    let time = 0
    await expect(waitForValidation(config, {
      statuses: async () => [],
      now: () => time,
      sleep: async () => {
        time += 1000
      },
      timeoutMs: 1000,
    })).rejects.toThrow('Timed out')
  })
})
