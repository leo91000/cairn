import {
  expect,
  expectSingleScroll,
  signIn,
  test,
  workspacePath,
} from './fixtures'
import { rulesFixture } from './rules-fixture'

test('rules show the actual saved tree, creator controls and real triggering history', async ({ page, workspace }, testInfo) => {
  const pageErrors: string[] = []
  page.on('pageerror', error => pageErrors.push(error.message))
  test.setTimeout(60000)
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: fixture.definition, enabled: true })
  workspace.service.emitHook(fixture.instanceId, 'message', { channel: 'support', author: 'alice', content: 'Could you help with this request?' }, 'review-event')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status, { timeout: 20000 }).toBe('completed')
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await signIn(page)
  await expect(page.getByRole('heading', { name: 'Support messages', exact: true })).toBeVisible()
  await expect(page.getByText('Acknowledge the message', { exact: true }).first()).toBeVisible()
  await page.getByRole('button', { name: 'History', exact: true }).click()
  await page.getByRole('button', { name: /Completed.*1 action/ }).click()
  await page.getByText('Result', { exact: true }).click()
  await expect(page.locator('details[open] pre').filter({ hasText: 'Received: Could you help with this request?' })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('rules-history.png') })
  await page.getByRole('button', { name: 'Flow', exact: true }).click()
  await page.getByRole('button', { name: 'Edit', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Rule name' })).toHaveValue('Support messages')
  await expect(page.getByRole('button', { name: 'Save version', exact: true })).toBeEnabled()
  await page.screenshot({ path: testInfo.outputPath('rules-editor.png'), animations: 'disabled' })
  expect(pageErrors).toEqual([])
  await expectSingleScroll(page)
})

test('the visual editor inserts branches and edits structured conditions without JSON', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const fixture = await rulesFixture(workspace)
  await page.goto(workspacePath(`/rules/new?agent=${fixture.agent.id}&instance=${fixture.instanceId}`))
  await signIn(page)
  await page.getByRole('textbox', { name: 'Rule name' }).fill('A visual support rule')
  await page.getByRole('button', { name: 'Add the first step', exact: true }).click()
  await page.getByRole('menuitem', { name: /If \/ otherwise/ }).click()
  await expect(page.getByRole('heading', { name: 'If / otherwise', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Add a condition', exact: true }).click()
  await page.getByRole('menuitem', { name: /The author is one/ }).click()
  await page.getByRole('button', { name: 'Choose people…', exact: true }).click()
  await page.getByRole('button', { name: 'Alice', exact: true }).click()
  await expect(page.getByText('Alice', { exact: true }).first()).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Rule name' })).toHaveValue('A visual support rule')
  await expect(page.locator('textarea[aria-label*="JSON"]')).toHaveCount(0)
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('region', { name: 'When it holds', exact: true }).getByRole('button', { name: 'Add the first step', exact: true }).click()
  await page.getByRole('menuitem', { name: /Remember a value/ }).click()
  await page.getByRole('textbox', { name: 'Name', exact: true }).fill('lastAuthor')
  await page.getByRole('textbox', { name: 'Value', exact: true }).fill('Saved {{payload.author}}')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Create rule', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'A visual support rule', exact: true })).toBeVisible()
  const saved = (await workspace.api('/api/hook-rules')).rules.find((rule: any) => rule.name === 'A visual support rule')
  const detail = await workspace.api(`/api/hook-rules/${saved.id}`)
  expect(detail.definition.steps[0].then[0].action).toMatchObject({ type: 'storeValue', name: 'lastAuthor', value: { type: 'template', text: 'Saved {{payload.author}}' } })
})

test('creator controls persist pauses, versions and disabled duplicates', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, name: 'Control checks' }, enabled: true })
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'Emergency pause', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks.map((block: any) => block.kind)).toContain('emergencyPaused')
  await page.getByRole('button', { name: 'Resume', exact: true }).click()
  await page.getByRole('button', { name: 'Disable', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks.map((block: any) => block.kind)).toContain('disabled')
  await page.getByRole('button', { name: 'Versions', exact: true }).click()
  await page.getByText('View this version’s flow', { exact: true }).click()
  await expect(page.getByText('Acknowledge the message', { exact: true }).first()).toBeVisible()
  await page.getByRole('button', { name: 'Duplicate', exact: true }).click()
  await expect.poll(async () => (await workspace.api('/api/hook-rules')).rules.filter((entry: any) => entry.name.startsWith('Control checks')).length).toBe(2)
  const copies = (await workspace.api('/api/hook-rules')).rules.filter((entry: any) => entry.name.startsWith('Control checks') && entry.id !== rule.id)
  expect(copies[0].blocks.map((block: any) => block.kind)).toContain('disabled')
})

test('an expanded active triggering updates when its wait finishes', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const definition = {
    ...fixture.definition,
    name: 'Wait and remember',
    steps: [
      { type: 'action', id: 'pause', action: { type: 'wait', seconds: { type: 'literal', value: 12 } } },
      {
        type: 'action',
        id: 'remember',
        action: {
          type: 'storeValue',
          scope: 'rule',
          name: 'last',
          value: { type: 'literal', value: 'done' },
        },
      },
    ],
  }
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: true })
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'Wait for this event' }, 'wait-and-remember')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('waiting')
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'History', exact: true }).click()
  await page.getByRole('button', { name: /^Waiting/ }).click()
  await expect(page.getByRole('button', { name: /^Completed.*2 actions/ })).toBeVisible({ timeout: 25000 })
  await expect(page.getByText('done', { exact: false }).first()).toBeAttached()
})
