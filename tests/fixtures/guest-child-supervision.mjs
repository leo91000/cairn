import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
} from 'node:fs'
import { tmpdir, uptime } from 'node:os'
import path from 'node:path'
import process from 'node:process'

const provider = `
  const { spawn } = require('node:child_process')
  const { writeFileSync } = require('node:fs')
  const child = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 30000)'], {
    detached: true,
    stdio: 'ignore',
  })
  child.once('spawn', () => {
    writeFileSync(process.argv[1], String(child.pid))
    child.unref()
  })
`

async function main() {
  const root = mkdtempSync(path.join(tmpdir(), 'cairn-child-'))
  const pidFile = path.join(root, 'child')
  const helper = spawn('/usr/local/bin/cairn', ['background-job-helper'], {
    stdio: ['ignore', 'ignore', 'inherit', 'pipe'],
  })
  const control = helper.stdio[3]
  try {
    let output = ''
    control.setEncoding('utf8')
    control.on('data', (chunk) => {
      output += chunk
    })
    const closed = Promise.race([
      once(helper, 'close', { signal: AbortSignal.timeout(10000) }),
      new Promise((_, reject) => control.once('error', reject)),
    ])
    control.write(`${JSON.stringify({
      command: '',
      program: [process.execPath, '-e', provider, pidFile],
      deadlineBootMs: Math.floor(uptime() * 1000) + 20000,
    })}\n`)

    const [code] = await closed
    assert.equal(code, 0)
    const receipt = JSON.parse(output)
    assert.equal(receipt.exitCode, 0)
    assert.equal(receipt.timedOut, false)
    const pid = readFileSync(pidFile, 'utf8')
    assert.match(pid, /^\d+$/)
    assert.equal(existsSync(`/proc/${pid}`), false, 'The helper must reap the adopted descendant before returning the turn receipt')
  }
  finally {
    control.destroy()
    helper.kill()
    rmSync(root, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
