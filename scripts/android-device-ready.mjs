import { writeFile } from 'node:fs/promises'
import { performance } from 'node:perf_hooks'
import { setTimeout as sleep } from 'node:timers/promises'

// One shell round trip also verifies that ActivityManager responds. No settings,
// packages or animations are changed while deciding whether Android is ready.
export const deviceSampleCommand = `
printf '__CAIRN_BOOT__\\n'
getprop sys.boot_completed
printf '__CAIRN_ANIMATION__\\n'
getprop init.svc.bootanim
printf '__CAIRN_SYSTEM__\\n'
pidof system_server || true
printf '__CAIRN_UI__\\n'
pidof com.android.systemui || true
printf '__CAIRN_CPU__\\n'
head -n 1 /proc/stat
printf '__CAIRN_CONFIG__\\n'
am get-config
printf '__CAIRN_PROCESSES__\\n'
ps -A -o NAME
`

export function parseDeviceSample(stdout) {
  const parts = stdout.replace(/\r/g, '').split(/^__CAIRN_(\w+)__\n/m)
  const sections = {}
  for (let index = 1; index < parts.length; index += 2)
    sections[parts[index]] = parts[index + 1].trim()
  for (const name of ['BOOT', 'ANIMATION', 'SYSTEM', 'UI', 'CPU', 'CONFIG', 'PROCESSES']) {
    if (!Object.hasOwn(sections, name))
      throw new Error(`Missing device readiness section: ${name}`)
  }

  // guest/guest_nice are already included in user/nice. Count iowait as busy:
  // disk saturation during dexopt can starve app startup even without CPU work.
  const counters = sections.CPU.split(/\s+/)
  const ticks = counters.slice(1, 9).map(Number)
  const config = /^config: (.+)$/m.exec(sections.CONFIG)?.[1]
  const validCpu = counters[0] === 'cpu' && ticks.length === 8 && ticks.every(value => Number.isFinite(value) && value >= 0)
  const processHeader = /^NAME\s*\n/.test(`${sections.PROCESSES}\n`)
  if (!validCpu || !config || !processHeader)
    throw new Error('Malformed device CPU, configuration or process sample')

  const processes = sections.PROCESSES.split('\n').map(line => line.trim())
  const compilers = processes.filter(name => /(?:^|\/)(?:dex2oat(?:32|64)?|odrefresh|profman)$/.test(name))

  return {
    boot: sections.BOOT,
    animation: sections.ANIMATION,
    system: sections.SYSTEM,
    ui: sections.UI,
    config,
    cpu: { total: ticks.reduce((sum, value) => sum + value, 0), idle: ticks[3] },
    compilers,
  }
}

function busyFraction(previous, current) {
  if (!previous)
    return null
  const total = current.total - previous.total
  const idle = current.idle - previous.idle
  return total > 0 && idle >= 0 && idle <= total ? 1 - idle / total : null
}

/** A boot flag is insufficient: require a continuous quiet window before instrumentation. */
export async function waitForDeviceReady({
  adb,
  output,
  now = () => performance.now(),
  sleep: pause = sleep,
  timeoutMs = 180000,
  quietMs = 10000,
  intervalMs = 2000,
  maxBusy = 0.35,
  log = console.log,
}) {
  const started = now()
  const samples = []
  let previous = null
  let quietSince = null
  let lastReason = 'No sample'
  let ready = false
  try {
    while (now() - started < timeoutMs) {
      try {
        const { stdout } = await adb(['shell', deviceSampleCommand], { timeout: Math.min(10000, timeoutMs - (now() - started)) })
        const sample = parseDeviceSample(stdout)
        const busy = busyFraction(previous?.cpu, sample.cpu)
        const sameProcesses = previous && sample.system === previous.system && sample.ui === previous.ui
        const sameConfiguration = previous?.config === sample.config
        const reasons = []
        if (sample.boot !== '1' || !['', 'stopped'].includes(sample.animation))
          reasons.push('Boot is not complete')
        if (!/^\d+$/.test(sample.system) || !/^\d+$/.test(sample.ui))
          reasons.push('System server or System UI is missing')
        if (!sameProcesses)
          reasons.push('System processes changed')
        if (!sameConfiguration)
          reasons.push('Configuration changed')
        if (sample.compilers.length)
          reasons.push(`Compilation active: ${sample.compilers.join(', ')}`)
        if (busy === null || busy > maxBusy)
          reasons.push(busy === null ? 'CPU baseline is not established' : `CPU ${(busy * 100).toFixed(0)}% busy`)

        previous = sample
        quietSince = reasons.length ? null : quietSince ?? now()
        lastReason = reasons.join('; ') || 'Quiet'
        samples.push({
          elapsedMs: now() - started,
          ...sample,
          busy,
          reason: lastReason,
        })
        if (quietSince !== null && now() - quietSince >= quietMs) {
          ready = true
          log(`Android settled after ${((now() - started) / 1000).toFixed(1)}s (${quietMs / 1000}s quiet, CPU <= ${maxBusy * 100}%, stable configuration and System UI, no dexopt)`)
          return
        }
      }
      catch (error) {
        previous = null
        quietSince = null
        lastReason = error.message
        samples.push({ elapsedMs: now() - started, error: lastReason })
      }

      await pause(Math.min(intervalMs, Math.max(0, timeoutMs - (now() - started))))
    }

    throw new Error(`Android did not settle within ${timeoutMs / 1000}s: ${lastReason}. See readiness.json; instrumentation was not started.`)
  }
  finally {
    if (output) {
      await writeFile(output, `${JSON.stringify({
        ready,
        elapsedMs: now() - started,
        quietMs,
        maxBusy,
        samples,
      }, null, 2)}\n`)
    }
  }
}
