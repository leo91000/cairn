import { readFile, writeFile } from 'node:fs/promises'
import { z } from 'zod'
import {
  chatInput,
  chatMessageInput,
  questionAnswerInput,
  questionFields,
} from '../packages/contracts/chats.ts'
import {
  taskInput,
} from '../packages/contracts/contracts.ts'
import { mcpInput } from '../packages/contracts/mcp.ts'

const schemas = {
  task: taskInput,
  chat: chatInput,
  message: chatMessageInput,
  answer: questionAnswerInput,
  questions: questionFields,
  mcp: mcpInput,
}
await writeFile('crates/installation/schemas/inputs.json', `${JSON.stringify(Object.fromEntries(Object.entries(schemas).map(([name, schema]) => [name, z.toJSONSchema(schema, { unrepresentable: 'any' })])), null, 2)}\n`)

const tools = JSON.parse(await readFile('crates/installation/schemas/mcp-operations.json', 'utf8'))
for (const tool of tools) {
  if (tool.name === 'create_chat')
    tool.inputSchema = z.toJSONSchema(chatInput)
  if (['send_chat_message', 'edit_chat_message'].includes(tool.name)) {
    const message = z.toJSONSchema(chatMessageInput, { unrepresentable: 'any' })
    if (tool.name === 'edit_chat_message') {
      delete message.properties.id
      // An edit without a speed keeps the stored one, so none is filled in.
      delete message.properties.speed.default
      message.required = message.required.filter(key => !['id', 'speed'].includes(key))
    }

    tool.inputSchema.properties.message = message
  }

  if (tool.name === 'answer_chat_question')
    tool.inputSchema.properties.answer = z.toJSONSchema(questionAnswerInput)
}

await writeFile('crates/installation/schemas/mcp-operations.json', `${JSON.stringify(tools, null, 2)}\n`)
