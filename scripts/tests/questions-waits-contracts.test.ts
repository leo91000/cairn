import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import askSchema from '../../packages/contracts/generated/AskQuestionsInput.json'
import questionSchema from '../../packages/contracts/generated/ChatQuestion.json'
import wakeupSchema from '../../packages/contracts/generated/ChatWakeup.json'
import delegateSchema from '../../packages/contracts/generated/DelegateTaskInput.json'
import answerSchema from '../../packages/contracts/generated/QuestionAnswerInput.json'
import corpus from '../../tests/fixtures/questions-waits-contracts.json'

it('qualifies questions, private answers, grouped plugin observations and project choices from Rust schemas', () => {
  const ajv = new Ajv({ strict: false, useDefaults: true })
  addFormats(ajv)
  const ask = ajv.compile(askSchema)
  const delegate = ajv.compile(delegateSchema)
  for (const input of corpus.asks)
    expect(ask(structuredClone(input)), JSON.stringify(ask.errors)).toBe(true)
  for (const input of corpus.invalidAsks)
    expect(ask(structuredClone(input)), JSON.stringify(input)).toBe(false)
  for (const [schema, values] of [[questionSchema, corpus.questions], [answerSchema, corpus.answers], [wakeupSchema, corpus.wakeups]] as const) {
    const validate = ajv.compile(schema)
    for (const value of values)
      expect(validate(structuredClone(value)), JSON.stringify(validate.errors)).toBe(true)
  }

  for (const input of corpus.delegates)
    expect(delegate(structuredClone(input)), JSON.stringify(delegate.errors)).toBe(true)
  for (const input of corpus.invalidDelegates)
    expect(delegate(structuredClone(input)), JSON.stringify(input)).toBe(false)
})
