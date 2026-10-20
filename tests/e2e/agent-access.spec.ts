import type { Page } from '@playwright/test'
import type { Workspace } from './fixtures'
import {
  signIn as authenticateWorkspace,
  createAgent,
  expect,
  expectSingleScroll,
  expectUncovered,
  openAgentSection,
  saveAgent,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

const MAIN_AGENT_ID = '00000000-0000-4000-8000-000000000001'

async function signIn(page: Page) {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible({ timeout: 30000 })
}

// Enrolls a machine through the node protocol; `exclusive` says whether its runtime can suspend others.
async function enroll(page: Page, workspace: Workspace, name: string, exclusive: boolean) {
  const invitation = await workspace.api('/api/nodes/enrollments', 'POST', { name })
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code: invitation.code,
      name,
      protocol: 1,
      runtimeId: 'fixture',
      capabilities: {
        os: 'linux',
        arch: 'x86_64',
        kvm: true,
        cpu: 8,
        memoryMiB: 16384,
        diskMiB: 65536,
      },
    },
  })
  expect(response.ok()).toBe(true)
  const node = await response.json()
  const heartbeat = await page.request.post(`${workspace.installationUrl}/internal/nodes/heartbeat`, {
    headers: { authorization: `Bearer ${node.token}` },
    data: { exclusiveReservations: exclusive },
  })
  expect(heartbeat.ok()).toBe(true)
  return node.nodeId as string
}

test('an agent is created on its page and its access is inherited by missions on mobile in both themes', async ({ page }, testInfo) => {
  await signIn(page)
  await page.goto(workspacePath('/agents'))
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.getByRole('heading', { name: 'Main agent', exact: true })).toBeVisible()
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await expect(page).toHaveURL(/\/agents\/new\?section=identity$/)
  await page.getByLabel('Name', { exact: true }).fill('Scoped reviewer')
  await openAgentSection(page, 'Projects')
  await page.getByRole('group', { name: 'Projects access' }).getByRole('button', { name: 'Choose', exact: true }).click()
  await page.getByLabel('Design system', { exact: true }).check()
  await openAgentSection(page, 'Skills')
  await page.getByRole('group', { name: 'Skills access' }).getByRole('button', { name: 'Choose', exact: true }).click()
  await page.getByLabel('review', { exact: true }).check()
  await openAgentSection(page, 'GitHub')
  await expect(page.getByRole('radio', { name: /Shared connection/ })).toBeDisabled()
  await expect(page.getByRole('radio', { name: /No GitHub/ })).toBeChecked()
  await openAgentSection(page, 'Execution mode')
  await page.getByRole('radio', { name: /Read only/ }).check()
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 844 })
      await expectSingleScroll(page)
      await page.screenshot({ path: testInfo.outputPath(`agent-access-${colorScheme}-${width}.png`), animations: 'disabled' })
    }
  }

  await page.setViewportSize({ width: 390, height: 844 })
  const id = await createAgent(page)
  await expect(page.getByRole('heading', { name: 'Scoped reviewer', level: 1 })).toBeVisible()
  await page.getByRole('link', { name: 'Back to agents' }).click()
  await expect(page.locator('article').filter({ hasText: 'Scoped reviewer' })).toContainText('1 project · 1 node')
  await expect(page.locator(`a[href$="/agents/${id}"]`).first()).toBeVisible()
  await page.getByRole('link', { name: 'Missions', exact: true }).filter({ visible: true }).click()
  await page.getByRole('button', { name: 'New mission', exact: true }).click()
  await expect(page.getByRole('combobox', { name: 'Agent', exact: true })).toHaveValue('Main agent')
  await expect(page.getByRole('combobox', { name: 'Project context', exact: true })).toHaveCount(0)
  await page.getByRole('combobox', { name: 'Agent', exact: true }).click()
  await page.getByRole('option', { name: 'Scoped reviewer', exact: true }).click()
  await page.getByLabel('Mission name', { exact: true }).fill('Review the repository')
  await page.getByLabel('What should happen?').fill('Review code without making changes.')
  await expect(page.locator('.agent-scope-summary')).toContainText('1 allowed project')
  await page.screenshot({ path: testInfo.outputPath('agent-first-task.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Create mission', exact: true }).click()
  // As on Android, the new mission opens in its sheet on phones.
  await expect(page.getByRole('dialog', { name: 'Review the repository' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Review the repository' })).toBeVisible()
})

// Agents are created on their own page; see the agent retry journeys below.
for (const kind of ['projects'] as const) {
  for (const failure of ['refresh', 'lost response'] as const) {
    test(`${kind} retry updates the same resource after ${failure}`, async ({ page, workspace }, testInfo) => {
      await useRelayForHttpMocks(page)
      await page.goto(workspacePath(`/${kind}`))
      await authenticateWorkspace(page)
      const label = 'project'
      await page.getByRole('button', { name: 'Add project', exact: true }).click()
      const dialog = page.getByRole('dialog')
      const name = `Retry ${kind} ${failure}`
      await dialog.getByLabel('Name', { exact: true }).fill(name)
      await dialog.getByLabel('Project directory').fill(workspace.projectPath)

      const creations: string[] = []
      let rejectRefresh = false
      let createdId = ''
      let updates = 0
      let rejectUpdate = false
      await page.route(`**/api/${kind}/*`, async (route) => {
        if (route.request().method() !== 'PUT') {
          await route.continue()
          return
        }

        updates++
        if (rejectUpdate)
          await route.fulfill({ status: 400, json: { error: 'Update temporarily rejected' } })
        else
          await route.continue()
      })
      await page.route(`**/api/${kind}`, async (route) => {
        const request = route.request()
        if (request.method() === 'POST') {
          creations.push(request.postData()!)
          const response = await route.fetch()
          createdId = (await response.json()).id
          if (creations.length === 1 && failure === 'lost response') {
            await route.abort('failed')
            return
          }

          rejectRefresh = creations.length === 1 && failure === 'refresh'
          await route.fulfill({ response })
        }
        else if (request.method() === 'GET' && rejectRefresh) {
          await route.fulfill({ status: 400, json: { error: 'Resource list temporarily rejected' } })
        }
        else {
          await route.continue()
        }
      })

      await page.setViewportSize({ width: 390, height: 844 })
      await dialog.getByRole('button', { name: `Save ${label}`, exact: true }).click()
      await expect(dialog.getByRole('alert')).toContainText(failure === 'refresh' ? 'saved, but the workspace could not be reloaded' : `Could not confirm whether the ${label} was saved`)
      await expectUncovered(dialog.getByRole('alert'))
      rejectRefresh = false
      const first = (await workspace.api(`/api/${kind}`)).filter((item: { id: string }) => item.id === createdId)
      expect(first).toHaveLength(1)
      const createdAt = first[0].createdAt
      await page.screenshot({ path: testInfo.outputPath(`${kind}-${failure.replaceAll(' ', '-')}-mobile.png`), animations: 'disabled' })
      await dialog.getByLabel('Name', { exact: true }).fill(`${name} edited`)
      if (failure === 'refresh') {
        rejectUpdate = true
        await dialog.getByRole('button', { name: `Save ${label}`, exact: true }).click()
        await expect(dialog.getByRole('alert')).toContainText(`Project saved, but the latest changes could not be saved. Update temporarily rejected`)
        await expectUncovered(dialog.getByRole('alert'))
        await page.screenshot({ path: testInfo.outputPath(`${kind}-update-mobile.png`), animations: 'disabled' })
        rejectUpdate = false
      }

      await dialog.getByRole('button', { name: `Save ${label}`, exact: true }).click()
      await expect(dialog).toHaveCount(0)

      const saved = (await workspace.api(`/api/${kind}`)).filter((item: { name: string }) => item.name.startsWith(name))
      expect(saved).toHaveLength(1)
      expect(saved[0]).toMatchObject({ id: createdId, name: `${name} edited`, createdAt })
      expect(updates).toBe(failure === 'refresh' ? 2 : 1)
      expect(creations).toHaveLength(failure === 'refresh' ? 1 : 2)
      if (failure === 'lost response')
        expect(creations[1]).toBe(creations[0])

      if (failure === 'refresh') {
        rejectUpdate = true
        await page.getByRole('button', { name: `Edit ${name} edited`, exact: true }).click()
        await dialog.getByRole('button', { name: `Save ${label}`, exact: true }).click()
        await expect(dialog.getByRole('alert')).toHaveText(`Could not save the ${label}. Update temporarily rejected`)
      }
    })
  }
}

for (const failure of ['refresh', 'lost response'] as const) {
  test(`agent creation retry updates the same agent after ${failure}`, async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    await page.goto(workspacePath('/agents/new'))
    await authenticateWorkspace(page)
    const name = `Retry agent ${failure}`
    await page.getByLabel('Name', { exact: true }).fill(name)

    const creations: string[] = []
    let createdId = ''
    let rejectRefresh = false
    await page.route('**/api/agents', async (route) => {
      const request = route.request()
      if (request.method() === 'POST') {
        creations.push(request.postData()!)
        const response = await route.fetch()
        createdId = (await response.json()).id
        if (creations.length === 1 && failure === 'lost response') {
          await route.abort('failed')
          return
        }

        rejectRefresh = creations.length === 1 && failure === 'refresh'
        await route.fulfill({ response })
      }
      else if (request.method() === 'GET' && rejectRefresh) {
        await route.fulfill({ status: 400, json: { error: 'Resource list temporarily rejected' } })
      }
      else {
        await route.continue()
      }
    })

    await page.setViewportSize({ width: 390, height: 844 })
    const bar = page.getByRole('region', { name: 'Unsaved changes' })
    await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
    await expect(bar.getByRole('alert')).toContainText(failure === 'refresh' ? 'Agent saved, but the workspace could not be reloaded' : 'Could not confirm whether the agent was saved')
    await expectUncovered(bar.getByRole('alert'))
    rejectRefresh = false
    await page.getByLabel('Name', { exact: true }).fill(`${name} edited`)
    // After a lost response the creation is replayed, then updated with the newer edits.
    let updates = 0
    await page.route('**/api/agents/*', async (route) => {
      if (route.request().method() !== 'PUT') {
        await route.continue()
        return
      }

      updates++
      if (updates === 1 && failure === 'lost response')
        await route.fulfill({ status: 500, json: { error: 'Update temporarily unavailable' } })
      else
        await route.continue()
    })
    await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
    if (failure === 'lost response') {
      await expect(bar.getByRole('alert')).toContainText('Agent saved, but the latest changes could not be saved. Update temporarily unavailable')
      await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
    }

    await expect(page).toHaveURL(new RegExp(`/agents/${createdId}$`))
    // Either way the newer edits reach the created agent by an update.
    expect(updates).toBe(failure === 'lost response' ? 2 : 1)

    const saved = (await workspace.api('/api/agents')).filter((item: { name: string }) => item.name.startsWith(name))
    expect(saved).toHaveLength(1)
    expect(saved[0]).toMatchObject({ id: createdId, name: `${name} edited` })
    expect(creations).toHaveLength(failure === 'refresh' ? 1 : 2)
    if (failure === 'lost response')
      expect(creations[1]).toBe(creations[0])
  })
}

test('a failed dedicated GitHub token keeps the created agent and retries only the connection and update', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/agents/new'))
  await authenticateWorkspace(page)
  await page.getByLabel('Name', { exact: true }).fill('Token retry agent')
  await openAgentSection(page, 'Projects')
  await page.getByRole('group', { name: 'Projects access' }).getByRole('button', { name: 'Choose', exact: true }).click()
  await openAgentSection(page, 'GitHub')
  await page.getByRole('radio', { name: /^Dedicated token/ }).check()
  await page.getByLabel('GitHub token').fill('fixture-token-not-a-secret')
  let tokenAttempts = 0
  let creates = 0
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/api/agents'))
      creates++
  })
  await page.route('**/api/agents/*/github-token', async (route) => {
    // The created agent's page reads the token state too; only saves are attempts.
    if (route.request().method() !== 'PUT') {
      await route.continue()
      return
    }

    tokenAttempts++
    await route.fulfill(tokenAttempts === 1
      ? { status: 400, json: { error: 'GitHub connection rejected' } }
      : { json: { configured: true } })
  })
  const bar = page.getByRole('region', { name: 'Unsaved changes' })
  await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(bar.getByRole('alert')).toContainText('Agent saved, but the GitHub connection could not be saved')
  await expect(page.getByLabel('GitHub token')).toHaveValue('fixture-token-not-a-secret')
  await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/)
  expect(creates).toBe(1)
  expect(tokenAttempts).toBe(2)
  expect((await workspace.api('/api/agents')).filter((item: { name: string }) => item.name === 'Token retry agent')).toHaveLength(1)
})

test('agents start unlimited and retain an optional limit after saving', async ({ page, workspace }) => {
  await signIn(page)
  await page.goto(workspacePath('/agents/new'))
  await page.getByLabel('Name', { exact: true }).fill('Long running agent')
  await openAgentSection(page, 'Limits')
  await expect(page.getByLabel('No time limit', { exact: true })).toBeChecked()
  await expect(page.getByLabel('Time limit (minutes)')).toHaveCount(0)
  const id = await createAgent(page)
  const saved = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === id)
  expect(saved.timeoutMinutes).toBe(0)
  await openAgentSection(page, 'Limits')
  await page.getByLabel('No time limit', { exact: true }).uncheck()
  await page.getByLabel('Time limit (minutes)').fill('45')
  await expect(page.getByRole('region', { name: 'Unsaved changes' })).toContainText('1 unsaved change')
  await saveAgent(page)
  await page.reload()
  await openAgentSection(page, 'Limits')
  await expect(page.getByLabel('Time limit (minutes)')).toHaveValue('45')
  await page.getByLabel('No time limit', { exact: true }).check()
  await saveAgent(page)
  expect((await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === id).timeoutMinutes).toBe(0)
})

test('installation management is a reviewed power, and the main agent keeps its fixed access', async ({ page, workspace }, testInfo) => {
  await signIn(page)
  await page.goto(workspacePath('/agents/new'))
  await page.getByLabel('Name', { exact: true }).fill('Installation helper')
  await openAgentSection(page, 'Elevated rights')
  const power = page.getByRole('switch', { name: 'Installation management', exact: true })
  await expect(power).not.toBeChecked()
  await power.check()
  await page.getByRole('button', { name: 'Create agent', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('Added: installation management.')
  await expect(review.getByText('Power', { exact: true })).toBeVisible()
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/)
  const agent = (await workspace.api('/api/agents')).find((item: { name: string }) => item.name === 'Installation helper')
  expect(agent.access.installation).toBe(true)
  await openAgentSection(page, 'Elevated rights')
  await expect(power).toBeChecked()
  // Removing a power saves directly: nothing is interrupted.
  await power.uncheck()
  await saveAgent(page)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).access.installation).toBe(false)

  await page.goto(workspacePath(`/agents/${MAIN_AGENT_ID}`))
  await expect(page.getByText('The main agent always has access to every project, skill, MCP connection and GitHub').filter({ visible: true })).toBeVisible()
  for (const section of ['Projects', 'Skills', 'MCP & tools', 'GitHub']) {
    await openAgentSection(page, section)
    await expect(page.getByText('Fixed', { exact: true }).last()).toBeVisible()
    await expect(page.getByRole('button', { name: 'Choose', exact: true })).toHaveCount(0)
    await expect(page.getByRole('radio')).toHaveCount(0)
  }

  await openAgentSection(page, 'Elevated rights')
  await power.check()
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 900 })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`main-agent-${width}.png`), animations: 'disabled' })
  }

  await saveAgent(page, true)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === MAIN_AGENT_ID).access.installation).toBe(true)
  const main = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === MAIN_AGENT_ID)
  await workspace.api(`/api/agents/${MAIN_AGENT_ID}`, 'PUT', { ...main, access: { ...main.access, installation: false } })
})

test('exclusive reservation is granted per node from the agent page once the agent may run there', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  const gpu = await enroll(page, workspace, 'gpu-box', true)
  await enroll(page, workspace, 'old-box', false)
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Release engineer', description: 'Publishes web and Android releases' })
  await signIn(page)
  await page.goto(workspacePath(`/agents/${agent.id}?section=nodes`))
  await expect(page.getByRole('heading', { name: 'Nodes', level: 2 })).toBeVisible()
  const exclusive = page.getByRole('switch', { name: 'Can reserve gpu-box exclusively' })
  const run = page.getByRole('switch', { name: 'Can run its conversations on gpu-box' })
  await expect(exclusive).toBeDisabled()
  await expect(page.getByRole('listitem', { name: 'gpu-box' })).toContainText('Requires permission to run')
  await run.check()
  await expect(exclusive).toBeEnabled()
  await exclusive.check()
  await page.getByRole('switch', { name: 'Can run its conversations on old-box' }).check()
  await expect(page.getByRole('switch', { name: 'Can reserve old-box exclusively' })).toBeDisabled()
  await expect(page.getByRole('listitem', { name: 'old-box' })).toContainText('Unavailable on this runtime')
  // Turning execution off also removes the reservation.
  await run.uncheck()
  await expect(exclusive).not.toBeChecked()
  await run.check()
  await exclusive.check()
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 900 })
      await expectSingleScroll(page)
      await page.screenshot({ path: testInfo.outputPath(`agent-nodes-${colorScheme}-${width}.png`), fullPage: true, animations: 'disabled' })
    }
  }

  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('Added: exclusive reservation on gpu-box. This agent will be able to suspend other agents’ conversations on this machine.')
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(review).toHaveCount(0)
  const saved = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id)
  expect(saved.exclusiveNodes).toEqual([gpu])
  expect(saved.access).not.toHaveProperty('exclusiveNodes')
  expect(saved.access.nodes).toContain(gpu)
  expect((await workspace.api('/api/nodes')).find((node: { id: string }) => node.id === gpu).exclusiveAgents).toEqual([agent.id])

  await page.getByRole('link', { name: 'Overview', exact: true }).click()
  await expect(page.getByRole('button', { name: /^Exclusive reservation gpu-box/ })).toBeVisible()
  // The node only shows who holds the grant.
  await page.goto(workspacePath('/nodes'))
  const card = page.getByRole('article').filter({ has: page.getByRole('heading', { name: 'gpu-box' }) })
  await expect(card).toContainText('1 can reserve it exclusively: Release engineer')
  await expect(card.getByRole('link', { name: 'Manage access from the agents ›' })).toBeVisible()
})

test('leaving with unsaved changes asks first, and a concurrent change is merged into the kept draft', async ({ page, workspace }) => {
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Docs writer' })
  await signIn(page)
  await page.goto(workspacePath(`/agents/${agent.id}?section=identity`))
  await page.getByLabel('Description', { exact: true }).fill('Keeps the documentation current')
  await expect(page.getByRole('region', { name: 'Unsaved changes' })).toContainText('1 unsaved change')
  await page.getByRole('link', { name: 'Back to agents' }).click()
  const guard = page.getByRole('dialog', { name: 'Discard unsaved changes?' })
  await guard.getByRole('button', { name: 'Keep editing' }).click()
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Keeps the documentation current')

  // Someone else saves the agent meanwhile.
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { name: 'Documentation writer' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const bar = page.getByRole('region', { name: 'Unsaved changes' })
  await expect(bar.getByRole('alert')).toHaveText('This agent was changed elsewhere. Reload it before saving.')
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Keeps the documentation current')
  // As on Android, reloading keeps the owner's edit on top of the newer agent.
  await bar.getByRole('button', { name: 'Reload', exact: true }).click()
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('Documentation writer')
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Keeps the documentation current')
  await expect(bar).toContainText('1 unsaved change')
  await saveAgent(page)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id)).toMatchObject({
    name: 'Documentation writer',
    description: 'Keeps the documentation current',
  })

  await page.getByLabel('Description', { exact: true }).fill('Something else')
  await page.getByRole('link', { name: 'Back to agents' }).click()
  await guard.getByRole('button', { name: 'Discard changes' }).click()
  await expect(page).toHaveURL(/\/agents$/)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).description).toBe('Keeps the documentation current')
})

test('a failed GitHub token keeps the pending token or removal, and the retry only saves the token', async ({ page, workspace }) => {
  // Earlier journeys share this worker's request budget. Start these
  // save-and-retry flows with a fresh limiter while preserving the seeded data.
  await workspace.restart()
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Token keeper', access: { projects: [], github: false } })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/agents/${agent.id}?section=identity`))
  await authenticateWorkspace(page)
  let rejectToken = true
  let tokenAttempts = 0
  let configured = false
  const profiles: { description: string }[] = []
  page.on('response', async (response) => {
    if (response.request().method() === 'PUT' && response.url().endsWith(`/api/agents/${agent.id}`) && response.ok())
      profiles.push(await response.json())
  })
  await page.route('**/api/agents/*/github-token', async (route) => {
    if (route.request().method() !== 'PUT') {
      await route.continue()
      return
    }

    tokenAttempts++
    if (rejectToken) {
      await route.fulfill({ status: 400, json: { error: 'GitHub connection rejected' } })
      return
    }

    const response = await route.fetch()
    configured = (await response.json()).configured
    await route.fulfill({ response })
  })
  const bar = page.getByRole('region', { name: 'Unsaved changes' })
  const save = bar.getByRole('button', { name: 'Save', exact: true })

  await page.getByLabel('Description', { exact: true }).fill('Releases with its own token')
  await openAgentSection(page, 'GitHub')
  await page.getByRole('radio', { name: /^Dedicated token/ }).check()
  await page.getByLabel('GitHub token').fill('fixture-token-not-a-secret')
  await save.click()
  await expect(bar.getByRole('alert')).toContainText('Agent saved, but the GitHub connection could not be saved')
  // The profile is saved; the token and its mode stay pending for the retry.
  expect(profiles.map(profile => profile.description)).toEqual(['Releases with its own token'])
  await expect(page.getByRole('radio', { name: /^Dedicated token/ })).toBeChecked()
  await expect(page.getByLabel('GitHub token')).toHaveValue('fixture-token-not-a-secret')
  await expect(save).toBeEnabled()
  rejectToken = false
  await save.click()
  await expect(bar).toHaveCount(0)
  expect(configured).toBe(true)
  expect(profiles).toHaveLength(1)
  expect(tokenAttempts).toBe(2)

  rejectToken = true
  await page.getByRole('radio', { name: /^No GitHub/ }).check()
  await save.click()
  await expect(bar.getByRole('alert')).toContainText('Agent saved, but the GitHub connection could not be saved')
  await expect(page.getByRole('radio', { name: /^No GitHub/ })).toBeChecked()
  await expect(save).toBeEnabled()
  expect(configured).toBe(true)
  rejectToken = false
  await save.click()
  await expect(bar).toHaveCount(0)
  expect(configured).toBe(false)
  await expect(page.getByRole('radio', { name: /^No GitHub/ })).toBeChecked()
  expect(profiles).toHaveLength(1)
  expect(tokenAttempts).toBe(4)
})

test('recovering a created agent keeps its revision, so a concurrent change is a conflict to reload', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/agents/new'))
  await authenticateWorkspace(page)
  await page.getByLabel('Name', { exact: true }).fill('Recovered agent')
  let createdId = ''
  let creations = 0
  let rejectRefresh = false
  await page.route('**/api/agents', async (route) => {
    const request = route.request()
    if (request.method() === 'POST') {
      creations++
      const response = await route.fetch()
      createdId = (await response.json()).id
      rejectRefresh = true
      await route.fulfill({ response })
    }
    else if (request.method() === 'GET' && rejectRefresh) {
      await route.fulfill({ status: 400, json: { error: 'Resource list temporarily rejected' } })
    }
    else {
      await route.continue()
    }
  })
  const bar = page.getByRole('region', { name: 'Unsaved changes' })
  await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(bar.getByRole('alert')).toContainText('Agent saved, but the workspace could not be reloaded')
  rejectRefresh = false

  // Someone else changes the created agent before the retry.
  await workspace.api(`/api/agents/${createdId}`, 'PUT', { name: 'Recovered agent', description: 'Changed elsewhere' })
  await workspace.api(`/api/agents/${createdId}/github-token`, 'PUT', { token: 'fixture-token-not-a-secret' })
  await page.getByLabel('Name', { exact: true }).fill('Recovered agent edited')
  await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(bar.getByRole('alert')).toHaveText('This agent was changed elsewhere. Reload it before saving.')
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === createdId)).toMatchObject({
    name: 'Recovered agent',
    description: 'Changed elsewhere',
  })

  // Reloading also re-reads its GitHub token, saved meanwhile, although the page is still /agents/new.
  const tokenState = page.waitForRequest(request => request.method() === 'GET' && request.url().endsWith(`/api/agents/${createdId}/github-token`))
  await bar.getByRole('button', { name: 'Reload', exact: true }).click()
  await tokenState
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('Recovered agent edited')
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Changed elsewhere')
  await openAgentSection(page, 'GitHub')
  await expect(page.getByRole('radio', { name: /^Dedicated token/ })).toBeChecked()
  await expect(page.getByText('A token is saved', { exact: true })).toBeVisible()
  await bar.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(page).toHaveURL(new RegExp(`/agents/${createdId}$`))
  expect(creations).toBe(1)
  expect((await workspace.api('/api/agents')).filter((item: { name: string }) => item.name.startsWith('Recovered agent'))).toEqual([
    expect.objectContaining({ id: createdId, name: 'Recovered agent edited', description: 'Changed elsewhere' }),
  ])
  expect(await workspace.api(`/api/agents/${createdId}/github-token`)).toEqual({ configured: true })
})

test('tool choices stay visible and editable when every MCP connection is granted', async ({ page, workspace }) => {
  const mcp = await workspace.api('/api/mcps', 'POST', {
    name: 'Design tools',
    transport: 'http',
    url: 'http://127.0.0.1:9/mcp',
    allowPrivateNetwork: true,
  })
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Limited MCP user', access: { mcps: null, mcpTools: { [mcp.id]: ['echo'] } } })
  await useRelayForHttpMocks(page)
  // The connection reports two discovered tools.
  await page.route('**/api/mcps', async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue()
      return
    }

    const response = await route.fetch()
    const list = (await response.json()).map((item: { id: string }) => item.id === mcp.id
      ? { ...item, tools: [{ name: 'echo', inputSchema: { type: 'object' } }, { name: 'search', inputSchema: { type: 'object' } }] }
      : item)
    await route.fulfill({ response, json: list })
  })
  await page.goto(workspacePath(`/agents/${agent.id}?section=mcps`))
  await authenticateWorkspace(page)
  const group = page.getByRole('group', { name: 'MCP connections access' })
  await expect(group.getByRole('button', { name: 'All, including future', exact: true })).toHaveAttribute('aria-pressed', 'true')
  const connection = page.getByRole('listitem', { name: 'Design tools' })
  await expect(connection).toContainText('Tools · 2 enabled on the connection · 1 chosen')
  const bar = page.getByRole('region', { name: 'Unsaved changes' })
  await expect(bar).toHaveCount(0)

  await connection.locator('summary').click()
  await expect(connection.getByLabel('echo', { exact: true })).toBeChecked()
  await connection.getByLabel('search', { exact: true }).check()
  await expect(bar).toContainText('1 unsaved change')
  await saveAgent(page)
  const saved = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id)
  expect(saved.access.mcps).toBeNull()
  expect(saved.access.mcpTools).toEqual({ [mcp.id]: ['echo', 'search'] })

  // Choosing starts with no connection selected, as for projects and skills and on Android;
  // switching back keeps the tool choices.
  await group.getByRole('button', { name: 'Choose', exact: true }).click()
  await expect(page.getByLabel('Design tools', { exact: true })).not.toBeChecked()
  await group.getByRole('button', { name: 'All, including future', exact: true }).click()
  await expect(connection).toContainText('2 chosen')
  await expect(bar).toHaveCount(0)
  // Leaving "All enabled tools" starts with no tool chosen.
  await connection.locator('details').evaluate((element) => {
    (element as HTMLDetailsElement).open = true
  })
  await connection.getByLabel('All enabled tools', { exact: true }).check()
  await expect(connection).toContainText('all enabled tools')
  await connection.getByLabel('All enabled tools', { exact: true }).uncheck()
  await expect(connection).toContainText('0 chosen')
  await expect(connection.getByLabel('echo', { exact: true })).not.toBeChecked()
  await expect(connection.getByLabel('search', { exact: true })).not.toBeChecked()
  await connection.getByLabel('echo', { exact: true }).check()
  await connection.getByLabel('search', { exact: true }).check()
  await expect(bar).toHaveCount(0)
  await page.getByRole('link', { name: 'Overview', exact: true }).click()
  await expect(page.getByRole('button', { name: /^MCP & tools/ })).toContainText('Tools limited on Design tools')

  // Choosing no connection saves: tool choices of connections no longer granted are not sent.
  await openAgentSection(page, 'MCP & tools')
  await group.getByRole('button', { name: 'Choose', exact: true }).click()
  const saving = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${agent.id}`))
  await bar.getByRole('button', { name: 'Save', exact: true }).click()
  expect((await saving).postDataJSON().access).toMatchObject({ mcps: [], mcpTools: {} })
  await expect(bar).toHaveCount(0)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).access).toMatchObject({ mcps: [], mcpTools: {} })
})

test('a held plugin grant whose package cannot give agents access is shown, counted and removable', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Ticket triager' })
  // The agent holds a grant on an instance whose package now waits for review.
  await page.route('**/api/agents', async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue()
      return
    }

    const response = await route.fetch()
    const agents = await response.json()
    await route.fulfill({ response, json: agents.map((item: { id: string }) => item.id === agent.id ? { ...item, plugins: ['queue'] } : item) })
  })
  await page.route('**/api/plugins', route => route.fulfill({
    json: {
      packages: [{
        id: 'tickets',
        builtin: false,
        status: 'needs_approval',
        active: null,
        manifest: {
          id: 'tickets',
          name: 'Tickets',
          version: '2.0.0',
          agentAccess: { description: 'Read tickets.' },
        },
        versions: [],
      }],
      capabilities: [],
      connections: [{
        id: 'queue',
        pluginId: 'tickets',
        name: 'Support queue',
        enabled: true,
        settings: {},
        agentIds: [agent.id],
        userIds: [],
        people: [],
        accessMode: 'allowlist',
        agentId: '',
        projectId: null,
        maxMessagesPerHour: 0,
        status: { state: 'ready' },
        usage: { hour: 0 },
        requests: [],
        activity: [],
      }],
    },
  }))
  await page.goto(workspacePath(`/agents/${agent.id}`))
  await authenticateWorkspace(page)
  const checks = page.getByRole('status').filter({ hasText: 'to check' })
  await expect(checks).toContainText('1 thing to check')
  await expect(checks).toContainText('Tickets “Support queue” is unavailable: its plugin needs review or no longer gives agents access.')

  await openAgentSection(page, 'Secrets & plugins')
  const unavailable = page.getByRole('region', { name: 'Unavailable plugin access' })
  await expect(unavailable).toContainText('Tickets · Support queue')
  await expect(unavailable.getByText('Unavailable', { exact: true })).toBeVisible()
  await expect(unavailable).toContainText('Its plugin needs review or no longer gives agents access.')
  await expect(page.getByRole('switch', { name: 'Access to Tickets Support queue' })).toHaveCount(0)
  await unavailable.getByRole('button', { name: 'Remove access to Tickets Support queue' }).click()
  await expect(unavailable).toHaveCount(0)

  const saving = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${agent.id}`))
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const body = (await saving).postDataJSON()
  expect(body.plugins).toEqual([])
  expect(body.access).not.toHaveProperty('plugins')
})

test('restricting projects moves the agent from the shared GitHub connection to its saved token', async ({ page, workspace }) => {
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Token keeper', access: { projects: null, github: true } })
  await workspace.api(`/api/agents/${agent.id}/github-token`, 'PUT', { token: 'fixture-token-not-a-secret' })
  await useRelayForHttpMocks(page)
  const tokenSaves: string[] = []
  page.on('request', (request) => {
    if (request.method() === 'PUT' && request.url().endsWith(`/api/agents/${agent.id}/github-token`))
      tokenSaves.push(request.postData() ?? '')
  })
  await page.goto(workspacePath(`/agents/${agent.id}?section=projects`))
  await authenticateWorkspace(page)
  await page.getByRole('group', { name: 'Projects access' }).getByRole('button', { name: 'Choose', exact: true }).click()
  await openAgentSection(page, 'GitHub')
  await expect(page.getByRole('radio', { name: /^Dedicated token/ })).toBeChecked()
  const note = page.getByRole('status').filter({ hasText: 'The shared connection was turned off: this agent will use its saved dedicated token.' })
  await expect(note).toBeVisible()
  // Moving to the saved token changes the agent's credentials: the review says so first.
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('Shared GitHub connection turned off: the agent will use its saved dedicated token.')
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(review).toHaveCount(0)
  await expect(page.getByRole('region', { name: 'Unsaved changes' })).toHaveCount(0)
  expect(tokenSaves).toEqual([])
  expect(await workspace.api(`/api/agents/${agent.id}/github-token`)).toEqual({ configured: true })
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).access).toMatchObject({ projects: [], github: false })
  await expect(page.getByRole('radio', { name: /^Dedicated token/ })).toBeChecked()

  // Only the owner's explicit choice removes it.
  await page.getByRole('button', { name: 'Remove saved token', exact: true }).click()
  await expect(page.getByRole('radio', { name: /^No GitHub/ })).toBeChecked()
  await expect(page.getByText('The saved token is removed when you save.')).toBeVisible()
  await saveAgent(page)
  expect(tokenSaves).toEqual([JSON.stringify({ token: '' })])
  expect(await workspace.api(`/api/agents/${agent.id}/github-token`)).toEqual({ configured: false })
})

test('when plugins cannot be loaded, plugin access is read-only and no grant looks deleted', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Catalog reader' })
  await page.route('**/api/agents', async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue()
      return
    }

    const response = await route.fetch()
    const agents = await response.json()
    await route.fulfill({ response, json: agents.map((item: { id: string }) => item.id === agent.id ? { ...item, plugins: ['queue'] } : item) })
  })
  let catalogFails = true
  await page.route('**/api/plugins', route => catalogFails
    ? route.fulfill({ status: 500, json: { error: 'Plugin host unavailable' } })
    : route.fulfill({ json: { packages: [], capabilities: [], connections: [] } }))
  await page.goto(workspacePath(`/agents/${agent.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByRole('button', { name: /^Secrets & plugins/ })).toContainText('Not loaded')
  await expect(page.getByText('to check')).toHaveCount(0)

  await openAgentSection(page, 'Secrets & plugins')
  const failure = page.getByRole('alert').filter({ hasText: 'Plugins could not be loaded' })
  await expect(failure).toContainText('Plugin host unavailable')
  await expect(page.getByRole('button', { name: 'Remove deleted plugin instances' })).toHaveCount(0)
  // The held grant stays listed, read-only.
  await expect(failure.getByRole('list', { name: 'Held plugin access' })).toHaveText('Granted instance queue…')
  await expect(failure.getByRole('button', { name: /Remove/ })).toHaveCount(0)
  // Once loaded, the grant is checked again: here its instance is really gone.
  catalogFails = false
  await failure.getByRole('button', { name: 'Try again', exact: true }).click()
  await expect(failure).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Remove deleted plugin instances' })).toBeVisible()
})

test('when nodes cannot be loaded, the page stays usable and node access is read-only', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Node reader', access: { nodes: null } })
  const gpu = await enroll(page, workspace, 'held-gpu', true)
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { name: 'Node reader', exclusiveNodes: [gpu] })
  let nodesFail = true
  await page.route('**/api/nodes', route => nodesFail
    ? route.fulfill({ status: 500, json: { error: 'Node registry unavailable' } })
    : route.continue())
  await page.goto(workspacePath(`/agents/${agent.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByRole('button', { name: /^Execution Not loaded/ })).toBeVisible()
  await expect(page.getByText('to check')).toHaveCount(0)

  // Other sections still save.
  await openAgentSection(page, 'Identity & instructions')
  await page.getByLabel('Description', { exact: true }).fill('Reads the node registry')
  await saveAgent(page)
  const saved = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id)
  expect(saved.description).toBe('Reads the node registry')
  // Nothing was restricted from an empty list.
  expect(saved.access.nodes).toBeNull()

  await openAgentSection(page, 'Nodes')
  const failure = page.getByRole('alert').filter({ hasText: 'Nodes could not be loaded, so this agent’s node access cannot be shown or changed.' })
  await expect(failure).toContainText('Node registry unavailable')
  await expect(page.getByRole('switch', { name: 'All nodes, including future' })).toHaveCount(0)
  // Held access stays listed, read-only.
  const held = failure.getByRole('list', { name: 'Held node access' })
  await expect(held).toContainText('Can run on all nodes, including future')
  await expect(held).toContainText(`Can reserve node ${gpu.slice(0, 8)}… exclusively`)
  nodesFail = false
  await failure.getByRole('button', { name: 'Try again', exact: true }).click()
  await expect(failure).toHaveCount(0)
  await expect(page.getByRole('switch', { name: 'All nodes, including future' })).toBeChecked()
})

test('the execution backend chooses which nodes an agent can be granted', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const vm = await enroll(page, workspace, 'vm-box', true)
  const invitation = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'native-box', backend: 'native' })
  const enrolled = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code: invitation.code,
      name: 'native-box',
      protocol: 1,
      runtimeId: 'fixture',
      capabilities: {
        backend: 'native',
        os: 'linux',
        arch: 'x86_64',
        cpu: 8,
        memoryMiB: 16384,
        diskMiB: 65536,
        kvm: false,
        fuse: false,
      },
    },
  })
  expect(enrolled.ok()).toBe(true)
  const native = (await enrolled.json()).nodeId as string
  const agent = await workspace.api('/api/agents', 'POST', { name: 'GPU renderer', access: { nodes: [vm] } })
  await page.goto(workspacePath(`/agents/${agent.id}?section=nodes`))
  await authenticateWorkspace(page)

  // Firecracker VM agents choose among VM nodes, the current runner included, or all of them.
  const nodes = page.getByRole('list', { name: 'Nodes' })
  await expect(page.getByRole('switch', { name: 'All nodes, including future' })).toBeVisible()
  await expect(nodes.getByRole('listitem', { name: 'vm-box' })).toBeVisible()
  // Unreported by this installation, the current runner is still offered, without a state.
  const runner = nodes.getByRole('listitem', { name: 'Current runner' })
  await expect(runner).toBeVisible()
  await expect(runner).not.toContainText('Online')
  await expect(page.getByRole('switch', { name: 'Can run its conversations on Current runner' })).not.toBeChecked()
  await expect(nodes.getByRole('listitem', { name: 'native-box' })).toHaveCount(0)

  // Native agents choose native nodes one by one; switching starts the selection over.
  await page.getByRole('combobox', { name: 'Agent execution backend' }).click()
  await page.getByRole('option', { name: 'Native process · trusted people only', exact: true }).click()
  await expect(page.getByText('Uses the host GPU and OS tools with weaker isolation.')).toBeVisible()
  await expect(page.getByRole('switch', { name: 'All nodes, including future' })).toHaveCount(0)
  await expect(nodes.getByRole('listitem', { name: 'vm-box' })).toHaveCount(0)
  await expect(nodes.getByRole('listitem', { name: 'Current runner' })).toHaveCount(0)
  const run = page.getByRole('switch', { name: 'Can run its conversations on native-box' })
  await expect(run).not.toBeChecked()
  await run.check()

  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('Added: native execution. Uses the host GPU and OS tools with weaker isolation.')
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(review).toHaveCount(0)
  const saved = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id)
  expect(saved.access).toMatchObject({ backend: 'native', nodes: [native] })
  await page.getByRole('link', { name: 'Overview', exact: true }).click()
  await expect(page.getByRole('button', { name: /^Execution Native process/ })).toContainText('Native process')
})
