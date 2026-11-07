import { expect, it } from 'vitest'
import { questionAnswerText } from '../src/question-answer'
import { fixtureEntries } from './timeline-fixture'

const text = `My answers to your questions:\n\n${JSON.stringify([{
  questionId: 'platforms',
  question: 'Which platforms?',
  selected: [{ id: 'android', label: 'Android' }, { id: 'web', label: 'Web' }],
  comment: 'Android first\nWeb next',
}])}`

it('presents selected labels and multiline comments in history while preserving ordinary messages', () => {
  const entries = fixtureEntries([{
    id: 1,
    runId: 'run',
    createdAt: 1,
    type: 'chat.user',
    text,
    payload: { questionId: 'question', text },
  }], true)
  expect(entries[0]).toMatchObject({ text: 'Your answers\n\nWhich platforms?\nChoices: Android, Web\nComment: Android first\nWeb next' })
  expect(questionAnswerText(text)).toBe(text)
  expect(questionAnswerText('Private answer supplied', 'question')).toBe('Private answer supplied')
  expect(questionAnswerText('My answers to your questions:\n\n[{"question":"Old format"}]', 'question')).toContain('Old format')
})
