export const LOCAL_NODE_ID = '00000000-0000-4000-8000-000000000002'

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
  capabilities: NodeResources & { os: string, arch: string, kvm: boolean }
  limits: NodeResources
  reserved?: NodeResources
  available?: NodeResources
  executionReady?: boolean
  maintenance?: string | null
  imageDigest?: string | null
  updateError?: string | null
  runtimeId: string
  lastSeen: number | null
}

export interface NodeEnrollment {
  installCommand?: string | null
  code: string
  expiresAt: number
}

export interface NodeBackupSettings {
  destination: 'master' | 's3'
  intervalSeconds: number
  retention: number
  budgetMiB: number
  disconnectTimeoutSeconds: number
  shutdownTimeoutSeconds: number
  maxCapacityWaitSeconds: number
}
