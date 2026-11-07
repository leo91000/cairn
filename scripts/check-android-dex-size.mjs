import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

// ART's switch interpreter narrows its instruction-handler dex PC to uint16_t.
// Oversized generated coroutine methods can relocate exception handlers outside
// the method. Check emitted code, since Kotlin source size does not predict it.
// https://android.googlesource.com/platform/art/+/refs/heads/android16-release/runtime/interpreter/interpreter_switch_impl-inl.h
const maxCodeUnits = 65535

export function inspectDex(bytes) {
  if (bytes.length < 112 || !/^dex\n0\d\d\0$/.test(bytes.toString('ascii', 0, 8)))
    throw new Error('Expected a standard DEX file')
  if (bytes.readUInt32LE(40) !== 0x12345678)
    throw new Error('Expected little-endian DEX')

  function uleb(cursor) {
    let value = 0
    for (let shift = 0; shift <= 28; shift += 7) {
      if (cursor.offset >= bytes.length)
        throw new Error('Truncated DEX ULEB128')
      const byte = bytes[cursor.offset++]
      value += (byte & 127) * 2 ** shift
      if (!(byte & 128))
        return value
    }

    throw new Error('Invalid DEX ULEB128')
  }

  const classCount = bytes.readUInt32LE(96)
  const classOffset = bytes.readUInt32LE(100)
  const methods = new Map()
  for (let index = 0; index < classCount; index++) {
    const offset = bytes.readUInt32LE(classOffset + index * 32 + 24)
    if (!offset)
      continue
    const cursor = { offset }
    const staticFields = uleb(cursor)
    const instanceFields = uleb(cursor)
    const directMethods = uleb(cursor)
    const virtualMethods = uleb(cursor)
    for (let field = 0; field < staticFields + instanceFields; field++) {
      uleb(cursor)
      uleb(cursor)
    }

    for (const count of [directMethods, virtualMethods]) {
      for (let method = 0; method < count; method++) {
        uleb(cursor)
        uleb(cursor)
        const codeOffset = uleb(cursor)
        if (!codeOffset)
          continue
        const codeUnits = bytes.readUInt32LE(codeOffset + 12)
        if (codeOffset + 16 + codeUnits * 2 > bytes.length)
          throw new Error(`Truncated DEX code item at ${codeOffset}`)
        methods.set(codeOffset, codeUnits)
      }
    }
  }

  let largest = 0
  for (const [offset, size] of methods) {
    if (size > maxCodeUnits)
      throw new Error(`DEX code item at ${offset} has ${size} code units; maximum ${maxCodeUnits}. Extract non-inline coroutine phase helpers.`)
    largest = Math.max(largest, size)
  }

  return { methods: methods.size, maxCodeUnits: largest }
}

export function checkApk(apk) {
  const options = { maxBuffer: 256 * 1024 * 1024 }
  const files = execFileSync('unzip', ['-Z1', apk], { ...options, encoding: 'utf8' })
    .split('\n')
    .filter(name => /^classes\d*\.dex$/.test(name))
  if (!files.length)
    throw new Error('APK contains no DEX files')
  return files.map((dex) => {
    try {
      return { dex, ...inspectDex(execFileSync('unzip', ['-p', apk, dex], options)) }
    }
    catch (error) {
      throw new Error(`${dex}: ${error.message}`)
    }
  })
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [file] = process.argv.slice(2)
  if (!file)
    throw new Error('Usage: node scripts/check-android-dex-size.mjs <apk-or-dex>')
  const report = file.endsWith('.dex') ? inspectDex(readFileSync(file)) : checkApk(file)
  console.log(JSON.stringify(report))
}
