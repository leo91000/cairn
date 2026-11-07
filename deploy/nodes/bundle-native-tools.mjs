import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  chmod,
  copyFile,
  mkdir,
  readdir,
  readFile,
  realpath,
  stat,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

// These are the admission limits of already installed native supervisors.
// A newer image must remain installable before its supervisor can be updated.
export function validateNativeToolBundleBudget({ extractedBytes, manifestBytes, compressedBytes }) {
  const sizes = [extractedBytes, manifestBytes, ...(compressedBytes === undefined ? [] : [compressedBytes])]
  if (sizes.some(size => !Number.isSafeInteger(size) || size <= 0))
    throw new Error('Invalid native tool bundle sizes.')
  if (extractedBytes > 1024 ** 3)
    throw new Error('Native tools exceed extracted size budget.')
  if (manifestBytes > 65536)
    throw new Error('Native tool manifest exceeds limit.')
  if (compressedBytes > 512 * 1024 ** 2)
    throw new Error('Native tools exceed download size budget.')
}

function nodeElf(bytes) {
  if (bytes.length < 64 || !bytes.subarray(0, 7).equals(Buffer.from([0x7F, 0x45, 0x4C, 0x46, 2, 1, 1]))
    || bytes.readUInt16LE(18) !== 62 || bytes.readUInt16LE(52) !== 64
    || bytes.readUInt16LE(54) !== 56 || bytes.readUInt16LE(58) !== 64) {
    throw new Error('Unsupported Node ELF layout.')
  }

  const headersOffset = Number(bytes.readBigUInt64LE(32))
  const headersSize = bytes.readUInt16LE(56) * 56
  const sectionsOffset = Number(bytes.readBigUInt64LE(40))
  const sectionsSize = bytes.readUInt16LE(60) * 64
  if (!Number.isSafeInteger(headersOffset) || headersOffset < 64 || !headersSize || headersOffset + headersSize > bytes.length
    || !Number.isSafeInteger(sectionsOffset) || sectionsOffset < 64 || !sectionsSize || sectionsOffset + sectionsSize !== bytes.length) {
    throw new Error('Unsupported Node ELF layout.')
  }

  const header = Buffer.from(bytes.subarray(0, 64))
  // Only the section table's location/count/string index may change. The loader
  // does not use them; all other ELF header and program-header bytes must match.
  header.fill(0, 40, 48)
  header.fill(0, 58, 64)
  const segments = []
  for (let offset = headersOffset; offset < headersOffset + headersSize; offset += 56) {
    if (bytes.readUInt32LE(offset) !== 1)
      continue
    const start = Number(bytes.readBigUInt64LE(offset + 8))
    const size = Number(bytes.readBigUInt64LE(offset + 32))
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(size) || start + size > bytes.length)
      throw new Error('Unsupported Node ELF layout.')
    segments.push(bytes.subarray(Math.max(start, 64), start + size))
  }

  if (!segments.length)
    throw new Error('Unsupported Node ELF layout.')
  return { header, programHeaders: bytes.subarray(headersOffset, headersOffset + headersSize), segments }
}

export function assertSameNodeRuntime(original, stripped) {
  const before = nodeElf(original)
  const after = nodeElf(stripped)
  if (!before.header.equals(after.header) || !before.programHeaders.equals(after.programHeaders)
    || before.segments.length !== after.segments.length
    || before.segments.some((segment, index) => !segment.equals(after.segments[index]))) {
    throw new Error('Node runtime image changed while removing debug data.')
  }
}

export async function copyNativeTool(name, source, destination) {
  await copyFile(source, destination)
  if (name === 'node') {
    // Official Node releases retain non-loaded DWARF data. Remove only that
    // data; never strip the coding engines or their embedded vendor payloads.
    const original = await readFile(source)
    nodeElf(original)
    execFileSync('/usr/bin/strip', ['--strip-debug', destination])
    assertSameNodeRuntime(original, await readFile(destination))
    if (!execFileSync(source, ['--version']).equals(execFileSync(destination, ['--version'])))
      throw new Error('Node version changed while removing debug data.')
  }

  await chmod(destination, 0o555)
  const bytes = await readFile(destination)
  return { path: `bin/${name}`, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') }
}

export async function createNativeToolsArchive(target, files) {
  const manifest = `${JSON.stringify({
    version: 1,
    os: 'linux',
    arch: 'x86_64',
    files,
  })}\n`
  const manifestBytes = Buffer.byteLength(manifest)
  const extractedBytes = files.reduce((total, entry) => total + entry.size, manifestBytes)
  validateNativeToolBundleBudget({ extractedBytes, manifestBytes })
  const archive = path.resolve(`${target}.zip`)
  try {
    await stat(archive)
    throw new Error('Native tool archive already exists.')
  }
  catch (error) {
    if (error.code !== 'ENOENT')
      throw error
  }

  await writeFile(`${target}/manifest.json`, manifest)
  execFileSync('/usr/bin/zip', ['-q', '-r', archive, 'bin', 'manifest.json'], { cwd: target })
  // Inspect the resulting ZIP, rather than trusting source sizes or compression
  // ratios. This also refuses stale/extra files from a reused output directory.
  const metadata = JSON.parse(execFileSync('/usr/bin/python3', ['-c', `
import json,pathlib,sys,zipfile
p=pathlib.Path(sys.argv[1])
with zipfile.ZipFile(p) as z:
 print(json.dumps(dict(extractedBytes=sum(x.file_size for x in z.infolist()),manifestBytes=z.getinfo('manifest.json').file_size,compressedBytes=p.stat().st_size,names=z.namelist())))
`, archive], { encoding: 'utf8' }))
  validateNativeToolBundleBudget(metadata)
  const expected = new Set(['bin/', 'manifest.json', ...files.map(entry => entry.path)])
  if (metadata.extractedBytes !== extractedBytes || metadata.manifestBytes !== manifestBytes
    || metadata.names.length !== expected.size || metadata.names.some(name => !expected.has(name))) {
    throw new Error('Native tool archive differs from its manifest.')
  }

  return archive
}

export async function selectPinnedBinaries(packageRoot, versions) {
  // Runs in the reviewed runtime image after its pinned coding engines are installed.
  // Ship their actual ELF executables, not npm launchers referring to /pnpm paths.
  const wanted = new Set(versions.keys())
  const binaries = new Map()
  const scores = new Map()
  const visited = new Set()
  const pending = [packageRoot]
  while (pending.length) {
    const directory = await realpath(pending.pop())
    if (visited.has(directory))
      continue
    visited.add(directory)
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const source = path.join(directory, entry.name)
      const metadata = await stat(source).catch(() => null)
      if (metadata?.isDirectory()) {
        pending.push(source)
        continue
      }

      if (!metadata?.isFile() || !wanted.has(entry.name))
        continue
      const bytes = await readFile(source)
      if (!bytes.subarray(0, 4).equals(Buffer.from([0x7F, 0x45, 0x4C, 0x46])) || bytes.readUInt16LE(18) !== 62)
        continue
      const resolved = await realpath(source)
      if (!resolved.includes(`@${versions.get(entry.name)}`))
        continue
      // Baseline builds support CPUs without newer instruction extensions;
      // prefer musl when the pinned package supplies it.
      const score = Number(resolved.includes('baseline')) * 4 + Number(resolved.includes('musl')) * 2
      const previous = binaries.get(entry.name)
      const previousScore = scores.get(entry.name) ?? -1
      if (score < previousScore)
        continue
      if (previous && score === previousScore && previous !== resolved && !bytes.equals(await readFile(previous)))
        throw new Error(`Ambiguous pinned native executable: ${entry.name}`)
      binaries.set(entry.name, resolved)
      scores.set(entry.name, score)
    }
  }

  for (const name of wanted) {
    if (!binaries.has(name))
      throw new Error(`Missing pinned native executable: ${name}`)
  }

  // Codex resolves this companion beside its executable. Select it from the
  // same pinned platform package, never from a different version or libc build.
  const companion = path.join(path.dirname(binaries.get('codex')), 'codex-code-mode-host')
  const bytes = await readFile(companion).catch(() => null)
  if (!bytes || bytes.length < 20 || !bytes.subarray(0, 4).equals(Buffer.from([0x7F, 0x45, 0x4C, 0x46])) || bytes.readUInt16LE(18) !== 62)
    throw new Error('Missing pinned native executable: codex-code-mode-host')
  binaries.set('codex-code-mode-host', companion)
  return binaries
}

// Native nodes receive exactly the engine versions this image's build resolved
// (APP_<NAME>_VERSION), never whatever another install left on PATH.
export function pinnedVersion(name, output, resolved) {
  const version = output.match(/\b\d+\.\d+\.\d+\b/)?.[0]
  if (!version)
    throw new Error(`Cannot identify pinned CLI version: ${name}`)
  if (resolved !== undefined && version !== resolved)
    throw new Error(`Installed ${name} ${version} is not the resolved ${resolved}`)
  return version
}

async function main() {
  const target = process.env.CAIRN_NATIVE_BUNDLE_OUTPUT || '/opt/cairn-native-tools'
  await mkdir(`${target}/bin`, { recursive: true })
  const versions = new Map(['codex', 'claude', 'opencode'].map((name) => {
    const output = execFileSync(name, ['--version'], { encoding: 'utf8' })
    return [name, pinnedVersion(name, output, process.env[`APP_${name.toUpperCase()}_VERSION`])]
  }))
  const binaries = await selectPinnedBinaries('/pnpm', versions)
  binaries.set('node', '/usr/local/bin/node')
  binaries.set('gh', '/usr/local/bin/gh')
  const files = []
  for (const [name, source] of binaries) {
    const destination = `${target}/bin/${name}`
    files.push(await copyNativeTool(name, source, destination))
  }

  await createNativeToolsArchive(target, files)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`${error.message}\n`)
    process.exitCode = 1
  })
}
