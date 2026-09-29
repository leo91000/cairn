import type { ExecutionNode } from '../shared/nodes'
import { api } from './api'

export interface Placement { nodes: ExecutionNode[], pinnedNodeId: string | null, preferredNodeId: string | null }
// Desktop and phone chips share only the request in flight, never a stale result.
const pending = new Map<string, Promise<Placement>>()
export function loadPlacement(runId: string) {
  let request = pending.get(runId)
  if (!request) {
    request = api<Placement>(`/nodes/placement/${runId}`)
    pending.set(runId, request)
    request.finally(() => pending.delete(runId)).catch(() => {})
  }

  return request
}
