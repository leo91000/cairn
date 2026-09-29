import type { ChildProcessWithoutNullStreams } from 'node:child_process'
import { Buffer } from 'node:buffer'
import { spawn } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import http from 'node:http'
import path from 'node:path'
import process from 'node:process'

export async function runnerProvider(directory: string) {
  const attempts = new Map<string, {
    child: ChildProcessWithoutNullStreams
    frames: Buffer[]
    streams: http.ServerResponse[]
    done: Promise<number | null>
    closed: boolean
  }>()
  const control = { available: true }
  const server = http.createServer(async (request, response) => {
    const [, , id, action] = request.url!.split('/')
    try {
      if (request.method === 'DELETE') {
        if (!control.available) {
          response.writeHead(503).end('{}')
          return
        }

        const attempt = attempts.get(id)
        if (attempt) {
          attempt.child.kill('SIGTERM')
          await attempt.done
        }

        response.end('{}')
        return
      }

      if (request.method === 'POST' && !action) {
        const plan = JSON.parse(await readFile(path.join(directory, 'runner-plans', `${id}.json`), 'utf8'))
        const home = plan.mounts.find((mount: { target: string }) => mount.target === '/home/node').source
        const child = spawn(plan.chat ? process.execPath : path.resolve('tests/fixtures/codex.mjs'), plan.chat ? ['--import', import.meta.resolve('tsx'), path.resolve('tests/legacy/server/chat-process.ts'), path.resolve('tests/fixtures/codex.mjs')] : plan.args, { cwd: plan.cwd, env: { ...process.env, CODEX_HOME: path.join(home, '.codex') }, stdio: ['pipe', 'pipe', 'pipe'] })
        const done = new Promise<number | null>(resolve => child.once('close', resolve))
        const attempt = {
          child,
          frames: [] as Buffer[],
          streams: [] as http.ServerResponse[],
          done,
          closed: false,
        }
        attempts.set(id, attempt)
        for (const [index, stream] of [child.stdout, child.stderr].entries()) {
          stream.on('data', (chunk: Buffer) => {
            const header = Buffer.alloc(8)
            header[0] = index + 1
            header.writeUInt32BE(chunk.length, 4)
            const frame = Buffer.concat([header, chunk])
            attempt.frames.push(frame)
            for (const output of attempt.streams)
              output.write(frame)
          })
        }

        void done.then(() => {
          attempt.closed = true
          for (const output of attempt.streams)
            output.end()
        })
        child.stdin.end(plan.chat ? JSON.stringify({ ...plan.chat, inputDirectory: plan.mounts.find((mount: { target: string }) => mount.target === '/run/leo-chat').source }) : plan.prompt)
        response.end('{}')
        return
      }

      const attempt = attempts.get(id)!
      if (action === 'logs') {
        response.writeHead(200, { 'content-type': 'application/octet-stream' })
        response.flushHeaders()
        for (const frame of attempt.frames)
          response.write(frame)
        if (attempt.closed)
          response.end()
        else
          attempt.streams.push(response)
        return
      }

      response.end(JSON.stringify({ StatusCode: await attempt.done ?? 143 }))
    }
    catch {
      response.writeHead(500).end('{}')
    }
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    control,
    attempts,
    url: `http://127.0.0.1:${(server.address() as { port: number }).port}`,
    close: async () => {
      for (const attempt of attempts.values())
        attempt.child.kill('SIGTERM')
      await Promise.all([...attempts.values()].map(attempt => attempt.done))
      server.closeAllConnections()
      await new Promise<void>(resolve => server.close(() => resolve()))
    },
  }
}
