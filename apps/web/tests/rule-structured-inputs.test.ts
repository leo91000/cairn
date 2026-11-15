import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import * as rules from '../src/hook-rules'
import * as schemas from '../src/rule-schema'

function component(name: string, inlineTemplate = false, extraVue = {}) {
  const source = readFileSync(new URL(`../src/components/${name}.vue`, import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: name, inlineTemplate }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const exports: any = {}
  vm.runInNewContext(code, {
    exports,
    require: (dependency: string) => {
      if (dependency === 'vue')
        return { ...vue, ...extraVue }
      if (dependency === '../rule-schema')
        return schemas
      if (dependency === '../hook-rules')
        return rules
      return { __esModule: true, default: { render: () => null } }
    },
  })
  return exports.default
}

interface HostNode {
  tag: string
  props: Record<string, any>
  children: HostNode[]
  parent?: HostNode
  text?: string
}

function literalEditor(schema: any, initial: any, depth: number) {
  const node = (tag: string): HostNode => ({ tag, props: {}, children: [] })
  const renderer = vue.createRenderer<HostNode, HostNode>({
    createElement: node,
    createText: text => ({ ...node('#text'), text }),
    createComment: text => ({ ...node('#comment'), text }),
    setText: (held, text) => { held.text = text },
    setElementText: (held, text) => {
      held.text = text
      held.children = []
    },
    parentNode: held => held.parent ?? null,
    nextSibling: held => held.parent?.children[held.parent.children.indexOf(held) + 1] ?? null,
    patchProp: (held, key, _previous, value) => { held.props[key] = value },
    insert: (held, parent, anchor) => {
      if (held.parent)
        held.parent.children = held.parent.children.filter(child => child !== held)
      held.parent = parent
      const index = anchor ? parent.children.indexOf(anchor) : -1
      parent.children.splice(index < 0 ? parent.children.length : index, 0, held)
    },
    remove: (held) => {
      if (held.parent)
        held.parent.children = held.parent.children.filter(child => child !== held)
    },
  })
  const model = vue.ref(initial)
  const field = component('RuleLiteralFields', true)
  const app = renderer.createApp({
    setup: () => () => vue.h(field, {
      schema,
      depth,
      'label': 'Nested list',
      'modelValue': model.value,
      'onUpdate:modelValue': (value: unknown) => { model.value = value },
    }),
  })
  const root = node('root')
  app.mount(root)
  const descendants = (held: HostNode): HostNode[] => [held, ...held.children.flatMap(descendants)]
  return {
    app,
    model,
    controls: () => descendants(root).filter(held => ['input', 'textarea'].includes(held.tag)),
    buttons: () => descendants(root).filter(held => held.tag === 'button'),
  }
}

it.each([false, true])('keeps the deepest list editable as JSON (nested=%s)', async (nested) => {
  let schema: any = { type: 'array', items: { type: 'integer' } }
  let initial: any = [1, 2]
  let expected: any = [3, 4]
  if (nested) {
    for (let level = 0; level < 8; level++) {
      schema = { type: 'array', items: schema }
      initial = [initial]
      expected = [expected]
    }
  }

  const editor = literalEditor(schema, initial, nested ? 0 : 8)
  try {
    const input = editor.controls()[0]
    expect(input.tag).toBe('textarea')
    expect(JSON.parse(input.props.value)).toEqual([1, 2])
    input.props.onInput({ target: { value: '[' } })
    await vue.nextTick()
    expect(editor.controls()[0].props.value).toBe('[')
    editor.controls()[0].props.onInput({ target: { value: '[3,4]' } })
    await vue.nextTick()
    expect(editor.model.value).toEqual(expected)
    expect(editor.controls()[0].props.value).toBe('[3,4]')
  }
  finally {
    editor.app.unmount()
  }
})

it.each(['integer', 'boolean'])('initializes a comparison with a typed first field (%s)', (type) => {
  const model = vue.ref<any>({ type: 'all', conditions: [] })
  const scope = vue.effectScope()
  try {
    const editor = component('RuleConditionEditor', false, { useModel: () => model })
    const props = { variables: [{ group: 'Event', expression: 'payload.first', type }], path: '/condition', problems: [] }
    const input = scope.run(() => editor.setup(props, { expose() {} }))
    input.add('compare')
    expect(model.value.conditions[0].left).toEqual({ type: 'formula', expression: 'payload.first' })
    expect(model.value.conditions[0].right).toEqual({ type: 'literal', value: type === 'integer' ? 0 : false })
  }
  finally {
    scope.stop()
  }
})

it.each(['new', 'change', 'optional'])('keeps the root schema when creating nested tool defaults (%s)', (mode) => {
  const schema = {
    type: 'object',
    required: ['options'],
    properties: { options: { type: 'object', required: ['style'], properties: { style: { $ref: '#/$defs/style' } } } },
    $defs: { style: { type: 'string', enum: ['plain', 'emphasis'] } },
  }
  const catalog: any = {
    tools: [{
      pluginId: 'team',
      instanceId: 'messages',
      tool: 'post',
      agentIds: ['agent'],
      inputSchema: schema,
    }],
  }
  if (mode === 'new') {
    const step = rules.newStep({ executionAgentId: 'agent', steps: [] } as any, 'callTool', catalog)
    expect((step as any).action.input.options).toEqual({ type: 'literal', value: { style: 'plain' } })
    return
  }

  const model = vue.ref<any>({
    type: 'action',
    id: 'post',
    action: {
      type: 'callTool',
      instanceId: 'messages',
      tool: 'post',
      input: {},
    },
  })
  const scope = vue.effectScope()
  try {
    const editor = component('RuleStepEditor', false, { useModel: () => model })
    const input = scope.run(() => editor.setup({
      catalog,
      executionAgentId: 'agent',
      path: '/steps/0',
      variables: [],
      problems: [],
    }, { expose() {} }))
    if (mode === 'optional') {
      expect(input.defaultValue({ schema: schema.properties.options, expects: 'json' })).toEqual({ type: 'literal', value: { style: 'plain' } })
      return
    }

    input.chooseTool('messages:post')
    expect(model.value.action.input.options).toEqual({ type: 'literal', value: { style: 'plain' } })
  }
  finally {
    scope.stop()
  }
})

it('adds list items with defaults resolved from the root schema', async () => {
  const schema = {
    type: 'array',
    items: { $ref: '#/$defs/item' },
    $defs: {
      item: { type: 'object', required: ['style'], properties: { style: { $ref: '#/$defs/style' } } },
      style: { type: 'string', enum: ['plain', 'emphasis'] },
    },
  }
  const editor = literalEditor(schema, [], 0)
  try {
    editor.buttons()[0].props.onClick()
    await vue.nextTick()
    expect(editor.model.value).toEqual([{ style: 'plain' }])
  }
  finally {
    editor.app.unmount()
  }
})
