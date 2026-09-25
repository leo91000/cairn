import { mkdir, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { buildApp } from './legacy/server/app.ts'

async function main() {
  const port = Number(process.env.TEST_PORT || 4321)
  const root = path.join(os.tmpdir(), `leo-manager-browser-${port}`)
  await rm(root, { recursive: true, force: true })
  await Promise.all(
    ['home', 'project'].map(name =>
      mkdir(path.join(root, name), { recursive: true }),
    ),
  )
  const { app } = await buildApp({
    dataDir: path.join(root, 'data'),
    home: path.join(root, 'home'),
    workspaceRoots: [path.join(root, 'project')],
    setupToken: 'browser-test-setup',
    codexBin: path.resolve('tests/fixtures/codex.mjs'),
    ghBin: '/nonexistent/fixture-gh',
    publicUrl: `http://127.0.0.1:${port}`,
    logger: false,
  })
  await app.listen({ host: '127.0.0.1', port })
  for (const signal of ['SIGINT', 'SIGTERM'] as const)
    process.once(signal, () => void app.close())
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
