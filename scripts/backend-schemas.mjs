import { readFile, writeFile } from 'node:fs/promises'
import { z } from 'zod'
import {
  chatInput,
  chatMessageInput,
  questionAnswerInput,
  questionFields,
} from '../packages/contracts/chats.ts'
import {
  agentInput,
  agentUpdate,
  projectInput,
  taskInput,
} from '../packages/contracts/contracts.ts'
import { mcpInput } from '../packages/contracts/mcp.ts'

const schemas = {
  agent: agentInput,
  project: projectInput,
  task: taskInput,
  chat: chatInput,
  message: chatMessageInput,
  answer: questionAnswerInput,
  questions: questionFields,
  mcp: mcpInput,
}
await writeFile('crates/installation/schemas/inputs.json', `${JSON.stringify(Object.fromEntries(Object.entries(schemas).map(([name, schema]) => [name, z.toJSONSchema(schema, { unrepresentable: 'any' })])), null, 2)}\n`)

// Derive the full agent payloads from the product contracts. Creation applies
// defaults; partial updates preserve omitted settings, including node restrictions.
const tools = JSON.parse(await readFile('crates/installation/schemas/mcp-tools.json', 'utf8'))
for (const [name, schema] of Object.entries({ save_agent: agentInput, update_agent: agentUpdate })) {
  const tool = tools.find(tool => tool.name === name)
  if (!tool)
    throw new Error(`Missing MCP tool: ${name}`)
  const inputSchema = z.toJSONSchema(schema, { unrepresentable: 'any' })
  if (name === 'save_agent') {
    tool.inputSchema = inputSchema
  }
  else {
    delete inputSchema.$schema
    tool.inputSchema.properties.agent = inputSchema
  }
}

await writeFile('crates/installation/schemas/mcp-tools.json', `${JSON.stringify(tools, null, 2)}\n`)
