import { createHash } from 'node:crypto'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'

// These inputs are also present in the Docker web-build stage. No Git checkout,
// server version, environment file or per-browser identifier enters this digest.
const inputs = [
  'apps/web/src',
  'apps/web/public',
  'apps/web/index.html',
  'apps/web/beacon.html',
  'apps/web/vite.config.ts',
  'apps/web/build-id.ts',
  'packages/contracts',
  'package.json',
  'pnpm-lock.yaml',
  'pnpm-workspace.yaml',
]

export function webBuildId(root: string) {
  const digest = createHash('sha256')
    .update('cairn-web-build-v1\0')
    .update(`${process.versions.node}\0${process.platform}\0${process.arch}\0`)

  function include(relative: string) {
    const filename = path.join(root, relative)
    if (statSync(filename).isDirectory()) {
      const names = readdirSync(filename).filter(name => !name.startsWith('.') && name !== 'node_modules').sort()
      for (const name of names)
        include(`${relative}/${name}`)
      return
    }

    const content = readFileSync(filename)
    digest.update(`${relative}\0${content.length}\0`).update(content)
  }

  for (const input of [...inputs].sort())
    include(input)
  return digest.digest('hex')
}
