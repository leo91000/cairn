import { readFile, writeFile } from 'node:fs/promises'
import { z } from 'zod'
import {
  askQuestionsInput,
  questionAnswerInput,
  questionFields,
} from '../packages/contracts/chats.ts'

const schemas = {
  answer: questionAnswerInput,
  questions: questionFields,
  askQuestions: askQuestionsInput,
}
await writeFile('crates/installation/schemas/inputs.json', `${JSON.stringify(Object.fromEntries(Object.entries(schemas).map(([name, schema]) => [name, z.toJSONSchema(schema, { unrepresentable: 'any' })])), null, 2)}\n`)

const tools = JSON.parse(await readFile('crates/installation/schemas/mcp-operations.json', 'utf8'))
for (const tool of tools) {
  if (tool.name === 'answer_chat_question')
    tool.inputSchema.properties.answer = z.toJSONSchema(questionAnswerInput)
}

await writeFile('crates/installation/schemas/mcp-operations.json', `${JSON.stringify(tools, null, 2)}\n`)
