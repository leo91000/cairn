import process from 'node:process'
import { dockerOutput } from './docker.ts'

async function main() {
  const id = process.argv[2]
  if (!/^[a-f0-9-]{36}$/.test(id))
    throw new Error('Invalid run identifier.')
  const url = `${process.env.RUNNER_URL}/runs/${id}`
  const headers = { authorization: `Bearer ${process.env.RUNNER_TOKEN}` }
  const remove = () => fetch(url, { method: 'DELETE', headers, signal: AbortSignal.timeout(2500) }).catch(() => {})
  process.on('SIGTERM', () => {
    void remove().finally(() => process.exit(143))
  })
  process.stdin.resume()
  try {
    const start = await fetch(url, { method: 'POST', headers, signal: AbortSignal.timeout(30000) })
    if (!start.ok)
      throw new Error(`Isolated runner could not start: ${await start.text()}`)
    const logs = await fetch(`${url}/logs`, { headers })
    if (!logs.ok || !logs.body)
      throw new Error('Could not read isolated runner output.')
    const output = (async () => {
      for await (const frame of dockerOutput(logs.body!))
        (frame.stderr ? process.stderr : process.stdout).write(frame.data)
    })()
    const wait = await fetch(`${url}/wait`, { method: 'POST', headers })
    if (!wait.ok)
      throw new Error('Could not wait for isolated runner.')
    const result = await wait.json() as { StatusCode: number }
    await output
    if (!Number.isInteger(result.StatusCode))
      throw new Error('Isolated runner did not return an exit status.')
    process.exitCode = result.StatusCode
  }
  finally {
    await remove()
  }
}

main().catch((error) => {
  console.error(error.message)
  process.exitCode = 1
})
