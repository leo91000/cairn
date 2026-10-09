import {
  expect,
  expectSingleScroll,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('the header and settings sections support keyboard navigation, deep links and mobile back navigation', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await signIn(page)
  const header = page.getByRole('banner', { name: 'Current installation' })
  await expect(header.getByText('Installation options', { exact: true })).toHaveCount(0)
  await header.getByRole('button', { name: 'Installation settings', exact: true }).click()
  await expect(page).toHaveURL(/\/settings\/installation$/)
  await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Worker environment', exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'Rename installation', exact: true }).click()
  const general = await page.getByRole('heading', { name: 'General', exact: true }).boundingBox()
  const nameField = await page.getByRole('textbox', { name: 'Installation name', exact: true }).boundingBox()
  expect(nameField!.x).toBeCloseTo(general!.x, 0)
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()

  const account = header.getByRole('button', { name: 'Account', exact: true })
  await account.press('ArrowDown')
  const menu = page.getByRole('menu', { name: 'Account', exact: true })
  await expect(menu.getByRole('menuitem', { name: 'Account settings', exact: true })).toBeFocused()
  await page.keyboard.press('c')
  await page.keyboard.press('End')
  await expect(menu.getByRole('menuitem', { name: 'Sign out', exact: true })).toBeFocused()
  await page.keyboard.press('Home')
  await page.keyboard.press('Escape')
  await expect(menu).not.toBeVisible()
  await expect(account).toBeFocused()
  await expect(page).toHaveURL(/\/settings\/installation$/)
  await account.click()
  await page.getByRole('heading', { name: 'Settings', exact: true }).click()
  await expect(menu).not.toBeVisible()
  await expect(account).toBeFocused()

  await account.click()
  await expect(menu.getByRole('menuitem', { name: 'Account settings', exact: true })).toBeFocused()
  await account.evaluate((trigger) => {
    trigger.dataset.focusCount = '0'
    trigger.addEventListener('focus', () => trigger.dataset.focusCount = String(Number(trigger.dataset.focusCount) + 1))
  })
  const mcpUrl = page.getByRole('textbox', { name: 'MCP server URL', exact: true })
  await mcpUrl.click()
  await expect(menu).not.toBeVisible()
  await expect(mcpUrl).toBeFocused()
  await expect(account).toHaveAttribute('data-focus-count', '0')

  await account.press('ArrowDown')
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/\/settings\/account$/)
  await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Appearance', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Account security', exact: true })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Audit log', exact: true }).getByText('installation · claimed', { exact: true })).toBeVisible()
  await expectSingleScroll(page)
  await page.reload()
  await expect(page).toHaveURL(/\/settings\/account$/)
  await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()

  const sections = page.getByRole('navigation', { name: 'Settings sections', exact: true })
  await sections.getByRole('link', { name: 'Sensitive zone', exact: true }).click()
  await expect(page).toHaveURL(/\/settings\/sensitive$/)
  await expect(page.getByRole('button', { name: 'Detach installation', exact: true })).toBeVisible()
  await page.goBack()
  await expect(page).toHaveURL(/\/settings\/account$/)
  await page.goForward()
  await expect(page).toHaveURL(/\/settings\/sensitive$/)

  await page.goto(workspacePath('/settings'))
  await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()
  await expect(sections.getByRole('link', { name: 'Account', exact: true })).toHaveAttribute('aria-current', 'page')
  await expect(sections.locator('[aria-current]')).toHaveCount(1)

  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(workspacePath('/settings'))
  await expect(sections.locator('[aria-current]')).toHaveCount(0)
  await sections.getByRole('link', { name: 'Account', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Back to settings', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Back to settings', exact: true }).click()
  await expect(page).toHaveURL(/\/settings$/)
  await expect(sections.getByRole('link', { name: 'Account', exact: true })).toBeVisible()
  await page.goForward()
  await expect(page).toHaveURL(/\/settings\/account$/)
  await page.goBack()
  await expect(page).toHaveURL(/\/settings$/)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)

  await page.goto(workspacePath('/settings/sensitive'))
  await page.getByRole('button', { name: 'Back to settings', exact: true }).click()
  await expect(page).toHaveURL(/\/settings$/)
  await expect(sections.getByRole('link', { name: 'Sensitive zone', exact: true })).toBeVisible()
})

test('a member can deep-link to Settings without exposing owner controls or requesting owner data', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await signIn(page)
  await page.route('**/api/account/session', async (route) => {
    const response = await route.fetch()
    const session = await response.json()
    session.installations = session.installations.map((installation: object) => ({ ...installation, role: 'member' }))
    await route.fulfill({ json: session })
  })
  const ownerRequests: string[] = []
  page.on('request', (request) => {
    if (/\/api\/(?:settings|tokens|audit)(?:\?|$)/.test(request.url()))
      ownerRequests.push(request.url())
  })
  await page.goto(workspacePath('/settings'))
  await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible()
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Installation', exact: true }).click()
  await expect(page).toHaveURL(/\/settings\/installation$/)
  await expect(page.getByRole('heading', { name: 'General', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Rename installation', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Share installation', exact: true })).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Worker environment', exact: true })).toHaveCount(0)
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Sensitive zone', exact: true }).click()
  await page.reload()
  await expect(page.getByRole('button', { name: 'Leave installation', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Detach installation', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Account', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Account settings', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()
  expect(ownerRequests).toEqual([])
})

test('the installation picker offers adding an installation after the options, also from the keyboard', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await signIn(page)
  await page.route('**/api/account/session', async (route) => {
    const response = await route.fetch()
    const session = await response.json()
    session.installations.push({
      id: 'other-installation',
      name: 'Other installation',
      role: 'member',
      online: false,
      updateRequired: false,
    })
    await route.fulfill({ json: session })
  })
  await page.reload()
  const picker = page.getByRole('combobox', { name: 'Current installation', exact: true })
  await picker.press('ArrowDown')
  const add = page.getByRole('button', { name: 'Add an installation', exact: true })
  await expect(add).toBeVisible()
  const list = await page.getByRole('listbox', { name: 'Current installation', exact: true }).boundingBox()
  const action = await add.boundingBox()
  expect(action!.y).toBeGreaterThanOrEqual(list!.y + list!.height)
  await picker.press('Tab')
  await expect(add).toBeFocused()
  await page.keyboard.press('Escape')
  await expect(picker).toBeFocused()
  await expect(picker).toHaveAttribute('aria-expanded', 'false')
  await picker.press('ArrowDown')
  await picker.press('Tab')
  await page.keyboard.press('Enter')
  await expect(picker).toHaveAttribute('aria-expanded', 'false')
  await expect(page.getByLabel('Installation claim code', { exact: true })).not.toHaveValue('')
  await expect(page.getByLabel('Installation command', { exact: true })).toHaveValue(/--claim-code/)
})

test('an installation awaiting an update still permits account and installation settings', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await signIn(page)
  await page.route('**/api/account/session', async (route) => {
    const response = await route.fetch()
    const session = await response.json()
    session.installations = session.installations.map((installation: object) => ({ ...installation, updateRequired: true }))
    await route.fulfill({ json: session })
  })
  await page.route('**/api/installations', async (route) => {
    const response = await route.fetch()
    const installations = await response.json()
    await route.fulfill({ json: installations.map((installation: object) => ({ ...installation, updateRequired: true })) })
  })
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('Mise à jour nécessaire')
  await page.getByRole('button', { name: 'Installation settings', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'General', exact: true })).toBeVisible()
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Sensitive zone', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Detach installation', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Account', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Account settings', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Account security', exact: true })).toBeVisible()
})

test('account method changes keep header actions disabled while the request is pending', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await signIn(page)
  await page.route('**/api/account/methods', route => route.fulfill({
    json: {
      methods: [
        { id: 'email-fixture', kind: 'email', label: 'fixture@example.test' },
        { id: 'passkey-fixture', kind: 'passkey', label: 'Backup key' },
      ],
    },
  }))
  let finish = () => {}
  const pending = new Promise<void>(resolve => finish = resolve)
  await page.route('**/api/account/methods/remove', async (route) => {
    await pending
    await route.fulfill({ status: 204 })
  })
  try {
    await page.goto(workspacePath('/settings/account'))
    const header = page.getByRole('banner', { name: 'Current installation' })
    await expect(header.getByRole('combobox', { name: 'Current installation', exact: true })).toHaveCount(0)
    await page.getByRole('button', { name: 'Remove Backup key', exact: true }).click()
    await expect(header.getByRole('button', { name: 'Installation settings', exact: true })).toBeDisabled()
    await expect(header.getByRole('button', { name: 'Account', exact: true })).toBeDisabled()
    finish()
    await expect(header.getByRole('button', { name: 'Installation settings', exact: true })).toBeEnabled()
  }
  finally {
    finish()
  }
})
