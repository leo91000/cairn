import { Buffer } from 'node:buffer'
import {
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { selectPinnedBinaries } from '../../deploy/nodes/bundle-native-tools.mjs'

const roots = []
const versions = new Map(['codex', 'claude', 'opencode'].map(name => [name, '1.2.3']))

afterEach(async () => {
  for (const root of roots.splice(0))
    await rm(root, { recursive: true, force: true })
})

async function executable(root, relative, machine = 62) {
  const destination = path.join(root, relative)
  await mkdir(path.dirname(destination), { recursive: true })
  const bytes = Buffer.alloc(64)
  bytes.set([0x7F, 0x45, 0x4C, 0x46])
  bytes.writeUInt16LE(machine, 18)
  await writeFile(destination, bytes)
  return destination
}

async function fixture() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-native-tools-'))
  roots.push(root)
  for (const name of versions.keys())
    await executable(root, `${name}@1.2.3-linux-x64-musl/bin/${name}`)
  return root
}

it('ships the code-mode companion beside the selected pinned Codex executable', async () => {
  const root = await fixture()
  const selected = await executable(root, 'codex@1.2.3-linux-x64-musl/bin/codex-code-mode-host')
  await executable(root, 'codex@9.9.9-linux-x64-musl/bin/codex-code-mode-host')
  await executable(root, 'codex@1.2.3-linux-x64-gnu/bin/codex')
  await executable(root, 'codex@1.2.3-linux-x64-gnu/bin/codex-code-mode-host')
  const binaries = await selectPinnedBinaries(root, versions)
  expect(binaries.get('codex-code-mode-host')).toBe(selected)
  expect(path.dirname(binaries.get('codex'))).toBe(path.dirname(selected))
})

it('fails the image build when only another package contains the companion', async () => {
  const root = await fixture()
  await executable(root, 'codex@9.9.9-linux-x64-musl/bin/codex-code-mode-host')
  await expect(selectPinnedBinaries(root, versions)).rejects.toThrow('Missing pinned native executable: codex-code-mode-host')
})

it.each([183, 0])('rejects a companion for a different ELF machine: %s', async (machine) => {
  const root = await fixture()
  await executable(root, 'codex@1.2.3-linux-x64-musl/bin/codex-code-mode-host', machine)
  await expect(selectPinnedBinaries(root, versions)).rejects.toThrow('Missing pinned native executable: codex-code-mode-host')
})
