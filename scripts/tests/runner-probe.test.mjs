import { Buffer } from 'node:buffer'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {
  afterEach,
  describe,
  expect,
  it,
} from 'vitest'
import { attemptOutput, inboxContains } from '../../tests/runner-probe.mjs'

const roots = []

afterEach(() => {
  for (const root of roots)
    fs.rmSync(root, { recursive: true, force: true })
  roots.length = 0
})

describe('vM smoke probe inbox', () => {
  it('waits through the unlink and replacement phases of a guest tar import', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cairn-inbox-probe-'))
    roots.push(root)
    const file = path.join(root, 'messages.json')
    fs.writeFileSync(file, '[{"text":"old"}]')
    expect(inboxContains(file, 'capture-update')).toBe(false)

    fs.unlinkSync(file)
    expect(inboxContains(file, 'capture-update')).toBe(false)
    fs.writeFileSync(file, '')
    expect(inboxContains(file, 'capture-update')).toBe(false)
    fs.writeFileSync(file, '[{"text":"capture-update"}]')
    expect(inboxContains(file, 'capture-update')).toBe(true)
  })

  it('surfaces filesystem failures other than the transient missing file', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cairn-inbox-probe-'))
    roots.push(root)
    expect(() => inboxContains(root, 'capture-update')).toThrow()
  })
})

describe('vM smoke probe output', () => {
  it('finds an update marker across every possible stdout record boundary', () => {
    const message = 'probe.updated\n'
    for (let boundary = 1; boundary < message.length; boundary++) {
      const records = [message.slice(0, boundary), message.slice(boundary)].map(text => ({
        type: 'output',
        data: Buffer.from(text).toString('base64'),
      }))
      expect(attemptOutput(records)).toContain('probe.updated')
    }
  })

  it('keeps stderr and split UTF-8 intact while excluding non-output events', () => {
    const bytes = Buffer.from('guest failed: é')
    const records = [
      { type: 'heartbeat' },
      { type: 'output', stderr: true, data: bytes.subarray(0, -1).toString('base64') },
      { type: 'output', stderr: true, data: bytes.subarray(-1).toString('base64') },
    ]
    expect(attemptOutput(records)).toBe('guest failed: é')
  })
})
