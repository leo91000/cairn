import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

// Use the running host's device classes, including kernels that use blkext.
export function ublkDeviceArguments() {
  const devices = readFileSync('/proc/devices', 'utf8')
  const character = devices.split('Character devices:')[1]?.split('Block devices:')[0]
  const block = devices.split('Block devices:')[1]
  const charMajor = Number(character?.match(/(?:^|\n)\s*(\d+) ublk-char(?:\n|$)/)?.[1])
  const blockMajor = Number(block?.match(/(?:^|\n)\s*(\d+) (?:ublk|blkext)(?:\n|$)/)?.[1])
  assert.ok(charMajor > 0 && blockMajor > 0, 'Load ublk_drv before starting the runner')
  return ['--cap-add', 'SYS_RESOURCE', '--device', '/dev/ublk-control', '--device-cgroup-rule', `c ${charMajor}:* rwm`, '--device-cgroup-rule', `b ${blockMajor}:* rwm`]
}
