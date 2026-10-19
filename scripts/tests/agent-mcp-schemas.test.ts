import { readFileSync } from 'node:fs'
import { expect, it } from 'vitest'
import { z } from 'zod'
import { agentInput, agentUpdate } from '../../packages/contracts/contracts'

it('publishes the full agent contracts for MCP creation and partial updates', () => {
  const tools = JSON.parse(readFileSync(new URL('../../crates/installation/schemas/mcp-operations.json', import.meta.url), 'utf8'))
  const create = tools.find((tool: { name: string }) => tool.name === 'save_agent').inputSchema
  const update = tools.find((tool: { name: string }) => tool.name === 'update_agent').inputSchema.properties.agent
  const { $schema: _dialect, ...updateContract } = z.toJSONSchema(agentUpdate, { unrepresentable: 'any' })

  expect(create).toEqual(z.toJSONSchema(agentInput, { unrepresentable: 'any' }))
  expect(update).toEqual(updateContract)
})
