import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import { blake3 } from '@noble/hashes/blake3.js'

// Independent implementation for the real VM capture and cold-restore checks.
export function blockDigest(bytes, identity) {
  if (identity.startsWith('b3-'))
    return `b3-${Buffer.from(blake3(bytes)).toString('hex')}`
  return createHash('sha256').update(bytes).digest('hex')
}
