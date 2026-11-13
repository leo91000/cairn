import { spawnSync } from 'node:child_process'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const step = workflow.jobs.image.steps.find(step => step.name === 'Qualify exclusive pause, private memory paging and live nested KVM')

function provision(initial, repetitions = 1) {
  const root = mkdtempSync(path.join(tmpdir(), 'cairn-exclusive-swap-'))
  const state = path.join(root, 'state.json')
  const mock = path.join(root, 'sudo.cjs')
  writeFileSync(state, JSON.stringify({
    active: false,
    initialized: false,
    calls: [],
    ...initial,
  }))
  writeFileSync(mock, `
const fs = require('node:fs')
const file = process.env.SWAP_STATE
const state = JSON.parse(fs.readFileSync(file, 'utf8'))
const args = process.argv.slice(2)
const command = args.join(' ')
state.calls.push(command)
fs.writeFileSync(file, JSON.stringify(state))
function fail(message) {
  console.error(message)
  process.exit(1)
}
if (command === state.failCommand) fail('injected provisioning failure')
if (args[0] === 'modprobe') {
  if (state.unavailable) fail('zram unavailable')
} else if (command === 'swapoff -a') {
  if (state.failSwapoff) fail('swapoff failed')
  state.active = false
} else if (args[0] === 'tee') {
  const value = fs.readFileSync(0, 'utf8').trim()
  if (args[1] === '/sys/block/zram0/reset') {
    if (state.active || value !== '1') fail('reset refused')
    state.initialized = false
    state.formatted = false
  } else if (args[1] === '/sys/block/zram0/comp_algorithm') {
    if (state.initialized) fail('tee: /sys/block/zram0/comp_algorithm: Device or resource busy')
    state.algorithm = value
  } else if (args[1] === '/sys/block/zram0/disksize') {
    if (state.initialized) fail('disksize busy')
    state.size = value
    state.initialized = true
  } else fail('unexpected sysfs write')
} else if (command === 'mkswap /dev/zram0') {
  if (!state.initialized) fail('device uninitialized')
  state.formatted = true
} else if (command === 'swapon --priority 100 /dev/zram0') {
  if (!state.formatted) fail('swap not formatted')
  state.active = true
} else fail('unexpected privileged command')
fs.writeFileSync(file, JSON.stringify(state))
`)

  try {
    // Function interception ensures the workflow can never touch host swap/sysfs.
    // PATH also denies accidental unmocked external commands.
    const script = `
sudo() { "${process.execPath}" "$SWAP_MOCK" "$@"; }
node() { printf 'qualified:%s\\n' "$CAIRN_EXCLUSIVE_REQUIRE_RECLAIM"; }
${Array.from({ length: repetitions }).fill(step.run).join('\n')}
`
    const result = spawnSync('/bin/bash', ['-e', '-c', script], {
      encoding: 'utf8',
      env: {
        PATH: root,
        SWAP_MOCK: mock,
        SWAP_STATE: state,
        SMOKE_IMAGE: 'fixture',
        ...step.env,
        CAIRN_EXCLUSIVE_ANDROID: 'false',
      },
    })
    return { ...result, state: JSON.parse(readFileSync(state, 'utf8')) }
  }
  finally {
    rmSync(root, { recursive: true, force: true })
  }
}

describe('exclusive qualification compressed swap', () => {
  it.each([false, true])('reinitializes preconfigured zram (active: %s) before changing its algorithm', (active) => {
    const result = provision({ initialized: true, active })
    expect(result.status, result.stderr).toBe(0)
    expect(result.state).toMatchObject({ active: true, algorithm: 'zstd', size: '4G' })
    expect(result.stdout).toContain('qualified:true')
  })

  it('can provision a fresh device twice without weakening reclamation', () => {
    const result = provision({}, 2)
    expect(result.status, result.stderr).toBe(0)
    expect(result.stdout.match(/qualified:true/g)).toHaveLength(2)
    expect(result.state.calls.filter(call => call === 'swapon --priority 100 /dev/zram0')).toHaveLength(2)
  })

  it('stops before resetting a device when swapoff fails', () => {
    const result = provision({ initialized: true, active: true, failSwapoff: true })
    expect(result.status).not.toBe(0)
    expect(result.state.calls).toEqual(['modprobe zram num_devices=1', 'swapoff -a'])
    expect(result.stdout).not.toContain('qualified:')
  })

  it.each([
    'tee /sys/block/zram0/reset',
    'tee /sys/block/zram0/comp_algorithm',
    'tee /sys/block/zram0/disksize',
    'mkswap /dev/zram0',
    'swapon --priority 100 /dev/zram0',
  ])('fails qualification if provisioning fails at %s', (failCommand) => {
    const result = provision({ failCommand })
    expect(result.status).not.toBe(0)
    expect(result.state.calls.at(-1)).toBe(failCommand)
    expect(result.stdout).not.toContain('qualified:')
  })

  it('retains the existing refusal-only qualification when zram is unavailable', () => {
    const result = provision({ unavailable: true })
    expect(result.status, result.stderr).toBe(0)
    expect(result.state.calls).toEqual(['modprobe zram num_devices=1'])
    expect(result.stdout).toContain('qualified:false')
  })
})
