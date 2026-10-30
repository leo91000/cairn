import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { chatFixture } from '../../tests/fixtures/chat-rpc.mjs'

let directory
let messages

beforeEach(() => {
  directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-stream-fixture-'))
  vi.stubEnv('CODEX_HOME', directory)
  vi.useFakeTimers()
  messages = []
  vi.spyOn(process.stdout, 'write').mockImplementation((line) => {
    messages.push(JSON.parse(line))
    return true
  })
})

afterEach(() => {
  vi.clearAllTimers()
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllEnvs()
  rmSync(directory, { recursive: true, force: true })
})

function start(text) {
  const handle = chatFixture()
  handle({ id: 1, method: 'thread/start', params: { cwd: directory, config: {} } })
  handle({ id: 2, method: 'turn/start', params: { input: [{ type: 'text', text }] } })
}

function parts() {
  return messages.filter(message => message.method === 'item/agentMessage/delta' && /^ \d{3}$/.test(message.params.delta)).map(message => Number(message.params.delta))
}

it('holds provider output at each consumer checkpoint without completing or duplicating the stream', () => {
  const limit = path.join(directory, 'stream-limit')
  writeFileSync(limit, '0')
  start(`fixture:stream\nfixture:stream-limit:${limit}`)
  vi.advanceTimersByTime(16000)
  expect(parts()).toEqual([])

  for (const checkpoint of [5, 20, 25, 35]) {
    writeFileSync(limit, String(checkpoint))
    vi.advanceTimersByTime(16000)
    expect(parts()).toEqual(Array.from({ length: checkpoint }, (_, index) => index + 1))
    expect(messages.filter(message => message.method === 'turn/completed')).toHaveLength(0)
    const thread = JSON.parse(readFileSync(path.join(directory, 'fixture-conversation.json'), 'utf8'))
    expect(thread.turns[0].items.find(item => item.id.startsWith('stream-')).text).toMatch(new RegExp(` ${String(checkpoint).padStart(3, '0')}$`))
  }

  writeFileSync(limit, '100')
  vi.advanceTimersByTime(16000)
  expect(parts()).toEqual(Array.from({ length: 100 }, (_, index) => index + 1))
  expect(messages.filter(message => message.method === 'item/completed' && message.params.item.id.startsWith('stream-'))).toHaveLength(1)
  expect(messages.filter(message => message.method === 'turn/completed')).toHaveLength(1)
  expect(vi.getTimerCount()).toBe(0)
})

it('keeps the ungated provider protocol available to existing callers', () => {
  start('fixture:stream')
  vi.advanceTimersByTime(16000)
  expect(parts()).toEqual(Array.from({ length: 100 }, (_, index) => index + 1))
  expect(messages.filter(message => message.method === 'turn/completed')).toHaveLength(1)
  expect(vi.getTimerCount()).toBe(0)
})
