import type { HookRuleProblem } from '../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { RuleValue } from '../../../packages/contracts/generated/typescript/RuleValue'

export interface JsonInputDraft { text: string, message: string }
export type JsonDrafts = Record<string, JsonInputDraft>
export function parseRuleJsonInput(text: string): unknown {
  return JSON.parse(text, (_key, value) => {
    if (typeof value === 'number' && !Number.isFinite(value))
      throw new Error('The JSON number is outside the supported range.')
    return value
  })
}

export function jsonInputDraft(text: string, message = 'Enter a valid JSON value.'): JsonDrafts {
  return { '': { text, message } }
}

const draftKey = '_jsonDrafts'
const segment = (key: string | number) => `/${`${key}`.replaceAll('~', '~0').replaceAll('/', '~1')}`

// UI-only syntax state travels with the value through step changes and draft recovery.
// Invalid definitions are checked locally and never sent to the rule APIs.
export function valueJsonDrafts(value: unknown): JsonDrafts {
  if (!value || typeof value !== 'object' || !Object.hasOwn(value, draftKey))
    return {}
  const held = (value as Record<string, unknown>)[draftKey]
  return held && typeof held === 'object'
    ? Object.fromEntries(Object.entries(held).filter((entry): entry is [string, JsonInputDraft] => !!entry[1] && typeof entry[1] === 'object' && typeof (entry[1] as JsonInputDraft).text === 'string' && typeof (entry[1] as JsonInputDraft).message === 'string'))
    : {}
}

export function literalWithJsonDrafts(value: unknown, drafts: JsonDrafts = {}): RuleValue {
  return { type: 'literal', value, ...(Object.keys(drafts).length ? { [draftKey]: drafts } : {}) }
}

export function childJsonDrafts(drafts: JsonDrafts, key: string | number): JsonDrafts {
  const prefix = segment(key)
  return Object.fromEntries(Object.entries(drafts)
    .filter(([path]) => path === prefix || path.startsWith(`${prefix}/`))
    .map(([path, raw]) => [path.slice(prefix.length), raw]))
}

export function replaceChildJsonDrafts(drafts: JsonDrafts, key: string | number, child: JsonDrafts): JsonDrafts {
  const prefix = segment(key)
  return {
    ...Object.fromEntries(Object.entries(drafts).filter(([path]) => path !== prefix && !path.startsWith(`${prefix}/`))),
    ...Object.fromEntries(Object.entries(child).map(([path, raw]) => [`${prefix}${path}`, raw])),
  }
}

export function removeJsonDraftItem(drafts: JsonDrafts, index: number): JsonDrafts {
  const output: JsonDrafts = {}
  for (const [path, raw] of Object.entries(drafts)) {
    const match = /^\/(\d+)(\/.*)?$/.exec(path)
    if (!match) {
      output[path] = raw
      continue
    }

    const at = Number(match[1])
    if (at !== index)
      output[`/${at > index ? at - 1 : at}${match[2] ?? ''}`] = raw
  }

  return output
}

export function ruleJsonDraftProblems(value: unknown): HookRuleProblem[] {
  const problems: HookRuleProblem[] = []

  function visit(current: unknown, path: string) {
    if (!current || typeof current !== 'object')
      return
    if (Array.isArray(current)) {
      for (const [index, item] of current.entries())
        visit(item, `${path}/${index}`)
      return
    }

    const record = current as Record<string, unknown>
    if (record.type === 'literal') {
      for (const [suffix, draft] of Object.entries(valueJsonDrafts(record)))
        problems.push({ path: `${path}/value${suffix}`, message: draft.message })
      return // Literal data can itself contain arbitrary objects resembling rule syntax.
    }

    for (const [key, item] of Object.entries(record))
      visit(item, `${path}${segment(key)}`)
  }

  visit(value, '')
  return problems
}
