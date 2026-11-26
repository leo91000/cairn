import { expect, it } from 'vitest'
import { prepareVmStorageCapacity } from '../ci/prepare-vm-storage-capacity.mjs'

const identity = `sha256:${'a'.repeat(64)}`
const valid = {
  GITHUB_ACTIONS: 'true',
  RUNNER_ENVIRONMENT: 'github-hosted',
  GITHUB_EVENT_NAME: 'workflow_dispatch',
  VM_STORAGE_QUALIFICATION: 'true',
  VALIDATION_ONLY: 'true',
  PUBLISH: 'false',
  SMOKE_IMAGE: 'cairn:test',
  VM_STORAGE_BUILDER: 'builder-owned-qualification',
}

function fixture(spaces, replacement = false) {
  const calls = []
  let inspections = 0
  return {
    calls,
    env: valid,
    statfs: (target) => {
      expect(target).toBe('/var/tmp')
      return { bavail: BigInt(spaces.shift()) * 1024n ** 3n, bsize: 1n }
    },
    run: (binary, args) => {
      expect(binary).toBe('docker')
      calls.push(args)
      if (args[0] === 'image')
        return replacement && inspections++ > 0 ? `sha256:${'b'.repeat(64)}` : identity
      return ''
    },
  }
}

it('refuses local, publishing, foreign-image and unowned-builder invocations before any command', () => {
  for (const change of [{ GITHUB_ACTIONS: 'false' }, { RUNNER_ENVIRONMENT: 'self-hosted' }, { PUBLISH: 'true' }, { SMOKE_IMAGE: 'other:image' }, { VM_STORAGE_BUILDER: 'default' }]) {
    const state = fixture([80, 80])
    expect(() => prepareVmStorageCapacity({ ...state, env: { ...valid, ...change } })).toThrow()
    expect(state.calls).toEqual([])
  }
})
it('preserves existing capacity without pruning any cache', () => {
  const state = fixture([80, 80])
  expect(prepareVmStorageCapacity(state).reclaimed).toBe(false)
  expect(state.calls.every(args => args[0] === 'image')).toBe(true)
})
it('reclaims only the explicitly selected disposable builder and preserves candidate identity', () => {
  const state = fixture([20, 60])
  expect(prepareVmStorageCapacity(state)).toMatchObject({ reclaimed: true, imageId: identity })
  expect(state.calls).toContainEqual(['buildx', 'prune', '--builder', valid.VM_STORAGE_BUILDER, '--all', '--force'])
  expect(state.calls).toHaveLength(3)
})
it('fails qualification when capacity remains missing instead of skipping the growth boundary', () => {
  const state = fixture([20, 30])
  expect(() => prepareVmStorageCapacity(state)).toThrow('available bytes')
  expect(state.calls.filter(args => args[0] === 'buildx')).toHaveLength(1)
})
it('rejects candidate replacement during reclamation', () => {
  expect(() => prepareVmStorageCapacity(fixture([20, 60], true))).toThrow('candidate image changed')
})
