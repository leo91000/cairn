import type { ExecutionNode } from '../../../packages/contracts/nodes'
import { describe, expect, it } from 'vitest'
import {
  fleetClock,
  fleetSummary,
  freeSlots,
  nodeAdministrator,
  nodeDiagnostics,
  nodeHealth,
  observeRunner,
  observeUnready,
  runnerStale,
  slotCapacity,
  usedPercent,
} from '../../../packages/contracts/nodes'

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
    expect(nodeHealth(node({ executionReady: false, updateError: 'image mismatch' }))).toMatchObject({ state: 'attention', detail: 'The update failed' })
    // A saved budget turns readiness off until the next heartbeat applies it: not a fault.
    expect(nodeHealth(node({ executionReady: false }))).toEqual({ state: 'updating', label: 'Not ready', detail: 'The runtime is not ready yet' })
    const unreported = node()
    delete (unreported as Partial<ExecutionNode>).executionReady
    expect(nodeHealth(unreported).state).toBe('updating')
    expect(nodeHealth(node({ maintenance: 'draining', executionReady: false })).state).toBe('updating')
    expect(nodeHealth(node({ agents: [], accepting: false }))).toMatchObject({ state: 'setup', detail: 'No agent can use this machine yet' })
    expect(nodeHealth(node({ accepting: false, pressure: 'memory' })).state).toBe('paused')
    expect(nodeHealth(node({ pressure: 'disk' }))).toMatchObject({ state: 'busy', detail: 'Shared disk space is low' })
    expect(nodeHealth(node({ pressure: 'memory' })).detail).toBe('Shared RAM is under pressure')
    const reservation = { reason: 'GPU benchmark' } as ExecutionNode['exclusiveReservation']
    expect(nodeHealth(node({ exclusiveReservation: reservation, pressure: 'memory' }))).toEqual({ state: 'reserved', label: 'Reserved', detail: 'GPU benchmark' })
    // Present but empty errors still count, as in the Kotlin contract.
    expect(nodeHealth(node({ executionReady: false, budgetError: '' })).state).toBe('attention')
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
    // A failure observed for an earlier request: the server reports the retry as pending.
    expect(nodeHealth({
      ...native,
      nativeModeStatus: 'pending',
      nativeModeObserved: { mode: 'administrator', status: 'failed' } as ExecutionNode['nativeModeObserved'],
      executionReady: false,
    }).state).toBe('updating')
    expect(nodeHealth(node({ maintenance: 'draining', maintenanceError: 'disk full' }))).toMatchObject({ state: 'attention', detail: 'Update preparation failed' })
  })

  it('reads a missing free-slot count as none, like the generated Kotlin contract', () => {
    const partial = node({ occupiedSlots: 0 })
    delete (partial as Partial<ExecutionNode>).availableSlots
    expect(nodeHealth(partial)).toMatchObject({ state: 'full', detail: 'All 4 slots in use' })
  })

  it('judges the built-in runner against server time, never against the device clock offset', () => {
    const runner = node({ local: true, status: 'local', lastSeen: 0 })
    const remote = node({ id: 'remote', status: 'online', lastSeen: 120_000 })
    expect(fleetClock([runner, remote, node({ id: 'gone', status: 'offline', lastSeen: 900_000 })])).toBe(120_000)
    expect(fleetClock([runner])).toBeNull()
    expect(nodeHealth(runner, 0, 120_000)).toEqual({ state: 'attention', label: 'Needs attention', detail: 'Last usable report 2 min ago' })
    expect(runnerStale(runner, 120_000)).toBe(true)
    expect(nodeDiagnostics(runner, 0, 120_000)[0]).toContain('last usable report was 2 min ago')
    expect(nodeHealth(runner, 0, 30_000).state).toBe('ready')
    // Offline ages follow the server clock too.
    expect(nodeHealth(node({ status: 'offline', lastSeen: 0 }), 3_600_000, 120_000).detail).toBe('Last contact 2 min ago')
    // A device clock far ahead of the server does not make a refreshed runner look stale.
    expect(nodeHealth(node({ local: true, status: 'local', lastSeen: 120_000 }), 900_000, 125_000).state).toBe('ready')
    // Without any reference, the runner is not judged.
    expect(nodeHealth(runner, 900_000).state).toBe('ready')
    expect(nodeHealth(node({ local: true, status: 'local', lastSeen: null }), 0, 120_000).state).toBe('ready')
  })

  it('finds a lone stale runner when its contact stops changing across listings, without shortening other ages', () => {
    // The device clock is an hour ahead of the server: only elapsed time between listings matters.
    const fresh = [node({ local: true, status: 'local', lastSeen: 1_000 })]
    const seen = observeRunner(fresh, null, 3_600_000)!
    expect(seen).toEqual({
      lastSeen: 1_000,
      observedAt: 3_600_000,
      checkedAt: 3_600_000,
      refreshed: false,
    })
    expect(nodeHealth(fresh[0], 3_600_000, null, seen).state).toBe('ready')
    // Refreshed contacts restart the count; an unchanged one goes stale after a minute of successful listings.
    const refreshed = [node({ local: true, status: 'local', lastSeen: 11_000 })]
    expect(observeRunner(refreshed, seen, 3_610_000)).toEqual({
      lastSeen: 11_000,
      observedAt: 3_610_000,
      checkedAt: 3_610_000,
      refreshed: true,
    })
    const later = observeRunner(fresh, seen, 3_659_000)!
    expect(later).toEqual({ ...seen, checkedAt: 3_659_000 })
    expect(nodeHealth(fresh[0], 3_659_000, null, later).state).toBe('ready')
    const silent = observeRunner(fresh, later, 3_660_000)!
    expect(nodeHealth(fresh[0], 3_660_000, null, silent)).toEqual({ state: 'attention', label: 'Needs attention', detail: 'No usable report for over a minute' })
    expect(nodeDiagnostics(fresh[0], 3_660_000, null, silent)[0]).toContain('last usable report was over a minute ago')
    // Failed listings leave the observation as it was: the page's connectivity is not blamed on the runner.
    expect(nodeHealth(fresh[0], 9_999_999, null, later).state).toBe('ready')
    expect(observeRunner([node({ local: true, status: 'local', lastSeen: null })], seen)).toBeNull()
    // A runner that was already stale when the page opened does not slow the clock used for other ages.
    const fleet = [node({ local: true, status: 'local', lastSeen: 0 }), node({ id: 'remote', status: 'offline', lastSeen: 0 })]
    const stale = observeRunner(fleet, null, 7_200_000)
    expect(fleetClock(fleet)).toBeNull()
    expect(nodeHealth(fleet[1], 7_200_000, fleetClock(fleet, stale), stale).detail).toBe('Last contact 2 h ago')
    // A contact that changed while watched is server time: it dates other machines without the device clock.
    const watched = observeRunner(refreshed, seen, 3_610_000)!
    const checked = observeRunner(refreshed, watched, 3_670_000)
    expect(fleetClock(refreshed, checked)).toBe(71_000)
    expect(nodeHealth(node({ id: 'remote', status: 'offline', lastSeen: 0 }), 99_999_999, fleetClock(refreshed, checked)).detail).toBe('Last contact 1 min ago')
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

  it('counts free slots as placement does, within the runtime pool', () => {
    expect(freeSlots(node({
      slots: 4,
      runtimeSlots: 2,
      occupiedSlots: 1,
      availableSlots: 3,
    }))).toBe(1)
    expect(freeSlots(node({
      slots: 4,
      runtimeSlots: null,
      occupiedSlots: 1,
      availableSlots: 3,
    }))).toBe(3)
    expect(nodeHealth(node({
      slots: 4,
      runtimeSlots: 2,
      occupiedSlots: 2,
      availableSlots: 2,
    }))).toEqual({ state: 'full', label: 'Full', detail: 'All 2 slots in use' })
  })

  it('escalates an unready runtime without a reported error once it lasts across listings', () => {
    const waiting = node({ executionReady: false })
    const first = observeUnready([waiting], {}, 1_000)
    expect(first).toEqual({ node: { since: 1_000, checkedAt: 1_000 } })
    expect(nodeHealth(waiting, 0, null, null, first).state).toBe('updating')
    const later = observeUnready([waiting], first, 1_000 + 5 * 60_000)
    expect(nodeHealth(waiting, 0, null, null, later)).toEqual({ state: 'attention', label: 'Needs attention', detail: 'Not ready for over 5 minutes' })
    // Recovery, or a reported error, starts over.
    expect(observeUnready([node()], later, 400_000)).toEqual({})
    expect(observeUnready([node({ executionReady: false, budgetError: 'cgroup' })], later, 400_000)).toEqual({})
    // Time spent offline or changing privileges does not count: the grace period starts at "Not ready".
    const offline = node({ status: 'offline', executionReady: false })
    const away = observeUnready([offline], observeUnready([offline], {}, 0), 10 * 60_000)
    expect(away).toEqual({})
    const back = observeUnready([{ ...offline, status: 'online' }], away, 10 * 60_000 + 10_000)
    expect(nodeHealth({ ...offline, status: 'online' }, 0, null, null, back).state).toBe('updating')
    const native = node({
      capabilities: {
        ...node().capabilities,
        backend: 'native',
        kvm: false,
        ublk: true,
        nativeModeProtocol: 1,
      },
      nativeEnabled: true,
      nativeAccounts: ['person'],
      executionReady: false,
    })
    expect(observeUnready([{ ...native, nativeModeStatus: 'pending' }], {}, 0)).toEqual({})
  })

  it('reads a runtime offering no slot as full', () => {
    expect(nodeHealth(node({
      slots: 2,
      runtimeSlots: 0,
      occupiedSlots: 0,
      availableSlots: 2,
    }))).toEqual({ state: 'full', label: 'Full', detail: 'The runtime offers no slot' })
  })

  it('reads partial payloads like the Kotlin contract and the server', () => {
    const partial = node()
    delete (partial as Partial<ExecutionNode>).status
    expect(nodeHealth(partial).state).toBe('offline')
    expect(slotCapacity(node({ slots: null, runtimeSlots: 3 }))).toBe(3)
    expect(slotCapacity(node({ slots: null, runtimeSlots: null }))).toBe(4)
  })

  it('summarizes the fleet as placement sees it', () => {
    const runner = node({
      id: 'runner',
      local: true,
      status: 'local',
      lastSeen: 0,
      slots: 4,
      runtimeSlots: 2,
      occupiedSlots: 3,
      availableSlots: 1,
    })
    const online = node({ id: 'online', status: 'online', lastSeen: 120_000 })
    const setup = node({
      id: 'setup',
      status: 'online',
      lastSeen: 120_000,
      agents: [],
    })
    const offline = node({ id: 'offline', status: 'offline', lastSeen: 0 })
    const gone = node({ id: 'gone', revoked: true })
    // The runner's contact lags the online machines: it went silent and leaves the totals.
    expect(fleetSummary([runner, online, setup, offline, gone], 0, 120_000)).toEqual({
      machines: 4,
      connected: 2,
      usedSlots: 2,
      totalSlots: 8,
      attention: 3,
    })
    // Fresh, its slots count within the runtime pool, used ones capped by that capacity.
    expect(fleetSummary([{ ...runner, lastSeen: 120_000 }, online], 0, 120_000)).toEqual({
      machines: 2,
      connected: 2,
      usedSlots: 3,
      totalSlots: 6,
      attention: 0,
    })
  })

  it('bounds usage to the limit', () => {
    expect(usedPercent(512, 1024)).toBe(50)
    expect(usedPercent(2048, 1024)).toBe(100)
    expect(usedPercent(1, 0)).toBe(0)
  })
})
