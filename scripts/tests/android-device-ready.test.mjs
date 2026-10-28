import { describe, expect, it } from 'vitest'
import { parseDeviceSample, waitForDeviceReady } from '../android-device-ready.mjs'

function wire({
  step = 0,
  idle = 90,
  boot = '1',
  animation = 'stopped',
  system = '100',
  ui = '200',
  config = 'en-rUS-w412dp-port',
  processes = ['artd', 'com.android.systemui'],
} = {}) {
  return `__CAIRN_BOOT__\n${boot}\n__CAIRN_ANIMATION__\n${animation}\n__CAIRN_SYSTEM__\n${system}\n__CAIRN_UI__\n${ui}\n__CAIRN_CPU__\ncpu ${1000 + step * (100 - idle)} 0 0 ${1000 + step * idle} 0 0 0 0 123 456\n__CAIRN_CONFIG__\nconfig: ${config}\nabi: x86_64\n__CAIRN_PROCESSES__\nNAME\n${processes.join('\n')}\n`
}

function loop(sequence, options = {}) {
  let time = 0
  let count = 0
  const samples = []
  const promise = waitForDeviceReady({
    adb: async () => {
      const value = sequence(Math.min(count++, 100), time)
      samples.push(value)
      if (value instanceof Error)
        throw value
      return { stdout: value }
    },
    now: () => time,
    sleep: async (ms) => { time += ms },
    timeoutMs: 30000,
    log: () => {},
    ...options,
  })
  return { promise, samples, time: () => time }
}

describe('device startup readiness', () => {
  it('parses Android samples without double-counting guest CPU or waiting for the idle artd daemon', () => {
    const sample = parseDeviceSample(wire())
    expect(sample.cpu).toEqual({ total: 2000, idle: 1000 })
    expect(sample.compilers).toEqual([])
    expect(sample.config).toBe('en-rUS-w412dp-port')
    expect(parseDeviceSample(wire({ processes: ['/apex/com.android.art/bin/dex2oat64', 'profman'] })).compilers).toHaveLength(2)
  })

  it('waits for a whole quiet window after the boot flag, dexopt, CPU burst and configuration changes', async () => {
    const run = loop((step) => {
      if (step < 2)
        return wire({ step, boot: '', animation: 'running' })
      if (step < 4)
        return wire({ step, processes: ['dex2oat64'] })
      if (step < 6)
        return wire({ step, idle: 10 })
      return wire({ step, config: step < 8 ? 'en-rUS-port' : 'en-rUS-land' })
    })
    await run.promise
    expect(run.time()).toBeGreaterThanOrEqual(28000)
    expect(run.samples.length).toBeGreaterThan(10)
  })

  it('restarts the quiet window when System UI restarts or ADB stops responding', async () => {
    const run = loop((step) => {
      if (step === 3)
        return new Error('device offline')
      return wire({ step, ui: step < 6 ? '200' : '300' })
    })
    await run.promise
    expect(run.time()).toBe(24000)
  })

  it.each([
    { boot: '' },
    { animation: 'running' },
    { system: '' },
    { ui: '' },
    { idle: 10 },
    { processes: ['dex2oat32'] },
  ])('times out instead of accepting an unhealthy device: %j', async (condition) => {
    const run = loop(step => wire({ step, ...condition }), { timeoutMs: 8000 })
    await expect(run.promise).rejects.toThrow('instrumentation was not started')
    expect(run.time()).toBe(8000)
  })

  it('keeps ADB timeouts positive integers near a fractional deadline', async () => {
    let time = 0
    const timeouts = []
    const ready = waitForDeviceReady({
      adb: async (_args, { timeout }) => {
        timeouts.push(timeout)
        time += 0.25
        return { stdout: wire({ step: timeouts.length, boot: '' }) }
      },
      now: () => time,
      sleep: async (ms) => { time += ms },
      timeoutMs: 7.25,
      intervalMs: 2,
      log: () => {},
    })

    await expect(ready).rejects.toThrow('Boot is not complete')
    expect(timeouts.every(timeout => Number.isInteger(timeout) && timeout > 0)).toBe(true)
    expect(timeouts).toContain(1)
    expect(time).toBe(7.25)
  })

  it('fails closed on partial or malformed ADB output and CPU counters that stop advancing', async () => {
    for (const response of ['', '__CAIRN_BOOT__\n1\n', wire().replace('cpu 1000', 'cpu nope'), wire()]) {
      const run = loop(() => response, { timeoutMs: 6000 })
      await expect(run.promise).rejects.toThrow('Android did not settle')
    }
  })
})
