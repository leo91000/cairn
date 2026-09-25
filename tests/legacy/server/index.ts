import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { buildApp } from './app.ts'
import { token } from './auth.ts'
import { config } from './config.ts'

async function main() {
  const settings = config()
  await mkdir(settings.dataDir, { recursive: true, mode: 0o700 })
  if (!settings.setupToken) {
    const file = path.join(settings.dataDir, 'setup-token')
    settings.setupToken = await readFile(file, 'utf8').catch(async () => {
      const value = token()
      await writeFile(file, value, { mode: 0o600 })
      return value
    })
  }

  const { app } = await buildApp(settings)
  await app.listen({ host: settings.host, port: settings.port })
  app.log.info(
    `Leo Agent Manager: ${settings.publicUrl}. First-run setup token is in ${path.join(settings.dataDir, 'setup-token')} (or SETUP_TOKEN).`,
  )
  for (const signal of ['SIGINT', 'SIGTERM'] as const)
    process.once(signal, () => void app.close())
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
