import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test.describe.configure({ mode: 'serial' })

// One fresh application exercises persistence between screens and real child execution.
test('sign in, author skills, schedule work, inspect results, and sign out', async ({
  page,
  workspace,
}) => {
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
  await page.screenshot({ path: 'test-results/theme-setup-desktop.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 568 })
  await page.screenshot({ path: 'test-results/theme-setup-mobile.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.emulateMedia({ colorScheme: 'light' })
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('link', { name: 'Agents', exact: true }).click()
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  await page
    .getByRole('dialog')
    .getByLabel('Name', { exact: true })
    .fill('Release engineer')
  await page.getByRole('button', { name: 'Save agent' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('link', { name: 'Projects', exact: true }).click()
  await page.getByRole('button', { name: 'Add project', exact: true }).click()
  await page
    .getByRole('dialog')
    .getByLabel('Name', { exact: true })
    .fill('Design system')
  await page.getByLabel('Project directory').fill('/does-not-exist')
  await page.getByRole('button', { name: 'Save project' }).click()
  await expect(page.getByRole('alert')).toContainText('does not exist')
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
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('link', { name: 'Missions', exact: true }).click()
  await page.getByRole('button', { name: 'New mission', exact: true }).click()
  await page.getByLabel('Mission name').fill('Weekly dependency review')
  await page
    .getByLabel('What should happen?')
    .fill('Review dependencies and report the checks you ran. fixture:activity')
  await page.getByRole('combobox', { name: 'When', exact: true }).click()
  await page.getByRole('option', { name: 'Every Monday', exact: true }).click()
  await page.getByLabel('Timezone').fill('Europe/Paris')
  await page.getByRole('button', { name: 'Preview next runs' }).click()
  await expect(page.locator('.schedule-preview span')).toHaveCount(3)
  await page.getByLabel('Timezone').fill('Invalid/Zone')
  await expect(page.locator('.schedule-preview span')).toHaveCount(0)
  await page.getByRole('button', { name: 'Preview next runs' }).click()
  await expect(page.getByRole('alert')).toContainText('IANA timezone')
  await page.getByLabel('Timezone').fill('Europe/Paris')
  await page.getByLabel('Customize mission scope').check()
  await page.getByLabel('Use the agent’s available skills').uncheck()
  await page.getByLabel('review', { exact: true }).check()
  await page.getByRole('button', { name: 'Create mission', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(
    page.getByRole('heading', { name: 'Weekly dependency review' }),
  ).toBeVisible()
  // As on Android, running a mission opens its run.
  await page.getByRole('button', { name: 'Run now', exact: true }).click()
  await expect(page).toHaveURL(/\/runs\//)
  await expect(page.locator('.run-title-meta .status')).toContainText(
    'Execution finished',
  )
  await page.getByRole('button', { name: 'Result', exact: true }).click()
  await expect(page.getByText('The fixture task passed.')).toBeVisible()
  await page.reload()
  await page.getByRole('button', { name: 'Result', exact: true }).click()
  await expect(page.getByText('The fixture task passed.')).toBeVisible()
  await page.getByRole('link', { name: 'Missions', exact: true }).click()
  await page
    .getByRole('button', { name: 'Pause schedule', exact: true })
    .click()
  await expect(page.locator('.mission-card').filter({ hasText: 'Weekly dependency review' })).toContainText('Paused · ')
  await page.getByRole('link', { name: 'Missions', exact: true }).click()
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
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('button', { name: 'Sign out', exact: true }).click()
  await expect(
    page.getByLabel('Email address'),
  ).toBeVisible()
  expect(errors).toEqual([])
})

test('edits supporting files, cancels work, and archives without losing history', async ({
  page,
}) => {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('link', { name: 'Skills', exact: true }).click()
  await page.getByRole('button', { name: 'Edit review', exact: true }).click()
  await page.getByLabel('New supporting file').fill('references/checks.md')
  await page.getByRole('button', { name: 'Add file', exact: true }).click()
  await page
    .getByLabel('Supporting file content')
    .fill('# Checks\nRun the test suite and inspect the diff.')
  await page.getByRole('button', { name: 'Save skill' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('button', { name: 'Edit review', exact: true }).click()
  await page.getByRole('combobox', { name: 'Skill file', exact: true }).click()
  await page.getByRole('option', { name: 'references/checks.md', exact: true }).click()
  await expect(page.getByLabel('Supporting file content')).toHaveValue(
    '# Checks\nRun the test suite and inspect the diff.',
  )
  await page.getByRole('button', { name: 'Preview', exact: true }).click()
  await expect(
    page.getByRole('heading', { name: 'Checks', exact: true }),
  ).toBeVisible()
  await page.keyboard.press('Escape')
  await page.getByRole('link', { name: 'Missions', exact: true }).click()
  await page.getByRole('button', { name: 'Edit mission', exact: true }).click()
  await page.getByLabel('What should happen?').fill('fixture:hang')
  await page.getByRole('button', { name: 'Save changes' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('button', { name: 'Run now', exact: true }).click()
  await expect(page.locator('.run-title-meta .status')).toHaveText('running')
  await page.getByRole('button', { name: 'Stop run', exact: true }).click()
  await page
    .getByRole('dialog')
    .getByRole('button', { name: 'Stop run', exact: true })
    .click()
  await expect(page.locator('.run-title-meta .status')).toHaveText('cancelled')
  await page.getByRole('link', { name: 'Missions', exact: true }).click()
  await page.getByRole('button', { name: 'Mission actions', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Archive', exact: true }).click()
  await expect(
    page.getByRole('heading', { name: 'Weekly dependency review' }),
  ).toHaveCount(0)
  await page.getByRole('group', { name: 'Filter missions' }).getByRole('button', { name: /^Archived/ }).click()
  await expect(
    page.getByRole('heading', { name: 'Weekly dependency review' }),
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Run now', exact: true }),
  ).toBeDisabled()
  await page.getByRole('button', { name: 'Mission actions', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Restore (paused)', exact: true }).click()
  await page.getByRole('group', { name: 'Filter missions' }).getByRole('button', { name: /^All/ }).click()
  await expect(
    page.getByRole('heading', { name: 'Weekly dependency review' }),
  ).toBeVisible()
  await page.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('link', { name: 'Runs', exact: true }).click()
  await expect(page.locator('tbody tr')).toHaveCount(2)
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
      'Start and cancel tasks in YOLO mode with full access inside a private VM',
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
  await page.goto(workspacePath('/settings'))
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
  await page.goto(workspacePath('/'))
  const root = page.locator('html')
  await expect(root).toHaveAttribute('data-theme', 'dark')
  await expect(root).toHaveAttribute('data-theme-preference', 'system')
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
  await page.goto(workspacePath('/settings'))
  await page.locator('.theme-control:not(.theme-compact) label').filter({ hasText: 'Light' }).click()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
  await page.locator('.theme-control:not(.theme-compact) label').filter({ hasText: 'Dark' }).click()
  await expect(page.getByRole('button', { name: 'Appearance: dark', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Appearance: dark', exact: true }).click()
  await page.keyboard.press('Escape')
  await expect(page.locator('.theme-popover')).not.toBeVisible()
  await expect(page.getByRole('button', { name: 'Appearance: dark', exact: true })).toBeFocused()
  await page.route('**/api/tasks', route => route.fulfill({ json: [] }))
  await page.goto(workspacePath('/tasks'))
  await expect(page.getByRole('heading', { name: 'No missions yet', exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('empty-tasks.png'), fullPage: true, animations: 'disabled' })
  await page.route('**/api/connections?*', route => route.fulfill({
    json: [
      { provider: 'github', installed: false, connected: false },
    ],
  }))
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
