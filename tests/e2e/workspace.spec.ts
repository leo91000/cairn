import {
  signIn as authenticateWorkspace,
  createAgent,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import { runSchedule, startConversation } from './schedule-fixture'

test.describe.configure({ mode: 'serial' })

// One fresh application exercises persistence between screens and real child execution.
test('sign in, author skills, schedule work, inspect results, and sign out', async ({
  page,
  workspace,
}) => {
  test.setTimeout(120000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', (message) => {
    if (
      message.type() === 'error'
      && !message
        .text()
        .startsWith(
          'Failed to load resource: the server responded with a status of 400',
        )
    ) {
      errors.push(message.text())
    }
  })
  await page.goto(workspacePath('/'))
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.waitForFunction(() => performance.getEntriesByType('paint').some(entry => entry.name === 'first-paint'))
  await page.screenshot({ path: 'test-results/theme-setup-desktop.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 568 })
  await page.screenshot({ path: 'test-results/theme-setup-mobile.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.emulateMedia({ colorScheme: 'light' })
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('navigation', { name: 'Atelier sections' }).getByRole('link', { name: 'Agents', exact: true }).click()
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Release engineer')
  await createAgent(page)
  await page.getByRole('link', { name: 'Back to agents' }).click()
  await page.getByRole('link', { name: 'Projects', exact: true }).click()
  await page.getByRole('button', { name: 'Add project', exact: true }).click()
  await page
    .getByRole('dialog')
    .getByLabel('Name', { exact: true })
    .fill('Design system')
  await page.getByLabel('Base branch').fill('')
  await expect(page.getByLabel('Base branch')).toHaveValue('')
  await page.getByLabel('Base branch').fill('main')
  await page.getByLabel('Project directory').fill('/does-not-exist')
  await page.getByRole('button', { name: 'Save project' }).click()
  // Direct requests have a 35s deadline; await the business error, not a latency target.
  await expect(page.getByRole('alert')).toContainText('does not exist', { timeout: 40000 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: 'test-results/theme-validation-error.png', animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page
    .getByLabel('Project directory')
    .fill(workspace.projectPath)
  await page.getByRole('button', { name: 'Save project' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('link', { name: 'Skills', exact: true }).click()
  await page.getByRole('button', { name: 'New skill', exact: true }).click()
  await page.getByLabel('Skill name').fill('review')
  await page
    .getByLabel('Skill content')
    .fill(
      '---\nname: review\ndescription: Review the project carefully\n---\nInspect the project and report checks.',
    )
  await page.getByRole('button', { name: 'Save skill' }).click()
  await expect(page.getByRole('dialog').getByText('All changes saved')).toBeVisible()
  await page.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('link', { name: 'Rules', exact: true }).click()
  await page.getByRole('button', { name: 'Schedule an agent' }).click()
  await page.getByLabel('Name', { exact: true }).fill('Weekly dependency review')
  await page.getByLabel('Brief').fill('Review dependencies. fixture:activity')
  await page.getByLabel('Cron (five fields)').fill('0 9 * * 1')
  await page.getByRole('button', { name: 'Create schedule' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  const rule = (await workspace.api('/api/hook-rules')).rules.find((entry: any) => entry.name === 'Weekly dependency review')
  const chat = await runSchedule(workspace, rule)
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).status).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.slug}`))
  await expect(page.getByText('Ready for the next step.', { exact: true })).toBeVisible()
  await page.reload()
  await expect(page.getByText('Ready for the next step.', { exact: true })).toBeVisible()
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await page.getByRole('button', { name: 'Disable', exact: true }).click()
  await expect(page.getByText('Disabled', { exact: true }).first()).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.mouse.move(0, 0)
  await expect(page.locator('.toast')).toHaveCount(0)
  await page.screenshot({
    path: 'docs/screenshots/overview-desktop.png',
    fullPage: true,
    animations: 'disabled',
  })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true)
  await page.screenshot({
    path: 'docs/screenshots/overview-mobile.png',
    fullPage: true,
    animations: 'disabled',
  })
  const ruleSheet = page.getByRole('dialog', { name: 'Weekly dependency review', exact: true })
  await ruleSheet.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(ruleSheet).toHaveCount(0)
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('button', { name: 'Sign out', exact: true }).click()
  await expect(
    page.getByLabel('Email address'),
  ).toBeVisible()
  expect(errors).toEqual([])
})

test('edits supporting files and cancels work without losing history', async ({
  page,
  workspace,
}) => {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('navigation', { name: 'Atelier sections' }).getByRole('link', { name: 'Skills', exact: true }).click()
  await page.getByRole('button', { name: 'Edit review', exact: true }).click()
  await page.getByRole('button', { name: 'New file', exact: true }).click()
  await page.getByLabel('New file path').fill('references/checks.md')
  await page.getByRole('button', { name: 'Create file', exact: true }).click()
  await page
    .getByLabel('Supporting file content')
    .fill('# Checks\nRun the test suite and inspect the diff.')
  await page.getByRole('button', { name: 'Save skill' }).click()
  await expect(page.getByRole('dialog').getByText('All changes saved')).toBeVisible()
  await page.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('button', { name: 'Edit review', exact: true }).click()
  await page.getByRole('button', { name: 'references/checks.md', exact: true }).click()
  await expect(page.getByLabel('Supporting file content')).toHaveValue(
    '# Checks\nRun the test suite and inspect the diff.',
  )
  await page.getByRole('button', { name: 'Preview', exact: true }).click()
  await expect(
    page.getByRole('heading', { name: 'Checks', exact: true }),
  ).toBeVisible()
  await page.keyboard.press('Escape')
  const { run } = await startConversation(workspace, 'Keep prior runs', 'fixture:chat-hang')
  await page.goto(workspacePath(`/runs/${run.id}`))
  await expect(page.getByRole('button', { name: 'Stop run', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Stop run', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Stop run', exact: true }).click()
  await expect.poll(() => workspace.service.store.run(run.id)?.status).toBe('cancelled')
  await page.getByRole('link', { name: 'Back to runs' }).click()
  await expect(page.locator(`a[href="${workspacePath(`/runs/${run.id}`)}"]`).first()).toBeVisible()
})

test('approves a scoped OAuth connector and revokes its grant', async ({
  page,
  request,
  workspace,
}) => {
  const { createHash } = await import('node:crypto')
  const verifier = 'test-verifier-'.repeat(5)
  const registration = await request.post('/oauth/register', {
    data: {
      client_name: 'Browser QA connector',
      redirect_uris: [`${workspace.url}/oauth-test-callback`],
    },
  })
  const client = await registration.json()
  const params = new URLSearchParams({
    client_id: client.client_id,
    redirect_uri: client.redirect_uris[0],
    response_type: 'code',
    code_challenge: createHash('sha256').update(verifier).digest('base64url'),
    code_challenge_method: 'S256',
    scope: 'read run',
    resource: `${workspace.url}/mcp`,
    state: 'qa-state',
  })
  await page.goto(workspacePath(`/oauth/authorize?${params}`))
  await authenticateWorkspace(page)
  await expect(
    page.getByText('Browser QA connector', { exact: true }),
  ).toBeVisible()
  await expect(
    page.getByText(
      'Start and cancel tasks in YOLO mode with full access on the worker',
    ),
  ).toBeVisible()
  await page.route('**/oauth-test-callback?*', route =>
    route.fulfill({
      contentType: 'text/plain',
      body: 'Connector callback received',
    }))
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: 'test-results/theme-consent-desktop.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 568 })
  await expect(page.getByRole('navigation', { name: 'Quick navigation' })).toHaveCount(0)
  await page.screenshot({ path: 'test-results/theme-consent-mobile.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.getByRole('button', { name: 'Allow access' }).click()
  await expect(page).toHaveURL(/oauth-test-callback\?state=qa-state&code=/)
  const code = new URL(page.url()).searchParams.get('code')!
  const token = await request.post('/oauth/token', {
    form: {
      grant_type: 'authorization_code',
      code,
      client_id: client.client_id,
      redirect_uri: client.redirect_uris[0],
      code_verifier: verifier,
    },
  })
  expect(token.status()).toBe(200)
  const credentials = await token.json()
  expect(credentials.scope).toBe('read run')
  await page.goto(workspacePath('/settings/installation'))
  await expect(
    page.getByText('Browser QA connector', { exact: true }),
  ).toBeVisible()
  await page.getByRole('button', { name: /Revoke/ }).click()
  await expect(
    page.getByText('Browser QA connector', { exact: true }),
  ).toHaveCount(0)
  const denied = await request.post('/mcp', {
    headers: { authorization: `Bearer ${credentials.access_token}` },
    data: { jsonrpc: '2.0', id: 1, method: 'tools/list' },
  })
  expect(denied.status()).toBe(401)
})

test('appearance follows the device, persists overrides, syncs tabs and paints before the app', async ({ page, context }, testInfo) => {
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.route('**/assets/*.js', route => route.abort())
  await page.goto(workspacePath('/'))
  const root = page.locator('html')
  await expect(root).toHaveAttribute('data-theme', 'dark')
  await expect(root).toHaveAttribute('data-theme-preference', 'system')
  await expect(page.locator('#app')).toBeEmpty()
  await page.waitForFunction(() => performance.getEntriesByType('paint').some(entry => entry.name === 'first-paint'))
  expect(await root.evaluate(el => getComputedStyle(el).backgroundColor)).toBe('rgb(15, 15, 20)')
  const beforeApp = await page.screenshot({ path: testInfo.outputPath('dark-before-app.png'), fullPage: true, animations: 'disabled' })
  const canvasPixel = await page.evaluate(async (png) => {
    const image = await createImageBitmap(new Blob([new Uint8Array(png)], { type: 'image/png' }))
    const canvas = new OffscreenCanvas(1, 1)
    const context = canvas.getContext('2d')!
    context.drawImage(image, 0, 0)
    const pixel = Array.from(context.getImageData(0, 0, 1, 1).data)
    image.close()
    return pixel
  }, Array.from(beforeApp))
  expect(canvasPixel).toEqual([15, 15, 20, 255])

  await page.unroute('**/assets/*.js')
  await page.reload()
  await expect(page.getByLabel('Email address')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('login-dark-desktop.png'), fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 568 })
  await page.screenshot({ path: testInfo.outputPath('login-dark-mobile.png'), fullPage: true, animations: 'disabled' })
  await page.getByRole('button', { name: 'Appearance: system', exact: true }).click()
  await page.screenshot({ path: testInfo.outputPath('theme-menu-mobile.png'), fullPage: false, animations: 'disabled' })
  await expect(page.getByRole('radio', { name: /System/ })).toBeFocused()
  const menu = await page.locator('.theme-popover').boundingBox()
  expect(menu!.y + menu!.height).toBeLessThanOrEqual(568)
  await page.locator('.theme-popover label').filter({ hasText: 'Light' }).click()
  await expect(root).toHaveAttribute('data-theme', 'light')
  await expect(page.getByRole('button', { name: 'Appearance: light', exact: true })).toBeFocused()
  await page.reload()
  await expect(root).toHaveAttribute('data-theme', 'light')
  await page.emulateMedia({ colorScheme: 'light' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(root).toHaveAttribute('data-theme', 'light')
  const other = await context.newPage()
  await other.goto(workspacePath('/'))
  await other.getByRole('button', { name: 'Appearance: light', exact: true }).click()
  await other.locator('.theme-popover label').filter({ hasText: 'Dark' }).click()
  await expect(root).toHaveAttribute('data-theme', 'dark')
  await other.close()
  await page.getByRole('button', { name: 'Appearance: dark', exact: true }).click()
  await page.locator('.theme-popover label').filter({ hasText: 'System' }).click()
  await page.emulateMedia({ colorScheme: 'light' })
  await expect(root).toHaveAttribute('data-theme', 'light')
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(root).toHaveAttribute('data-theme', 'dark')
  await page.getByRole('button', { name: 'Appearance: system', exact: true }).click()
  await page.locator('.theme-popover label').filter({ hasText: 'Dark' }).click()
  await page.emulateMedia({ colorScheme: 'light' })
  // With the application module blocked, the saved override must already be painted.
  await page.route('**/assets/*.js', route => route.abort())
  await page.reload()
  await expect(page.locator('#app')).toBeEmpty()
  await expect(root).toHaveAttribute('data-theme', 'dark')
  expect(await root.evaluate(el => getComputedStyle(el).backgroundColor)).toBe('rgb(15, 15, 20)')
})

test('dark appearance settings, empty states and connection sign-in feedback', async ({ page }, testInfo) => {
  await useRelayForHttpMocks(page)
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible()
  await page.goto(workspacePath('/settings/account'))
  await page.locator('.theme-control:not(.theme-compact) label').filter({ hasText: 'Light' }).click()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
  await page.locator('.theme-control:not(.theme-compact) label').filter({ hasText: 'Dark' }).click()
  await expect(page.getByRole('button', { name: 'Appearance: dark', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Appearance: dark', exact: true }).click()
  await page.keyboard.press('Escape')
  await expect(page.locator('.theme-popover')).not.toBeVisible()
  await expect(page.getByRole('button', { name: 'Appearance: dark', exact: true })).toBeFocused()
  await page.route('**/api/hook-rules', route => route.fulfill({ json: { rules: [] } }))
  await page.goto(workspacePath('/rules'))
  await expect(page.getByRole('heading', { name: 'No rules yet', exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('empty-tasks.png'), fullPage: true, animations: 'disabled' })
  // Synthetic feedback only: never initiate a real device login or capture a real code.
  const signIn = {
    accountId: 'demo',
    provider: 'codex',
    state: 'pending',
    phase: 'authorizing',
    code: 'DEMO-CODE',
    url: 'https://example.com',
    acceptsCode: false,
    expiresAt: Date.now() + 600000,
    error: null,
  }
  await page.route('**/api/accounts', route => route.fulfill({ json: { accounts: [], signIn } }))
  await page.goto(workspacePath('/connections'))
  // A sign-in in progress reopens its window on page load.
  await expect(page.getByText('DEMO-CODE', { exact: true })).toBeVisible()
  for (const width of [1440, 320]) {
    await page.setViewportSize({ width, height: width === 320 ? 568 : 1000 })
    await page.screenshot({ path: testInfo.outputPath(`${width}-connection-pending.png`), fullPage: true, animations: 'disabled' })
  }

  await page.unroute('**/api/accounts')
  await page.route('**/api/accounts', route => route.fulfill({
    json: {
      accounts: [],
      signIn: {
        ...signIn,
        state: 'failed',
        code: null,
        error: 'The verification code expired. Please try again.',
      },
    },
  }))
  await expect(page.getByRole('heading', { name: 'Let’s try that again', exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('connection-failed.png'), fullPage: true, animations: 'disabled' })
})
