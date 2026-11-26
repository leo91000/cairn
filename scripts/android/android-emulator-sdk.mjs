import { spawnSync } from 'node:child_process'
import { mkdirSync, statfsSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'

const BYTES_PER_MIB = 1024 ** 2

// The emulator prints MiB using an MB label. Its free-space gate in CI run
// 41514210202 was 7372.80 MiB for API 36 x86_64 Google APIs (including its 1.2x
// factor over the configured userdata partition).
export const AVD_USERDATA_BYTES_REQUIRED = Math.ceil(7_372.8 * BYTES_PER_MIB)
// Reserve extra host free space beyond the emulator gate; this does not reduce
// the AVD userdata partition.
export const AVD_HOST_HEADROOM_BYTES = 512 * BYTES_PER_MIB

const retainedPackages = new Set([
  'build-tools;37.0.0',
  'platforms;android-36',
  'system-images;android-36;google_apis;x86_64',
])

export function installedSdkPackageIds(output) {
  const packageIds = new Set()

  for (const line of output.split(/\r?\n/)) {
    const match = /^\s*([^\s|]+)\s*\|/.exec(line)
    if (match?.[1].includes(';'))
      packageIds.add(match[1])
  }

  return [...packageIds]
}

export function sdkPackagesToRemove(output) {
  return installedSdkPackageIds(output).filter((packageId) => {
    const separator = packageId.indexOf(';')
    const family = packageId.slice(0, separator)

    if (family === 'ndk' || family === 'cmake')
      return true

    if (family !== 'build-tools' && family !== 'platforms' && family !== 'system-images')
      return false

    return !retainedPackages.has(packageId)
  })
}

export function capacityPlan(availableBytes) {
  const requiredBytes = AVD_USERDATA_BYTES_REQUIRED + AVD_HOST_HEADROOM_BYTES

  return {
    availableBytes,
    userdataBytesRequired: AVD_USERDATA_BYTES_REQUIRED,
    hostHeadroomBytes: AVD_HOST_HEADROOM_BYTES,
    requiredBytes,
    remainingHeadroomBytes: availableBytes - requiredBytes,
    sufficient: availableBytes >= requiredBytes,
  }
}

function availableBytesAt(directory) {
  const filesystem = statfsSync(directory)
  return filesystem.bavail * filesystem.bsize
}

function runSdkManager(args, { captureOutput = false } = {}) {
  const result = spawnSync('sdkmanager', args, {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    stdio: captureOutput ? ['ignore', 'pipe', 'pipe'] : 'inherit',
  })

  if (result.error || result.status !== 0)
    throw new Error('CI_ANDROID_SDK_PRUNE_FAILED')

  return result.stdout ?? ''
}

function pruneUnusedSdkPackages() {
  const sdkHome = process.env.ANDROID_HOME || process.env.ANDROID_SDK_ROOT
  if (!sdkHome)
    throw new Error('CI_ANDROID_SDK_HOME_MISSING')

  const freeBeforeBytes = availableBytesAt(sdkHome)
  const installedPackages = runSdkManager(['--list_installed'], { captureOutput: true })
  const packages = sdkPackagesToRemove(installedPackages)

  if (packages.length > 0)
    runSdkManager(['--uninstall', ...packages])

  const freeAfterBytes = availableBytesAt(sdkHome)
  console.log(JSON.stringify({
    event: 'CI_ANDROID_SDK_PRUNE',
    packages,
    freeBeforeBytes,
    freeAfterBytes,
    reclaimedBytes: freeAfterBytes - freeBeforeBytes,
  }))
}

function checkAvdHeadroom() {
  const avdHome = process.env.ANDROID_AVD_HOME || path.join(os.homedir(), '.android', 'avd')
  mkdirSync(avdHome, { recursive: true })

  const plan = capacityPlan(availableBytesAt(avdHome))
  console.log(JSON.stringify({ event: 'CI_ANDROID_AVD_CAPACITY', ...plan }))

  if (!plan.sufficient) {
    const availableMib = (plan.availableBytes / BYTES_PER_MIB).toFixed(2)
    const requiredMib = (plan.userdataBytesRequired / BYTES_PER_MIB).toFixed(2)
    throw new Error(`CI_ANDROID_AVD_DISK_CAPACITY: ${availableMib} MiB free; userdata requires ${requiredMib} MiB plus 512 MiB host headroom`)
  }
}

function main(mode) {
  if (mode === 'prune') {
    pruneUnusedSdkPackages()
    return
  }

  if (mode === 'preflight') {
    checkAvdHeadroom()
    return
  }

  throw new Error('CI_ANDROID_SDK_MODE_INVALID')
}

if (import.meta.main) {
  try {
    main(process.argv[2])
  }
  catch (error) {
    console.error(error instanceof Error ? error.message : 'CI_ANDROID_SDK_OPERATION_FAILED')
    process.exitCode = 1
  }
}
