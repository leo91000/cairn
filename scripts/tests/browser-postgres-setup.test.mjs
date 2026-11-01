import { spawnSync } from 'node:child_process'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const setup = workflow.jobs.browser.steps.find(step => step.name === 'Install Postgres client for beacon account fixtures')

function executeSetup({ failedInstalls = 0, updateStatus = 0, installStatus = 100 } = {}) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-postgres-setup-'))
  const history = path.join(directory, 'calls.json')

  try {
    writeFileSync(history, '[]')
    writeFileSync(path.join(directory, 'apt-get'), `#!${process.execPath}
const fs = require('node:fs')
const history = ${JSON.stringify(history)}
const calls = JSON.parse(fs.readFileSync(history, 'utf8'))
const args = process.argv.slice(2)
const operation = args.includes('update') ? 'update' : 'install'
calls.push({ operation, args })
fs.writeFileSync(history, JSON.stringify(calls))
if (operation === 'update') process.exit(${updateStatus})
process.exit(calls.filter(call => call.operation === 'install').length <= ${failedInstalls} ? ${installStatus} : 0)
`, { mode: 0o700 })
    writeFileSync(path.join(directory, 'sleep'), '#!/bin/sh\nexit 0\n', { mode: 0o700 })

    // Execute the real workflow step with disposable package-manager fixtures.
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', setup.run], {
      cwd: directory,
      encoding: 'utf8',
      timeout: 10000,
      env: { PATH: `${directory}${path.delimiter}${process.env.PATH}` },
    })
    expect(result.error).toBeUndefined()
    return { status: result.status, calls: JSON.parse(readFileSync(history, 'utf8')) }
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

describe('browser Postgres setup', () => {
  it('finishes after the first successful install', () => {
    const result = executeSetup()
    expect(result.status).toBe(0)
    expect(result.calls.map(call => call.operation)).toEqual(['update', 'install'])
    expect(result.calls[1].args).toContain('postgresql-client')
  })

  it('starts fresh index and installation processes after transient failures', () => {
    const result = executeSetup({ failedInstalls: 2 })
    expect(result.status).toBe(0)
    expect(result.calls.map(call => call.operation)).toEqual(['update', 'install', 'update', 'install', 'update', 'install'])
  })

  it('stops after three failed installs and preserves the failure status', () => {
    const result = executeSetup({ failedInstalls: 99, installStatus: 42 })
    expect(result.status).toBe(42)
    expect(result.calls.filter(call => call.operation === 'install')).toHaveLength(3)
  })

  it('never installs from stale indexes when every update fails', () => {
    const result = executeSetup({ updateStatus: 100 })
    expect(result.status).toBe(100)
    expect(result.calls.map(call => call.operation)).toEqual(['update', 'update', 'update'])
    for (const call of result.calls)
      expect(call.args).toContain('APT::Update::Error-Mode=any')
  })
})
