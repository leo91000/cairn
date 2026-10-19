import { execFileSync, spawn } from 'node:child_process'
import { once } from 'node:events'
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { finishRequested } from '../../tests/fixtures/exclusive-inbox.mjs'

const roots = []

afterEach(async () => {
  await Promise.all(roots.splice(0).map(root => rm(root, { recursive: true, force: true })))
})

async function fixture() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-exclusive-inbox-'))
  roots.push(root)
  return { root, inbox: path.join(root, 'messages.json') }
}

it('keeps polling when the inbox disappears during replacement', async () => {
  const { inbox } = await fixture()
  await writeFile(inbox, '[]')
  expect(finishRequested(inbox)).toBe(false)
  await rm(inbox)
  expect(finishRequested(inbox)).toBe(false)
  // Continued absence must never authorize successful completion.
  expect(finishRequested(inbox)).toBe(false)
  await writeFile(inbox, '[{"text":"finish"}]')
  expect(finishRequested(inbox)).toBe(true)
})

it('waits for a complete inbox while a real tar import is streaming', async () => {
  const { root, inbox } = await fixture()
  const source = path.join(root, 'source')
  await mkdir(source)
  await writeFile(inbox, '[]')
  const messages = JSON.stringify([{ text: 'finish', padding: 'x'.repeat(128 * 1024) }])
  await writeFile(path.join(source, 'messages.json'), messages)
  const archive = execFileSync('tar', ['-cf', '-', '-C', source, 'messages.json'])
  const tar = spawn('tar', ['--no-same-owner', '-xf', '-', '-C', root], { stdio: ['pipe', 'ignore', 'pipe'] })
  const exited = once(tar, 'exit')
  try {
    // Deliver one GNU tar record, leaving the regular file partly extracted.
    tar.stdin.write(archive.subarray(0, 10240))
    await expect.poll(async () => (await stat(inbox)).size).toBeGreaterThan(2)
    const partial = await readFile(inbox, 'utf8')
    expect(partial).toContain('finish')
    expect(partial.length).toBeLessThan(messages.length)
    expect(finishRequested(inbox)).toBe(false)

    tar.stdin.end(archive.subarray(10240))
    expect((await exited)[0]).toBe(0)
    expect(finishRequested(inbox)).toBe(true)
  }
  finally {
    if (tar.exitCode === null && tar.signalCode === null) {
      tar.kill('SIGKILL')
      await exited
    }
  }
})

it('requires the finish message and preserves hard filesystem failures', async () => {
  const { root, inbox } = await fixture()
  for (const content of ['[]', '', '[{"text":"finish"', '[{"text":"not finished"}]', '{"text":"finish"}', '[null]']) {
    await writeFile(inbox, content)
    expect(finishRequested(inbox)).toBe(false)
  }

  expect(() => finishRequested(root)).toThrow()
})
