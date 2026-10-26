import type { ExecutionNode } from '../../packages/contracts/nodes'
import { expect, it } from 'vitest'
import {
  formatBytes,
  formatMiB,
  nodeDiagnostics,
  relativeAge,
} from '../../packages/contracts/nodes'

it('formats node sizes, recovery ages and blocking reasons for people', () => {
  expect(formatMiB(512)).toBe('512 MiB')
  expect(formatMiB(32768)).toBe('32 GiB')
  expect(formatMiB(15872)).toBe('15.5 GiB')
  expect(formatBytes(191_392_768)).toBe('182.5 MiB')
  expect(formatBytes(183_890_000)).toBe('175.4 MiB')
  expect(formatBytes(4_294_967_296)).toBe('4 GiB')
  expect(formatBytes(0)).toBe('0 B')
  expect(formatBytes(1)).toBe('1 B')
  expect(formatBytes(1536)).toBe('1.5 KiB')
  expect(relativeAge(0, 30_000)).toBe('less than a minute ago')
  expect(relativeAge(0, 3 * 60_000)).toBe('3 min ago')
  expect(relativeAge(0, 5 * 3_600_000)).toBe('5 h ago')
  const node: ExecutionNode = {
    id: 'n',
    name: 'Desktop',
    local: false,
    accepting: true,
    revoked: false,
    status: 'online',
    slots: 4,
    tags: [],
    capabilities: {
      os: 'linux',
      arch: 'x86_64',
      kvm: true,
      cpu: 8,
      memoryMiB: 16384,
      diskMiB: 65536,
    },
    limits: { cpu: 7, memoryMiB: 15872, diskMiB: 52428 },
    executionReady: true,
    runtimeId: 'r',
    lastSeen: 0,
    agents: [{ id: 'a', name: 'Main', allNodes: false }],
  }
  expect(nodeDiagnostics(node)).toEqual([])
  expect(nodeDiagnostics({ ...node, executionReady: false, budgetError: 'RAM below current usage' })[0]).toContain('RAM below current usage')
  expect(nodeDiagnostics({ ...node, agents: [] })).toEqual(['No agent can use this machine yet.'])
  expect(nodeDiagnostics({ ...node, status: 'offline', lastSeen: 0 }, 10 * 60_000)[0]).toContain('No contact since 10 min ago')

  // Native nodes need ublk without KVM, and run under their own service.
  const native = {
    ...node,
    nativeEnabled: true,
    nativeAccounts: ['p'],
    capabilities: {
      ...node.capabilities,
      backend: 'native' as const,
      kvm: false,
      ublk: true,
    },
  }
  expect(nodeDiagnostics(native)).toEqual([])
  expect(nodeDiagnostics({ ...native, capabilities: { ...native.capabilities, ublk: false } })).toEqual([
    'ublk is unavailable: this node needs a current native runtime and the ublk_drv kernel module (/dev/ublk-control) to mount S3-backed conversation disks.',
  ])
  expect(nodeDiagnostics({ ...native, status: 'offline', lastSeen: 0 }, 10 * 60_000)[0]).toContain('the cairn-native-node service is running')
})
