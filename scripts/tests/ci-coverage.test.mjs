import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const quality = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const android = parse(readFileSync(new URL('../../.github/workflows/android.yaml', import.meta.url), 'utf8'))

describe('parallel CI coverage', () => {
  it('keeps every split validation in the image publication gate', () => {
    for (const name of ['build', 'quality', 'rust-check', 'backend', 'node-checks', 'browser', 'network-bench', 'native-runtime', 'image', 'beacon-image']) {
      expect(quality.jobs.publish.needs).toContain(name)
      expect(quality.jobs.publish.if).toContain(`needs.${name}.result == 'success'`)
    }
  })

  it('refreshes one compiled cache per commit and restores compatible earlier builds', () => {
    const action = parse(readFileSync(new URL('../../.github/actions/cache-rust/action.yaml', import.meta.url), 'utf8'))
    const key = action.runs.steps.find(step => step.id === 'key')
    expect(key.env.CACHE_REVISION).toContain('github.sha')
    expect(key.run).toContain('primary=$CACHE_PREFIX-$CACHE_REVISION')
    expect(key.run).toContain('fallback=$CACHE_PREFIX-')
    const writer = action.runs.steps.find(step => step.uses?.startsWith('actions/cache@'))
    const reader = action.runs.steps.find(step => step.uses?.startsWith('actions/cache/restore@'))
    expect(writer.if).toBe('inputs.save == \'true\'')
    expect(reader.if).toBe('inputs.save != \'true\'')
    expect(writer.with).toEqual(reader.with)
    expect(writer.with['restore-keys']).toContain('steps.key.outputs.fallback')
    const owners = Object.entries(quality.jobs).filter(([, job]) => job.steps?.some(step => step.uses === './.github/actions/cache-rust' && step.with?.save === true))
    expect(owners.map(([name]) => name)).toEqual(['backend'])
  })

  it('runs every network topology exactly once on independent hosts', () => {
    const scenarios = quality.jobs['network-bench'].strategy.matrix.include.flatMap(group => group.scenarios.split(' '))
    expect(scenarios.sort()).toEqual(['same-lan', 'mdns-only-client', 'nat-client', 'nat-installation', 'nat-both', 'udp-blocked', 'symmetric-nat', 'symmetric-client', 'same-server', 'network-change', 'packet-loss', 'qualification'].sort())
    expect(quality.jobs['network-bench'].strategy['fail-fast']).toBe(false)
  })

  it('retains complete browser shard coverage and unique evidence names', () => {
    const groups = quality.jobs.browser.strategy.matrix.include
    expect(new Set(groups.map(group => group.group)).size).toBe(groups.length)
    for (const projects of ['journeys*', 'layout-webkit-*'])
      expect(groups.filter(group => group.projects === projects).map(group => group.shard).sort()).toEqual(['1/4', '2/4', '3/4', '4/4'])
    expect(groups.filter(group => group.projects === 'layout-chromium-*')).toHaveLength(1)
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
