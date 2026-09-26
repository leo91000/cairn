import { readFile, writeFile } from 'node:fs/promises'
import { z } from 'zod'
import { chatInput, chatMessageInput, questionAnswerInput, questionFields } from '../shared/chats.ts'
import { agentInput, agentUpdate, projectInput, taskInput } from '../shared/contracts.ts'
import { mcpInput } from '../shared/mcp.ts'

const schemas = { agent: agentInput, project: projectInput, task: taskInput, chat: chatInput, message: chatMessageInput, answer: questionAnswerInput, questions: questionFields, mcp: mcpInput }
await writeFile('backend/schemas/inputs.json', `${JSON.stringify(Object.fromEntries(Object.entries(schemas).map(([name, schema]) => [name, z.toJSONSchema(schema, { unrepresentable: 'any' })])), null, 2)}\n`)

// Keep both owner interfaces aligned, including the distinction between creation
// defaults and partial updates that must preserve existing node restrictions.
const tools = JSON.parse(await readFile('backend/schemas/mcp-tools.json', 'utf8'))
for (const [name, schema] of Object.entries({ save_agent: agentInput, update_agent: agentUpdate })) {
  const tool = tools.find(tool => tool.name === name)
  if (!tool)
    throw new Error(`Missing MCP tool: ${name}`)
  const properties = name === 'save_agent' ? tool.inputSchema.properties : tool.inputSchema.properties.agent.properties
  properties.access = z.toJSONSchema(schema).properties.access
}
await writeFile('backend/schemas/mcp-tools.json', `${JSON.stringify(tools, null, 2)}\n`)
