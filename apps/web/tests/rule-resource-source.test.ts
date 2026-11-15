import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it, vi } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'

it('scopes resource reads to the chosen agent and discards replies from the previous agent', async () => {
  vi.useFakeTimers()
  const pending: Array<{ input: any, resolve: (value: any) => void }> = []
  const reads = vi.fn((_request: unknown, input: any) => new Promise(resolve => pending.push({ input, resolve })))
  const source = readFileSync(new URL('../src/components/RuleResources.vue', import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: 'resource-source-test' }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const model = vue.ref([{ resourceType: 'person', id: 'alice' }])
  const exports: any = {}
  const scope = vue.effectScope()
  try {
    vm.runInNewContext(code, {
      exports,
      setTimeout,
      clearTimeout,
      require: (name: string) => name === 'vue'
        ? { ...vue, useModel: () => model, onBeforeUnmount() {} }
        : { listPluginResources: reads, resolvePluginResource: reads },
    })
    const props = vue.reactive({
      pluginId: 'team',
      instanceId: 'messages',
      resourceType: 'person',
      agentId: 'first',
      label: 'People',
    })
    const input = scope.run(() => exports.default.setup(props, { expose() {} }))
    input.open.value = true
    await vue.nextTick()
    await vi.advanceTimersByTimeAsync(200)
    expect(pending.map(item => item.input.agentId)).toEqual(['first', 'first'])

    props.agentId = 'second'
    await vue.nextTick()
    await vi.advanceTimersByTimeAsync(200)
    expect(pending.map(item => item.input.agentId)).toEqual(['first', 'first', 'second', 'second'])
    pending[2].resolve({ resource: { resourceType: 'person', id: 'alice', name: 'Alice for second' } })
    pending[3].resolve({ items: [{ resourceType: 'person', id: 'bob', name: 'Bob for second' }], nextPage: null })
    await vue.nextTick()
    pending[0].resolve({ resource: { resourceType: 'person', id: 'alice', name: 'Old Alice' } })
    pending[1].resolve({ items: [{ resourceType: 'person', id: 'alice', name: 'Old Alice' }], nextPage: null })
    await vue.nextTick()
    expect(input.names.value).toEqual({ 'person:alice': 'Alice for second', 'person:bob': 'Bob for second' })
    expect(input.items.value.map((item: any) => item.id)).toEqual(['bob'])
    expect(input.loading.value).toBe(false)
  }
  finally {
    scope.stop()
    vi.useRealTimers()
  }
})
