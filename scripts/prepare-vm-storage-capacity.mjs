import { execFileSync } from 'node:child_process'
import { statfsSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

const REQUIRED_BYTES = 48n * 1024n ** 3n

// The 33 GiB writable-image promise must coexist with extracted guest assets,
// immutable images and reserve. Reclaim only this disposable job's builder cache.
export function prepareVmStorageCapacity({ env = process.env, run = execFileSync, statfs = statfsSync } = {}) {
  if (env.GITHUB_ACTIONS !== 'true' || env.RUNNER_ENVIRONMENT !== 'github-hosted'
    || env.GITHUB_EVENT_NAME !== 'workflow_dispatch' || env.VM_STORAGE_QUALIFICATION !== 'true'
    || env.VALIDATION_ONLY !== 'true' || env.PUBLISH !== 'false' || env.SMOKE_IMAGE !== 'cairn:test') {
    throw new Error('VM storage capacity preparation requires the local-only disposable qualification job.')
  }

  const builder = env.VM_STORAGE_BUILDER || ''
  if (!/^builder-[\w.-]{1,100}$/.test(builder))
    throw new Error('VM storage qualification requires its explicitly created builder identity.')

  const command = args => run('docker', args, { encoding: 'utf8', timeout: 120000, maxBuffer: 1024 * 1024 }).trim()
  const inspect = () => command(['image', 'inspect', '--format', '{{.Id}}', 'cairn:test'])
  const imageId = inspect()
  if (!/^sha256:[a-f0-9]{64}$/.test(imageId))
    throw new Error('Local candidate image identity is unavailable.')

  const free = () => {
    const value = statfs('/var/tmp', { bigint: true })
    return value.bavail * value.bsize
  }

  const before = free()
  let reclaimed = false
  if (before < REQUIRED_BYTES) {
    command(['buildx', 'prune', '--builder', builder, '--all', '--force'])
    reclaimed = true
  }

  if (inspect() !== imageId)
    throw new Error('The local candidate image changed during builder-cache reclamation.')
  const after = free()
  if (after < REQUIRED_BYTES)
    throw new Error(`VM storage qualification needs ${REQUIRED_BYTES} available bytes; ${after} remain after owned builder-cache reclamation.`)
  return {
    beforeBytes: before.toString(),
    afterBytes: after.toString(),
    requiredBytes: REQUIRED_BYTES.toString(),
    reclaimed,
    imageId,
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href)
  process.stdout.write(`${JSON.stringify(prepareVmStorageCapacity())}\n`)
