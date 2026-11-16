import { expect, it } from 'vitest'
import { typedComparisonRight } from '../src/hook-rules'
import {
  childJsonDrafts,
  jsonInputDraft,
  literalWithJsonDrafts,
  parseRuleJsonInput,
  removeJsonDraftItem,
  replaceChildJsonDrafts,
  ruleJsonDraftProblems,
} from '../src/rule-json-drafts'

it('rejects malformed and overflowing JSON instead of coercing it to text or null', () => {
  for (const raw of ['plain', '01', '[plain]', '1e999', '{"x":1e999}'])
    expect(() => parseRuleJsonInput(raw)).toThrow()
  for (const raw of ['null', 'true', 'false', '-1.25e+2', '"plain"', '[]', '{"x":[1,"text",null]}'])
    expect(parseRuleJsonInput(raw)).toEqual(JSON.parse(raw))
})

it('keeps syntax intent with the draft and ignores lookalike keys in literal data', () => {
  const invalid = literalWithJsonDrafts('{"count":', jsonInputDraft('{"count":'))
  for (const expects of ['json', 'number', 'boolean', 'text'] as const)
    expect(typedComparisonRight(invalid, expects, false)).toBe(invalid)
  const definition = { steps: [{ type: 'action', action: { type: 'storeValue', value: invalid } }] }
  expect(ruleJsonDraftProblems(JSON.parse(JSON.stringify(definition)))).toEqual([{ path: '/steps/0/action/value/value', message: 'Enter a valid JSON value.' }])
  expect(ruleJsonDraftProblems(literalWithJsonDrafts({ type: 'literal', _jsonDrafts: { '': { text: 'data', message: 'also data' } } }))).toEqual([])
  expect(ruleJsonDraftProblems(literalWithJsonDrafts('{"count":'))).toEqual([])
})

it('moves nested errors with array items and removes only the edited subtree', () => {
  let drafts = replaceChildJsonDrafts({}, 1, replaceChildJsonDrafts({}, 'a/b~c', jsonInputDraft('')))
  drafts = replaceChildJsonDrafts(drafts, 2, jsonInputDraft('['))
  drafts = removeJsonDraftItem(drafts, 0)
  expect(childJsonDrafts(childJsonDrafts(drafts, 0), 'a/b~c')).toEqual(jsonInputDraft(''))
  expect(childJsonDrafts(drafts, 1)).toEqual(jsonInputDraft('['))
  drafts = replaceChildJsonDrafts(drafts, 0, {})
  expect(childJsonDrafts(drafts, 0)).toEqual({})
  expect(childJsonDrafts(drafts, 1)).toEqual(jsonInputDraft('['))
  expect(removeJsonDraftItem(drafts, 1)).toEqual({})
})
