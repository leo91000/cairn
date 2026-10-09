import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import { limits } from '../fixtures/codex-account-fixture'
import { codexSignInMode, seedCodexAccount } from './accounts'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

async function signIn(page: import('@playwright/test').Page) {
  await page.goto(workspacePath('/connections'))
  await authenticateWorkspace(page)
}

test('both coding agents share one account list, detail panel and sign-in', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const work = await seedCodexAccount(workspace, 'Work', { ...limits(4, 18), rateLimitResetCredits: { availableCount: 3 } })
  const personal = await seedCodexAccount(workspace, 'Personal', limits(28, 37))
  await seedCodexAccount(workspace, 'Almost empty', limits(97, 32))
  await seedCodexAccount(workspace, 'Travel', limits(20, 50), { enabled: false, maxConcurrentRuns: 2 })
  await workspace.api('/api/accounts/refresh', 'POST')
  await signIn(page)

  // Claude Code signs in with the same window, pasting the code shown by Anthropic.
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  let dialog = page.getByRole('dialog', { name: 'Add an account' })
  await dialog.getByRole('radio', { name: /Claude Code/ }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('Studio')
  await dialog.getByRole('button', { name: 'Continue to sign-in' }).click()
  dialog = page.getByRole('dialog', { name: 'Connect your Claude account' })
  await expect(dialog.getByRole('link', { name: 'Open Claude sign-in' })).toHaveAttribute('href', 'https://claude.com/oauth/authorize?fixture=1')
  for (const [theme, width] of [['light', 1440], ['dark', 390], ['dark', 320]] as const) {
    await page.emulateMedia({ colorScheme: theme })
    await page.setViewportSize({ width, height: 900 })
    await page.screenshot({ path: testInfo.outputPath(`claude-sign-in-${theme}-${width}.png`), animations: 'disabled' })
  }

  await page.setViewportSize({ width: 1440, height: 1000 })
  await dialog.getByLabel('Claude authorization code').fill('wrong')
  await dialog.getByRole('button', { name: 'Finish sign-in' }).click()
  dialog = page.getByRole('dialog', { name: 'Let’s try that again' })
  await expect(dialog.getByRole('alert')).toContainText('could not finish')
  await expect(dialog).not.toContainText('never-return-this-secret')
  await dialog.getByRole('button', { name: 'Try again' }).click()
  dialog = page.getByRole('dialog', { name: 'Connect your Claude account' })
  await dialog.getByLabel('Claude authorization code').fill('fixture-code')
  await dialog.getByRole('button', { name: 'Finish sign-in' }).click()
  await expect(dialog).toHaveCount(0, { timeout: 15000 })
  const claude = page.getByRole('region', { name: 'Claude Code accounts' })
  await expect(claude.getByRole('button', { name: /^Studio,/ })).toContainText('Next up')
  await expect(claude.getByRole('progressbar', { name: 'Studio Weekly remaining' })).toHaveAttribute('aria-valuenow', '40')

  const codex = page.getByRole('region', { name: 'Codex accounts' })
  await expect(codex.getByRole('button', { name: /^Work,/ })).toContainText('Next up')
  // A run holds a slot on the account with the most capacity, whose avatar shows the comet.
  const task = await workspace.api('/api/tasks', 'POST', {
    name: 'Long review',
    prompt: 'fixture:chat-hang',
    agentId: '00000000-0000-4000-8000-000000000001',
    worktree: false,
  })
  const run = await workspace.api(`/api/tasks/${task.id}/run`, 'POST')
  await expect(codex.getByRole('button', { name: /^Work,/ })).toContainText('1 running', { timeout: 20000 })
  await expect(codex.getByRole('button', { name: /^Work,/ }).locator('.avatar-orbit')).toBeVisible()
  await expect(codex.getByRole('button', { name: /^Almost empty,/ })).toContainText('Low')
  await expect(codex.getByRole('button', { name: /^Travel,/ })).toContainText('Paused')
  await expect(codex.getByRole('progressbar', { name: 'Work Weekly remaining' })).toHaveAttribute('aria-valuenow', '82')
  for (const theme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme: theme })
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: width === 1440 ? 1000 : 844 })
      await expectSingleScroll(page)
      await page.screenshot({ path: testInfo.outputPath(`connections-${theme}-${width}.png`), fullPage: true, animations: 'disabled' })
    }
  }

  await workspace.api(`/api/runs/${run.id}/cancel`, 'POST')
  await expect(codex.getByRole('button', { name: /^Work,/ })).not.toContainText('running', { timeout: 20000 })

  // Everything else about an account is in its detail panel.
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.emulateMedia({ colorScheme: 'light' })
  await codex.getByRole('button', { name: /^Work,/ }).click()
  let panel = page.getByRole('dialog', { name: 'Work' })
  await expect(panel.getByText('3 banked resets')).toBeVisible()
  await expect(panel.getByText('Nothing is running on this account.')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('account-detail-light-1440.png'), animations: 'disabled' })
  await panel.getByRole('button', { name: 'More parallel runs' }).click()
  await expect(panel.getByText('Running now · 0 of 5')).toBeVisible()
  await panel.getByRole('switch', { name: /Use for new runs/ }).click()
  await expect(codex.getByRole('button', { name: /^Work,/ })).toContainText('Paused')
  await expect(codex.getByRole('button', { name: /^Personal,/ })).toContainText('Next up')
  await panel.getByLabel('Name', { exact: true }).fill('Work subscription')
  await panel.getByLabel('Name', { exact: true }).press('Enter')
  panel = page.getByRole('dialog', { name: 'Work subscription' })
  await expect(panel).toBeVisible()
  const saved = (await workspace.api('/api/accounts')).accounts.find((a: { id: string }) => a.id === work.id)
  expect(saved).toMatchObject({ name: 'Work subscription', enabled: false, maxConcurrentRuns: 5 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.screenshot({ path: testInfo.outputPath('account-detail-dark-390.png'), animations: 'disabled' })
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)

  // Usage reaching zero makes an account wait for its reset.
  await page.setViewportSize({ width: 1440, height: 1000 })
  await workspace.setAccountUsage(personal.id, limits(100, 37))
  await page.getByRole('button', { name: 'Check usage now', exact: true }).click()
  await expect(codex.getByRole('button', { name: /^Personal,/ })).toContainText('Resets in')

  // Removing an account deletes it and its credentials.
  await codex.getByRole('button', { name: /^Almost empty,/ }).click()
  panel = page.getByRole('dialog', { name: 'Almost empty' })
  await panel.getByRole('button', { name: 'Remove', exact: true }).click()
  await page.getByRole('dialog', { name: 'Remove Almost empty?' }).getByRole('button', { name: 'Remove account' }).click()
  await expect(codex.getByRole('button', { name: /^Almost empty,/ })).toHaveCount(0)
  expect(errors).toEqual([])
})

test('a Codex reconnection shows its device code, survives a reload and can be cancelled', async ({ page, workspace, context }, testInfo) => {
  test.setTimeout(90000)
  const account = await seedCodexAccount(workspace, 'Second account', limits(20, 30))
  await workspace.api('/api/accounts/refresh', 'POST')
  const home = await codexSignInMode(workspace, account.id, 'hold')
  await signIn(page)
  const codex = page.getByRole('region', { name: 'Codex accounts' })
  await codex.getByRole('button', { name: /^Second account,/ }).click()
  await page.getByRole('dialog', { name: 'Second account' }).getByRole('button', { name: 'Reconnect' }).click()
  const dialog = page.getByRole('dialog', { name: 'Connect your ChatGPT account' })
  await expect(dialog.getByLabel('Verification code', { exact: true })).toHaveText('ABCD-12345')
  await page.reload()
  await expect(dialog.getByLabel('Verification code', { exact: true })).toHaveText('ABCD-12345')
  for (const [theme, width] of [['light', 1440], ['dark', 390], ['dark', 320]] as const) {
    await page.emulateMedia({ colorScheme: theme })
    await page.setViewportSize({ width, height: 844 })
    await page.screenshot({ path: testInfo.outputPath(`codex-sign-in-${theme}-${width}.png`), animations: 'disabled' })
  }

  // Keep the provider fully synthetic, including clipboard permissions.
  await context.route('https://auth.openai.com/**', route => route.fulfill({ contentType: 'text/html', body: '<h1>Fixture verification page</h1>' }))
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: async (text: string) => {
          document.documentElement.dataset.copiedCode = text
        },
      },
    })
  })
  const popupPromise = page.waitForEvent('popup')
  await dialog.getByRole('link', { name: 'Copy code & open sign-in' }).click()
  const popup = await popupPromise
  await expect(popup.getByRole('heading')).toHaveText('Fixture verification page')
  await popup.close()
  await expect(page.locator('html')).toHaveAttribute('data-copied-code', 'ABCD-12345')
  await expect(dialog.getByRole('button', { name: 'Code copied' })).toBeVisible()
  await dialog.getByRole('button', { name: 'Cancel sign-in' }).click()
  await expect(dialog).toHaveCount(0)
  expect((await workspace.api('/api/accounts')).signIn).toBeNull()
  // A connected account survives a cancelled reconnection.
  await expect(codex.getByRole('button', { name: /^Second account,/ })).toBeVisible()

  await codexSignInMode(workspace, account.id, 'hold')
  await codex.getByRole('button', { name: /^Second account,/ }).click()
  await page.getByRole('dialog', { name: 'Second account' }).getByRole('button', { name: 'Reconnect' }).click()
  await expect(dialog.getByLabel('Verification code', { exact: true })).toHaveText('ABCD-12345')
  await writeFile(path.join(home, 'fixture-login-approve'), '')
  await expect(dialog).toHaveCount(0, { timeout: 15000 })
  expect((await workspace.api('/api/accounts')).signIn.state).toBe('complete')
})

test('statuses come from the server: stale, unknown and expired accounts stay understandable', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const now = Date.now()
  const base = {
    enabled: true,
    plan: 'max',
    createdAt: now,
    checkedAt: now,
    lastUsedAt: null,
    maxConcurrentRuns: 4,
    activeRunIds: [],
    error: '',
    exhausted: false,
    resetsAt: null,
  }
  const accounts = [
    {
      ...base,
      id: 'a',
      provider: 'claude',
      name: 'Stale',
      email: 'stale@example.test',
      state: 'ready',
      status: 'next',
      stale: true,
      remainingPercent: 0,
      usage: {
        allowed: true,
        checkedAt: now - 3600000,
        error: 'Claude Code usage is temporarily unavailable.',
        resets: null,
        windows: [{
          id: 'five_hour',
          label: '5-hour window',
          usedPercent: 105,
          resetsAt: null,
          durationMins: 300,
          models: [],
        }],
      },
    },
    {
      ...base,
      id: 'b',
      provider: 'claude',
      name: 'Unknown',
      email: 'unknown@example.test',
      state: 'ready',
      status: 'next',
      stale: true,
      remainingPercent: null,
      usage: null,
    },
    {
      ...base,
      id: 'c',
      provider: 'claude',
      name: 'Expired',
      email: 'expired@example.test',
      state: 'error',
      status: 'reconnect',
      stale: true,
      remainingPercent: null,
      usage: null,
      error: 'Reconnect this Claude account.',
    },
  ]
  await page.route('**/api/accounts', route => route.fulfill({ json: { accounts, signIn: null } }))
  await signIn(page)
  const claude = page.getByRole('region', { name: 'Claude Code accounts' })
  await expect(claude.getByRole('button', { name: /^Stale,/ }).getByRole('progressbar')).toHaveAttribute('aria-valuenow', '0')
  await expect(claude.getByRole('button', { name: /^Unknown,/ })).toContainText('Usage not reported yet')
  await expect(claude.getByRole('button', { name: /^Expired,/ })).toContainText('Reconnect')
  await expect(claude.getByRole('button', { name: /^Expired,/ })).not.toContainText('Usage')
  await claude.getByRole('button', { name: /^Stale,/ }).click()
  await expect(page.getByRole('dialog', { name: 'Stale' })).toContainText('Claude Code usage is temporarily unavailable.')
  await page.keyboard.press('Escape')
  await claude.getByRole('button', { name: /^Unknown,/ }).click()
  await expect(page.getByRole('dialog', { name: 'Unknown' })).toContainText('Runs can still use this account.')
})
