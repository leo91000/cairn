export const LOCAL_NODE_ID = '00000000-0000-4000-8000-000000000002'

// The backend applies the same defaults when a conversation has not requested resources.
export const DEFAULT_RESOURCES = { cpu: 2, memoryMiB: 4096, diskMiB: 32768 }

export interface NodeResources {
  cpu: number
  memoryMiB: number
  diskMiB: number
}

export interface ExecutionNode {
  id: string
  name: string
  local: boolean
  accepting: boolean
  revoked: boolean
  status: 'online' | 'offline' | 'revoked' | 'local'
  tags: string[]
  systemTags?: string[]
  capabilities: NodeResources & {
    os: string
    arch: string
    kvm: boolean
    fuse?: boolean
  }
  limits: NodeResources
  reserved?: NodeResources
  available?: NodeResources
  executionReady?: boolean
  maintenance?: string | null
  maintenanceError?: string | null
  imageDigest?: string | null
  updateError?: string | null
  storage?: NodeStoragePolicy
  runtimeId: string
  lastSeen: number | null
  agents?: NodeAgent[]
  // Disk kept on the node that no conversation needs there any more.
  staleDisks?: { count: number, diskMiB: number }
}

export interface NodeAgent {
  id: string
  name: string
  allNodes: boolean
}

export interface NodeEnrollment {
  installCommand?: string | null
  code: string
  expiresAt: number
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

export function formatResources(value: NodeResources) {
  return `${value.cpu} CPU · ${formatMiB(value.memoryMiB)} RAM · ${formatMiB(value.diskMiB)} disk`
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
    reasons.push(node.lastSeen != null ? `No contact since ${relativeAge(node.lastSeen, now)}: check that the machine is on, online and that the leo-node service is running.` : 'The machine has never connected: run the installation command on it.')
  if (!node.capabilities.kvm)
    reasons.push('KVM is unavailable: enable virtualization in the BIOS and load the kvm module.')
  if (!node.capabilities.fuse)
    reasons.push('FUSE is unavailable: this node cannot mount S3-backed conversation disks.')
  if (node.status !== 'offline' && node.executionReady === false)
    reasons.push(node.updateError ? `The runtime is not ready: ${node.updateError}` : 'The runtime is not ready yet or does not match the version approved by the master.')
  if (node.maintenance)
    reasons.push('Maintenance in progress: new work waits until the update finishes.')
  if (!node.accepting)
    reasons.push('New work is paused on this machine (Configure → Accept new work).')
  if (!node.agents?.length)
    reasons.push('No agent can use this machine yet.')
  return reasons
}

export interface NodeStoragePolicy {
  cacheMiB: number
  reserveMiB: number
  reservePercent: number
  backupSeconds: number
  maxDirtySeconds: number
}
