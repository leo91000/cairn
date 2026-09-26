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
  runtimeId: string
  lastSeen: number | null
}

export interface NodeEnrollment {
  code: string
  expiresAt: number
}
