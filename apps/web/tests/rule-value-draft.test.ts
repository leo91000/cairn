import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import * as jsonDrafts from '../src/rule-json-drafts'

function valueEditor(initial: any, expects: string) {
  const source = readFileSync(new URL('../src/components/RuleValueInput.vue', import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: 'value-draft-test' }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const model = vue.ref(initial)
  const exports: any = {}
  vm.runInNewContext(code, {
    exports,
    document: { addEventListener() {}, removeEventListener() {} },
    require: (name: string) => name === 'vue' ? { ...vue, useModel: () => model, onBeforeUnmount() {} } : name === '../rule-json-drafts' ? jsonDrafts : {},
  })
  const props = vue.reactive({ label: 'Value', variables: [], expects })
  const input = exports.default.setup(props, { expose() {} })
  return { input, model, props }
}

it('keeps decimal keystrokes while emitting a numeric comparison value', () => {
  const { input, model } = valueEditor({ type: 'literal', value: 0 }, 'number')
  for (const text of ['1', '1.', '1.5', '1.5e', '1.5e2']) {
    input.text.value = text
    expect(input.text.value).toBe(text)
  }

  expect(model.value).toEqual({ type: 'literal', value: 150 })
  model.value = { type: 'literal', value: 8 }
  expect(input.text.value).toBe('8')
})

it('shows JSON string literals as strings and keeps their type while editing', () => {
  for (const value of ['5', 'true', 'null', '{"n":1}']) {
    const { input, model } = valueEditor({ type: 'literal', value }, 'json')
    expect(input.text.value).toBe(JSON.stringify(value))
    input.text.value = JSON.stringify(`${value}6`)
    expect(model.value).toEqual({ type: 'literal', value: `${value}6` })
    input.switchMode('template')
    expect(model.value).toEqual({ type: 'template', text: `${value}6` })
  }
})

it('inserts variables into decoded JSON text and maps escaped caret positions', () => {
  const { input, model } = valueEditor({ type: 'literal', value: 'Avant ' }, 'json')
  input.insert({ expression: 'payload.author' })
  expect(model.value).toEqual({ type: 'template', text: 'Avant {{payload.author}}' })
  for (const [at, expected] of [[4, 'a\n{{payload.author}}b'], [3, 'a{{payload.author}}\nb']] as const) {
    const editor = valueEditor({ type: 'literal', value: 'a\nb' }, 'json')
    editor.input.field.value = {
      selectionStart: at,
      selectionEnd: at,
      style: {},
      scrollHeight: 0,
      focus() {},
      setSelectionRange() {},
    }
    editor.input.insert({ expression: 'payload.author' })
    expect(editor.model.value).toEqual({ type: 'template', text: expected })
  }
})

it('updates string representation when the receiving field changes to JSON', () => {
  const { input, model, props } = valueEditor({ type: 'literal', value: '5' }, 'text')
  input.text.value = '56'
  props.expects = 'json'
  expect(input.text.value).toBe('"56"')
  expect(model.value).toEqual({ type: 'literal', value: '56' })
})

it.each([null, false, 0, '5', [], {}])('keeps the literal meaning when switching %j to a formula', (value) => {
  const { input, model } = valueEditor({ type: 'literal', value }, 'json')
  input.switchMode('formula')
  expect(model.value).toEqual({ type: 'formula', expression: JSON.stringify(value) })
  input.switchMode('literal')
  expect(model.value).toEqual({ type: 'literal', value })
})

it.each([[false, 'boolean'], ['fixed', 'text'], [1.5, 'number']] as const)('preserves the typed constant %j through a formula round trip', (value, expects) => {
  const editor = valueEditor({ type: 'literal', value }, expects)
  editor.input.switchMode('formula')
  editor.input.switchMode('literal')
  expect(editor.model.value).toEqual({ type: 'literal', value })
  expect(editor.input.conversionNotice.value).toBe('')
})

it('explains replacing a nonconstant formula with a fixed value', () => {
  const editor = valueEditor({ type: 'formula', expression: 'payload.active' }, 'boolean')
  editor.input.switchMode('literal')
  expect(editor.input.conversionNotice.value).toContain('Check this value before saving')
})

it.each(['json', 'number'])('keeps an invalid %s draft after recovery and blocks it locally', (expects) => {
  const raw = expects === 'json' ? '{"count":' : '1.5e'
  const editor = valueEditor({ type: 'literal', value: expects === 'json' ? { count: 1 } : 1 }, expects)
  editor.input.text.value = raw
  expect(editor.input.text.value).toBe(raw)
  expect(jsonDrafts.ruleJsonDraftProblems({ steps: [editor.model.value] })).toHaveLength(1)
  const reopened = valueEditor(JSON.parse(JSON.stringify(editor.model.value)), expects)
  expect(reopened.input.text.value).toBe(raw)
  reopened.input.text.value = expects === 'json' ? JSON.stringify(raw) : '1.5e2'
  expect(jsonDrafts.ruleJsonDraftProblems({ steps: [reopened.model.value] })).toEqual([])
  expect(reopened.model.value).toEqual({ type: 'literal', value: expects === 'json' ? raw : 150 })
})
