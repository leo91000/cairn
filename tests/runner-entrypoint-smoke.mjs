// Prove that a retained guest's obsolete runner cannot shadow the controller's
// current chat adapter. The guest OS, provider CLI and writable disk stay intact.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'

export async function entrypointSmoke({ root, api, storage }) {
  const runId = randomUUID()
  const workspace = `/data/runs/${runId}/workspace`
  const source = path.join(root, 'data/runs', runId, 'workspace')
  await mkdir(source, { recursive: true })
  const stale = `#!/bin/sh\nif [ "$1" = guest ]; then exec /usr/local/bin/cairn-retained "$@"; fi\necho OBSOLETE_RUNNER\nexit 97\n`
  const prepare = `
    const fs=require('node:fs'),cp=require('node:child_process');
    cp.execFileSync('sudo',['/usr/local/bin/node','-e',
      'require("node:fs").renameSync("/usr/local/bin/cairn","/usr/local/bin/cairn-retained");'+
      'require("node:fs").writeFileSync("/usr/local/bin/cairn",'+${JSON.stringify(JSON.stringify(stale))}+',{mode:0o755});']);
    fs.writeFileSync(${JSON.stringify(`${workspace}/sentinel`)},'preserved');
  `
  for (const retained of [false, true]) {
    const id = randomUUID()
    const plan = {
      id,
      runId,
      storage,
      expires: Date.now() + 180000,
      sandbox: 'yolo',
      cwd: workspace,
      resources: { cpu: 1, memoryMiB: 1024, diskMiB: 512 },
      imports: [{ source: workspace, target: workspace }],
      args: ['--version'],
      ...(retained ? {} : { command: ['/usr/local/bin/node', '-e', prepare] }),
    }
    await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
    await api(`/runs/${id}`, 'POST')
    const logs = (async () => {
      const response = await api(`/runs/${id}/logs`)
      let pending = ''
      let output = ''
      for await (const chunk of response.body) {
        pending += Buffer.from(chunk).toString('utf8')
        const lines = pending.split('\n')
        pending = lines.pop()
        for (const line of lines) {
          if (!line)
            continue
          const event = JSON.parse(line)
          if (event.type === 'output')
            output += Buffer.from(event.data, 'base64').toString('utf8')
        }
      }

      return output
    })()
    const result = await (await api(`/runs/${id}/wait`, 'POST')).json()
    const output = await logs
    assert.equal(result.StatusCode, 0, output)
    if (retained) {
      assert.match(output, /codex-cli /)
      assert.ok(!output.includes('OBSOLETE_RUNNER'))
    }

    await api(`/runs/${id}`, 'DELETE')
  }

  process.stdout.write(`${JSON.stringify({ mode: 'retained-current-entrypoint', status: 'passed' })}\n`)
}
