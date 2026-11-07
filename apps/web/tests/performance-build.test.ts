import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { webBuildId } from '../build-id'

const roots: string[] = []

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-web-build-'))
  roots.push(root)
  for (const filename of [
    'apps/web/src/main.ts',
    'apps/web/public/theme.js',
    'apps/web/index.html',
    'apps/web/beacon.html',
    'apps/web/vite.config.ts',
    'apps/web/build-id.ts',
    'packages/contracts/contracts.ts',
    'package.json',
    'pnpm-lock.yaml',
    'pnpm-workspace.yaml',
  ]) {
    mkdirSync(path.dirname(path.join(root, filename)), { recursive: true })
    writeFileSync(path.join(root, filename), filename)
  }

  return root
}

afterEach(() => {
  for (const root of roots.splice(0))
    rmSync(root, { recursive: true, force: true })
})

it('identifies the same shipped sources without Git or host paths', () => {
  const first = fixture()
  const second = fixture()
  expect(webBuildId(first)).toMatch(/^[a-f0-9]{64}$/)
  expect(webBuildId(second)).toBe(webBuildId(first))
  writeFileSync(path.join(first, 'README.md'), 'Unrelated server documentation')
  writeFileSync(path.join(first, '.env'), 'private local configuration')
  expect(webBuildId(first)).toBe(webBuildId(second))
})

it('changes for UI, unbundled bootstrap, contracts and dependency changes', () => {
  const root = fixture()
  let previous = webBuildId(root)
  for (const filename of ['apps/web/src/main.ts', 'apps/web/public/theme.js', 'packages/contracts/contracts.ts', 'pnpm-lock.yaml']) {
    writeFileSync(path.join(root, filename), `${filename}: changed`)
    const current = webBuildId(root)
    expect(current).not.toBe(previous)
    previous = current
  }
})
