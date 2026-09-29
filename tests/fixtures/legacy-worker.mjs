import { readFile } from 'node:fs/promises'
import process from 'node:process'
import { buildApp } from '../legacy/server/app.ts'

async function main() {
  const config = JSON.parse(await readFile(process.env.LEO_CONFIG, 'utf8'))
  const { app } = await buildApp({ ...config, workerEnabled: true })
  await app.listen({ host: '127.0.0.1', port: 0 })
  process.stdout.write('Listening on legacy fixture\n')
  process.once('SIGTERM', async () => {
    await app.close()
  })
}

main().catch(() => {
  process.exitCode = 1
})
