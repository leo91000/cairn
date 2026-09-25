#!/usr/bin/env node
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'

const emit = value => process.stdout.write(`${JSON.stringify(value)}\n`)
let account = 'fixture'
let thread
let threads = 0
let reducedSummaries = false
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
    account = params.chatgptAccountId
    result = { type: 'chatgptAuthTokens' }
  }

  if (method === 'model/list')
    result = { data: [{ model: 'gpt-6-luna', supportedReasoningEfforts: [{ reasoningEffort: account === 'unsupported' ? 'low' : 'xhigh' }] }], nextCursor: null }
  if (method === 'thread/start') {
    assert.equal(params.model, 'gpt-6-luna')
    assert.equal(params.ephemeral, true)
    assert.equal(params.sandbox, 'read-only')
    assert.equal(params.approvalPolicy, 'never')
    assert.equal(params.config['features.shell_tool'], false)
    assert.equal(params.config['features.apps'], false)
    assert.equal(params.config['features.plugins'], false)
    assert.equal(params.cwd, process.cwd())
    thread = `title-thread-${++threads}`
    result = { thread: { id: thread } }
  }

  if (method === 'turn/start') {
    assert.equal(params.threadId, thread)
    assert.equal(params.model, 'gpt-6-luna')
    assert.equal(params.effort, 'xhigh')
    const summary = params.outputSchema.required[0] === 'summary'
    assert.deepEqual(params.outputSchema.required, [summary ? 'summary' : 'title'])
    if (account === 'hang') {
      emit({ id, result: { turn: { id: 'title-turn' } } })
      continue
    }

    const input = JSON.parse(params.input[0].text)
    assert.ok(Array.isArray(input.messages))
    assert.ok(JSON.stringify(input.messages).length <= 64_000)
    const transcript = input.messages.map(m => m.text).join('\n')
    let output
    if (summary) {
      reducedSummaries ||= input.messages.some(m => m.role === 'summary')
      const topics = [...new Set(transcript.match(/TOPIC_[A-Z]+/g) || [])].join(' ')
      output = { summary: `${topics} ${'x'.repeat(7000)}` }
      if (account === 'oversized-summary')
        output.summary = 'x'.repeat(8001)
      if (account === 'empty-summary')
        output.summary = ''
    }
    else if (account === 'full-history') {
      assert.ok(reducedSummaries, 'Long histories must reduce summaries recursively')
      for (const marker of ['TOPIC_START', 'TOPIC_MIDDLE', 'TOPIC_END'])
        assert.ok(transcript.includes(marker), `Lost ${marker}`)
      output = { title: 'Historique complet Android' }
    }
    else {
      output = { title: transcript.includes('Android') ? 'Mises à jour Android' : input.currentTitle }
    }

    const text = account === 'malformed' ? 'not JSON' : JSON.stringify(output)
    // Notifications deliberately precede the response to catch lost fast results.
    emit({ method: 'item/completed', params: { threadId: thread, item: { type: 'agentMessage', text } } })
    emit({ method: 'turn/completed', params: { threadId: thread, turn: { status: account === 'failed' ? 'failed' : 'completed' } } })
    result = { turn: { id: 'title-turn' } }
  }

  emit({ id, result })
}
