import { mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { expect, it, vi } from 'vitest'
import { RunnerLifecycle } from './legacy/server/runner-lifecycle.ts'

it('fences in-flight starts, rejects delayed starts across broker restarts, and propagates removal failures', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-runner-lifecycle-'))
  let release!: () => void
  const gate = new Promise<void>(resolve => release = resolve)
  const create = vi.fn(() => gate)
  const remove = vi.fn(async () => {})
  try {
    const lifecycle = new RunnerLifecycle(directory, create, remove)
    const started = lifecycle.start('attempt-one')
    await expect.poll(() => create.mock.calls.length).toBe(1)
    const stopped = lifecycle.stop('attempt-one')
    expect(remove).not.toHaveBeenCalled()
    release()
    await Promise.all([started, stopped])
    expect(remove).toHaveBeenCalledOnce()
    const restarted = new RunnerLifecycle(directory, create, remove)
    await expect(restarted.start('attempt-one')).rejects.toThrow(/already stopped/)
    await restarted.start('attempt-two')
    expect(create).toHaveBeenCalledTimes(2)
    remove.mockRejectedValueOnce(new Error('Docker unavailable'))
    await expect(restarted.stop('attempt-two')).rejects.toThrow('Docker unavailable')
    await restarted.stop('attempt-two')
  }
  finally {
    release()
    await rm(directory, { recursive: true, force: true })
  }
})
