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

test('a state-only revision change preserves the draft and a newer version requires a choice', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, name: 'Concurrent editing' }, enabled: true })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('textbox', { name: 'Rule name' }).fill('My preserved draft')
  await workspace.api(`/api/hook-rules/${rule.id}/emergency-pause`, 'POST', { ruleId: rule.id, paused: true })
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).definition.name).toBe('My preserved draft')
  await page.getByRole('button', { name: 'Edit', exact: true }).click()
  await page.getByRole('textbox', { name: 'Rule name' }).fill('Keep my newer draft')
  const current = await workspace.api(`/api/hook-rules/${rule.id}`)
  await workspace.api(`/api/hook-rules/${rule.id}`, 'PUT', { ruleId: rule.id, revision: current.revision, definition: { ...current.definition, name: 'A concurrently saved version' } })
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect(page.getByText('Your draft is preserved.', { exact: false })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Rule name' })).toHaveValue('Keep my newer draft')
  await page.getByRole('button', { name: 'Save my draft as the next version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).definition.name).toBe('Keep my newer draft')
})

test('inserting a variable into a fixed value saves a template rather than raw expression text', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      name: 'Fixed value insertion',
      steps: [{
        type: 'action',
        id: 'remember',
        action: {
          type: 'storeValue',
          scope: 'rule',
          name: 'last',
          value: { type: 'literal', value: '' },
        },
      }],
    },
    enabled: true,
  })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit Remember last', exact: true }).click()
  await page.getByRole('button', { name: 'Insert a variable into Value', exact: true }).click()
  await page.getByRole('button').filter({ has: page.getByText('author', { exact: true }) }).click()
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].action.value).toEqual({ type: 'template', text: '{{payload.author}}' })
})

test('a member sees shared rule metadata and locked history, with only permitted controls', async ({ page, browser, workspace }, info) => {
  test.setTimeout(120000)
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, name: 'Shared private history' }, enabled: true })
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'PRIVATE EVENT FOR THE CREATOR' }, 'member-private-event')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('completed')
  await page.goto(workspacePath('/rules'))
  await signIn(page)
  const owner = await (await page.request.get(`${workspace.url}/api/account/session`)).json()
  const email = `rule-member-${crypto.randomUUID()}@example.test`
  const invitation = await page.request.post(`${workspace.url}/api/installations/${workspace.installationId}/sharing/invitations`, { headers: { 'origin': workspace.url, 'x-csrf-token': owner.csrf }, data: { email } })
  expect(invitation.ok()).toBe(true)
  const context = await browser.newContext({
    viewport: page.viewportSize(),
    isMobile: info.project.use.isMobile,
    hasTouch: info.project.use.hasTouch,
    serviceWorkers: 'block',
  })
  const member = await context.newPage()
  try {
    await member.goto(workspace.url)
    await member.getByLabel('Email address', { exact: true }).fill(email)
    await member.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => workspace.emailProof().code).not.toBe('')
    await member.getByLabel('Email code', { exact: true }).fill(workspace.emailProof().code)
    await member.getByRole('button', { name: 'Sign in', exact: true }).click()
    await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
    await expect(member).toHaveURL(`${workspace.url}/installations/${workspace.installationSlug}/`)
    await member.waitForLoadState('load')
    await expect(member.getByRole('status', { name: 'Installation availability', exact: true })).toBeVisible()
    const session = await (await member.request.get(`${workspace.url}/api/account/session`)).json()
    await member.goto(workspacePath(`/rules?rule=${rule.id}`))
    await expect(member.getByRole('heading', { name: 'Shared private history', exact: true })).toBeVisible()
    await expect(member.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0)
    await expect(member.getByRole('button', { name: 'Disable', exact: true })).toHaveCount(0)
    await expect(member.getByRole('button', { name: 'Emergency pause', exact: true })).toHaveCount(0)
    await member.getByRole('button', { name: 'History', exact: true }).click()
    await member.getByRole('button', { name: /^Completed/ }).click()
    await expect(member.getByText('Its event and actions are visible', { exact: false })).toBeVisible()
    await expect(member.getByText('PRIVATE EVENT FOR THE CREATOR', { exact: false })).toHaveCount(0)
    const accountId = session.account.id
    await workspace.api(`/api/agents/${fixture.agent.id}/accounts/${accountId}`, 'PUT', { agentId: fixture.agent.id, accountId })
    await member.reload()
    await member.getByRole('button', { name: 'History', exact: true }).click()
    await member.getByRole('button', { name: /^Completed/ }).click()
    await member.locator('summary').filter({ hasText: 'Event ·' }).click()
    await expect(member.locator('details[open] pre').filter({ hasText: 'PRIVATE EVENT FOR THE CREATOR' })).toBeVisible()
    await expect(member.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0)
  }
  finally {
    await context.close()
  }
})

test('a fixed wait can be changed into a validated formula in the visual editor', async ({ page, workspace }) => {
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition: { ...fixture.definition, name: 'Formula wait', steps: [{ type: 'action', id: 'pause', action: { type: 'wait', seconds: { type: 'literal', value: 600 } } }] }, enabled: false })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  await page.getByRole('button', { name: /^Edit Wait/ }).click()
  await expect(page.getByRole('button', { name: 'Use a formula', exact: true })).toBeInViewport()
  await page.getByRole('button', { name: 'Use a formula', exact: true }).click()
  await page.getByRole('textbox', { name: 'Seconds to wait', exact: true }).fill('900')
  await page.getByRole('button', { name: 'Close the step', exact: true }).click()
  await page.getByRole('button', { name: 'Save version', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.steps[0].action.seconds).toEqual({ type: 'formula', expression: '900' })
})
