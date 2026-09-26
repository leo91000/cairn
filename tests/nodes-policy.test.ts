import { expect, it } from 'vitest'
import { agentInput, agentUpdate } from '../shared/contracts'
import { LOCAL_NODE_ID } from '../shared/nodes'

it('new agents retain the current runner but partial updates do not grant it', () => {
  expect(agentInput.parse({ name: 'New agent' }).access.nodes).toEqual([LOCAL_NODE_ID])
  expect(agentUpdate.parse({ access: { mcps: [] } }).access).not.toHaveProperty('nodes')
  expect(agentUpdate.parse({ access: { nodes: [] } }).access?.nodes).toEqual([])
  expect(agentUpdate.parse({ access: { nodes: null } }).access?.nodes).toBeNull()
})
