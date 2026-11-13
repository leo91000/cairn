import { Buffer } from 'node:buffer'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
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
import process from 'node:process'
import { afterEach, expect, it } from 'vitest'
import { storageProbeCommand } from '../../tests/storage-probe-command.mjs'

const cleanups = []

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse())
    await cleanup()
})

async function fixture() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-storage-probe-'))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  const workspace = path.join(root, 'workspace')
  const inbox = path.join(root, 'guest/messages.json')
  await mkdir(workspace)
  await mkdir(path.dirname(inbox))
  const unused = Buffer.alloc(32 * 1024 * 1024, 73)
  await writeFile(path.join(workspace, 'saved'), 'journal survives')
  await writeFile(path.join(workspace, 'unused'), unused)
  await writeFile(path.join(workspace, 'unused.sha256'), createHash('sha256').update(unused).digest('hex'))
  const bin = path.join(root, 'bin')
  const syncCalls = path.join(root, 'sync-calls')
  const syncRelease = path.join(root, 'sync-release')
  const syncEmptyRead = path.join(root, 'sync-empty-read')
  await mkdir(bin)
  // Exercise the real probe's synchronous subprocess without flushing the host.
  // The test controls completion and failure through this private barrier.
  await writeFile(path.join(bin, 'sync'), `#!${process.execPath}
const fs = require('node:fs');
fs.appendFileSync(${JSON.stringify(syncCalls)}, JSON.stringify(process.argv.slice(2)) + '\\n');
// An asynchronous release can create the file before writing its exit code.
let release = '';
while (release === '') {
  if (fs.existsSync(${JSON.stringify(syncRelease)})) {
    release = fs.readFileSync(${JSON.stringify(syncRelease)}, 'utf8');
    if (release === '')
      fs.writeFileSync(${JSON.stringify(syncEmptyRead)}, '');
  }
  if (release === '')
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10);
}
process.exit(Number(release));
`, { mode: 0o755 })
  return {
    root,
    workspace,
    inbox,
    bin,
    syncCalls,
    syncRelease,
    syncEmptyRead,
  }
}

function probe({
  workspace,
  inbox,
  bin,
  syncRelease,
}) {
  const command = storageProbeCommand({
    workspace,
    inbox,
    first: false,
    benchmark: true,
  })
  const child = spawn(process.execPath, ['-e', command], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, PATH: bin },
  })
  const exited = once(child, 'close')
  let output = ''
  let errors = ''
  child.stdout.on('data', chunk => output += chunk)
  child.stderr.on('data', chunk => errors += chunk)
  cleanups.push(async () => {
    await writeFile(syncRelease, '0')
    if (child.exitCode === null && child.signalCode === null)
      child.kill('SIGKILL')
    await exited
  })
  return {
    child,
    exited,
    output: () => output,
    errors: () => errors,
    async waitFor(text) {
      await expect.poll(() => output.includes(text) || child.exitCode !== null, { timeout: 5000 }).toBe(true)
      expect(output, errors).toContain(text)
    },
  }
}

async function archive(root, messages) {
  const source = path.join(root, 'source')
  await mkdir(source)
  await writeFile(path.join(source, 'messages.json'), messages)
  return execFileSync('tar', ['--exclude=cairn-auth.sock', '--exclude=*.sock', '-C', source, '-cf', '-', '.'])
}

function importInbox(root) {
  const tar = spawn('tar', ['--no-same-owner', '-xf', '-', '-C', path.join(root, 'guest')], { stdio: ['pipe', 'ignore', 'pipe'] })
  const exited = once(tar, 'exit')
  cleanups.push(async () => {
    if (tar.exitCode === null && tar.signalCode === null)
      tar.kill('SIGKILL')
    await exited
  })
  return { tar, exited }
}

it('keeps the real benchmark alive through an absent inbox and measures after import', async () => {
  const files = await fixture()
  const guest = probe(files)
  // The real importer unlinks the previous file before recreating it. Keep the
  // missing-file interval open across the benchmark's 50 ms polling interval.
  await guest.waitFor('storage.tick')
  expect(guest.output()).not.toContain('storage.metrics')

  const bytes = await archive(files.root, '[{"text":"measure"}]')
  const { tar, exited } = importInbox(files.root)
  tar.stdin.end(bytes)
  expect((await exited)[0]).toBe(0)
  await guest.waitFor('storage.metrics ')
  expect(guest.child.exitCode).toBeNull()
})

it('waits for a complete measure message while a real tar import is streaming', async () => {
  const files = await fixture()
  await writeFile(files.inbox, '[{"text":"unrelated"}]')
  const guest = probe(files)
  await guest.waitFor('storage.tick')
  expect(guest.output()).not.toContain('storage.metrics')

  const messages = JSON.stringify([{ text: 'measure', padding: 'x'.repeat(128 * 1024) }])
  const bytes = await archive(files.root, messages)
  const { tar, exited } = importInbox(files.root)
  tar.stdin.write(bytes.subarray(0, 10240))
  await expect.poll(async () => (await stat(files.inbox)).size).toBeGreaterThan(2)
  const ticks = guest.output().split('storage.tick').length
  await expect.poll(() => guest.output().split('storage.tick').length > ticks || guest.child.exitCode !== null, { timeout: 5000 }).toBe(true)
  expect(guest.output(), guest.errors()).not.toContain('storage.metrics')
  expect(guest.child.exitCode, guest.errors()).toBeNull()

  tar.stdin.end(bytes.subarray(10240))
  expect((await exited)[0]).toBe(0)
  await guest.waitFor('storage.metrics ')
})

it('preserves hard filesystem failures in the benchmark', async () => {
  const files = await fixture()
  await mkdir(files.inbox)
  const guest = probe(files)
  expect((await guest.exited)[0]).toBe(1)
  expect(guest.errors()).toContain('EISDIR')
  expect(guest.output()).not.toContain('storage.metrics')
})

it('finishes successfully only after the explicit completion message', async () => {
  const files = await fixture()
  const guest = probe(files)
  await guest.waitFor('storage.ready')
  expect(guest.child.exitCode).toBeNull()
  await expect(stat(files.syncCalls)).rejects.toThrow('ENOENT')
  await writeFile(files.inbox, '[{"text":"finish"}]')
  await expect.poll(async () => readFile(files.syncCalls, 'utf8').catch(() => '')).toBe('[]\n')
  expect(guest.child.exitCode).toBeNull()
  expect(guest.output()).not.toContain('storage.finished')
  await writeFile(files.syncRelease, '0')
  expect((await guest.exited)[0], guest.errors()).toBe(0)
  expect(guest.output()).toContain('storage.finished')
  expect(guest.output()).not.toContain('storage.metrics')
  expect(await readFile(files.syncCalls, 'utf8')).toBe('[]\n')
})

it('fails completion when the synchronization subprocess fails', async () => {
  const files = await fixture()
  const guest = probe(files)
  await guest.waitFor('storage.ready')
  await writeFile(files.inbox, '[{"text":"finish"}]')
  await expect.poll(async () => readFile(files.syncCalls, 'utf8').catch(() => '')).toBe('[]\n')
  expect(guest.child.exitCode).toBeNull()
  // Hold open the create-before-write interval of the asynchronous release.
  await writeFile(files.syncRelease, '')
  await expect.poll(async () => stat(files.syncEmptyRead).then(() => true).catch(() => false)).toBe(true)
  await writeFile(files.syncRelease, '7')
  expect((await guest.exited)[0]).toBe(1)
  expect(guest.errors()).toContain('Command failed: sync')
  expect(guest.output()).not.toContain('storage.finished')
  expect(await readFile(files.syncCalls, 'utf8')).toBe('[]\n')
})
