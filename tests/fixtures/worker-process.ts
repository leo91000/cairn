import process from 'node:process'
import { buildApp } from './legacy/server/app.ts'

process.once('message', async (config) => {
  const { app } = await buildApp({ ...config as object, workerEnabled: true })
  await app.ready()
  process.send?.('ready')
  process.once('SIGTERM', async () => {
    await app.close()
    process.disconnect()
  })
})
