import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'

it('keeps decimal keystrokes while emitting a numeric comparison value', () => {
  const source = readFileSync(new URL('../src/components/RuleValueInput.vue', import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: 'value-draft-test' }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const model = vue.ref({ type: 'literal', value: 0 })
  const exports: any = {}
  vm.runInNewContext(code, {
    exports,
    document: { addEventListener() {}, removeEventListener() {} },
    require: (name: string) => name === 'vue' ? { ...vue, useModel: () => model, onBeforeUnmount() {} } : {},
  })
  const input = exports.default.setup({ label: 'Value', variables: [], expects: 'number' }, { expose() {} })
  for (const text of ['1', '1.', '1.5', '1.5e', '1.5e2']) {
    input.text.value = text
    expect(input.text.value).toBe(text)
  }

  expect(model.value).toEqual({ type: 'literal', value: 150 })
  model.value = { type: 'literal', value: 8 }
  expect(input.text.value).toBe('8')
})
