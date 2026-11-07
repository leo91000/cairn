// Prove that a retained guest's obsolete runner cannot shadow the controller's
// current chat adapter. The guest OS, provider CLI and writable disk stay intact.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { copyFile, mkdir, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import path from 'node:path'
import process from 'node:process'
import { blockDigest } from './block-digest.mjs'

export async function entrypointSmoke({
  root,
  api,
  storage,
  origin,
  docker,
  image,
  networkName,
  until,
}) {
  const runId = randomUUID()
  const runRoot = `/data/runs/${runId}`
  const workspace = `${runRoot}/workspace`
  const source = path.join(root, 'data/runs', runId)
  const endpoint = 'http://93.184.216.4:8080'
  const peer = `cairn-entrypoint-peer-${runId}`
  for (const directory of ['workspace', 'home/.codex', 'chat-input', 'output'])
    await mkdir(path.join(source, directory), { recursive: true })
  await writeFile(path.join(source, 'chat-input/messages.json'), '[]')
  await copyFile(new URL('./fixtures/codex-response-server.cjs', import.meta.url), path.join(root, 'entrypoint-peer.cjs'))
  const home = path.join(source, 'home/.codex')
  await writeFile(path.join(home, 'config.toml'), `cli_auth_credentials_store = "file"\nchatgpt_base_url = "${endpoint}"\nmodel_provider = "fixture"\n[model_providers.fixture]\nname = "Fixture"\nbase_url = "${endpoint}"\nwire_api = "responses"\nrequires_openai_auth = false\n`)
  await writeFile(path.join(home, 'cairn-managed-auth'), '1')
  const broker = createServer(client => client.once('data', () => {
    const claims = Buffer.from(JSON.stringify({ 'sub': 'synthetic-entrypoint', 'exp': Math.floor(Date.now() / 1000) + 3600, 'https://api.openai.com/auth': { chatgpt_account_id: 'synthetic-entrypoint', chatgpt_plan_type: 'plus' } })).toString('base64url')
    client.end(`${JSON.stringify({ accessToken: `eyJhbGciOiJub25lIn0.${claims}.synthetic`, chatgptAccountId: 'synthetic-entrypoint', chatgptPlanType: 'plus' })}\n`)
  }))
  await new Promise(resolve => broker.listen(path.join(home, 'cairn-auth.sock'), resolve))
  const stale = `#!/bin/sh\nif [ "$1" = guest ]; then exec /usr/local/bin/cairn-retained "$@"; fi\necho OBSOLETE_RUNNER\nexit 97\n`
  const prepare = `
    const fs=require('node:fs'),cp=require('node:child_process');
    cp.execFileSync('sudo',['/usr/local/bin/node','-e',
      'require("node:fs").renameSync("/usr/local/bin/cairn","/usr/local/bin/cairn-retained");'+
      'require("node:fs").writeFileSync("/usr/local/bin/cairn",'+${JSON.stringify(JSON.stringify(stale))}+',{mode:0o755});']);
    fs.writeFileSync(${JSON.stringify(`${workspace}/sentinel`)},'preserved');
  `
  try {
    docker('run', '-d', '--name', peer, '--network', networkName, '--ip', '93.184.216.4', '-v', `${root}/entrypoint-peer.cjs:/peer.cjs:ro`, '--entrypoint', 'node', image, '/peer.cjs')
    await until(() => {
      try {
        return docker('exec', peer, 'node', '-e', 'fetch(\'http://127.0.0.1:8080/records\').then(r=>{if(!r.ok)process.exit(1)})') !== undefined
      }
      catch { return false }
    }, 10000)
    for (const retained of [false, true]) {
      const id = randomUUID()
      const plan = {
        id,
        runId,
        storage,
        expires: Date.now() + 180000,
        sandbox: 'yolo',
        cwd: workspace,
        resources: { cpu: 2, memoryMiB: 2048, diskMiB: 1024 },
        imports: [
          { source: workspace, target: workspace },
          { source: `${runRoot}/home`, target: '/home/node' },
          { source: `${runRoot}/output`, target: `${runRoot}/output` },
          { source: `${runRoot}/chat-input`, target: '/run/cairn-chat' },
        ],
        ...(retained
          ? {
              chat: {
                provider: 'codex',
                runId,
                attemptId: id,
                execution: { messageId: randomUUID(), text: 'Reply RETENTION_NATIVE_OK.', recovery: false },
                instructions: 'Reply with the requested marker.',
                inputDirectory: '/run/cairn-chat',
                output: `${runRoot}/output/result.md`,
                cwd: workspace,
                model: 'gpt-5.4',
                reasoning: 'low',
                sandbox: 'yolo',
                writableRoots: [workspace],
                codexConfig: {
                  chatgpt_base_url: endpoint,
                  model_provider: 'fixture',
                  model_providers: {
                    fixture: {
                      name: 'Fixture',
                      base_url: endpoint,
                      wire_api: 'responses',
                      requires_openai_auth: false,
                    },
                  },
                  mcp_servers: {},
                },
              },
            }
          : { command: ['/usr/local/bin/node', '-e', prepare] }),
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
        assert.match(output, /RETENTION_NATIVE_OK/)
        assert.ok(!output.includes('OBSOLETE_RUNNER'))
      }

      await api(`/runs/${id}`, 'DELETE')
    }

    // A chat can retain its paused VM until its journal is acknowledged. Finish
    // the fixture's publication before the independent slot-admission test.
    const snapshot = await (await api(`/disks/${runId}/snapshot`, 'POST', { consistency: 'filesystem' })).json()
    if (!snapshot.alreadyPublished) {
      for (const block of snapshot.manifest.blocks) {
        if (!block.hash)
          continue
        const bytes = Buffer.from(await (await api(`/snapshots/${snapshot.id}/${block.hash}`)).arrayBuffer())
        assert.equal(blockDigest(bytes, block.hash), block.hash)
        await writeFile(path.join(origin, block.hash), bytes)
      }

      await api(`/disks/${runId}/published`, 'POST', { generation: snapshot.manifest.generation, grantId: snapshot.grantId, backupId: randomUUID() })
    }

    if (snapshot.id)
      await api(`/snapshots/${snapshot.id}/discard`, 'DELETE')
    const status = await (await api(`/disks/${runId}/storage-status`, 'POST', {})).json()
    assert.equal(status.dirtyBytes, 0, 'The fixture must publish its retained journal before deletion')
    await api(`/disks/${runId}/delete`, 'POST', {})

    process.stdout.write(`${JSON.stringify({ mode: 'retained-current-chat-entrypoint', status: 'passed' })}\n`)
  }
  finally {
    try {
      docker('rm', '-f', peer)
    }
    catch {}

    await new Promise(resolve => broker.close(resolve))
  }
}
