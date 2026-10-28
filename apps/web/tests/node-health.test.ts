import type { ExecutionNode } from '../../../packages/contracts/nodes'
import { describe, expect, it } from 'vitest'
import { nodeAdministrator, nodeHealth, usedPercent } from '../../../packages/contracts/nodes'

function node(overrides: Partial<ExecutionNode> = {}): ExecutionNode {
  return {
    id: 'node',
    name: 'Workstation',
    revision: '1',
    local: false,
    accepting: true,
    revoked: false,
    status: 'online',
    tags: [],
    systemTags: [],
    capabilities: {
      backend: 'firecracker',
      ublk: false,
      nativeModeProtocol: 1,
      diskTotalMiB: 0,
      cpu: 8,
      memoryMiB: 16384,
      diskMiB: 102400,
      os: 'linux',
      arch: 'x86_64',
      kvm: true,
    },
    limits: { cpu: 4, memoryMiB: 8192, diskMiB: 51200 },
    slots: 4,
    nativeEnabled: false,
    nativeAccounts: [],
    nativeReviewedAgents: [],
    exclusiveReservations: true,
    exclusiveReservation: null,
    exclusiveAgents: [],
    executionReady: true,
    runtimeId: 'runtime',
    lastSeen: 0,
    createdAt: 0,
    storage: {
      cacheMiB: 102400,
      memoryCacheMiB: 256,
      reserveMiB: 10240,
      reservePercent: 5,
      backupSeconds: 60,
      maxDirtySeconds: 300,
    },
    occupiedSlots: 1,
    availableSlots: 3,
    agents: [{ id: 'agent', name: 'Main agent', allNodes: false }],
    staleDisks: { count: 0, diskMiB: 0 },
    ...overrides,
  }
}

describe('node health', () => {
  it('reports a usable node with its free slots', () => {
    expect(nodeHealth(node())).toEqual({ state: 'ready', label: 'Ready', detail: '1 running · 3 of 4 slots free' })
    expect(nodeHealth(node({ occupiedSlots: 0, availableSlots: 4 })).detail).toBe('4 of 4 slots free')
    expect(nodeHealth(node({ occupiedSlots: 4, availableSlots: 0 }))).toMatchObject({ state: 'full', detail: 'All 4 slots in use' })
  })

  it('puts the blocker that needs the owner first', () => {
    expect(nodeHealth(node({ revoked: true, status: 'offline' })).state).toBe('revoked')
    expect(nodeHealth(node({ status: 'offline', lastSeen: 0, agents: [] }), 3 * 3_600_000)).toEqual({ state: 'offline', label: 'Offline', detail: 'Last contact 3 h ago' })
    expect(nodeHealth(node({ status: 'offline', lastSeen: null })).detail).toBe('Never connected')
    expect(nodeHealth(node({ capabilities: { ...node().capabilities, kvm: false }, agents: [] }))).toMatchObject({ state: 'attention', detail: 'KVM is unavailable' })
    expect(nodeHealth(node({ executionReady: false, budgetError: 'cgroup' }))).toMatchObject({ state: 'attention', detail: 'The shared budget could not be applied' })
    expect(nodeHealth(node({ maintenance: 'draining', executionReady: false })).state).toBe('updating')
    expect(nodeHealth(node({ agents: [], accepting: false }))).toMatchObject({ state: 'setup', detail: 'No agent can use this machine yet' })
    expect(nodeHealth(node({ accepting: false, pressure: 'memory' })).state).toBe('paused')
    expect(nodeHealth(node({ pressure: 'disk' }))).toMatchObject({ state: 'busy', detail: 'Shared disk space is low' })
  })

  it('raises privilege changes and failed updates for the owner', () => {
    const native = node({
      capabilities: {
        ...node().capabilities,
        backend: 'native',
        kvm: false,
        ublk: true,
      },
      nativeEnabled: true,
      nativeAccounts: ['person'],
    })
    expect(nodeHealth({ ...native, capabilities: { ...native.capabilities, nativeModeProtocol: 0 }, nativeModeStatus: 'pending' })).toMatchObject({ state: 'attention', detail: 'Update the supervisor to confirm privileges' })
    expect(nodeHealth({ ...native, nativeModeProposal: { mode: 'administrator' } as ExecutionNode['nativeModeProposal'] })).toMatchObject({ state: 'attention', detail: 'An agent requested Native administrator' })
    expect(nodeHealth({ ...native, nativeModeStatus: 'pending', executionReady: false })).toMatchObject({ state: 'updating', detail: 'Privilege change in progress' })
    expect(nodeHealth({ ...native, nativeModeStatus: 'failed', executionReady: false })).toMatchObject({ state: 'attention', detail: 'The privilege change failed' })
    expect(nodeHealth(node({ maintenance: 'draining', maintenanceError: 'disk full' }))).toMatchObject({ state: 'attention', detail: 'The update failed' })
  })

  it('reads a missing free-slot count as none, like the generated Kotlin contract', () => {
    const partial = node({ occupiedSlots: 0 })
    delete (partial as Partial<ExecutionNode>).availableSlots
    expect(nodeHealth(partial)).toMatchObject({ state: 'full', detail: 'All 4 slots in use' })
  })

  it('keeps the built-in runner online without a heartbeat status', () => {
    expect(nodeHealth(node({ local: true, status: 'local' })).state).toBe('ready')
  })

  it('asks for native activation before a native node takes work', () => {
    const native = node({
      capabilities: {
        ...node().capabilities,
        backend: 'native',
        kvm: false,
        ublk: true,
      },
    })
    expect(nodeHealth(native)).toMatchObject({ state: 'setup', detail: 'Enable native execution and choose trusted people' })
    expect(nodeHealth({ ...native, nativeEnabled: true, nativeAccounts: ['person'] }).state).toBe('ready')
    expect(nodeHealth({ ...native, capabilities: { ...native.capabilities, ublk: false } })).toMatchObject({ state: 'attention', detail: 'ublk is unavailable' })
  })

  it('names administrator nodes from the confirmed mode, or the request until confirmation', () => {
    const native = node({ capabilities: { ...node().capabilities, backend: 'native' } })
    expect(nodeAdministrator(native)).toBe(false)
    expect(nodeAdministrator({ ...native, nativeModeRequest: { mode: 'administrator' } as ExecutionNode['nativeModeRequest'] })).toBe(true)
    expect(nodeAdministrator({ ...native, nativeModeRequest: { mode: 'administrator' } as ExecutionNode['nativeModeRequest'], nativeModeObserved: { mode: 'protected' } as ExecutionNode['nativeModeObserved'] })).toBe(false)
    expect(nodeAdministrator(node({ nativeModeRequest: { mode: 'administrator' } as ExecutionNode['nativeModeRequest'] }))).toBe(false)
  })

  it('bounds usage to the limit', () => {
    expect(usedPercent(512, 1024)).toBe(50)
    expect(usedPercent(2048, 1024)).toBe(100)
    expect(usedPercent(1, 0)).toBe(0)
  })
})
