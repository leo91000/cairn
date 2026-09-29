import { expect, test } from './fixtures'

test('GitHub picker handles retry, search, keyboard selection and import on a phone', async ({ page }) => {
  test.setTimeout(90000)
  await page.goto('/projects')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  let failed = true
  const requested: string[] = []
  await page.route('**/api/github/repositories?*', async (route) => {
    if (failed) {
      failed = false
      return route.fulfill({ status: 502, json: { error: 'Check the GitHub connection and retry.' } })
    }

    const second = route.request().url().endsWith('page=2')
    requested.push(second ? '2' : '1')
    const repo = (name: string, extra = {}) => ({
      fullName: `team/${name}`,
      name,
      owner: 'team',
      description: 'A private repository',
      defaultBranch: 'develop',
      private: true,
      archived: false,
      fork: false,
      language: 'Rust',
      stars: 1200,
      pushedAt: new Date(Date.now() - 3 * 86400000).toISOString(),
      imported: false,
      ...extra,
    })
    await route.fulfill({ json: second ? { repositories: [repo('selected')], nextPage: null } : { repositories: [repo('existing', { imported: true }), repo('public-site', { private: false, language: 'Vue' })], nextPage: 2 } })
  })
  let payload: any
  await page.route('**/api/projects/github', async (route) => {
    payload = route.request().postDataJSON()
    await route.fulfill({ json: { id: 'imported-project' } })
  })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('button', { name: 'Add project', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox', { name: 'Add from' }).click()
  await page.getByRole('option', { name: 'GitHub', exact: true }).click()
  await expect(dialog.getByText('Check the GitHub connection and retry.')).toBeVisible()
  await dialog.getByRole('button', { name: 'Retry' }).click()
  await expect(dialog.getByRole('option', { name: /team\/existing/ })).toHaveAttribute('aria-disabled', 'true')
  await expect(dialog.getByRole('option', { name: /team\/existing/ })).toContainText('Added')
  await expect(dialog.getByRole('option', { name: /public-site/ })).toContainText('Public')
  await expect(dialog.getByRole('option', { name: /public-site/ })).toContainText('Updated 3 days ago')
  await dialog.getByRole('button', { name: /Private/ }).click()
  await expect(dialog.getByRole('option', { name: /public-site/ })).toHaveCount(0)
  const search = dialog.getByRole('combobox', { name: 'Search repositories' })
  await search.fill('selec')
  // Searching fetches further pages on its own until matches appear.
  await expect(dialog.getByRole('option', { name: /team\/selected/ })).toBeVisible()
  expect(requested).toContain('2')
  await expect(dialog.locator('mark', { hasText: 'selec' })).toBeVisible()
  await search.press('ArrowDown')
  await search.press('Enter')
  await expect(dialog.getByRole('listbox')).toHaveCount(0)
  await expect(dialog.getByText('team/selected')).toBeVisible()
  await expect(dialog.getByLabel('Name', { exact: true })).toHaveValue('selected')
  await expect(dialog.getByLabel('Base branch', { exact: false })).toHaveValue('develop')
  await expect(dialog.getByLabel('Project directory', { exact: false })).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await dialog.getByRole('button', { name: 'Change' }).click()
  await expect(search).toBeFocused()
  await dialog.getByRole('option', { name: /team\/selected/ }).click()
  await dialog.getByRole('button', { name: 'Save project', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(payload).toEqual({
    repository: 'team/selected',
    name: 'selected',
    description: 'A private repository',
    baseBranch: 'develop',
  })
})
