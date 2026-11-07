import { Buffer } from 'node:buffer'
import { describe, expect, it } from 'vitest'
import { inspectDex } from '../check-android-dex-size.mjs'

function fixture(size, { virtual = false, abstract = false } = {}) {
  const bytes = Buffer.alloc(176 + size * 2)
  bytes.write('dex\n035\0', 0, 'ascii')
  bytes.writeUInt32LE(0x12345678, 40)
  bytes.writeUInt32LE(1, 96)
  bytes.writeUInt32LE(112, 100)
  bytes.writeUInt32LE(144, 136)
  // Two fields precede one encoded method. Its code item is at byte 160.
  Buffer.from([1, 1, virtual ? 0 : 1, virtual ? 1 : 0, 0, 0, 0, 0, 0, 0, ...(abstract ? [0] : [0xA0, 1])]).copy(bytes, 144)
  bytes.writeUInt32LE(size, 172)
  return bytes
}

describe('emitted Android DEX method size', () => {
  it('accepts the interpreter limit for direct and virtual methods', () => {
    expect(inspectDex(fixture(65535))).toEqual({ methods: 1, maxCodeUnits: 65535 })
    expect(inspectDex(fixture(65535, { virtual: true }))).toEqual({ methods: 1, maxCodeUnits: 65535 })
  })

  it('rejects the first oversized method and the original direct-bench size', () => {
    expect(() => inspectDex(fixture(65536))).toThrow('65536 code units; maximum 65535')
    expect(() => inspectDex(fixture(117868))).toThrow('117868 code units; maximum 65535')
  })

  it('skips abstract methods without code items', () => {
    expect(inspectDex(fixture(0, { abstract: true }))).toEqual({ methods: 0, maxCodeUnits: 0 })
  })

  it('refuses malformed input rather than reporting a passing size check', () => {
    expect(() => inspectDex(Buffer.alloc(112))).toThrow('Expected a standard DEX file')
    expect(() => inspectDex(fixture(10).subarray(0, 180))).toThrow('Truncated DEX code item')
    const truncatedClass = fixture(0).subarray(0, 147)
    expect(() => inspectDex(truncatedClass)).toThrow('Truncated DEX ULEB128')
  })
})
