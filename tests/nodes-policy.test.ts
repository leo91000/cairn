import type { ExecutionNode } from '../shared/nodes'
import { expect, it } from 'vitest'
import { agentInput, agentUpdate } from '../shared/contracts'
import { formatMiB, LOCAL_NODE_ID, nodeDiagnostics, relativeAge } from '../shared/nodes'

it('new agents retain the current runner but partial updates do not grant it', () => {
  expect(agentInput.parse({ name: 'New agent' }).access.nodes).toEqual([LOCAL_NODE_ID])
  expect(agentUpdate.parse({ access: { mcps: [] } }).access).not.toHaveProperty('nodes')
  expect(agentUpdate.parse({ access: { nodes: [] } }).access?.nodes).toEqual([])
  expect(agentUpdate.parse({ access: { nodes: null } }).access?.nodes).toBeNull()
})

it('formats node sizes, recovery ages and blocking reasons for people', () => {
  expect(formatMiB(512)).toBe('512 MiB')
  expect(formatMiB(32768)).toBe('32 GiB')
  expect(formatMiB(15872)).toBe('15.5 GiB')
  expect(relativeAge(0, 30_000)).toBe('less than a minute ago')
  expect(relativeAge(0, 3 * 60_000)).toBe('3 min ago')
  expect(relativeAge(0, 5 * 3_600_000)).toBe('5 h ago')
  const node: ExecutionNode = { id: 'n', name: 'Desktop', local: false, accepting: true, revoked: false, status: 'online', tags: [], capabilities: { os: 'linux', arch: 'x86_64', kvm: true, cpu: 8, memoryMiB: 16384, diskMiB: 65536 }, limits: { cpu: 7, memoryMiB: 15872, diskMiB: 52428 }, executionReady: true, runtimeId: 'r', lastSeen: 0, agents: [{ id: 'a', name: 'Main', allNodes: false }] }
  expect(nodeDiagnostics(node)).toEqual([])
  expect(nodeDiagnostics({ ...node, agents: [] })).toEqual(['No agent can use this machine yet.'])
  expect(nodeDiagnostics({ ...node, status: 'offline', lastSeen: 0 }, 10 * 60_000)[0]).toContain('No contact since 10 min ago')
})
