import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import {
  referencedToolRulesFixture,
  rulesFixture,
  scalarRulesFixture,
  untypedTextRulesFixture,
} from './rules-fixture'

for (const kind of ['boolean', 'text'] as const) {
  test(`a constant mode round trip keeps its saved ${kind} value`, async ({ page, workspace }) => {
    const fixture = await (kind === 'boolean' ? scalarRulesFixture(workspace, 'boolean') : rulesFixture(workspace))
    const call = fixture.definition.steps[0].then?.[0]
    const step = kind === 'boolean'
      ? {
          type: 'if',
          id: 'constant',
          condition: {
            type: 'compare',
            left: { type: 'formula', expression: 'payload.first' },
            operator: 'equals',
            right: { type: 'literal', value: false },
          },
          then: [],
          otherwise: [],
        }
      : { ...call, label: 'Constant text', action: { ...call.action, input: { ...call.action.input, text: { type: 'literal', value: 'fixed' } } } }
    const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, steps: [step] }, enabled: false })
    await page.goto(workspacePath(`/rules/${rule.id}/edit`))
    await signIn(page)
    await page.getByRole('button', { name: kind === 'boolean' ? 'Edit constant' : 'Edit Constant text', exact: true }).click()
    const modes = page.getByRole('group', { name: kind === 'boolean' ? 'How to fill Value' : 'How to fill Message', exact: true })
    await modes.getByRole('button', { name: 'ƒ', exact: true }).click()
    await modes.getByRole('button', { name: 'Value', exact: true }).click()
    if (kind === 'boolean')
      await expect(page.getByRole('button', { name: 'No', exact: true })).toHaveAttribute('aria-pressed', 'true')
    else
      await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('fixed')
    await page.getByRole('button', { name: 'Close the step', exact: true }).click()
    await page.getByRole('button', { name: 'Save version', exact: true }).click()
    await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
    const saved = (await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0]
    expect(kind === 'boolean' ? saved.condition.right : saved.action.input.text).toEqual({ type: 'literal', value: kind === 'boolean' ? false : 'fixed' })
  })
}

for (const kind of ['stored JSON', 'schema JSON', 'number'] as const) {
  test(`an invalid literal draft blocks saving and survives closing its step (${kind})`, async ({ page, workspace }) => {
    const numeric = kind === 'number'
    const fixture = await (kind === 'schema JSON' ? untypedTextRulesFixture(workspace) : numeric ? scalarRulesFixture(workspace, 'integer') : rulesFixture(workspace))
    const call = fixture.definition.steps[0].then?.[0]
    const step = kind === 'schema JSON'
      ? { ...call, label: 'Syntax check', action: { ...call.action, input: { ...call.action.input, text: { type: 'literal', value: 'hello' } } } }
      : numeric
        ? {
            type: 'if',
            id: 'syntax',
            condition: {
              type: 'compare',
              left: { type: 'formula', expression: 'payload.first' },
              operator: 'greaterThan',
              right: { type: 'literal', value: 1 },
            },
            then: [],
            otherwise: [],
          }
        : {
            type: 'action',
            id: 'syntax',
            label: 'Syntax check',
            action: {
              type: 'storeValue',
              scope: 'rule',
              name: 'last',
              value: { type: 'literal', value: { count: 1 } },
            },
          }
    const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, steps: [step] }, enabled: false })
    await page.goto(workspacePath(`/rules/${rule.id}/edit`))
    await signIn(page)
    const open = () => page.getByRole('button', { name: numeric ? 'Edit syntax' : 'Edit Syntax check', exact: true }).click()
    await open()
    const field = page.getByRole('textbox', { name: kind === 'schema JSON' ? 'Message (JSON)' : 'Value', exact: true })
    const raw = numeric ? '1.5e' : '{"count":'
    await field.fill(raw)
    if (numeric) {
      await page.getByRole('combobox', { name: 'Test', exact: true }).selectOption('lessThan')
      await page.getByRole('textbox', { name: 'Field', exact: true }).fill('payload.first + 0')
      await expect(field).toHaveValue(raw)
    }

    await expect(page.getByRole('button', { name: 'Save version', exact: true })).toBeDisabled()
    await page.getByRole('button', { name: 'Close the step', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Save version', exact: true })).toBeDisabled()
    expect((await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(1)
    await open()
    await expect(field).toHaveValue(raw)
    // An intentionally quoted JSON string is valid and must not keep the syntax marker.
    const fixed = numeric ? '150' : JSON.stringify(raw)
    await field.fill(fixed)
    await page.getByRole('button', { name: 'Close the step', exact: true }).click()
    await page.getByRole('button', { name: 'Save version', exact: true }).click()
    await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
    const saved = (await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0]
    const value = kind === 'schema JSON' ? saved.action.input.text : numeric ? saved.condition.right : saved.action.value
    expect(value).toEqual({ type: 'literal', value: numeric ? 150 : raw })
  })
}

test('starting an agent associates the event author and clears optional values', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      name: 'Conversation authors',
      steps: [{
        type: 'action',
        id: 'start',
        label: 'Start a conversation',
        action: {
          type: 'startAgent',
          agentId: fixture.agent.id,
          prompt: { type: 'template', text: 'Answer this message.' },
          key: { type: 'template', text: 'support:{{payload.channel}}' },
        },
      }],
    },
    enabled: false,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit Start a conversation', exact: true }).click()
  await page.getByRole('textbox', { name: 'External author (optional)', exact: true }).fill('payload.author')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  expect(saved.definition.steps[0].action.author).toEqual({ type: 'formula', expression: 'payload.author' })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await page.getByRole('button', { name: 'Edit Start a conversation', exact: true }).click()
  await page.getByRole('textbox', { name: 'External author (optional)', exact: true }).fill('')
  await page.getByRole('textbox', { name: 'Conversation key (optional)', exact: true }).fill('')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(3)
  const cleared = (await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].action
  expect(cleared.author ?? null).toBeNull()
  expect(cleared.key ?? null).toBeNull()
})

for (const free of [false, true]) {
  test(`${free ? 'free ' : ''}schema text selection is replaced by a variable and the saved message executes`, async ({ page, workspace }) => {
    const fixture = await (free ? untypedTextRulesFixture(workspace) : rulesFixture(workspace))
    const call = fixture.definition.steps[0].then[0]
    const rule = await workspace.api('/api/hook-rules', 'POST', {
      definition: { ...fixture.definition, name: 'Selected message text', steps: [{ ...call, action: { ...call.action, input: { ...call.action.input, text: { type: 'literal', value: 'Hello Alice!' } } } }] },
      enabled: true,
    })
    await page.goto(workspacePath(`/rules/${rule.id}/edit`))
    await signIn(page)
    await page.getByRole('button', { name: 'Edit Acknowledge the message', exact: true }).click()
    const message = page.getByRole('textbox', { name: free ? 'Message (JSON)' : 'Message', exact: true })
    await expect(message).toHaveValue(free ? JSON.stringify('Hello Alice!') : 'Hello Alice!')
    await message.evaluate((element: HTMLInputElement | HTMLTextAreaElement, offset: number) => {
      element.focus()
      element.setSelectionRange(6 + offset, 11 + offset)
    }, free ? 1 : 0)
    await page.getByRole('button', { name: 'Insert a variable into Message', exact: true }).click()
    await page.getByRole('dialog', { name: 'Variables for Message', exact: true }).getByRole('button').filter({ has: page.getByText('author', { exact: true }) }).click()
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Hello {{payload.author}}!')
    await page.getByRole('button', { name: 'Close the step', exact: true }).click()
    await page.getByRole('button', { name: 'Save version', exact: true }).click()
    await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
    expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].action.input.text).toEqual({ type: 'template', text: 'Hello {{payload.author}}!' })
    workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'Selection' }, 'selected-message')
    let triggering: any
    await expect.poll(async () => {
      triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
      return triggering?.status
    }).toBe('completed')
    const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
    expect(JSON.stringify(history.steps[0].result)).toContain('Hello alice!')
  })
}

test('editing a literal list loop keeps list semantics through execution', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      name: 'Constant list',
      steps: [{
        type: 'forEach',
        id: 'numbers',
        items: { type: 'literal', value: [1, 2] },
        itemName: 'entry',
        body: [{
          type: 'action',
          id: 'last',
          action: {
            type: 'storeValue',
            scope: 'rule',
            name: 'latest',
            value: { type: 'formula', expression: 'entry' },
          },
        }],
      }],
    },
    enabled: true,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit numbers', exact: true }).click()
  const items = page.getByRole('textbox', { name: 'List to go through', exact: true })
  await expect(items).toHaveValue('[1,2]')
  await items.fill('[1,3]')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].items).toEqual({ type: 'formula', expression: '[1,3]' })
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'Iterate this list' }, 'constant-list')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.filter((step: any) => step.nodeId === 'last').map((step: any) => step.result)).toEqual([1, 3])
})

test('a tool with a referenced root schema exposes its fields and executes the saved values', async ({ page, workspace }) => {
  const fixture = await referencedToolRulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      name: 'Referenced tool input',
      steps: [{
        type: 'action',
        id: 'remember',
        action: {
          type: 'storeValue',
          scope: 'rule',
          name: 'last',
          value: { type: 'literal', value: true },
        },
      }],
    },
    enabled: true,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Add a step here', exact: true }).last().click()
  await page.getByRole('menuitem', { name: /Call a plugin tool/ }).click()
  await page.getByRole('textbox', { name: 'Channel', exact: true }).fill('support')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('From the referenced schema')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  const call = saved.definition.steps.find((node: any) => node.type === 'action' && node.action.type === 'callTool')
  expect(call.action.input.text).toEqual({ type: 'literal', value: 'From the referenced schema' })
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'Root schema event' }, 'root-schema-event')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === call.id && step.status === 'completed' && JSON.stringify(step.result ?? null).includes('From the referenced schema'))).toBe(true)
})

for (const type of ['integer', 'boolean'] as const) {
  test(`a newly added comparison types its first ${type} event field and executes`, async ({ page, workspace }) => {
    const fixture = await scalarRulesFixture(workspace, type)
    const expected = type === 'integer' ? 5 : true
    const rule = await workspace.api('/api/hook-rules', 'POST', {
      definition: {
        ...fixture.definition,
        name: `First ${type} field`,
        steps: [{
          type: 'if',
          id: 'check',
          condition: { type: 'formula', expression: 'true' },
          then: [{
            type: 'action',
            id: 'matched',
            action: {
              type: 'storeValue',
              scope: 'rule',
              name: 'matched',
              value: { type: 'literal', value: true },
            },
          }],
          otherwise: [],
        }],
      },
      enabled: true,
    })
    await page.goto(workspacePath(`/rules/${rule.id}/edit`))
    await signIn(page)
    await page.getByRole('button', { name: 'Edit check', exact: true }).click()
    await page.getByRole('button', { name: 'Add a condition', exact: true }).click()
    await page.getByRole('menuitem', { name: 'Compare a field', exact: false }).click()
    if (type === 'integer')
      await page.getByRole('textbox', { name: 'Value', exact: true }).fill('5')
    else
      await page.getByRole('button', { name: 'Yes', exact: true }).click()
    await page.getByRole('button', { name: 'Close the step', exact: true }).click()
    await page.getByRole('button', { name: 'Save version', exact: true }).click()
    await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
    const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
    expect(saved.definition.steps[0].condition.conditions[1].right).toEqual({ type: 'literal', value: expected })
    workspace.service.emitHook(fixture.instanceId, 'message', { first: expected }, `first-${type}`)
    let triggering: any
    await expect.poll(async () => {
      triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
      return triggering?.status
    }).toBe('completed')
    const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
    expect(history.steps.some((step: any) => step.nodeId === 'matched' && step.status === 'completed')).toBe(true)
  })
}

test('variable insertion keeps numeric and boolean comparisons as formulas', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [{
        type: 'if',
        id: 'typed',
        condition: {
          type: 'all',
          conditions: [
            {
              type: 'compare',
              left: { type: 'formula', expression: 'payload.count' },
              operator: 'equals',
              right: { type: 'literal', value: 0 },
            },
            {
              type: 'compare',
              left: { type: 'formula', expression: 'payload.approved' },
              operator: 'equals',
              right: { type: 'literal', value: false },
            },
          ],
        },
        then: fixture.definition.steps,
        otherwise: [],
      }],
    },
    enabled: true,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit typed', exact: true }).click()
  for (const [index, name] of ['count', 'approved'].entries()) {
    await page.getByRole('button', { name: 'Insert a variable into Value', exact: true }).nth(index).click()
    await page.getByRole('dialog', { name: 'Variables for Value', exact: true }).getByRole('button').filter({ has: page.getByText(name, { exact: true }) }).click()
  }

  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  expect(saved.definition.steps[0].condition.conditions.map((entry: any) => entry.right)).toEqual([
    { type: 'formula', expression: 'payload.count' },
    { type: 'formula', expression: 'payload.approved' },
  ])
  workspace.service.emitHook(fixture.instanceId, 'message', {
    count: 8,
    approved: true,
    author: 'alice',
    channel: 'support',
    content: 'Typed variables',
  }, 'typed-variables')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('completed')
})

test('formula conditions insert a variable at the caret and keep surrounding expression', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [{
        type: 'if',
        id: 'formula',
        condition: { type: 'formula', expression: 'payload.count > 0 && true' },
        then: [],
        otherwise: [],
      }],
    },
    enabled: false,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit formula', exact: true }).click()
  const expression = page.getByRole('textbox', { name: 'Formula (true or false)', exact: true })
  await expression.fill('payload.count >  && true')
  await expression.evaluate((element: HTMLTextAreaElement) => element.setSelectionRange(16, 16))
  await page.getByRole('button', { name: 'Insert a variable into Formula (true or false)', exact: true }).click()
  await page.getByRole('dialog', { name: 'Variables for Formula (true or false)', exact: true }).getByRole('button').filter({ has: page.getByText('count', { exact: true }) }).click()
  await expect(expression).toHaveValue('payload.count > payload.count && true')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].condition.expression).toBe('payload.count > payload.count && true')
})

test('new rules keep the agent whose page requested them', async ({ page, workspace }) => {
  await rulesFixture(workspace)
  const agent = workspace.service.agent({ name: 'Requested assistant without plugin access' })
  await page.goto(workspacePath(`/rules/new?agent=${agent.id}`))
  await signIn(page)
  await expect(page.getByRole('combobox', { name: 'Execution agent', exact: true })).toHaveValue(agent.id)
  await expect(page.getByRole('combobox', { name: 'Hook', exact: true })).toHaveValue(/^cairn:/)
})

test('a catalog load failure explains disabled creation and can be retried', async ({ page, workspace }) => {
  await rulesFixture(workspace)
  await page.goto(workspacePath('/rules'))
  await signIn(page)
  await useRelayForHttpMocks(page)
  await page.route('**/api/hook-rules/catalog*', route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Catalog temporarily unavailable' }) }))
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: 'The rule catalog could not be loaded' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'New rule', exact: true })).toBeDisabled()
  await page.unroute('**/api/hook-rules/catalog*')
  await page.getByRole('button', { name: 'Retry loading the catalog', exact: true }).click()
  await expect(page.getByRole('button', { name: 'New rule', exact: true })).toBeEnabled()
  await expect(page.getByRole('alert').filter({ hasText: 'The rule catalog could not be loaded' })).toHaveCount(0)
})

test('loop insertion does not offer an unsupported wait', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [{
        type: 'forEach',
        id: 'loop',
        items: { type: 'literal', value: [1] },
        itemName: 'entry',
        body: [],
      }],
    },
    enabled: false,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('region', { name: 'For each entry', exact: true }).getByRole('button', { name: 'Add the first step', exact: true }).click()
  await expect(page.getByRole('menuitem', { name: /^Wait/ })).toHaveCount(0)
  await expect(page.getByRole('menuitem', { name: /Remember a value/ })).toBeVisible()
})

test('a committed creation with an unavailable response keeps later edits for a new version', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const before = new Set((await workspace.api('/api/hook-rules')).rules.map((rule: any) => rule.id))
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/rules/new?agent=${fixture.agent.id}`))
  await signIn(page)
  let unavailable = true
  let createdId = ''
  await page.route('**/api/hook-rules', async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    const response = await route.fetch()
    const created = await response.json()
    createdId = created.id
    if (unavailable)
      return route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Creation response unavailable' }) })
    return route.fulfill({ response })
  })
  await page.getByRole('textbox', { name: 'Rule name', exact: true }).fill('First definition')
  await page.getByRole('button', { name: 'Add the first step', exact: true }).click()
  await page.getByRole('menuitem', { name: /Remember a value/ }).click()
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Create rule', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Creation response unavailable' })).toBeVisible()
  expect((await workspace.api(`/api/hook-rules/${createdId}`)).definition.name).toBe('First definition')
  await page.getByRole('textbox', { name: 'Rule name', exact: true }).fill('Latest draft')
  unavailable = false
  await page.getByRole('button', { name: 'Create rule', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Rule name', exact: true })).toHaveValue('Latest draft')
  await expect(page.getByText('Your save succeeded. Your later edits are preserved', { exact: false })).toBeVisible()
  expect((await workspace.api('/api/hook-rules')).rules.filter((rule: any) => !before.has(rule.id))).toHaveLength(1)
  await page.getByRole('button', { name: 'Save my draft as the next version', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Latest draft', exact: true })).toBeVisible()
  const saved = await workspace.api(`/api/hook-rules/${createdId}`)
  expect(saved.version).toBe(2)
  expect(saved.definition.name).toBe('Latest draft')
})

test('inverted groups respect the server condition depth including the not node', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  let inner: any = { type: 'formula', expression: 'true' }
  for (let depth = 0; depth < 3; depth++)
    inner = { type: 'all', conditions: [inner] }
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [{
        type: 'if',
        id: 'depth',
        condition: { type: 'all', conditions: [{ type: 'not', condition: inner }] },
        then: [],
        otherwise: [],
      }],
    },
    enabled: false,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit depth', exact: true }).click()
  const inversions = page.getByRole('button', { name: 'Invert', exact: true })
  for (const button of await inversions.all())
    await expect(button).toBeDisabled()
  const deepest = page.locator('.condition-group').last()
  await deepest.getByRole('button', { name: 'Add a condition', exact: true }).click()
  await expect(page.getByRole('menuitem', { name: /^A group/ })).toHaveCount(0)
  await deepest.getByRole('button', { name: 'Add a condition', exact: true }).click()
  await page.getByRole('button', { name: 'Stop inverting', exact: true }).click()
  await page.getByRole('button', { name: 'Invert', exact: true }).first().click()
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
})

test('membership comparisons use typed list items instead of an invalid text list', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [{
        type: 'if',
        id: 'membership',
        condition: {
          type: 'compare',
          left: { type: 'formula', expression: 'payload.count' },
          operator: 'in',
          right: { type: 'literal', value: [] },
        },
        then: fixture.definition.steps,
        otherwise: [],
      }],
    },
    enabled: true,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit membership', exact: true }).click()
  await expect(page.getByRole('group', { name: 'How to fill Values (a list)', exact: true }).getByRole('button', { name: 'Text', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Add an item', exact: true }).click()
  await page.getByRole('textbox', { name: 'Values (a list) 1', exact: true }).fill('8')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].condition.right).toEqual({ type: 'literal', value: [8] })
  workspace.service.emitHook(fixture.instanceId, 'message', {
    count: 8,
    author: 'alice',
    channel: 'support',
    content: 'Typed list',
  }, 'typed-list')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('completed')
})

test('the last block level permits actions but no deeper branches', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  let node: any = {
    type: 'action',
    id: 'last',
    action: {
      type: 'storeValue',
      scope: 'rule',
      name: 'last',
      value: { type: 'literal', value: true },
    },
  }
  for (let depth = 0; depth < 7; depth++) {
    node = {
      type: 'if',
      id: `level${depth}`,
      condition: { type: 'formula', expression: 'true' },
      then: [node],
      otherwise: [],
    }
  }

  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, steps: [node] }, enabled: false })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('region', { name: 'When it holds', exact: true }).last().getByRole('button', { name: 'Add a step here', exact: true }).first().click()
  await expect(page.getByRole('menuitem', { name: /^If/ })).toHaveCount(0)
  await expect(page.getByRole('menuitem', { name: /^For each/ })).toHaveCount(0)
  await expect(page.getByRole('menuitem', { name: /Remember a value/ })).toBeVisible()
})

test('a rule budget notification link opens its actual rule detail', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: fixture.definition, enabled: false })
  await page.goto(workspacePath('/rules'))
  await signIn(page)
  await page.goto(workspacePath(`/?rule=${rule.id}`))
  await expect(page).toHaveURL(new RegExp(`/rules\\?rule=${rule.id}$`))
  await expect(page.getByRole('heading', { name: fixture.definition.name, exact: true })).toBeVisible()
})

test('removing a branch asks before dropping its nested steps', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [...fixture.definition.steps, {
        type: 'action',
        id: 'keep',
        action: {
          type: 'storeValue',
          scope: 'rule',
          name: 'keep',
          value: { type: 'literal', value: true },
        },
      }],
    },
    enabled: false,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Remove this step', exact: true }).first().click()
  await expect(page.getByRole('dialog', { name: 'Remove this step and its branches?', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Keep the step', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit Acknowledge the message', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Remove this step', exact: true }).first().click()
  await page.getByRole('button', { name: 'Remove the block', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit Acknowledge the message', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps.map((step: any) => step.id)).toEqual(['keep'])
})

test('a rejected creation accepts the corrected form without a duplicate rule', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const before = new Set((await workspace.api('/api/hook-rules')).rules.map((rule: any) => rule.id))
  await useRelayForHttpMocks(page)
  let reject = true
  const creations: any[] = []
  await page.route('**/api/hook-rules', async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    creations.push(route.request().postDataJSON())
    if (reject)
      return route.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ error: 'Creation rejected' }) })
    return route.continue()
  })
  await page.goto(workspacePath(`/rules/new?agent=${fixture.agent.id}`))
  await signIn(page)
  await page.getByRole('textbox', { name: 'Rule name', exact: true }).fill('Rejected draft')
  await page.getByRole('button', { name: 'Add the first step', exact: true }).click()
  await page.getByRole('menuitem', { name: /Remember a value/ }).click()
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Create rule', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Creation rejected' })).toBeVisible()
  expect((await workspace.api('/api/hook-rules')).rules.filter((rule: any) => !before.has(rule.id))).toHaveLength(0)
  await page.getByRole('textbox', { name: 'Rule name', exact: true }).fill('Corrected draft')
  reject = false
  await page.getByRole('button', { name: 'Create rule', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Corrected draft', exact: true })).toBeVisible()
  expect((await workspace.api('/api/hook-rules')).rules.filter((rule: any) => !before.has(rule.id))).toHaveLength(1)
  expect(creations[0].creationId).toBe(creations.at(-1).creationId)
  expect(creations.at(-1).definition.name).toBe('Corrected draft')
})

test('clearing a step label saves once and uses its default display title', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: fixture.definition, enabled: false })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit Acknowledge the message', exact: true }).click()
  await page.getByRole('textbox', { name: 'Label', exact: true }).fill('Temporary label')
  await page.getByRole('textbox', { name: 'Label', exact: true }).fill('')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect(page.getByRole('heading', { name: fixture.definition.name, exact: true })).toBeVisible()
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  expect(saved.version).toBe(2)
  expect(saved.definition.steps[0].then[0].label).toBeUndefined()
})

test('retrying a committed replacement recovers its version without a duplicate write', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: fixture.definition, enabled: false })
  await useRelayForHttpMocks(page)
  let unavailable = true
  await page.route(`**/api/hook-rules/${rule.id}`, async (route) => {
    if (route.request().method() !== 'PUT')
      return route.continue()
    const response = await route.fetch()
    if (unavailable)
      return route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Replacement response unavailable' }) })
    return route.fulfill({ response })
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('textbox', { name: 'Rule name', exact: true }).fill('Recovered replacement')
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Replacement response unavailable' })).toBeVisible()
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  unavailable = false
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Recovered replacement', exact: true })).toBeVisible()
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
})

test('changing a numeric compared field to text also converts its fixed value', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      steps: [{
        type: 'if',
        id: 'fieldchange',
        condition: {
          type: 'compare',
          left: { type: 'formula', expression: 'payload.count' },
          operator: 'equals',
          right: { type: 'literal', value: 5 },
        },
        then: fixture.definition.steps,
        otherwise: [],
      }],
    },
    enabled: true,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit fieldchange', exact: true }).click()
  await page.getByRole('button', { name: 'Insert a variable into Field', exact: true }).click()
  await page.getByRole('dialog', { name: 'Variables for Field', exact: true }).getByRole('button').filter({ has: page.getByText('content', { exact: true }) }).click()
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].condition.right).toEqual({ type: 'literal', value: '5' })
  workspace.service.emitHook(fixture.instanceId, 'message', { content: '5', author: 'alice', channel: 'support' }, 'text-field-change')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('completed')
})

test('contains keeps a numeric member typed and runs the matching branch', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition: any = structuredClone(fixture.definition)
  definition.steps[0].condition = {
    type: 'compare',
    left: { type: 'formula', expression: 'payload.counts' },
    operator: 'equals',
    right: { type: 'literal', value: 5 },
  }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit trusted', exact: true }).click()
  await page.getByRole('combobox', { name: 'Test', exact: true }).selectOption('contains')
  await expect(page.getByRole('textbox', { name: 'Value', exact: true })).toHaveValue('5')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].condition.right).toEqual({ type: 'literal', value: 5 })
  workspace.service.emitHook(fixture.instanceId, 'message', {
    counts: [5],
    author: 'alice',
    channel: 'support',
    content: 'Typed list',
  }, 'numeric-member')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === 'post' && step.status === 'completed')).toBe(true)
})

test('adding an author condition selects the nested person identifier', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition: any = structuredClone(fixture.definition)
  definition.trigger.hook = 'nested-message'
  definition.steps[0].condition = { type: 'all', conditions: [{ type: 'formula', expression: 'true' }] }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit trusted', exact: true }).click()
  await page.getByRole('button', { name: 'Add a condition', exact: true }).click()
  await page.getByRole('menuitem', { name: /The author is one of/ }).click()
  await expect(page.getByRole('textbox', { name: 'Author field', exact: true })).toHaveValue('payload.author.id')
  await page.getByRole('button', { name: 'Choose people…', exact: true }).click()
  await page.getByRole('button', { name: 'Alice', exact: true }).click()
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  workspace.service.emitHook(fixture.instanceId, 'nested-message', { author: { id: 'alice', name: 'Alice' }, content: 'Nested author' }, 'nested-author')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === 'post' && step.status === 'completed')).toBe(true)
})

test('changing equality operators preserves structured operands and matches the event', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition: any = structuredClone(fixture.definition)
  definition.steps[0].condition = {
    type: 'all',
    conditions: [
      {
        type: 'compare',
        left: { type: 'formula', expression: 'payload.counts' },
        operator: 'equals',
        right: { type: 'literal', value: [1, 2] },
      },
      {
        type: 'compare',
        left: { type: 'formula', expression: 'payload.meta' },
        operator: 'equals',
        right: { type: 'literal', value: { ok: true } },
      },
    ],
  }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit trusted', exact: true }).click()
  for (const index of [0, 1]) {
    const select = page.getByRole('combobox', { name: 'Test', exact: true }).nth(index)
    await select.selectOption('notEquals')
    await select.selectOption('equals')
  }

  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  expect(saved.definition.steps[0].condition.conditions.map((condition: any) => condition.right.value)).toEqual([[1, 2], { ok: true }])
  workspace.service.emitHook(fixture.instanceId, 'message', {
    counts: [1, 2],
    meta: { ok: true },
    author: 'alice',
    channel: 'support',
    content: 'Structured equality',
  }, 'structured-equality')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === 'post' && step.status === 'completed')).toBe(true)
})

test('formula equality and editing nested membership keep JSON types through execution', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition: any = structuredClone(fixture.definition)
  definition.steps[0].condition = {
    type: 'all',
    conditions: [
      {
        type: 'compare',
        left: { type: 'formula', expression: 'size(payload.counts)' },
        operator: 'equals',
        right: { type: 'literal', value: 2 },
      },
      {
        type: 'compare',
        left: { type: 'formula', expression: 'payload.counts' },
        operator: 'in',
        right: { type: 'literal', value: [[1, 2]] },
      },
    ],
  }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit trusted', exact: true }).click()
  const operator = page.getByRole('combobox', { name: 'Test', exact: true }).first()
  await operator.selectOption('notEquals')
  await operator.selectOption('equals')
  await page.getByRole('textbox', { name: 'Values (a list) 1 1', exact: true }).fill('5')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  expect(saved.definition.steps[0].condition.conditions.map((condition: any) => condition.right.value)).toEqual([2, [[5, 2]]])
  workspace.service.emitHook(fixture.instanceId, 'message', {
    counts: [5, 2],
    author: 'alice',
    channel: 'support',
    content: 'Nested comparison',
  }, 'nested-comparison')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === 'post' && step.status === 'completed')).toBe(true)
})

test('editing a comparison field preserves templates and their evaluated branch', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition: any = structuredClone(fixture.definition)
  const right = { type: 'template', text: 'support-{{payload.author}}' }
  definition.steps[0].condition = {
    type: 'all',
    conditions: [
      {
        type: 'compare',
        left: { type: 'formula', expression: 'payload.channel' },
        operator: 'equals',
        right,
      },
      {
        type: 'compare',
        left: { type: 'formula', expression: '"support-" + payload.author' },
        operator: 'equals',
        right,
      },
    ],
  }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit trusted', exact: true }).click()
  const field = page.getByRole('textbox', { name: 'Field', exact: true }).first()
  await field.fill('payload.channe')
  await field.pressSequentially('l')
  const operator = page.getByRole('combobox', { name: 'Test', exact: true }).nth(1)
  await operator.selectOption('notEquals')
  await operator.selectOption('equals')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
  expect(saved.definition.steps[0].condition.conditions.map((condition: any) => condition.right)).toEqual([right, right])
  workspace.service.emitHook(fixture.instanceId, 'message', {
    channel: 'support-alice',
    author: 'alice',
    content: 'Template comparison',
  }, 'template-comparison')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === 'post' && step.status === 'completed')).toBe(true)
})

test('reopening a JSON string comparison preserves its string type through execution', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition: any = structuredClone(fixture.definition)
  definition.steps[0].condition = {
    type: 'compare',
    left: { type: 'formula', expression: '"5" + "6"' },
    operator: 'equals',
    right: { type: 'literal', value: '5' },
  }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit trusted', exact: true }).click()
  const value = page.getByRole('textbox', { name: 'Value', exact: true })
  await expect(value).toHaveValue('"5"')
  await value.press('End')
  await value.press('ArrowLeft')
  await value.pressSequentially('6')
  await expect(value).toHaveValue('"56"')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].condition.right).toEqual({ type: 'literal', value: '56' })
  workspace.service.emitHook(fixture.instanceId, 'message', {
    channel: 'support',
    author: 'alice',
    content: 'JSON string comparison',
  }, 'json-string-comparison')
  let triggering: any
  await expect.poll(async () => {
    triggering = (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]
    return triggering?.status
  }).toBe('completed')
  const history = await workspace.api(`/api/hook-rule-triggerings/${triggering.id}`)
  expect(history.steps.some((step: any) => step.nodeId === 'post' && step.status === 'completed')).toBe(true)
})
