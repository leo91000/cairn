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

/** Why a node cannot take new work right now; empty when it can. */
export function nodeDiagnostics(node: ExecutionNode, now = Date.now()) {
  if (node.revoked)
    return []
  const reasons: string[] = []
  if (!node.local && node.status === 'offline')
    reasons.push(node.lastSeen != null ? `No contact since ${relativeAge(node.lastSeen, now)}: check that the machine is on, online and that the ${node.capabilities.backend === 'native' ? 'cairn-native-node' : 'cairn-node'} service is running.` : 'The machine has never connected: run the installation command on it.')
  if (node.capabilities.backend !== 'native' && !node.capabilities.kvm)
    reasons.push('KVM is unavailable: enable virtualization in the BIOS and load the kvm module.')
  if (node.capabilities.backend === 'native' && !node.capabilities.ublk)
    reasons.push('ublk is unavailable: this node needs a current native runtime and the ublk_drv kernel module (/dev/ublk-control) to mount S3-backed conversation disks.')
  if (node.status !== 'offline' && node.executionReady === false)
    reasons.push(node.budgetError ? `The shared budget could not be applied: ${node.budgetError}` : node.updateError ? `The runtime is not ready: ${node.updateError}` : 'The runtime is not ready yet or does not match the version approved by the master.')
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
