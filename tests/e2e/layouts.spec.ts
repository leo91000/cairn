import type { Locator, Page, TestInfo } from '@playwright/test'
import type { RunEvent } from '../../shared/contracts'
import type { Workspace } from './fixtures'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
  workspacePath,
} from './fixtures'

async function checkMobileLayouts(page: Page, workspace: Workspace, testInfo: TestInfo, colorScheme: 'light' | 'dark' = 'light') {
  await page.emulateMedia({ colorScheme })
  test.setTimeout(180000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  let virtualAgents: object[] | undefined
  // Changing WebKit's interception while a background poll is in flight can cancel it.
  // Install the route before navigation and only switch the fixture it returns.
  await page.route('**/api/agents', route => virtualAgents ? route.fulfill({ json: virtualAgents }) : route.continue())
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible()
  const runs = await page.request.get(`/api/installations/${workspace.installationId}/api/runs`).then(response => response.json())
  const run = runs.find((item: { status: string }) => item.status === 'succeeded')

  // The rail (desktop) or the dock (phone) leads to the Fil, Missions and the Atelier.
  async function navigate(url: string) {
    const destination = url.startsWith('/runs/') ? '/runs' : url
    const place = destination === '/' ? 'Fil' : destination === '/tasks' ? 'Missions' : 'Atelier'
    const navigation = page.getByRole('navigation', { name: /^(Workspace|Quick) navigation$/ }).filter({ visible: true })
    if (await navigation.count())
      await navigation.getByRole('link', { name: place, exact: true }).click()
    else
      await page.goto(workspacePath(place === 'Atelier' ? '/atelier' : destination))
    const sections: Record<string, string> = {
      '/runs': 'Runs',
      '/agents': 'Agents',
      '/projects': 'Projects',
      '/skills': 'Skills',
      '/connections': 'Connections',
      '/settings': 'Settings',
      '/mcps': 'MCPs',
    }
    if (sections[destination]) {
      await expect(page.getByRole('heading', { name: 'Atelier', exact: true }).or(page.getByRole('navigation', { name: 'Atelier sections' }))).toBeVisible()
      await page.getByRole('link', { name: sections[destination], exact: true }).first().click()
    }

    const headings: Record<string, string> = {
      '/': 'Fil',
      '/tasks': 'Missions',
      '/runs': 'Run history',
      '/agents': 'Agents',
      '/projects': 'Projects',
      '/skills': 'Skills library',
      '/connections': 'Connections',
      '/settings': 'Settings',
      '/mcps': 'MCPs',
    }
    if (headings[destination])
      await expect(page.getByRole('heading', { name: headings[destination], exact: true }).first()).toBeVisible()
    if (url.startsWith('/runs/')) {
      await page.locator(`.run-table a[href="${workspacePath(url)}"]`).first().click()
      await expect(page.locator('.run-title-meta')).toBeVisible()
    }
  }

  async function fits() {
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    await expectSingleScroll(page)
    const missingIcons = await page.locator('.ui-icon').evaluateAll(elements => elements.filter((element) => {
      const style = getComputedStyle(element)
      const rect = element.getBoundingClientRect()
      return rect.width > 0 && rect.height > 0 && style.maskImage === 'none' && style.backgroundImage === 'none'
    }).map(element => element.className))
    expect(missingIcons).toEqual([])
    for (const dialog of await page.locator('dialog[open]').all()) {
      const bounds = await dialog.boundingBox()
      expect(Math.abs(bounds!.x + bounds!.width / 2 - page.viewportSize()!.width / 2)).toBeLessThan(2)
    }
  }

  async function screenshot(name: string) {
    await page.evaluate(() => document.fonts.ready)
    await fits()
    if (colorScheme === 'dark') {
      const brightSurfaces = await page.locator('.run-facts, .settings-section, .resource-card, .mission-card, .skill-card, .connection-card, dialog[open], .vs-popup:popover-open').evaluateAll(elements => elements.filter((el) => {
        const color = getComputedStyle(el).backgroundColor.match(/[\d.]+/g)?.map(Number)
        return color && color.length >= 3 && (color[3] ?? 1) > 0.5 && Math.min(...color.slice(0, 3)) > 180
      }).map(el => el.className))
      expect(brightSurfaces).toEqual([])
    }

    const overlay = await page.locator('dialog[open], .sidebar.open').count()
    await page.screenshot({ path: testInfo.outputPath(`${name}.png`), fullPage: !overlay, animations: 'disabled' })
  }

  const screens = [
    ['overview', '/', '.fil'],
    ['tasks', '/tasks', '.mission-card'],
    ['runs', '/runs', 'tbody tr'],
    ['agents', '/agents', '.resource-card'],
    ['projects', '/projects', '.resource-card'],
    ['skills', '/skills', '.skill-card'],
    ['connections', '/connections', '.connection-card'],
    ['settings', '/settings', '.settings-section'],
    ['mcps', '/mcps', '.page-heading'],
    ['result', `/runs/${run.id}`, '.run-panel'],
  ]
  for (const viewport of [...(colorScheme === 'dark' ? [{ width: 1440, height: 1000 }] : []), { width: 320, height: 568 }, { width: 390, height: 664 }, { width: 430, height: 932 }, { width: 844, height: 390 }]) {
    await page.setViewportSize(viewport)
    for (const [name, url, ready] of screens) {
      await navigate(url)
      await expect(page.locator(ready).first()).toBeVisible()
      await screenshot(`${viewport.width}-${name}`)
      if (name === 'tasks')
        await page.getByRole('button', { name: 'Search missions', exact: true }).click()
      if (name === 'tasks' || name === 'skills') {
        const geometry = await page.locator('.search-field').evaluate((field) => {
          const icon = field.querySelector('.ui-icon')!.getBoundingClientRect()
          const input = field.querySelector('input')!.getBoundingClientRect()
          return { aligned: Math.abs(icon.y + icon.height / 2 - input.y - input.height / 2) < 2, separated: icon.right <= input.left }
        })
        expect(geometry).toEqual({ aligned: true, separated: true })
      }
    }

    await page.getByRole('button', { name: /^Conversation/ }).click()
    await expect(page.getByTestId('agent-actions').first()).toBeAttached()
    expect((await page.locator('.activity-scroll').boundingBox())!.height).toBeGreaterThan(65)
    const head = await page.locator('.run-panel-head').boundingBox()
    const actions = await page.locator('.activity-toolbar').boundingBox()
    expect(actions!.y).toBeGreaterThanOrEqual(head!.y + head!.height)
    await page.getByLabel('Follow output').uncheck()
    await expect(page.getByLabel('Follow output')).not.toBeChecked()
    await screenshot(`${viewport.width}-activity`)
    // The agent's actions collapse into a sentence, expand into a timeline and open one step in a sheet.
    const sheet = page.getByTestId('agent-step-sheet')

    async function openStep(scope: Locator, text: string) {
      // The history arrives after navigation: wait for it before expanding every summary.
      await expect(scope.getByTestId('agent-actions').first()).toBeVisible()
      const collapsed = scope.getByTestId('agent-actions').locator(':scope > button[aria-expanded="false"]')
      while (await collapsed.count())
        await collapsed.first().click()
      await scope.getByTestId('agent-step').filter({ hasText: text }).first().click()
      await expect(sheet).toBeVisible()
    }

    await openStep(page.locator('body'), 'File changes')
    await expect(sheet.locator('.hljs-addition').first()).toBeVisible()
    await page.keyboard.press('Escape')
    await openStep(page.locator('body'), 'Run command')
    await expect(sheet.getByText('TypeScript: no errors found.', { exact: false })).toBeVisible()
    await screenshot(`${viewport.width}-activity-details`)
    await page.keyboard.press('Escape')
    await expect(sheet).toHaveCount(0)
    await page.getByRole('button', { name: 'Open activity fullscreen' }).click()
    const viewer = page.getByRole('dialog', { name: 'Fullscreen activity' })
    await expect(viewer).toBeVisible()
    await expect(page.getByRole('button', { name: 'Exit fullscreen' })).toBeFocused()
    const fullBox = await viewer.boundingBox()
    expect(fullBox!.height).toBe(viewport.height)
    expect(fullBox!.width).toBe(viewport.width)
    await screenshot(`${viewport.width}-activity-fullscreen`)
    await openStep(viewer, 'functions.ts')
    const read = sheet.getByRole('region', { name: 'Read functions.ts' })
    await expect(read.locator('.hljs-keyword').first()).toBeVisible()
    await read.getByRole('button', { name: 'Show command' }).click()
    await expect(read.getByRole('button', { name: 'Copy Command' })).toBeVisible()
    await page.keyboard.press('Escape')
    await openStep(viewer, 'SKILL.md')
    const skillRead = sheet.getByRole('region', { name: 'Read SKILL.md' })
    await expect(skillRead.getByRole('heading', { name: 'CSS Baseline update and release' })).toBeVisible()
    await screenshot(`${viewport.width}-file-read-preview`)
    await skillRead.getByRole('button', { name: 'View source' }).click()
    await expect(skillRead.getByRole('button', { name: 'Copy File content' })).toBeVisible()
    await page.keyboard.press('Escape')
    await openStep(viewer, 'Exit 1')
    await expect(sheet).toContainText('Failed · Exit 1')
    await screenshot(`${viewport.width}-operation-cards`)
    await page.keyboard.press('Escape')
    await expect(sheet).toHaveCount(0)
    const checks = viewer.getByRole('region', { name: 'Workflow checks', exact: true })
    await checks.scrollIntoViewIfNeeded()
    await expect(checks.getByText('11 passed', { exact: true })).toBeVisible()
    await checks.getByRole('button', { name: /Show more/ }).click()
    await expect(checks.getByText('Skipped', { exact: true })).toBeVisible()
    await checks.locator('.data-content').evaluate(el => el.scrollTo(0, 0))
    await screenshot(`${viewport.width}-structured-checks`)
    await checks.locator('.data-source > summary').click()
    await expect(checks.locator('.hljs-attr').first()).toBeVisible()
    await expect(checks.getByRole('button', { name: 'Copy JSON' })).toBeVisible()
    await checks.locator('.data-source > summary').click()
    const pullRequest = viewer.getByRole('region', { name: 'Pull request details', exact: true })
    await pullRequest.scrollIntoViewIfNeeded()
    await expect(pullRequest.getByText('.changeset/september-baseline-authoring.md', { exact: true })).toBeVisible()
    await pullRequest.getByRole('button', { name: /Show more/ }).click()
    await expect(pullRequest.getByText('scripts/generate-css-feature-target.ts', { exact: true })).toBeVisible()
    await pullRequest.locator('.data-content').evaluate(el => el.scrollTo(0, 0))
    await screenshot(`${viewport.width}-structured-files`)
    await page.keyboard.press('Escape')
    await expect(viewer).not.toBeVisible()
    await expect(page.getByRole('button', { name: 'Open activity fullscreen' })).toBeFocused()
    await expect(page.getByTestId('agent-step').first()).toBeVisible()
    await page.getByRole('button', { name: 'Mission brief', exact: true }).click()
    await screenshot(`${viewport.width}-brief`)

    for (const [name, url, button] of [
      ['task-editor', '/tasks', 'New mission'],
      ['agent-editor', '/agents', 'New agent'],
      ['project-editor', '/projects', 'Add project'],
      ['skill-editor', '/skills', 'New skill'],
      ['skill-files', '/skills', 'Edit review'],
      ['token-editor', '/settings', 'New token'],
    ]) {
      await navigate(url)
      await page.getByRole('button', { name: button, exact: true }).click()
      const dialog = page.getByRole('dialog')
      await expect(dialog).toBeVisible()
      const box = await dialog.boundingBox()
      expect(box!.y).toBeGreaterThanOrEqual(0)
      expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height)
      await screenshot(`${viewport.width}-${name}`)
      for (const select of await dialog.getByRole('combobox').all()) {
        if (await select.isDisabled())
          continue
        expect((await select.boundingBox())!.width).toBeGreaterThan(80)
        await select.click()
        const list = page.getByRole('listbox')
        await expect(list).toBeVisible()
        const popup = page.locator('.vs-popup:popover-open')
        const bounds = await popup.boundingBox()
        expect(bounds!.x).toBeGreaterThanOrEqual(0)
        expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(viewport.width)
        expect(bounds!.y).toBeGreaterThanOrEqual(0)
        expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(viewport.height)
        await screenshot(`${viewport.width}-${name}-${await select.getAttribute('aria-label')}-select`)
        await page.keyboard.press('Escape')
        await expect(list).not.toBeVisible()
        await expect(dialog).toBeVisible()
      }

      await dialog.getByRole('button').last().scrollIntoViewIfNeeded()
      await expect(dialog.getByRole('button').last()).toBeInViewport()
      await page.keyboard.press('Escape')
      await expect(dialog).toHaveCount(0)
    }

    await page.keyboard.press('Control+k')
    await page.getByRole('dialog').getByLabel('Search', { exact: true }).fill('review')
    await screenshot(`${viewport.width}-workspace-search`)
    // The first Escape clears the query, the second closes the palette.
    await page.keyboard.press('Escape')
    await page.keyboard.press('Escape')
    await expect(page.getByRole('dialog')).toHaveCount(0)
  }

  await page.setViewportSize({ width: 390, height: 664 })
  // The floating dock stays reachable at every phone height and hides while reading.
  const dock = page.getByRole('navigation', { name: 'Quick navigation' })
  await navigate('/')
  for (const height of [360, 568, 844]) {
    await page.setViewportSize({ width: 390, height })
    await expect(dock).toBeInViewport()
    await expect(dock.getByRole('link', { name: 'New conversation' })).toBeInViewport()
    await screenshot(`dock-${height}`)
  }

  await dock.getByRole('link', { name: 'Atelier', exact: true }).click()
  await page.getByRole('button', { name: 'Sign out' }).scrollIntoViewIfNeeded()
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeInViewport()
  await dock.getByRole('link', { name: 'Missions', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(dock.getByRole('link', { name: 'Missions', exact: true })).toHaveAttribute('aria-current', 'page')
  await expect(page.locator('.mission-card').first()).toBeVisible()
  virtualAgents = Array.from({ length: 10000 }, (_, index) => ({
    id: `virtual-${index}`,
    name: index === 4999 ? 'Équipe sécurité' : `Agent ${index.toString().padStart(5, '0')}`,
    description: `Maintains project ${index}`,
    model: '',
    reasoning: 'high',
    access: {
      projects: null,
      skills: null,
      github: true,
      sandbox: 'yolo',
    },
  }))
  await page.goto(workspacePath('/tasks'))
  await page.getByRole('button', { name: 'New mission', exact: true }).click()
  const agentSelect = page.getByRole('combobox', { name: 'Agent', exact: true })
  await agentSelect.click()
  await expect(page.getByRole('option').first()).toBeVisible()
  expect(await page.getByRole('option').count()).toBeLessThan(20)
  await agentSelect.press('End')
  await expect(page.getByRole('option', { name: 'Agent 09999', exact: true })).toBeVisible()
  expect(await page.getByRole('option').count()).toBeLessThan(20)
  await screenshot('select-10000-options')
  await agentSelect.press('Enter')
  await expect(agentSelect).toHaveValue('Agent 09999')
  await expect(page.getByRole('dialog')).toBeVisible()
  await agentSelect.click()
  await agentSelect.fill('equipe')
  await expect(page.getByRole('option')).toHaveCount(1)
  await agentSelect.press('Enter')
  await expect(agentSelect).toHaveValue('Équipe sécurité')
  await agentSelect.click()
  await agentSelect.fill('does-not-exist')
  await expect(page.getByText('No matches found', { exact: true })).toBeVisible()
  await screenshot('select-no-results')
  await agentSelect.press('Escape')
  await expect(agentSelect).toHaveValue('Équipe sécurité')
  await agentSelect.click()
  await agentSelect.press('Tab')
  await expect(page.getByRole('listbox')).not.toBeVisible()
  await expect(page.getByRole('textbox', { name: /^Tags/ })).toBeFocused()
  await page.keyboard.press('Escape')
  virtualAgents = undefined

  function history(events: RunEvent[]) {
    workspace.service.store.db.prepare('DELETE FROM events WHERE run_id=?').run(run.id)
    for (const event of events)
      workspace.service.store.event(run.id, event.type, event.text, event.payload)
  }

  history([
    {
      id: 1,
      runId: run.id,
      createdAt: 1000,
      type: 'item.started',
      text: '',
    },
    {
      id: 2,
      runId: run.id,
      createdAt: 1800,
      type: 'item.completed',
      text: 'Already up to date.\ncompatibility/css-feature-target.json',
    },
    {
      id: 3,
      runId: run.id,
      createdAt: 2000,
      type: 'item.started',
      text: '',
    },
    {
      id: 4,
      runId: run.id,
      createdAt: 2500,
      type: 'item.completed',
      text: '[{"id":123,"jobs":[{"name":"quality","conclusion":"success"}]}]',
    },
    {
      id: 5,
      runId: run.id,
      createdAt: 3000,
      type: 'item.started',
      text: '',
    },
    {
      id: 6,
      runId: run.id,
      createdAt: 3500,
      type: 'item.completed',
      text: 'implementation-pr {"files":["src/activity.ts"',
    },
  ])
  await page.goto(workspacePath(`/runs/${run.id}`))
  const sheet = page.getByTestId('agent-step-sheet')

  async function openStep(text: string) {
    await expect(page.getByTestId('agent-actions').first()).toBeVisible()
    const collapsed = page.getByTestId('agent-actions').locator(':scope > button[aria-expanded="false"]')
    while (await collapsed.count())
      await collapsed.first().click()
    await page.getByTestId('agent-step').filter({ hasText: text }).first().click()
    await expect(sheet).toBeVisible()
  }

  await openStep('Recorded output')
  await expect(sheet).toContainText('Recorded · step 1 of 3')
  await expect(sheet.getByText(/Its command and exit code weren’t recorded/)).toBeVisible()
  await screenshot('historical-operation')
  await page.keyboard.press('Escape')
  await expect(page.getByTestId('agent-step').nth(1)).toContainText('Workflow checks')
  await expect(page.getByTestId('agent-step').nth(1)).not.toContainText('"jobs"')
  await openStep('Workflow checks')
  await expect(sheet.getByRole('region', { name: 'Workflow checks' }).last()).toBeVisible()
  await expect(sheet.locator('pre')).toHaveCount(0)
  await page.keyboard.press('Escape')
  await expect(page.getByTestId('agent-step').nth(2)).toContainText('Incomplete result')
  await expect(page.getByTestId('agent-step').nth(2)).not.toContainText('"files"')
  await openStep('Incomplete result')
  const incomplete = sheet.getByRole('region', { name: 'Incomplete result' }).last()
  await expect(incomplete).toBeVisible()
  await expect(incomplete.locator('pre')).toHaveCount(0)
  await incomplete.scrollIntoViewIfNeeded()
  await screenshot('historical-json-results')
  await incomplete.locator('summary').click()
  await expect(incomplete.getByRole('button', { name: 'Copy Saved source' })).toBeVisible()
  await expect(incomplete.locator('pre')).toContainText('{"files":["src/activity.ts"')
  await page.keyboard.press('Escape')
  history([
    {
      id: 1,
      runId: run.id,
      createdAt: 1000,
      type: 'error',
      text: '',
      payload: { message: 'WebSocket connection failed: 503 Service Unavailable' },
    },
    {
      id: 2,
      runId: run.id,
      createdAt: 2000,
      type: 'item.completed',
      text: '',
      payload: {
        item: {
          type: 'command_execution',
          command: 'rg needle src',
          exit_code: 1,
          status: 'failed',
        },
      },
    },
    {
      id: 3,
      runId: run.id,
      createdAt: 3000,
      type: 'item.completed',
      text: '',
      payload: {
        item: {
          type: 'command_execution',
          command: 'diff before after',
          exit_code: 1,
          status: 'failed',
        },
      },
    },
    {
      id: 4,
      runId: run.id,
      createdAt: 4000,
      type: 'item.completed',
      text: '',
      payload: {
        item: {
          type: 'command_execution',
          command: 'pnpm test',
          exit_code: 1,
          status: 'failed',
        },
      },
    },
    {
      id: 5,
      runId: run.id,
      createdAt: 5000,
      type: 'turn.completed',
      text: '',
      payload: {},
    },
  ])
  await page.goto(workspacePath(`/runs/${run.id}`))
  // Expected outcomes and a recovered connection are not failures: only the test run counts.
  await expect(page.getByTestId('agent-actions')).toContainText('1 failure')
  for (const [step, label] of [['Connection restored', 'Recovered'], ['rg needle src', 'No matches'], ['diff before after', 'Differences found']]) {
    await openStep(step)
    await expect(sheet).toContainText(`${label} · step`)
    await expect(sheet.locator('.operation-card')).toHaveAttribute('data-status', 'info')
    await page.keyboard.press('Escape')
  }

  await expect(page.getByTestId('agent-step').filter({ hasText: 'Exit 1' })).toHaveCount(1)
  await screenshot('command-outcomes')
  expect(errors).toEqual([])
}

test('pages, dialogs, navigation and activity', async ({ page, workspace }, testInfo) => {
  await checkMobileLayouts(page, workspace, testInfo, testInfo.project.use.colorScheme === 'dark' ? 'dark' : 'light')
})
