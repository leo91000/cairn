import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

// A package with a project source and two enabled accounts; a paused one cannot import.
const manifest = {
  id: 'codehost',
  name: 'Code host',
  version: '1.0.0',
  apiVersion: 2,
  description: 'Synthetic project source.',
  capabilities: ['http'],
  instances: { singular: 'Account', plural: 'Accounts' },
  projectSources: [{ name: 'repositories', title: 'Code host repository', description: 'Repositories this account reaches.' }],
}

function account(id: string, name: string, enabled = true) {
  return {
    id,
    pluginId: 'codehost',
    name,
    enabled,
    settings: {},
    agentIds: [],
    agentId: '',
    accessMode: 'requests',
    userIds: [],
    people: [],
    requests: [],
    activity: [],
    usage: { hour: 0 },
    maxMessagesPerHour: 20,
    projectId: null,
    status: { state: 'ready' },
  }
}

test('a plugin project source handles retry, search, keyboard selection and import on a phone', async ({ page }) => {
  test.setTimeout(90000)
  await useRelayForHttpMocks(page)
  await page.route('**/api/plugins', route => route.fulfill({
    json: {
      packages: [{
        id: 'codehost',
        builtin: true,
        status: 'active',
        active: '1.0.0',
        manifest,
        versions: [],
      }],
      capabilities: [],
      connections: [account('work', 'Work'), account('paused', 'Paused', false), account('personal', 'Personal')],
    },
  }))
  let failed = true
  const requested: string[] = []
  await page.route('**/api/plugins/codehost/projects?*', async (route) => {
    const url = new URL(route.request().url())
    if (failed) {
      failed = false
      return route.fulfill({ status: 502, json: { error: 'Check the Code host account and retry.' } })
    }

    requested.push(`${url.searchParams.get('instance')}:${url.searchParams.get('page')}`)
    expect(url.searchParams.get('source')).toBe('repositories')
    const item = (name: string, extra = {}) => ({
      id: `team/${name}`,
      name: `team/${name}`,
      description: 'A private repository',
      defaultBranch: 'develop',
      badges: ['Private', 'Rust'],
      updatedAt: new Date(Date.now() - 3 * 86400000).toISOString(),
      url: `https://code.example/team/${name}`,
      imported: false,
      ...extra,
    })
    const second = url.searchParams.get('page') === '2'
    await route.fulfill({ json: second ? { items: [item('selected')], nextPage: null } : { items: [item('existing', { imported: true }), item('public-site', { badges: ['Vue'] })], nextPage: 2 } })
  })
  let payload: any
  await page.route('**/api/projects/import', async (route) => {
    payload = route.request().postDataJSON()
    await route.fulfill({ json: { id: 'imported-project', path: '/workspaces/selected', sourceMode: 'remote' } })
  })
  await page.goto(workspacePath('/projects'))
  await authenticateWorkspace(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('button', { name: 'Add project', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox', { name: 'Add from' }).click()
  await page.getByRole('option', { name: /Code host repository/ }).click()
  // Several enabled accounts: the first is chosen, the paused one is not offered.
  await expect(dialog.getByRole('combobox', { name: 'Account' })).toHaveValue('Work')
  await expect(dialog.getByText('Check the Code host account and retry.')).toBeVisible()
  await dialog.getByRole('button', { name: 'Retry' }).click()
  await expect(dialog.getByRole('option', { name: /team\/existing/ })).toHaveAttribute('aria-disabled', 'true')
  await expect(dialog.getByRole('option', { name: /team\/existing/ })).toContainText('Added')
  await expect(dialog.getByRole('option', { name: /public-site/ })).toContainText('Vue')
  await expect(dialog.getByRole('option', { name: /public-site/ })).toContainText('Updated 3 days ago')
  await expect(dialog.getByRole('option', { name: /public-site/ }).getByRole('link', { name: 'Open team/public-site' })).toHaveAttribute('href', 'https://code.example/team/public-site')
  const search = dialog.getByRole('combobox', { name: 'Search projects' })
  // Badges match too, and every typed word must.
  await search.fill('vue public')
  await expect(dialog.getByRole('option')).toHaveCount(1)
  await search.fill('selec')
  // Searching fetches further pages on its own until matches appear.
  await expect(dialog.getByRole('option', { name: /team\/selected/ })).toBeVisible()
  expect(requested).toEqual(['work:1', 'work:2'])
  await expect(dialog.locator('mark', { hasText: 'selec' })).toBeVisible()
  await search.press('ArrowDown')
  await search.press('Enter')
  await expect(dialog.getByRole('listbox')).toHaveCount(0)
  await expect(dialog.getByText('team/selected')).toBeVisible()
  await expect(dialog.getByLabel('Name', { exact: true })).toHaveValue('team/selected')
  await expect(dialog.getByLabel('Base branch', { exact: false })).toHaveValue('develop')
  await expect(dialog.getByLabel('Project directory', { exact: false })).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await dialog.getByRole('button', { name: 'Change' }).click()
  await expect(search).toBeFocused()
  await search.fill('')

  // Another account lists its own projects from the first page.
  await dialog.getByRole('combobox', { name: 'Account' }).click()
  await page.getByRole('option', { name: 'Personal', exact: true }).click()
  await expect(dialog.getByRole('option', { name: /team\/existing/ })).toBeVisible()
  expect(requested).toContain('personal:1')
  await search.fill('selected')
  await dialog.getByRole('option', { name: /team\/selected/ }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('Selected')
  await dialog.getByRole('button', { name: 'Save project', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(payload).toEqual({
    pluginId: 'codehost',
    source: 'repositories',
    instance: 'personal',
    id: 'team/selected',
    name: 'Selected',
    description: 'A private repository',
    baseBranch: 'develop',
  })
})
