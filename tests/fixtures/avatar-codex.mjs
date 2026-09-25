#!/usr/bin/env node
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'

const emit = value => process.stdout.write(`${JSON.stringify(value)}\n`)

async function main() {
  let endpoint
  for await (const line of createInterface({ input: process.stdin })) {
    const { id, method, params } = JSON.parse(line)
    if (id === undefined)
      continue
    let result = {}
    if (method === 'account/read')
      result = { account: { type: 'chatgpt', email: 'fixture@example.test', planType: 'plus' } }
    if (method === 'account/rateLimits/read')
      result = { rateLimits: { primary: { usedPercent: 5 }, secondary: { usedPercent: 10 } } }
    if (method === 'account/login/start') {
      assert.equal(params.type, 'chatgptAuthTokens')
      assert.equal(existsSync(path.join(process.env.CODEX_HOME, 'auth.json')), false)
      endpoint = params.chatgptAccountId
      result = { type: 'chatgptAuthTokens' }
    }

    if (method === 'model/list')
      result = { data: [{ model: 'gpt-6-astra', isDefault: true, supportedReasoningEfforts: [] }], nextCursor: null }
    if (method === 'thread/start') {
      assert.equal(params.model, 'gpt-6-astra')
      assert.equal(params.ephemeral, true)
      assert.equal(params.sandbox, 'read-only')
      assert.equal(params.approvalPolicy, 'never')
      assert.equal(params.config['features.image_generation'], true)
      for (const tool of ['shell_tool', 'unified_exec', 'multi_agent', 'apps', 'plugins', 'browser_use', 'computer_use', 'code_mode', 'code_mode_host'])
        assert.equal(params.config[`features.${tool}`], false)
      assert.equal(params.config.project_doc_max_bytes, 0)
      assert.deepEqual(params.config.mcp_servers, {})
      assert.equal(params.cwd, process.cwd())
      assert.ok(!process.env.OPENAI_API_KEY)
      assert.ok(!process.env.CODEX_API_KEY)
      result = { thread: { id: 'portrait-thread' } }
    }

    if (method === 'turn/start') {
      assert.equal(params.model, 'gpt-6-astra')
      assert.equal(params.threadId, 'portrait-thread')
      // The endpoint is a synthetic account identity, supplied only by the test broker.
      // Notifications deliberately precede the response to catch lost fast results.
      const response = await fetch(endpoint, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ ...params, fixturePid: process.pid }) })
      const item = await response.json()
      emit({
        method: 'item/completed',
        params: {
          threadId: params.threadId,
          item: response.ok
            ? item
            : {
                type: 'imageGeneration',
                status: 'failed',
                result: '',
                failure: item,
              },
        },
      })
      emit({ method: 'turn/completed', params: { threadId: params.threadId, turn: { status: 'completed' } } })
      result = { turn: { id: 'portrait-turn' } }
    }

    emit({ id, result })
  }
}

main().catch(() => process.exit(1))
