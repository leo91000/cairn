import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import { rulesFixture } from './rules-fixture'

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
