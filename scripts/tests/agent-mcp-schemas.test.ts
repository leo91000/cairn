import { readFileSync } from 'node:fs'
import { expect, it } from 'vitest'
import { z } from 'zod'
import { agentInput, agentUpdate } from '../../packages/contracts/contracts'

// Exclusive node and plugin grants (such as 1Password) are set only from the owner's app.
function withoutOwnerGrants(schema: any) {
  const access = schema.properties.access
  for (const key of ['exclusiveNodes', 'plugins']) {
    delete access.properties[key]
    access.required = access.required?.filter((name: string) => name !== key)
    if (access.default)
      delete access.default[key]
  }

  return schema
}

it('publishes the full agent contracts for MCP creation and partial updates', () => {
  const tools = JSON.parse(readFileSync(new URL('../../crates/installation/schemas/mcp-operations.json', import.meta.url), 'utf8'))
  const create = tools.find((tool: { name: string }) => tool.name === 'save_agent').inputSchema
  const update = tools.find((tool: { name: string }) => tool.name === 'update_agent').inputSchema.properties.agent
  const { $schema: _dialect, ...updateContract } = z.toJSONSchema(agentUpdate, { unrepresentable: 'any' })

  expect(create).toEqual(withoutOwnerGrants(z.toJSONSchema(agentInput, { unrepresentable: 'any' })))
  expect(update).toEqual(withoutOwnerGrants(updateContract))
  expect(JSON.stringify(create)).not.toContain('exclusiveNodes')
  expect(JSON.stringify(update)).not.toContain('"plugins"')
})
