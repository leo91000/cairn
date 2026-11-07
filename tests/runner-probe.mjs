import { Buffer } from 'node:buffer'
import fs from 'node:fs'

// Guest inbox imports may briefly unlink messages.json while tar replaces it.
// Keep the caller's deadline authoritative, including when input never arrives.
export function inboxContains(file, marker) {
  try {
    return fs.readFileSync(file, 'utf8').includes(marker)
  }
  catch (error) {
    if (error.code === 'ENOENT')
      return false
    throw error
  }
}

export function attemptOutput(records) {
  const chunks = records
    .filter(record => record.type === 'output')
    .map(record => Buffer.from(record.data, 'base64'))
  return Buffer.concat(chunks).toString()
}
