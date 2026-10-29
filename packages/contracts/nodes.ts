import type { ExecutionNode } from './generated/typescript/ExecutionNode'
import type { NativeMode } from './generated/typescript/NativeMode'

export type { ExecutionNode } from './generated/typescript/ExecutionNode'
export type { NativeCaller } from './generated/typescript/NativeCaller'
export type { NativeMode } from './generated/typescript/NativeMode'
export type { NativeModeObserved } from './generated/typescript/NativeModeObserved'
export type { NativeModePreview } from './generated/typescript/NativeModePreview'
export type { NativeModeRequest } from './generated/typescript/NativeModeRequest'
export type { NodeAgent } from './generated/typescript/NodeAgent'
export type { NodeDiskAccounting } from './generated/typescript/NodeDiskAccounting'
export type { NodeEnrollment } from './generated/typescript/NodeEnrollment'
export type { NodeResources } from './generated/typescript/NodeResources'
export type { NodeStoragePolicy } from './generated/typescript/NodeStoragePolicy'
export const LOCAL_NODE_ID = '00000000-0000-4000-8000-000000000002'

export function diskCapacity(node: ExecutionNode): number {
  const total = node.capabilities.diskTotalMiB
  if (total !== undefined && total > 0)
    return total

  return node.capabilities.diskMiB
}

export function nativeModeLabel(mode: NativeMode) {
  return mode === 'administrator' ? 'Native administrator' : 'Native protected'
}

/** Native nodes report the observed privileges, or the requested ones until the supervisor reports any. */
export function nodeAdministrator(node: ExecutionNode) {
  return node.capabilities.backend === 'native' && (node.nativeModeObserved ?? node.nativeModeRequest)?.mode === 'administrator'
}

/** Free slots as placement counts them: the runtime's pool can be smaller than the configured slots. */
export function freeSlots(node: ExecutionNode) {
  return Math.max(0, Math.min(node.availableSlots ?? 0, slotCapacity(node) - (node.occupiedSlots ?? 0)))
}

/** Slots placement can use: the configured ones, within the runtime's pool once it reports one. */
export function slotCapacity(node: ExecutionNode) {
  const slots = node.slots ?? 0
  return Math.min(slots, node.runtimeSlots ?? slots)
}

/** Percentage of a limit in use, between 0 and 100. */
export function usedPercent(used: number, limit: number) {
  return limit > 0 ? Math.min(100, Math.max(0, used / limit * 100)) : 0
}

export interface ExclusiveReservation {
  id: string
  nodeId: string
  ownerRunId: string
  reason: string
  state: 'queued' | 'acquiring' | 'acquired' | 'releasing' | 'released' | 'failed' | 'expired'
  createdAt: number
  expiresAt: number | null
  renewalPending?: boolean
  waitUntil: number
  pausedRunIds: string[]
  minimumAvailableMemoryMiB: number
  error?: string | null
}

export interface NodeSyncSettings {
  intervalSeconds: number
  budgetMiB: number
  disconnectTimeoutSeconds: number
  shutdownTimeoutSeconds: number
  maxCapacityWaitSeconds: number
  s3Configured?: boolean
}

export function formatMiB(value: number) {
  return value < 1024 ? `${Number(value.toFixed(1))} MiB` : `${Number((value / 1024).toFixed(1))} GiB`
}

export function formatBytes(bytes: number) {
  if (bytes < 1024)
    return `${bytes} B`
  if (bytes < 1048576)
    return `${Number((bytes / 1024).toFixed(1))} KiB`
  return formatMiB(bytes / 1048576)
}

export function relativeAge(since: number, now = Date.now()) {
  const seconds = Math.max(0, Math.floor((now - since) / 1000))
  if (seconds < 60)
    return 'less than a minute ago'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60)
    return `${minutes} min ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 48)
    return `${hours} h ago`
  return `${Math.floor(hours / 24)} days ago`
}

export type NodeHealthState = 'ready' | 'full' | 'busy' | 'reserved' | 'paused' | 'updating' | 'setup' | 'attention' | 'offline' | 'revoked'

export interface NodeHealth {
  state: NodeHealthState
  label: string
  detail: string
}

/** The server's heartbeat timeout; listing nodes refreshes the built-in runner within it when reachable. */
const HEARTBEAT_TIMEOUT_MS = 60_000

/**
 * The server's clock as seen in an inventory: the latest contact of a machine the server reports online.
 * Device clocks can drift from the server's, so staleness is judged against this rather than `Date.now()`.
 */
export function fleetClock(nodes: ExecutionNode[], runner: RunnerObservation | null = null): number | null {
  const seen = nodes
    .filter(node => !node.local && !node.revoked && node.status === 'online' && node.lastSeen != null)
    .map(node => node.lastSeen!)
  // A runner contact that changed while this device watched was server time then; the device time elapsed
  // until the latest successful listing has passed since.
  if (runner?.refreshed)
    seen.push(runner.lastSeen + runner.checkedAt - runner.observedAt)
  return seen.length ? Math.max(...seen) : null
}

/** The built-in runner's last contact, and when this device first saw that value. */
export interface RunnerObservation {
  lastSeen: number
  observedAt: number
  // The latest successful listing that still showed this contact: only listings confirm it is unchanged.
  checkedAt: number
  // The contact changed while this device watched: it was fresh when observed. A first observation may be old.
  refreshed: boolean
}

/** Keeps the first time each runner contact was seen across inventory refreshes. */
export function observeRunner(nodes: ExecutionNode[], previous: RunnerObservation | null, now = Date.now()): RunnerObservation | null {
  const runner = nodes.find(node => node.local && !node.revoked)
  if (runner?.lastSeen == null)
    return null
  if (previous?.lastSeen === runner.lastSeen)
    return { ...previous, checkedAt: now }
  return {
    lastSeen: runner.lastSeen,
    observedAt: now,
    checkedAt: now,
    refreshed: previous != null,
  }
}

/** How long the built-in runner's contact is known to lag server time, if it is. */
function runnerLag(node: ExecutionNode, clock: number | null) {
  return clock != null && node.lastSeen != null ? clock - node.lastSeen : null
}

/**
 * The built-in runner always reports "local". Listing refreshes its contact (every page refresh, 10 s)
 * only when it answers with usable health, so it is stale when it lags the server clock by the heartbeat
 * timeout, or when its contact has not changed for that long across successful listings. Neither depends on the
 * device clock's offset; a runner already stale when the page opens is found after a minute at most.
 * The failure may be no answer, a mismatched version or missing capabilities.
 */
export function runnerStale(node: ExecutionNode, clock: number | null, runner: RunnerObservation | null = null) {
  if (!node.local || node.revoked || node.lastSeen == null)
    return false
  const lag = runnerLag(node, clock)
  if (lag != null && lag >= HEARTBEAT_TIMEOUT_MS)
    return true
  // Failed listings do not count: they say nothing about the runner.
  return runner?.lastSeen === node.lastSeen && runner.checkedAt - runner.observedAt >= HEARTBEAT_TIMEOUT_MS
}

/** The stale runner's age when a server reference shows it; otherwise only its lower bound is known. */
function runnerSilence(node: ExecutionNode, clock: number | null) {
  const lag = runnerLag(node, clock)
  return lag != null && lag >= HEARTBEAT_TIMEOUT_MS ? relativeAge(node.lastSeen!, clock!) : null
}

/**
 * Whether a node can take work, as one state and a short sentence; the first blocker wins.
 * Online follows the server's 60-second heartbeat timeout; placement wants a heartbeat within 30 seconds,
 * so a machine that just stopped answering can read Ready for up to half a minute.
 */
export function nodeHealth(node: ExecutionNode, now = Date.now(), clock: number | null = null, runner: RunnerObservation | null = null): NodeHealth {
  if (node.revoked)
    return { state: 'revoked', label: 'Revoked', detail: 'No longer has access' }
  // Ages use the server clock when the fleet shows it, so device clock drift does not shift them.
  const reference = clock ?? now
  if (!node.local && node.status === 'offline')
    return { state: 'offline', label: 'Offline', detail: node.lastSeen != null ? `Last contact ${relativeAge(node.lastSeen, Math.max(reference, node.lastSeen))}` : 'Never connected' }
  if (runnerStale(node, clock, runner)) {
    const silence = runnerSilence(node, clock)
    return { state: 'attention', label: 'Needs attention', detail: silence ? `Last usable report ${silence}` : 'No usable report for over a minute' }
  }

  const native = node.capabilities.backend === 'native'
  if (!native && !node.capabilities.kvm)
    return { state: 'attention', label: 'Needs attention', detail: 'KVM is unavailable' }
  if (native && !node.capabilities.ublk)
    return { state: 'attention', label: 'Needs attention', detail: 'ublk is unavailable' }
  // A privilege change blocks placements until the supervisor confirms it. The server reports
  // a failure only for the current request; an older failed attempt is still pending.
  if (native && node.nativeModeStatus === 'failed')
    return { state: 'attention', label: 'Needs attention', detail: 'The privilege change failed' }
  // Supervisors without the privilege protocol never confirm a mode: the status stays pending.
  if (native && node.capabilities.nativeModeProtocol !== 1)
    return { state: 'attention', label: 'Needs attention', detail: 'Update the supervisor to confirm privileges' }
  if (native && node.nativeModeStatus === 'pending')
    return { state: 'updating', label: 'Updating', detail: 'Privilege change in progress' }
  // Optional fields are tested for presence, as in the Kotlin contract.
  if (node.maintenance != null && node.maintenanceError != null)
    return { state: 'attention', label: 'Needs attention', detail: 'Update preparation failed' }
  if (node.maintenance != null)
    return { state: 'updating', label: 'Updating', detail: 'New work waits for the update' }
  // Without a reported error, readiness is usually on its way, as after a budget change until the
  // next heartbeat: it waits rather than alarms. The inventory cannot tell a lasting cause (image,
  // pool, data root) from that wait; the diagnostics list the possible causes in the panel.
  // A missing flag reads as not ready, like the generated Kotlin contract.
  if (!node.executionReady) {
    if (node.budgetError != null)
      return { state: 'attention', label: 'Needs attention', detail: 'The shared budget could not be applied' }
    if (node.updateError != null)
      return { state: 'attention', label: 'Needs attention', detail: 'The update failed' }
    return { state: 'updating', label: 'Not ready', detail: 'The runtime is not ready yet' }
  }

  if (native && node.nativeModeProposal)
    return { state: 'attention', label: 'Needs attention', detail: `An agent requested ${nativeModeLabel(node.nativeModeProposal.mode)}` }
  if (native && (!node.nativeEnabled || !node.nativeAccounts?.length))
    return { state: 'setup', label: 'Setup needed', detail: 'Enable native execution and choose trusted people' }
  if (!node.agents?.length)
    return { state: 'setup', label: 'Setup needed', detail: 'No agent can use this machine yet' }
  if (!node.accepting)
    return { state: 'paused', label: 'Paused', detail: 'New work is paused' }
  if (node.exclusiveReservation)
    return { state: 'reserved', label: 'Reserved', detail: node.exclusiveReservation.reason }
  if (node.pressure != null)
    return { state: 'busy', label: 'Busy', detail: node.pressure === 'memory' ? 'Shared RAM is under pressure' : 'Shared disk space is low' }

  const slots = slotCapacity(node)
  const running = node.occupiedSlots ?? 0
  const free = freeSlots(node)
  if (slots > 0 && free <= 0)
    return { state: 'full', label: 'Full', detail: `All ${slots} slots in use` }
  return { state: 'ready', label: 'Ready', detail: running ? `${running} running · ${free} of ${slots} slots free` : `${free} of ${slots} slots free` }
}

/** A native privilege change keeps the runtime unready until the supervisor confirms it. */
function privilegeReason(node: ExecutionNode) {
  if (node.capabilities.backend !== 'native')
    return null
  if (node.nativeModeStatus === 'failed')
    return 'The privilege change failed: retry it or return to Native protected in Access.'
  if (node.nativeModeStatus === 'pending')
    return node.capabilities.nativeModeProtocol === 1 ? 'A privilege change waits for the supervisor to confirm it.' : 'Update the supervisor to confirm privileges.'
  return null
}

/** Why a node cannot take new work right now; empty when it can. */
export function nodeDiagnostics(node: ExecutionNode, now = Date.now(), clock: number | null = null, runner: RunnerObservation | null = null) {
  if (node.revoked)
    return []
  const reasons: string[] = []
  const reference = clock ?? now
  if (runnerStale(node, clock, runner)) {
    const silence = runnerSilence(node, clock)
    reasons.push(`The built-in runner's last usable report was ${silence ?? 'over a minute ago'}: check that it is running, at the same version as the server.`)
  }

  if (!node.local && node.status === 'offline')
    reasons.push(node.lastSeen != null ? `No contact since ${relativeAge(node.lastSeen, Math.max(reference, node.lastSeen))}: check that the machine is on, online and that the ${node.capabilities.backend === 'native' ? 'cairn-native-node' : 'cairn-node'} service is running.` : 'The machine has never connected: run the installation command on it.')
  if (node.capabilities.backend !== 'native' && !node.capabilities.kvm)
    reasons.push('KVM is unavailable: enable virtualization in the BIOS and load the kvm module.')
  if (node.capabilities.backend === 'native' && !node.capabilities.ublk)
    reasons.push('ublk is unavailable: this node needs a current native runtime and the ublk_drv kernel module (/dev/ublk-control) to mount S3-backed conversation disks.')
  if (node.status !== 'offline' && !node.executionReady)
    reasons.push(privilegeReason(node) ?? (node.budgetError ? `The shared budget could not be applied: ${node.budgetError}` : node.updateError ? `The runtime is not ready: ${node.updateError}` : 'The runtime is not ready yet or does not match the version approved by the master.'))
  if (node.maintenance)
    reasons.push('Maintenance in progress: new work waits until the update finishes.')
  if (node.exclusiveReservation)
    reasons.push(`Exclusive reservation: ${node.exclusiveReservation.reason}. Other work resumes automatically after release.`)
  if (node.pressure)
    reasons.push(node.pressure === 'memory' ? 'Shared RAM is under pressure: new work waits for headroom.' : 'Shared disk space is low: new work waits for headroom.')
  if (!node.accepting)
    reasons.push('New work is paused on this machine (Configure → Accept new work).')
  if (node.capabilities.backend === 'native' && (!node.nativeEnabled || !node.nativeAccounts?.length))
    reasons.push('Native execution needs explicit activation and trusted people in Configure.')
  if (!node.agents?.length)
    reasons.push('No agent can use this machine yet.')
  return reasons
}
