import type { SideQuestionInput } from '../../packages/contracts/generated/typescript/SideQuestionInput'
import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { askChatSideQuestion } from '../../packages/contracts/generated/askChatSideQuestion'
import schema from '../../packages/contracts/generated/SideQuestionInput.json'

it('accepts an unanchored question without optional context fields', async () => {
  const input = {
    id: 'a1b2c3d4-0000-4000-8000-000000000001',
    text: 'What are you doing?',
  } satisfies SideQuestionInput
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  expect(ajv.compile(schema)(input)).toBe(true)
  let sent = ''
  await askChatSideQuestion(async <T>(_path: string, options: RequestInit): Promise<T> => {
    sent = String(options.body)
    return {} as T
  }, { chatId: 'a1b2c3d4-0000-4000-8000-0000000000c1', question: input })
  expect(JSON.parse(sent)).toEqual(input)
})
