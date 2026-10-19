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

async function main() {
  // Runs in the reviewed runtime image after its pinned coding engines are installed.
  // Ship their actual ELF executables, not npm launchers referring to /pnpm paths.
  const target = process.env.CAIRN_NATIVE_BUNDLE_OUTPUT || '/opt/cairn-native-tools'
  await mkdir(`${target}/bin`, { recursive: true })
  const wanted = new Set(['codex', 'claude', 'opencode'])
  const binaries = new Map()
  const versions = new Map([...wanted].map((name) => {
    const version = execFileSync(name, ['--version'], { encoding: 'utf8' }).match(/\b\d+\.\d+\.\d+\b/)?.[0]
    if (!version)
      throw new Error(`Cannot identify pinned CLI version: ${name}`)
    return [name, version]
  }))
  const scores = new Map()
  const visited = new Set()
  const pending = ['/pnpm']
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

  binaries.set('node', '/usr/local/bin/node')
  binaries.set('gh', '/usr/local/bin/gh')
  const files = []
  for (const [name, source] of binaries) {
    const destination = `${target}/bin/${name}`
    await copyFile(source, destination)
    await chmod(destination, 0o555)
    const bytes = await readFile(destination)
    files.push({ path: `bin/${name}`, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') })
  }

  await writeFile(`${target}/manifest.json`, `${JSON.stringify({
    version: 1,
    os: 'linux',
    arch: 'x86_64',
    files,
  })}\n`)
  execFileSync('/usr/bin/zip', ['-q', '-r', `${target}.zip`, 'bin', 'manifest.json'], { cwd: target })
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`)
  process.exitCode = 1
})
