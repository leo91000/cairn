import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
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
import { fileURLToPath } from 'node:url'
import { afterEach, expect, it } from 'vitest'
import * as bundler from '../../deploy/nodes/bundle-native-tools.mjs'
import { pinnedVersion, selectPinnedBinaries } from '../../deploy/nodes/bundle-native-tools.mjs'

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

async function temporaryRoot() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-native-tools-'))
  roots.push(root)
  return root
}

async function fixture() {
  const root = await temporaryRoot()
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

it('bundles only the engine versions resolved for this image', () => {
  expect(pinnedVersion('claude', '2.1.308 (Claude Code)\n', '2.1.308')).toBe('2.1.308')
  expect(pinnedVersion('codex', 'codex-cli 0.164.0\n', undefined)).toBe('0.164.0')
  expect(() => pinnedVersion('opencode', '1.19.0\n', '1.19.4')).toThrow('Installed opencode 1.19.0 is not the resolved 1.19.4')
  expect(() => pinnedVersion('claude', 'unknown\n', '2.1.308')).toThrow('Cannot identify pinned CLI version: claude')
})

it('refuses the released payload that existing native supervisors cannot extract', () => {
  expect(() => bundler.validateNativeToolBundleBudget({
    extractedBytes: 1075902086,
    manifestBytes: 750,
    compressedBytes: 422145445,
  })).toThrow('Native tools exceed extracted size budget.')
})

it('keeps the existing supervisor limits inclusive and rejects each next byte', () => {
  const limits = { extractedBytes: 1024 ** 3, manifestBytes: 65536, compressedBytes: 512 * 1024 ** 2 }
  expect(() => bundler.validateNativeToolBundleBudget(limits)).not.toThrow()
  expect(() => bundler.validateNativeToolBundleBudget({ ...limits, extractedBytes: limits.extractedBytes + 1 })).toThrow('Native tools exceed extracted size budget.')
  expect(() => bundler.validateNativeToolBundleBudget({ ...limits, manifestBytes: limits.manifestBytes + 1 })).toThrow('Native tool manifest exceeds limit.')
  expect(() => bundler.validateNativeToolBundleBudget({ ...limits, compressedBytes: limits.compressedBytes + 1 })).toThrow('Native tools exceed download size budget.')
})

it.each([0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1])('rejects invalid bundle size metadata: %s', (size) => {
  for (const field of ['extractedBytes', 'manifestBytes', 'compressedBytes']) {
    expect(() => bundler.validateNativeToolBundleBudget({
      extractedBytes: 1000,
      manifestBytes: 100,
      compressedBytes: 200,
      [field]: size,
    })).toThrow('Invalid native tool bundle sizes.')
  }
})

it('stops the publisher before writing a manifest for the incompatible v0.76 payload', async () => {
  const root = await temporaryRoot()
  const files = [
    ['gh', 41070752],
    ['codex-code-mode-host', 79209744],
    ['opencode', 214386864],
    ['claude', 293776752],
    ['codex', 320575976],
    ['node', 126881248],
  ].map(([name, size]) => ({ path: `bin/${name}`, size, sha256: 'a'.repeat(64) }))
  await expect(bundler.createNativeToolsArchive(root, files)).rejects.toThrow('Native tools exceed extracted size budget.')
  await expect(stat(path.join(root, 'manifest.json'))).rejects.toMatchObject({ code: 'ENOENT' })
  await expect(stat(`${root}.zip`)).rejects.toMatchObject({ code: 'ENOENT' })
})

async function archivedFixture(extraFile = false) {
  const root = await temporaryRoot()
  // Keep ZIP output inside the task-owned directory that afterEach already owns.
  const target = path.join(root, 'bundle')
  await mkdir(target)
  const files = []
  for (const name of ['codex', 'codex-code-mode-host', 'opencode', 'claude', 'node', 'gh']) {
    const source = await executable(target, `bin/${name}`)
    const bytes = await readFile(source)
    files.push({ path: `bin/${name}`, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') })
  }

  if (extraFile)
    await writeFile(path.join(target, 'bin/unexpected'), 'stale file')
  return { root, target, files }
}

it('publishes a ZIP admitted by the actual native supervisor with matching tool hashes', async () => {
  const { root, target, files } = await archivedFixture()
  const archive = await bundler.createNativeToolsArchive(target, files)
  const consumer = fileURLToPath(new URL('../../deploy/nodes/native-host.py', import.meta.url))
  execFileSync('python3', ['-c', `
import pathlib,runpy,sys
host=runpy.run_path(sys.argv[1],run_name='bundle_contract_test')
host['unpack_tools'](pathlib.Path(sys.argv[2]),pathlib.Path(sys.argv[3]))
`, consumer, archive, root])
  for (const file of files) {
    const bytes = await readFile(path.join(root, 'tools', file.path))
    expect(bytes.length).toBe(file.size)
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(file.sha256)
  }

  await expect(bundler.createNativeToolsArchive(target, files)).rejects.toThrow('Native tool archive already exists.')
})

it('refuses an archive containing stale files outside its manifest', async () => {
  const { target, files } = await archivedFixture(true)
  await expect(bundler.createNativeToolsArchive(target, files)).rejects.toThrow('Native tool archive differs from its manifest.')
})

function nodeElf(debugBytes = 192) {
  const sectionsOffset = 256 + debugBytes
  const bytes = Buffer.alloc(sectionsOffset + 64)
  bytes.set([0x7F, 0x45, 0x4C, 0x46, 2, 1, 1])
  bytes.writeUInt16LE(2, 16)
  bytes.writeUInt16LE(62, 18)
  bytes.writeBigUInt64LE(64n, 32)
  bytes.writeBigUInt64LE(BigInt(sectionsOffset), 40)
  bytes.writeUInt16LE(64, 52)
  bytes.writeUInt16LE(56, 54)
  bytes.writeUInt16LE(1, 56)
  bytes.writeUInt16LE(64, 58)
  bytes.writeUInt16LE(1, 60)
  bytes.writeUInt32LE(1, 64)
  bytes.writeUInt32LE(5, 68)
  bytes.writeBigUInt64LE(256n, 96)
  bytes.writeBigUInt64LE(256n, 104)
  bytes.fill(0xA5, 128, 256)
  return bytes
}

it('allows removal of non-loaded Node debug data while preserving the loader image', () => {
  expect(() => bundler.assertSameNodeRuntime(nodeElf(), nodeElf(0))).not.toThrow()
})

it('refuses a Node transformation that changes loaded code or program headers', () => {
  const changedCode = nodeElf(0)
  changedCode[200] ^= 1
  expect(() => bundler.assertSameNodeRuntime(nodeElf(), changedCode)).toThrow('Node runtime image changed')
  const changedHeader = nodeElf(0)
  changedHeader.writeUInt32LE(7, 68)
  expect(() => bundler.assertSameNodeRuntime(nodeElf(), changedHeader)).toThrow('Node runtime image changed')
})

it('refuses unknown Node ELF layouts and appended vendor payloads', () => {
  expect(() => bundler.assertSameNodeRuntime(Buffer.from('not ELF'), nodeElf(0))).toThrow('Unsupported Node ELF')
  expect(() => bundler.assertSameNodeRuntime(Buffer.concat([nodeElf(), Buffer.from('payload')]), nodeElf(0))).toThrow('Unsupported Node ELF')
})
