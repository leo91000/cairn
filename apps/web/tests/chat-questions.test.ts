import { describe, expect, it } from 'vitest'
import { questionAnswerInput, questionFields } from '../../../packages/contracts/chats'

describe('agent questionnaires', () => {
  it('accepts multiple choices and a distinct comment without losing option identities', () => {
    const fields = questionFields.parse([{
      id: 'platforms',
      title: 'Which platforms?',
      mode: 'multiple',
      minChoices: 2,
      maxChoices: 3,
      options: [
        { id: 'android', label: 'Android', recommended: true },
        { id: 'web', label: 'Web' },
        { id: 'desktop', label: 'Desktop' },
      ],
    }])
    expect(fields[0]).toMatchObject({ mode: 'multiple', minChoices: 2, maxChoices: 3 })
    expect(fields[0]!.options[0]).toMatchObject({ id: 'android', recommended: true })
    const input = { id: '00000000-0000-4000-8000-000000000001', answers: { platforms: { selected: ['android', 'web'], comment: 'Android first' } } }
    expect(questionAnswerInput.parse(input)).toEqual(input)
  })
})

it('validates a text-only alternative and never counts a comment as a missing choice', async () => {
  const { validQuestionAnswer } = await import('../../../packages/contracts/chats')
  const [field] = questionFields.parse([{
    id: 'q',
    title: 'Pick two',
    mode: 'multiple',
    minChoices: 2,
    maxChoices: 2,
    options: [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }, { id: 'c', label: 'C' }],
  }])
  expect(validQuestionAnswer(field!, { selected: [], comment: 'Another idea' })).toBe(true)
  expect(validQuestionAnswer(field!, { selected: ['a'], comment: 'Another idea' })).toBe(false)
  expect(validQuestionAnswer(field!, { selected: ['a', 'b'], comment: '' })).toBe(true)
  expect(validQuestionAnswer(field!, { selected: ['a', 'b', 'c'], comment: '' })).toBe(false)
  expect(validQuestionAnswer(field!, { selected: ['a', 'a'], comment: '' })).toBe(false)
  expect(validQuestionAnswer(field!, { selected: ['a', 'unknown'], comment: '' })).toBe(false)
  expect(validQuestionAnswer(field!, { selected: [], comment: '  ' })).toBe(false)
})
