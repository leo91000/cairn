import {
  mkdtempSync,
  readFileSync,
  rmSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import { chatFixture } from '../../tests/fixtures/chat-rpc.mjs'

const writes = vi.hoisted(() => ({ observe: undefined }))

vi.mock('node:fs', async (original) => {
  const fs = await original()
  return {
    ...fs,
    writeFileSync(file, ...args) {
      fs.writeFileSync(file, ...args)
      writes.observe?.(file)
    },
  }
})

let directory

afterEach(() => {
  writes.observe = undefined
  vi.clearAllTimers()
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllEnvs()
  rmSync(directory, { recursive: true, force: true })
})

it('publishes restart readiness after saving the conversation and workspace', () => {
  directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-restart-fixture-'))
  vi.stubEnv('CODEX_HOME', directory)
  vi.spyOn(process, 'cwd').mockReturnValue(directory)
  vi.spyOn(process.stdout, 'write').mockReturnValue(true)
  vi.useFakeTimers()
  let ready = false
  writes.observe = (file) => {
    if (path.basename(file) !== 'fixture-restart.json')
      return

    expect(readFileSync(path.join(directory, 'restart-work.txt'), 'utf8')).toBe('preserved before restart')
    const thread = JSON.parse(readFileSync(path.join(directory, 'fixture-conversation.json'), 'utf8'))
    expect(thread.turns[0].items.some(item => item.type === 'userMessage' && item.content[0].text === 'fixture:restart')).toBe(true)
    ready = true
  }

  const initial = chatFixture()
  initial({ id: 1, method: 'thread/start', params: { cwd: directory, config: {} } })
  initial({ id: 2, method: 'turn/start', params: { input: [{ type: 'text', text: 'fixture:restart' }] } })
  expect(ready).toBe(true)
  expect(vi.getTimerCount()).toBe(0)

  // A fresh provider process must be able to resume as soon as readiness is observed.
  const resumed = chatFixture()
  resumed({ id: 3, method: 'thread/resume', params: { config: {} } })
  resumed({ id: 4, method: 'turn/start', params: { input: [{ type: 'text', text: 'Continue the interrupted task' }] } })
  vi.runAllTimers()
  const thread = JSON.parse(readFileSync(path.join(directory, 'fixture-conversation.json'), 'utf8'))
  expect(thread.turns.at(-1).status).toBe('completed')
  expect(thread.turns.at(-1).items.at(-1).text).toContain('saved conversation and work survived')
})
