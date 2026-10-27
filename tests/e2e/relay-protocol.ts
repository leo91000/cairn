import { readFileSync } from 'node:fs'

const source = readFileSync(new URL('../../crates/protocol/src/lib.rs', import.meta.url), 'utf8')
const declaration = source.match(/pub const PROTOCOL_VERSION: u16 = (\d+);/)
if (!declaration)
  throw new Error('The relay protocol version declaration is missing.')

export const relayProtocolVersion = Number(declaration[1])
