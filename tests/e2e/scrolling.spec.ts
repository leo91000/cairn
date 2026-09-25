import { expect, expectSingleScroll, test } from './fixtures'

test('keeps activity scrolling inside the workspace and gives tabs breathing room', async ({ page, workspace }, testInfo) => {
  // This journey covers multiple viewports and screenshots; individual assertions retain their deadlines.
  test.setTimeout(90000)
  await page.setViewportSize({ width: 1440, height: 900 })
  // Seed durable history so the real SSE endpoint supplies the layout fixture.
  for (const run of workspace.service.store.runs()) {
    for (let index = 0; index < 100; index++) {
      const text = `Review step ${index + 1}: the checks passed and the next change is ready to inspect.`
      workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `layout-${index}`, type: 'agent_message', text } })
    }
  }

  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Missions', exact: true })).toBeVisible()
  const runs = await page.request.get('/api/runs').then(response => response.json())
  for (const theme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme: theme })
    for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }, { width: 390, height: 664 }, { width: 320, height: 568 }, { width: 844, height: 390 }]) {
      await page.setViewportSize(viewport)
      // Missions: the list scrolls on its own and the mission's actions stay reachable.
      await page.goto('/tasks')
      await expect(page.getByTestId('missions')).toBeVisible()
      await expectSingleScroll(page)
      if (viewport.width <= 900)
        await page.locator('.mission-card button').first().click()
      const detail = page.getByTestId('mission-detail')
      await detail.getByRole('button', { name: 'Mission actions', exact: true }).click()
      await expect(detail.getByRole('menuitem').last()).toBeInViewport({ ratio: 1 })
      await detail.getByRole('button', { name: 'Mission actions', exact: true }).click()
      await detail.getByRole('button', { name: 'Run now', exact: true }).scrollIntoViewIfNeeded()
      await expect(detail.getByRole('button', { name: 'Run now', exact: true })).toBeInViewport()
      await page.screenshot({ path: testInfo.outputPath(`${theme}-${viewport.width}-tasks.png`), animations: 'disabled' })
      // A run: its conversation owns the scrolling below the title and tabs.
      await page.goto(`/runs/${runs[0].id}`)
      await page.getByRole('button', { name: /^Conversation/ }).click()
      await expect(page.locator('.activity-message').first()).toBeAttached()
      await page.getByLabel('Follow output').uncheck()
      await expectSingleScroll(page)
      const scroller = page.getByRole('region', { name: 'Activity output' })
      const box = await scroller.boundingBox()
      expect(box!.height).toBeGreaterThan(65)
      const geometry = await scroller.evaluate((element) => {
        const chain = []
        for (let parent: Element | null = element; parent; parent = parent.parentElement) {
          const style = getComputedStyle(parent)
          chain.push({
            name: parent.className,
            height: parent.clientHeight,
            min: style.minHeight,
            flex: style.flex,
            display: style.display,
          })
        }

        return chain
      })
      expect(box!.y + box!.height, JSON.stringify({ geometry })).toBeLessThanOrEqual(viewport.height)
      await scroller.evaluate(element => element.scrollTop = 0)
      await scroller.focus()
      await page.keyboard.press('PageDown')
      await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBeGreaterThan(0)
      await expectSingleScroll(page)
      const tab = page.getByRole('button', { name: /^Conversation/ })
      const spacing = await tab.evaluate(element => ({ left: Number.parseFloat(getComputedStyle(element).paddingLeft), right: Number.parseFloat(getComputedStyle(element).paddingRight) }))
      expect(spacing.left).toBeGreaterThanOrEqual(10)
      expect(spacing.right).toBeGreaterThanOrEqual(10)
      await scroller.evaluate((element) => {
        element.scrollTop = 0
        if (element instanceof HTMLElement)
          element.blur()
      })
      await page.screenshot({ path: testInfo.outputPath(`${theme}-${viewport.width}-run.png`), animations: 'disabled' })
      await page.getByRole('button', { name: 'Mission brief', exact: true }).click()
      await expectSingleScroll(page)
      await page.getByRole('button', { name: 'Result', exact: true }).click()
      await expectSingleScroll(page)
    }
  }
})

test('a tab click survives completion of the cached run refresh', async ({ page, workspace }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.shell')).toBeVisible()
  const runs = await workspace.api('/api/runs')
  const run = runs.find((value: { status: string }) => value.status === 'succeeded')
  await page.goto(`/runs/${run.id}`)
  await expect(page.locator('.activity-message').first()).toBeVisible()
  await page.getByRole('link', { name: 'Back to runs' }).click()
  let release!: () => void
  const refresh = new Promise<void>(resolve => release = resolve)
  await page.route(`**/api/runs/${run.id}/stream?*`, async (route) => {
    await refresh
    await route.continue()
  })
  try {
    await page.locator(`.run-table a[href="/runs/${run.id}"]`).first().click()
    const updating = page.getByRole('status').filter({ hasText: 'Updating…' })
    await expect(updating).toBeVisible()
    const tab = page.getByRole('button', { name: 'Result', exact: true })
    const box = await tab.boundingBox()
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2)
    await page.mouse.down()
    release()
    await expect(updating).not.toBeVisible()
    await page.mouse.up()
    await expect(tab).toHaveAttribute('aria-pressed', 'true')
    await expect(page.locator('.result-content')).toBeVisible()
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'ignoreErrors' })
  }
})
