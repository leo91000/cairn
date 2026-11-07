#!/usr/bin/env node
import {
  appendFileSync,
  closeSync,
  readFileSync,
  writeSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'

// This fixture never authenticates, submits work, or records request parameters.
const home = process.env.CODEX_HOME
const scenario = readFileSync(path.join(home, 'fixture-termination-case'), 'utf8')
const journal = path.join(home, 'fixture-termination-events.jsonl')
const privateStderr = 'PRIVATE_STARTUP_STDERR_MARKER\n'

function record(event) {
  appendFileSync(journal, `${JSON.stringify({ pid: process.pid, ...event })}\n`)
}

function abandon(event, code) {
  // The test may already have removed its temporary home during unwinding.
  // Neither a failed journal write nor owner loss may leave this child alive.
  try {
    record({ event })
  }
  finally {
    process.exit(code)
  }
}

const owner = process.ppid
process.stdin.once('end', () => abandon('owner_stdin_closed', 90))
process.stdin.once('close', () => abandon('owner_stdin_closed', 90))
process.stdin.once('error', () => abandon('owner_stdin_error', 93))
setInterval(() => {
  if (process.ppid !== owner)
    abandon('owner_lost', 91)
}, 50)
// Longer than the Rust helper's 15-second liveness guard; never a normal exit.
setTimeout(abandon, 20_000, 'fixture_lifetime_expired', 92)

async function main() {
  for await (const line of createInterface({ input: process.stdin })) {
    const request = JSON.parse(line)
    record({ event: 'request', method: request.method, request_id: request.id })
    if (request.method !== 'initialize' || request.id !== 1)
      process.exit(98)

    writeSync(2, privateStderr)
    if (scenario === 'exit') {
      record({ event: 'self_exit', code: 23 })
      process.exit(23)
    }

    if (scenario === 'signal') {
      record({ event: 'self_signal', signal: 9 })
      process.kill(process.pid, 'SIGKILL')
      await new Promise(() => {})
    }

    if (scenario === 'reader_eof') {
      // Stay alive after stdout EOF. Only Cairn's subsequent cleanup may end us.
      process.on('SIGTERM', () => {
        record({ event: 'cleanup_signal', signal: 15, code: 43 })
        process.exit(43)
      })
      record({ event: 'stdout_closed' })
      closeSync(1)
      continue
    }

    process.exit(99)
  }

  abandon('owner_stdin_closed', 90)
}

void main()
