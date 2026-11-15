import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'

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
    require: (name: string) => name === 'vue' ? { ...vue, useModel: () => model, onBeforeUnmount() {} } : {},
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
})
