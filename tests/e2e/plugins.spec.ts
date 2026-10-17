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

// Discord itself is never contacted: the package's setup form is answered by the browser
// fixture, the way the Discord package's `form` handler answers it.
async function discordForm(route: Route) {
  const body = route.request().postDataJSON()
  if (body.token !== token && typeof body.instance !== 'string') {
    await route.fulfill({
      json: {
        options: {},
        values: {},
        errors: {},
        view: [{
          type: 'alert',
          tone: 'info',
          title: 'Create the bot',
          text: 'Paste the token from the Developer Portal.',
        }],
      },
    })
    return
  }

  const settings = body.settings ?? {}
  const guild = settings.guildId === '100'
  const channel = guild && settings.channelId === '200' && body.changed !== 'guildId'
  await route.fulfill({
    json: {
      options: {
        guildId: [{ value: '100', label: 'Acme' }],
        ...(guild && {
          channelId: [
            { value: '200', label: '#support', description: 'Can reply' },
            {
              value: '201',
              label: '#leadership',
              description: 'The bot can’t read this channel',
              disabled: true,
            },
          ],
        }),
      },
      values: {
        botId: '400',
        botName: 'Cairn Bot',
        ...(guild && { guildName: 'Acme' }),
        ...(channel && { channelName: 'support' }),
        ...(body.changed === 'guildId' && { channelId: '', channelName: '' }),
      },
      errors: {},
      view: [
        {
          type: 'alert',
          tone: 'success',
          title: 'Signed in as Cairn Bot',
          text: 'Bot account · in 1 server',
        },
        { type: 'link', label: 'Invite the bot to a server', url: 'https://discord.com/oauth2/authorize?client_id=400&scope=bot&permissions=309237746752' },
      ],
    },
  })
}

test('Discord bots are set up through the package’s own form and keep the token private', async ({ page, workspace }) => {
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
  await page.route('**/plugins/discord/form', discordForm)
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
  await expect(page.getByText('No bots yet. Add one to start using this plugin.')).toBeVisible()
  await page.getByRole('button', { name: 'Add bot' }).click()
  const dialog = page.getByRole('dialog', { name: 'New bot' })
  await expect(dialog.getByText('Create the bot')).toBeVisible()

  await expect(dialog.getByLabel('Bot token')).toHaveAttribute('type', 'password')
  await dialog.getByLabel('Bot token').fill(token)
  await expect(dialog.getByRole('status').filter({ hasText: 'Signed in as Cairn Bot' })).toBeVisible()
  await expect(dialog.getByRole('link', { name: 'Invite the bot to a server' })).toHaveAttribute('href', /permissions=309237746752/)
  await dialog.getByRole('combobox', { name: 'Server' }).selectOption('100')
  await expect(dialog.getByRole('radio', { name: /#leadership/ })).toBeDisabled()
  await expect(dialog.getByText('The bot can’t read this channel')).toBeVisible()
  await dialog.getByRole('radio', { name: /#support/ }).check()
  await expect(dialog.getByRole('checkbox', { name: /Read replies without a mention/ })).not.toBeChecked()

  await expect(dialog.getByRole('radio', { name: /Approve people as they ask/ })).toBeChecked()
  // Anyone may only reach a read-only agent; this one isn't.
  await dialog.getByRole('radio', { name: /^Anyone/ }).check()
  await expect(dialog.getByRole('radio', { name: /Discord helper/ })).toBeDisabled()
  await expect(dialog.getByText('Not read-only').first()).toBeVisible()
  await dialog.getByRole('radio', { name: /Approve people as they ask/ }).check()
  await dialog.getByLabel('Person’s ID').fill('300')
  await dialog.getByRole('button', { name: 'Add', exact: true }).click()
  await expect(dialog.getByText('300', { exact: true })).toBeVisible()
  await expect(dialog.getByRole('radio', { name: /Release helper/ })).toBeDisabled()
  await dialog.getByRole('radio', { name: /Discord helper/ }).check()
  await dialog.getByLabel('Name', { exact: true }).fill('#support on Acme')
  await dialog.getByRole('button', { name: 'Save bot' }).click()

  const bot = page.getByRole('article', { name: '#support on Acme' })
  await expect(bot.getByText('Paused', { exact: true })).toBeVisible()
  await expect(bot.getByText('1 person · approve as they ask')).toBeVisible()
  await expect(bot.getByText('Discord helper', { exact: true })).toBeVisible()
  await expect(bot.getByRole('meter', { name: 'Messages this hour' })).toBeVisible()
  await expect(bot.getByText('Recent activity')).toBeVisible()

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
      messageContent: false,
    },
    userIds: ['300'],
    accessMode: 'requests',
  })
  expect(JSON.stringify(catalog)).not.toContain(token)

  await bot.getByRole('button', { name: 'Remove User 300' }).click()
  await expect(bot.getByText('0 people · approve as they ask')).toBeVisible()

  await bot.getByRole('button', { name: 'Edit #support on Acme' }).click()
  const editor = page.getByRole('dialog', { name: 'Edit #support on Acme' })
  await expect(editor.getByLabel('New bot token (optional)')).toHaveValue('')
  await expect(editor.getByRole('status').filter({ hasText: 'Signed in as Cairn Bot' })).toBeVisible()
  await expect(editor.getByRole('radio', { name: /#support/ })).toBeChecked()
  await editor.getByRole('radio', { name: /Only people I add/ }).check()
  await editor.getByLabel('Person’s ID').fill('300')
  await editor.getByRole('button', { name: 'Add', exact: true }).click()
  await editor.getByRole('button', { name: 'Save bot' }).click()
  await expect(bot.getByText('1 person · only listed people')).toBeVisible()
  expect((await workspace.api('/api/plugins')).connections[0].settings.channelName).toBe('support')

  await bot.getByRole('button', { name: 'Delete #support on Acme' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete bot' }).click()
  await expect(page.getByText('No bots yet. Add one to start using this plugin.')).toBeVisible()
})

test('installed packages wait for review and connect through a generated form', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const manifest = {
    id: 'notes',
    name: 'Team notes',
    version: '1.0.0',
    description: 'Keeps a note for each team.',
    apiVersion: 2,
    capabilities: ['storage', 'timers'],
    settings: [{
      key: 'team',
      title: 'Team',
      type: 'string',
      required: true,
    }],
  }
  // Split into a module, loaded with require.
  const source = 'local notes = require("./notes")\nreturn { start = function(event) cairn.storage.set("started", notes.started) end }'
  const module = 'return { started = true }'
  await page.goto(workspacePath('/plugins'))
  await signIn(page)
  await page.getByRole('button', { name: 'Install package' }).click()
  const install = page.getByRole('dialog', { name: 'Install a plugin package' })
  await install.getByLabel('Package files').setInputFiles([
    { name: 'manifest.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(manifest)) },
    { name: 'main.luau', mimeType: 'text/plain', buffer: Buffer.from(source) },
    { name: 'notes.luau', mimeType: 'text/plain', buffer: Buffer.from(module) },
  ])
  await install.getByRole('button', { name: 'Install', exact: true }).click()

  const review = page.getByRole('dialog', { name: 'Review Team notes' })
  await expect(review.getByText('Storage', { exact: true })).toBeVisible()
  await expect(review.getByText('Timers', { exact: true })).toBeVisible()
  await expect(review.getByRole('list', { name: 'Package files' })).toContainText('notes.luau')
  await review.getByRole('button', { name: 'Approve and activate' }).click()
  await expect(review).not.toBeVisible()
  await page.getByRole('link', { name: 'Team notes · Set up' }).click()

  await page.getByRole('button', { name: 'Add instance' }).click()
  const form = page.getByRole('dialog', { name: 'New Team notes instance' })
  await form.getByLabel('Team').fill('core')
  await form.getByRole('button', { name: 'Save instance' }).click()
  const connection = page.getByRole('article', { name: 'Team notes' })
  await expect(connection.getByText('core', { exact: true })).toBeVisible()
  await expect(connection.getByRole('status')).toHaveText('Online', { timeout: 20000 })

  const catalog = await workspace.api('/api/plugins')
  expect(catalog.packages.find((item: { id: string }) => item.id === 'notes')).toMatchObject({ status: 'active', active: '1.0.0' })
  expect(catalog.connections[0]).toMatchObject({ pluginId: 'notes', settings: { team: 'core' }, agentId: '' })

  await connection.getByRole('button', { name: 'Delete Team notes' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete instance' }).click()
  await expect(page.getByText('No instances yet. Add one to start using this plugin.')).toBeVisible()
  await page.getByRole('button', { name: 'Access and versions' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Uninstall' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Confirm uninstall' }).click()
  await expect(page.getByText('This plugin isn’t installed.')).toBeVisible()
})

test('owner resumes an inert setup, prepares resources and activates the same instance', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'workspace-service',
    name: 'Workspace service',
    version: '1',
    apiVersion: 2,
    description: 'A service with separate workspaces.',
    capabilities: ['resources'],
    instanceForm: true,
    settings: [
      {
        key: 'clientId',
        title: 'Client ID',
        type: 'string',
        required: true,
      },
      {
        key: 'apiKey',
        title: 'App key',
        type: 'secret',
        required: true,
      },
      {
        key: 'setupIdentity',
        title: 'Setup identity',
        type: 'string',
        hidden: true,
      },
    ],
    resources: [{
      name: 'workspace',
      title: 'Workspaces',
      singular: 'Workspace',
      settings: [
        {
          key: 'externalId',
          title: 'Workspace ID',
          type: 'string',
          required: true,
        },
        {
          key: 'token',
          title: 'Workspace token',
          type: 'secret',
          required: true,
        },
      ],
    }],
  }
  const source = `return {form = function(event)
    assert(event.instance ~= nil and event.setup ~= nil)
    assert(event.values.apiKey == nil)
    return {values = {setupIdentity = event.instance}, view = {{type = 'alert', tone = 'info', title = 'Prepare the service', text = 'Save settings before adding workspaces.'}}}
  end}`
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source })
  await workspace.api('/api/plugins/packages/workspace-service/approve', 'POST', { version: '1', digest: installed.digest })
  await page.goto(workspacePath('/plugins/workspace-service'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add instance', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Workspace service' })
  await expect(setup.getByText('Prepare the service', { exact: true })).toBeVisible()
  await setup.getByLabel('Name', { exact: true }).fill('Production service')
  await setup.getByLabel('Client ID', { exact: true }).fill('client-browser-fixture')
  await setup.getByLabel('App key', { exact: true }).fill('app-browser-fixture')
  await setup.getByRole('button', { name: 'Save setup', exact: true }).click()
  await expect(setup.getByText('All changes saved', { exact: true })).toBeVisible()
  let drafts = await workspace.api('/api/plugins/workspace-service/setup-drafts')
  expect(drafts.items).toHaveLength(1)
  const draft = drafts.items[0]
  expect((await workspace.api('/api/plugins')).connections.filter((item: any) => item.pluginId === manifest.id)).toHaveLength(0)
  expect(JSON.stringify(drafts)).not.toContain('app-browser-fixture')
  await setup.getByRole('button', { name: 'Close dialog' }).click()
  await page.reload()
  await page.getByRole('button', { name: 'Continue Production service', exact: true }).click()
  await expect(setup.getByLabel('App key', { exact: true })).toHaveValue('')
  await setup.getByRole('button', { name: 'Add workspace', exact: true }).click()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  const resource = setup.getByRole('form', { name: 'New workspace' })
  await resource.getByLabel('Name', { exact: true }).fill('Engineering')
  await resource.getByLabel('Workspace ID', { exact: true }).fill('T111')
  await resource.getByLabel('Workspace token', { exact: true }).fill('workspace-browser-fixture')
  let releaseResource!: () => void
  let resourceStarted!: () => void
  const held = new Promise<void>((resolve) => {
    releaseResource = resolve
  })
  const entered = new Promise<void>((resolve) => {
    resourceStarted = resolve
  })
  await page.route('**/plugins/workspace-service/setup-drafts/*/resources/workspace', async (route) => {
    if (route.request().method() === 'POST') {
      resourceStarted()
      await held
    }

    await route.continue()
  })
  await resource.getByRole('button', { name: 'Save workspace', exact: true }).click()
  await entered
  try {
    await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  }
  finally {
    releaseResource()
  }

  await expect(setup.getByRole('article', { name: 'Engineering', exact: true })).toBeVisible()
  await setup.getByRole('button', { name: 'Activate instance', exact: true }).click()
  await expect(setup).not.toBeVisible()
  const active = (await workspace.api('/api/plugins')).connections.find((item: any) => item.pluginId === manifest.id)
  expect(active.id).toBe(draft.id)
  expect(active.settings.setupIdentity).toBe(draft.id)
  expect(active.agentIds).toEqual([])
  const resources = await workspace.api(`/api/plugins/workspace-service/connections/${active.id}/resources/workspace`)
  expect(resources.items).toHaveLength(1)
  expect(resources.items[0].name).toBe('Engineering')
  expect(JSON.stringify(resources)).not.toContain('workspace-browser-fixture')
  const instance = page.getByRole('article', { name: 'Production service', exact: true })
  await instance.getByRole('button', { name: 'Edit Engineering', exact: true }).click()
  const editor = instance.getByRole('form', { name: 'Edit Engineering', exact: true })
  await expect(editor.getByLabel('Workspace token', { exact: true })).toHaveValue('')
  await editor.getByLabel('Name', { exact: true }).fill('Engineering team')
  await editor.getByRole('button', { name: 'Save workspace', exact: true }).click()
  const updated = await workspace.api(`/api/plugins/workspace-service/connections/${active.id}/resources/workspace/${resources.items[0].id}`)
  expect(updated.name).toBe('Engineering team')
  expect(updated.secretKeys).toEqual(['token'])
  await instance.getByRole('button', { name: 'Delete Engineering team', exact: true }).click()
  await instance.getByRole('button', { name: 'Confirm delete', exact: true }).click()
  await expect(instance.getByRole('article', { name: 'Engineering team', exact: true })).not.toBeVisible()
  drafts = await workspace.api('/api/plugins/workspace-service/setup-drafts')
  expect(drafts.items).toEqual([])
  await page.getByRole('button', { name: 'Add instance', exact: true }).click()
  await expect(setup).toBeVisible()
  await setup.getByRole('button', { name: 'Close dialog' }).click()
  const replacement = await workspace.api('/api/plugins/packages', 'POST', {
    manifest: {
      ...manifest,
      version: '2',
      capabilities: [],
      settings: [],
      resources: [],
      instanceForm: false,
    },
    source: 'return {}',
  })
  await workspace.api('/api/plugins/packages/workspace-service/approve', 'POST', { version: '2', digest: replacement.digest })
  await page.reload()
  await expect(page.getByText(/Package review needed · expires/)).toBeVisible()
  await page.getByRole('button', { name: 'Discard Workspace service', exact: true }).click()
  const discard = page.getByRole('dialog', { name: 'Discard Workspace service?' })
  await discard.getByRole('button', { name: 'Cancel', exact: true }).click()
  expect((await workspace.api('/api/plugins/workspace-service/setup-drafts')).items).toHaveLength(1)
  await page.getByRole('button', { name: 'Discard Workspace service', exact: true }).click()
  await discard.getByRole('button', { name: 'Discard setup', exact: true }).click()
  await expect(discard).not.toBeVisible()
  expect((await workspace.api('/api/plugins/workspace-service/setup-drafts')).items).toEqual([])
  expect((await workspace.api('/api/plugins')).connections.find((item: any) => item.id === active.id)).toBeDefined()
})

test('setup explains credential errors and accepts an empty form answer', async ({ page, workspace }) => {
  const manifest = {
    id: 'setup-feedback',
    name: 'Optional form',
    version: '1',
    apiVersion: 2,
    description: 'A form that can return no additional guidance.',
    capabilities: ['resources'],
    instanceForm: true,
    credential: { label: 'Service credential', header: 'authorization' },
    settings: [{
      key: 'verifiedTeam',
      title: 'Verified team',
      type: 'string',
      hidden: true,
    }],
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
  }
  const source = `return {form = function(event)
    if not event.credential then return {errors = {credential = 'Enter the service credential.', verifiedTeam = 'Invite the app to the workspace before continuing.'}} end
    if event.changed == 'credential' then return nil end
    return cairn.json.decode('{"options":null,"values":null,"errors":null,"view":null}')
  end}`
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source })
  await workspace.api('/api/plugins/packages/setup-feedback/approve', 'POST', { version: '1', digest: installed.digest })
  await page.goto(workspacePath('/plugins/setup-feedback'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add instance', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Optional form' })
  await expect(setup.getByText('Enter the service credential.', { exact: true })).toBeVisible()
  await expect(setup.getByText('Invite the app to the workspace before continuing.', { exact: true })).toBeVisible()
  await expect(setup.getByLabel('Service credential', { exact: true })).toHaveAttribute('aria-invalid', 'true')
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  await setup.getByLabel('Service credential', { exact: true }).fill('credential-browser-fixture')
  await setup.getByRole('button', { name: 'Save setup', exact: true }).click()
  await expect(setup.getByText('All changes saved', { exact: true })).toBeVisible()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeEnabled()
  await setup.getByRole('button', { name: 'Activate instance', exact: true }).click()
  await expect(setup).not.toBeVisible()
  const catalog = await workspace.api('/api/plugins')
  expect(catalog.connections.find((item: any) => item.pluginId === manifest.id)).toBeDefined()
  expect(JSON.stringify(catalog)).not.toContain('credential-browser-fixture')
})
