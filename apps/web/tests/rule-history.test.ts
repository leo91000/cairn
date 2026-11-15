import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it, vi } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'

it('keeps an older history page when the poll fires before its response arrives', async () => {
  const source = readFileSync(new URL('../src/components/RuleHistory.vue', import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: 'history-test' }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  let mounted = () => {}
  let poll = () => {}
  let deliverOlder!: (value: unknown) => void
  const older = new Promise((resolve) => {
    deliverOlder = resolve
  })
  const list = vi.fn(async (_api, input) => input.cursor
    ? older
    : {
        triggerings: [{ id: 'newer' }],
        withoutEffect: 0,
        nextCursor: 'older',
      })
  const exports: any = {}
  vm.runInNewContext(code, {
    exports,
    document: { hidden: false },
    setInterval: (callback: () => void) => {
      poll = callback
      return 1
    },
    require: (path: string) => {
      if (path === 'vue') {
        return {
          ...vue,
          onMounted: (callback: () => void) => { mounted = callback },
          onBeforeUnmount() {},
          watch() {},
        }
      }

      if (path.endsWith('/listHookRuleTriggerings'))
        return { listHookRuleTriggerings: list }
      return {}
    },
  })
  const history = exports.default.setup({ ruleId: 'rule' }, { expose() {} })
  mounted()
  for (let turn = 0; turn < 10 && history.loading.value; turn++)
    await Promise.resolve()
  expect(history.next.value).toBe('older')
  const request = history.load(true)
  poll()
  expect(list).toHaveBeenCalledTimes(2)
  deliverOlder({ triggerings: [{ id: 'older' }], withoutEffect: 0, nextCursor: null })
  await request
  expect(history.triggerings.value.map((entry: any) => entry.id)).toEqual(['newer', 'older'])
})
