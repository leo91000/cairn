import type { Route } from '@playwright/test'
import { Buffer } from 'node:buffer'
import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

const token = `synthetic_browser_discord_token_${'x'.repeat(48)}`

// Discord itself is never contacted: setup discovery is answered by the browser fixture.
async function discord(route: Route) {
  const body = route.request().postDataJSON()
  expect(body.token === token || typeof body.connectionId === 'string').toBe(true)
  await route.fulfill({
    json: {
      bot: { id: '400', name: 'Cairn Bot', username: 'cairn' },
      applicationId: '400',
      inviteUrl: 'https://discord.com/oauth2/authorize?client_id=400&scope=bot&permissions=68608',
      guilds: [{ id: '100', name: 'Acme' }],
      ...(body.guildId === '100' && {
        channels: [
          {
            id: '200',
            name: 'support',
            canRead: true,
            canReply: true,
          },
          {
            id: '201',
            name: 'leadership',
            canRead: false,
            canReply: false,
          },
        ],
      }),
      ...(body.userIds && { users: [{ id: '300', name: 'Sarah' }] }),
    },
  })
}

test('Discord setup guides the owner and keeps the bot token private', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const helper = await workspace.api('/api/agents', 'POST', {
    name: 'Discord helper',
    access: {
      projects: [],
      skills: [],
      mcps: [],
      github: false,
      installation: false,
    },
  })
  await workspace.api('/api/agents', 'POST', { name: 'Release helper' })
  await useRelayForHttpMocks(page)
  await page.route('**/plugins/discord/discover', discord)
  // Keep the journey offline: the saved bot starts paused instead of opening a Gateway session.
  await page.route('**/plugins/discord/connections', async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    await route.continue({ postData: JSON.stringify({ ...route.request().postDataJSON(), enabled: false }) })
  })
  await page.goto(workspacePath('/plugins'))
  await signIn(page)

  await expect(page.getByRole('link', { name: '1Password · Set up' })).toBeVisible()
  await page.getByRole('link', { name: 'Discord · Set up' }).click()
  await expect(page.getByRole('heading', { name: 'Your agents, right in Discord.' })).toBeVisible()
  await page.getByRole('link', { name: 'Set up a bot' }).click()

  const continueButton = page.getByRole('button', { name: 'Continue' })
  await expect(continueButton).toBeDisabled()
  await expect(page.getByLabel('Bot token')).toHaveAttribute('type', 'password')
  await page.getByLabel('Bot token').fill(token)
  await expect(page.getByRole('status').filter({ hasText: 'Cairn Bot' })).toContainText('Valid')
  await continueButton.click()

  await expect(page.getByRole('link', { name: /Invite bot/ })).toHaveAttribute('href', /permissions=68608/)
  await page.getByRole('radio', { name: 'Acme' }).check()
  await expect(page.getByRole('radio', { name: /leadership/ })).toBeDisabled()
  await page.getByRole('radio', { name: /support/ }).check()
  await continueButton.click()

  await expect(page.getByRole('radio', { name: /Approve people as they ask/ })).toBeChecked()
  await page.getByLabel('Discord user ID').fill('300')
  await page.getByRole('button', { name: 'Add', exact: true }).click()
  await expect(page.getByText('Sarah')).toBeVisible()
  await continueButton.click()

  await expect(page.getByRole('radio', { name: /Release helper/ })).toBeDisabled()
  await page.getByRole('radio', { name: /Discord helper/ }).check()
  await expect(page.getByLabel('Name in Cairn')).toHaveValue('#support on Acme')
  await expect(page.getByText('Summary')).toBeVisible()
  await page.getByRole('button', { name: 'Activate bot' }).click()

  await expect(page).toHaveURL(/\/plugins\/discord$/)
  const bot = page.getByRole('region', { name: '#support on Acme' })
  await expect(bot.getByText('Paused', { exact: true })).toBeVisible()
  await expect(bot.getByText('1 person · approve as they ask')).toBeVisible()
  await expect(bot.getByText('Start conversations with Discord helper')).toBeVisible()

  const catalog = await workspace.api('/api/plugins')
  expect(catalog.connections).toHaveLength(1)
  expect(catalog.connections[0]).toMatchObject({
    pluginId: 'discord',
    agentId: helper.id,
    projectId: null,
    settings: {
      guildId: '100',
      guildName: 'Acme',
      channelId: '200',
      channelName: 'support',
      botId: '400',
      botName: 'Cairn Bot',
    },
    userIds: ['300'],
    people: [{ id: '300', name: 'Sarah' }],
    accessMode: 'requests',
  })
  expect(JSON.stringify(catalog)).not.toContain(token)

  await bot.getByRole('button', { name: 'Manage people for #support on Acme' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Remove Sarah' }).click()
  await expect(page.getByRole('dialog').getByText('Nobody yet.')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(bot.getByText('0 people · approve as they ask')).toBeVisible()

  await bot.getByRole('link', { name: 'Edit setup' }).click()
  await expect(page.getByLabel('New bot token (optional)')).toHaveValue('')
  await expect(page.getByRole('status').filter({ hasText: 'Cairn Bot' })).toContainText('Valid')
  await continueButton.click()
  await expect(page.getByRole('radio', { name: /support/ })).toBeChecked()
  await continueButton.click()
  await page.getByRole('radio', { name: /Only people I add/ }).check()
  await expect(continueButton).toBeDisabled()
  await page.getByLabel('Discord user ID').fill('300')
  await page.getByRole('button', { name: 'Add', exact: true }).click()
  await continueButton.click()
  await page.getByRole('button', { name: 'Save changes' }).click()
  await expect(bot.getByText('1 person', { exact: true })).toBeVisible()

  await bot.getByRole('button', { name: 'Remove bot' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Remove bot' }).click()
  await expect(page.getByRole('heading', { name: 'Your agents, right in Discord.' })).toBeVisible()
})

test('installed packages wait for review and connect through a generated form', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const manifest = {
    id: 'notes',
    name: 'Team notes',
    version: '1.0.0',
    description: 'Keeps a note for each team.',
    apiVersion: 1,
    capabilities: ['storage', 'timers'],
    settings: [{
      key: 'team',
      title: 'Team',
      type: 'string',
      required: true,
    }],
  }
  const source = 'return { start = function(event) cairn.storage.set("started", true) end }'
  await page.goto(workspacePath('/plugins'))
  await signIn(page)
  await page.getByRole('button', { name: 'Install package' }).click()
  const install = page.getByRole('dialog', { name: 'Install a plugin package' })
  await install.getByLabel('Package files').setInputFiles([
    { name: 'manifest.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(manifest)) },
    { name: 'main.luau', mimeType: 'text/plain', buffer: Buffer.from(source) },
  ])
  await install.getByRole('button', { name: 'Install', exact: true }).click()

  const review = page.getByRole('dialog', { name: 'Review Team notes' })
  await expect(review.getByText('Storage', { exact: true })).toBeVisible()
  await expect(review.getByText('Timers', { exact: true })).toBeVisible()
  await review.getByRole('button', { name: 'Approve and activate' }).click()
  await expect(review).not.toBeVisible()
  await page.getByRole('link', { name: 'Team notes · Set up' }).click()

  await page.getByRole('button', { name: 'Add connection' }).click()
  const form = page.getByRole('dialog', { name: 'Connect Team notes' })
  await form.getByLabel('Team').fill('core')
  await form.getByRole('button', { name: 'Save connection' }).click()
  const connection = page.getByRole('article', { name: 'Team notes' })
  await expect(connection.getByText('core', { exact: true })).toBeVisible()
  await expect(connection.getByRole('status')).toHaveText('Online', { timeout: 20000 })

  const catalog = await workspace.api('/api/plugins')
  expect(catalog.packages.find((item: { id: string }) => item.id === 'notes')).toMatchObject({ status: 'active', active: '1.0.0' })
  expect(catalog.connections[0]).toMatchObject({ pluginId: 'notes', settings: { team: 'core' }, agentId: '' })

  await connection.getByRole('button', { name: 'Delete Team notes' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete connection' }).click()
  await expect(page.getByText('No connections yet.')).toBeVisible()
  await page.getByRole('button', { name: 'Access and versions' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Uninstall' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Confirm uninstall' }).click()
  await expect(page.getByText('This plugin isn’t installed.')).toBeVisible()
})
