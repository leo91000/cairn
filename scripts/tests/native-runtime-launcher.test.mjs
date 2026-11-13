import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { expect, it } from 'vitest'

it('isolates agent and legacy storage credentials before the real native fixture entrypoint', () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'native-launcher-'))
  try {
    writeFileSync(path.join(directory, 'python3'), `#!/usr/bin/env node
const keys = ['CAIRN_AUTH_SOCKET', 'CAIRN_MCP_RUN_TOKEN', 'OPENAI_API_KEY', 'AWS_SECRET_ACCESS_KEY', 'STORAGE_S3_BUCKET', 'ARCHIVE_S3_BUCKET']
process.stdout.write(JSON.stringify({ args: process.argv.slice(2), leaked: keys.filter(key => key in process.env) }))
`, { mode: 0o700 })
    const result = JSON.parse(execFileSync(process.execPath, ['scripts/test-native-runtime.mjs', '--repo', directory, '--native-runtime', 'candidate', '--qualifier', 'qualifier'], {
      encoding: 'utf8',
      env: {
        ...process.env,
        PATH: `${directory}:${process.env.PATH}`,
        CAIRN_AUTH_SOCKET: 'fixture',
        CAIRN_MCP_RUN_TOKEN: 'fixture',
        OPENAI_API_KEY: 'fixture',
        AWS_SECRET_ACCESS_KEY: 'fixture',
        STORAGE_S3_BUCKET: 'fixture',
        ARCHIVE_S3_BUCKET: 'fixture',
      },
    }))
    expect(result.leaked).toEqual([])
    expect(result.args).toEqual([path.join(directory, 'scripts/tests/native_runtime_test.py'), path.resolve('candidate'), path.resolve('qualifier')])
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
})
