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

test('personal plugin accounts select scopes and complete only their own authorization', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'personal-ui',
    name: 'Personal service',
    version: '1',
    apiVersion: 2,
    description: 'Personal authorizations on separate workspaces.',
    capabilities: ['resources', 'oauth', 'http'],
    hosts: ['provider.example.test'],
    settings: [{ key: 'clientId', title: 'Client ID', type: 'string' }],
    resources: [{
      name: 'workspace',
      title: 'Workspaces',
      singular: 'Workspace',
      settings: [{ key: 'externalId', title: 'Workspace ID', type: 'string' }],
    }],
    oauth: [{
      name: 'personal',
      title: 'Personal workspace access',
      collection: 'workspace',
      authorizationUrl: 'https://provider.example.test/authorize',
      tokenUrl: 'https://provider.example.test/token',
      clientId: 'clientId',
      pkce: true,
      scopes: ['messages:read', 'messages:write'],
      token: { accessToken: '/access_token', scope: '/scope' },
      metadata: { externalId: '/team/id' },
      personal: { subject: '/authed_user/id' },
    }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api('/api/plugins/packages/personal-ui/approve', 'POST', { version: '1', digest: installed.digest })
  const instance = await workspace.api('/api/plugins/personal-ui/connections', 'POST', { name: 'Company service', enabled: true, settings: { clientId: 'fixture-ui-client' } })
  const resource = await workspace.api(`/api/plugins/personal-ui/connections/${instance.id}/resources/workspace`, 'POST', { name: 'Engineering', settings: { externalId: 'T111' } })
  const oauth = `/plugins/personal-ui/connections/${instance.id}/resources/workspace/${resource.id}/oauth/personal`
  const nonce = 's'.repeat(43)
  let approved = false
  let authorized = false
  const starts: any[] = []
  const status = () => authorized
    ? {
        status: 'authorized',
        scopes: ['messages:read'],
        requestedScopes: ['messages:read'],
        subject: 'U111',
      }
    : { status: 'disconnected' }
  // The directory is real. Provider consent/exchange is answered at the public HTTP
  // seam; the native provider/identity protocol is exercised by plugin_oauth.rs.
  await page.route('**/plugins/personal-authorizations*', async (route) => {
    const response = await route.fetch()
    const result = await response.json()
    result.items = result.items.map((item: any) => ({ ...item, authorization: status() }))
    await route.fulfill({ response, json: result })
  })
  await page.route(`**${oauth}`, async (route) => {
    if (route.request().method() === 'POST') {
      starts.push(route.request().postDataJSON())
      await route.fulfill({ json: { state: nonce, url: `https://provider.example.test/authorize?state=${nonce}`, expiresAt: Date.now() + 60000 } })
    }
    else if (route.request().method() === 'DELETE') {
      authorized = false
      await route.fulfill({ json: { status: 'disconnected' } })
    }
    else { await route.fulfill({ json: status() }) }
  })
  await page.route(`**${oauth}/finish`, async (route) => {
    expect(route.request().postDataJSON()).toEqual({ state: nonce })
    if (approved)
      authorized = true
    await route.fulfill({ json: approved ? status() : { status: 'pending' } })
  })
  await page.context().route('https://provider.example.test/**', async (route) => {
    approved = true
    await route.fulfill({ contentType: 'text/html', body: '<p>Authorized. Return to Cairn.</p>' })
  })
  await page.goto(workspacePath('/atelier'))
  await signIn(page)
  await page.getByRole('link', { name: 'Your plugin accounts', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Your plugin accounts', exact: true })).toBeVisible()
  const account = page.getByRole('article', { name: 'Engineering · Personal workspace access', exact: true })
  await expect(account.getByRole('button', { name: 'Connect', exact: true })).toBeDisabled()
  await account.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await account.getByRole('button', { name: 'Connect', exact: true }).click()
  expect(starts).toEqual([{ scopes: ['messages:read'] }])
  await account.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(account.getByText('Finish authorizing in your browser.', { exact: true })).toBeVisible()
  const opened = page.context().waitForEvent('page')
  await account.getByRole('link', { name: 'Open authorization page', exact: true }).click()
  const provider = await opened
  await expect(provider.getByText('Authorized. Return to Cairn.')).toBeVisible()
  await provider.close()
  await account.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(account.getByText('Connected as U111', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled()
  await page.screenshot({ path: test.info().outputPath('personal-plugin-account.png'), fullPage: true })
  await account.getByRole('button', { name: 'Disconnect', exact: true }).click()
  await page.getByRole('dialog', { name: 'Disconnect Personal workspace access?' }).getByRole('button', { name: 'Disconnect', exact: true }).click()
  await expect(account.getByText('Not connected', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled()
})

test('owner resource OAuth gates setup until every consent settles and remains manageable after activation', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await useRelayForHttpMocks(page)
  const owner = {
    name: 'owner',
    title: 'Workspace authorization',
    collection: 'workspace',
    authorizationUrl: 'https://provider.example.test/authorize',
    tokenUrl: 'https://provider.example.test/token',
    clientId: 'clientId',
    redirectOriginSetting: 'callbackOrigin',
    pkce: true,
    scopes: ['messages:read', 'messages:write'],
    token: { accessToken: '/access_token', scope: '/scope' },
    metadata: { externalId: '/team/id' },
  }
  const manifest = {
    id: 'owner-ui',
    name: 'Owner service',
    version: '1',
    apiVersion: 2,
    description: 'Shared resource consent.',
    capabilities: ['resources', 'oauth', 'http'],
    hosts: ['provider.example.test'],
    settings: [{
      key: 'clientId',
      title: 'Client ID',
      type: 'string',
      required: true,
    }, { key: 'callbackOrigin', title: 'Callback origin', type: 'url' }],
    resources: [{
      name: 'workspace',
      title: 'Workspaces',
      singular: 'Workspace',
      settings: [{ key: 'externalId', title: 'Workspace ID', type: 'string' }],
    }],
    oauth: [owner, {
      ...owner,
      name: 'personal',
      title: 'Personal access',
      personal: { subject: '/authed_user/id' },
    }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api('/api/plugins/packages/owner-ui/approve', 'POST', { version: '1', digest: installed.digest })
  const draft = await workspace.api('/api/plugins/owner-ui/setup-drafts', 'POST', { name: 'Shared service', settings: { clientId: 'fixture-client', callbackOrigin: 'https://saved.example.test' } })
  const root = `/plugins/owner-ui/setup-drafts/${draft.id}`
  const engineering = await workspace.api(`/api${root}/resources/workspace`, 'POST', { setupRevision: draft.revision, name: 'Engineering', settings: { externalId: 'T111' } })
  const sales = await workspace.api(`/api${root}/resources/workspace`, 'POST', { setupRevision: draft.revision, name: 'Sales', settings: { externalId: 'T222' } })
  // Two actual resources are exposed as separate HTTP pages to exercise paging
  // without creating a hundred unrelated authorization widgets in this journey.
  await page.route('**/plugins/owner-ui/*/*/resources/workspace*', async (route) => {
    const url = new URL(route.request().url())
    if (route.request().method() !== 'GET' || !url.pathname.endsWith('/resources/workspace'))
      return route.continue()
    const base = url.pathname.slice(url.pathname.indexOf('/plugins/'))
    const later = url.searchParams.has('cursor')
    const item = await workspace.api(`/api${base}/${later ? sales.id : engineering.id}`)
    await route.fulfill({ json: { items: [item], nextCursor: later ? null : engineering.id } })
  })
  const approved = new Set<string>()
  const authorized = new Set<string>()
  const writes: Array<{ method: string, path: string, body: any }> = []
  const status = (id: string) => authorized.has(id)
    ? { status: 'authorized', scopes: ['messages:read'], requestedScopes: ['messages:read'] }
    : { status: 'disconnected' }
  // Native setup/resource CRUD and activation are real. Provider consent is synthetic;
  // the native OAuth tests separately cover capture, exchange, provenance and promotion.
  await page.route('**/plugins/owner-ui/*/*/resources/workspace/*/oauth/owner**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const id = path.split('/resources/workspace/')[1].split('/')[0]
    const base = path.slice(path.indexOf('/plugins/'), path.indexOf('/oauth/'))
    const body = request.postData() ? request.postDataJSON() : {}
    if (request.method() !== 'GET')
      writes.push({ method: request.method(), path, body })
    if (request.method() === 'GET')
      return route.fulfill({ json: status(id) })
    if (path.endsWith('/finish') && !approved.has(id))
      return route.fulfill({ json: { status: 'pending' } })
    if (path.endsWith('/finish') || request.method() === 'DELETE') {
      if (request.method() === 'DELETE')
        authorized.delete(id)
      else
        authorized.add(id)
      // Exercise the public client's refresh after OAuth changes resource revision.
      const current = await workspace.api(`/api${base}`)
      await workspace.api(`/api${base}`, 'PUT', {
        revision: current.revision,
        name: current.name,
        settings: current.settings,
        ...(path.includes('/setup-drafts/') && { setupRevision: draft.revision }),
      })
      return route.fulfill({ json: status(id) })
    }

    return route.fulfill({
      json: {
        state: id.replaceAll('-', '').padEnd(43, 'x'),
        url: `https://provider.example.test/authorize?resource=${id}`,
        redirectUri: `${body.directOrigin ?? draft.settings.callbackOrigin}/callback`,
        expiresAt: Date.now() + 60000,
      },
    })
  })
  await page.context().route('https://provider.example.test/**', async (route) => {
    approved.add(new URL(route.request().url()).searchParams.get('resource')!)
    await route.fulfill({ contentType: 'text/html', body: '<p>Workspace authorized. Return to Cairn.</p>' })
  })
  await page.goto(workspacePath('/plugins/owner-ui'))
  await signIn(page)
  await page.getByRole('button', { name: 'Continue Shared service', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Owner service' })
  const first = setup.getByRole('article', { name: 'Engineering', exact: true })
  const second = setup.getByRole('article', { name: 'Sales', exact: true })
  await expect(first.getByRole('button', { name: 'Connect', exact: true })).toBeDisabled()
  await expect(setup.getByText('Personal access', { exact: true })).not.toBeVisible()
  await first.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await first.getByLabel('Direct HTTPS origin (optional)').fill('https://cairn.example.test')
  await first.getByRole('button', { name: 'Connect', exact: true }).click()
  await expect(first.getByRole('link', { name: 'Open authorization page' })).toBeVisible()
  await expect(first.getByText('Callback URL: https://cairn.example.test/callback', { exact: true })).toBeVisible()
  await setup.getByRole('button', { name: 'More workspaces', exact: true }).click()
  await second.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await second.getByRole('button', { name: 'Connect', exact: true }).click()
  await expect(second.getByRole('link', { name: 'Open authorization page' })).toBeVisible()
  await expect(second.getByText('Callback URL: https://saved.example.test/callback', { exact: true })).toBeVisible()
  await setup.getByRole('button', { name: 'Refresh resources', exact: true }).click()
  await expect(setup.getByRole('button', { name: 'Refresh resources', exact: true })).toBeEnabled()
  await expect(second.getByRole('link', { name: 'Open authorization page' })).toBeVisible()
  const starts = writes.filter(item => item.method === 'POST' && !item.path.endsWith('/finish'))
  expect(starts.map(item => item.body)).toEqual([
    { scopes: ['messages:read'], setupRevision: draft.revision, directOrigin: 'https://cairn.example.test' },
    { scopes: ['messages:read'], setupRevision: draft.revision },
  ])
  await expect(setup.getByRole('button', { name: 'Save setup', exact: true })).toBeDisabled()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  await expect(first.getByRole('button', { name: 'Edit Engineering', exact: true })).toBeDisabled()
  await first.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(first.getByText('Finish authorizing in your browser.')).toBeVisible()
  const opened = page.context().waitForEvent('page')
  await first.getByRole('link', { name: 'Open authorization page' }).click()
  const provider = await opened
  await expect(provider.getByText('Workspace authorized. Return to Cairn.')).toBeVisible()
  await provider.close()
  await first.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(first.getByText('Connected', { exact: true })).toBeVisible()
  await expect(setup.getByRole('button', { name: 'Refresh resources', exact: true })).toBeEnabled()
  await expect(first.getByRole('button', { name: 'Refresh authorization', exact: true })).toBeEnabled()
  // Settling one consent must not clear another resource's pending state.
  await expect(second.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  await second.getByRole('button', { name: 'Disconnect', exact: true }).click()
  await page.getByRole('dialog', { name: 'Disconnect Workspace authorization?' }).getByRole('button', { name: 'Disconnect', exact: true }).click()
  await expect(second.getByText('Not connected', { exact: true })).toBeVisible()
  expect(writes.find(item => item.method === 'DELETE')?.body).toEqual({ setupRevision: draft.revision })
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeEnabled()
  await setup.getByRole('button', { name: 'Activate instance', exact: true }).click()
  await expect(setup).not.toBeVisible()
  const live = page.getByRole('article', { name: 'Shared service', exact: true }).getByRole('article', { name: 'Engineering', exact: true })
  await expect(live.getByText('Connected', { exact: true })).toBeVisible()
  await live.getByRole('button', { name: 'Disconnect', exact: true }).click()
  await page.getByRole('dialog', { name: 'Disconnect Workspace authorization?' }).getByRole('button', { name: 'Disconnect', exact: true }).click()
  await expect(live.getByText('Not connected', { exact: true })).toBeVisible()
  expect(writes.filter(item => item.method === 'DELETE').at(-1)?.body).toEqual({})
  const resources = await workspace.api(`/api/plugins/owner-ui/connections/${draft.id}/resources/workspace`)
  expect(resources.items.map((item: any) => item.id).sort()).toEqual([engineering.id, sales.id].sort())
  await page.screenshot({ path: test.info().outputPath('owner-resource-authorization.png'), fullPage: true })
})

async function resourceAuthorizationFixture(page: import('@playwright/test').Page, workspace: import('./fixtures').Workspace, id: string, withPages = false) {
  await useRelayForHttpMocks(page)
  const provider = {
    title: 'First authorization',
    collection: 'workspace',
    authorizationUrl: 'https://provider.example.test/authorize',
    tokenUrl: 'https://provider.example.test/token',
    clientId: 'clientId',
    pkce: true,
    scopes: ['messages:read'],
    token: { accessToken: '/access_token', scope: '/scope' },
  }
  const manifest = {
    id,
    name: id,
    version: '1',
    apiVersion: 2,
    description: 'Resource authorization coordination.',
    capabilities: ['resources', 'oauth', 'http'],
    hosts: ['provider.example.test'],
    settings: [{ key: 'clientId', title: 'Client ID', type: 'string' }],
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
    oauth: [{ ...provider, name: 'owner' }, { ...provider, name: 'other', title: 'Second authorization' }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
  const instance = await workspace.api(`/api/plugins/${id}/connections`, 'POST', { name: 'Application', settings: { clientId: 'fixture-client' }, enabled: true })
  const root = `/plugins/${id}/connections/${instance.id}/resources/workspace`
  const engineering = await workspace.api(`/api${root}`, 'POST', { name: 'Engineering' })
  const intro = withPages ? await workspace.api(`/api${root}`, 'POST', { name: 'Intro' }) : undefined
  const sales = withPages ? await workspace.api(`/api${root}`, 'POST', { name: 'Sales' }) : undefined
  const authorized = new Set<string>()
  const starts: string[] = []
  await page.route(`**/plugins/${id}/connections/*/resources/workspace/*/oauth/**`, async (route) => {
    const request = route.request()
    const parts = new URL(request.url()).pathname.split('/resources/workspace/')[1].split('/')
    const [resource, , providerName, finish] = parts
    const key = `${resource}/${providerName}`
    const status = () => authorized.has(key) ? { status: 'authorized', scopes: ['messages:read'], requestedScopes: ['messages:read'] } : { status: 'disconnected' }
    if (request.method() === 'GET')
      return route.fulfill({ json: status() })
    if (finish === 'finish' || request.method() === 'DELETE') {
      if (request.method() === 'DELETE')
        authorized.delete(key)
      else
        authorized.add(key)
      const current = await workspace.api(`/api${root}/${resource}`)
      await workspace.api(`/api${root}/${resource}`, 'PUT', { name: current.name, revision: current.revision, settings: current.settings })
      return route.fulfill({ json: status() })
    }

    starts.push(providerName)
    return route.fulfill({ json: { state: key.replaceAll('-', '').replaceAll('/', '').padEnd(43, 'x'), url: 'https://provider.example.test/authorize', expiresAt: Date.now() + 60000 } })
  })
  return {
    root,
    engineering,
    intro,
    sales,
    starts,
  }
}

test('shared providers on one resource serialize their revision-changing authorization', async ({ page, workspace }) => {
  const id = 'owner-provider-lock'
  const fixture = await resourceAuthorizationFixture(page, workspace, id)
  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  const resource = page.getByRole('article', { name: 'Engineering', exact: true })
  const first = resource.getByRole('region', { name: 'First authorization', exact: true })
  const second = resource.getByRole('region', { name: 'Second authorization', exact: true })
  await first.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await second.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await expect(second.getByRole('button', { name: 'Connect', exact: true })).toBeEnabled()
  await first.getByRole('button', { name: 'Connect', exact: true }).click()
  await expect(first.getByRole('button', { name: 'Check authorization', exact: true })).toBeVisible()
  await expect(second.getByRole('button', { name: 'Connect', exact: true })).toBeDisabled()
  expect(fixture.starts).toEqual(['owner'])
  await first.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(first.getByText('Connected', { exact: true })).toBeVisible()
  await second.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await second.getByRole('button', { name: 'Connect', exact: true }).click()
  await expect(first.getByRole('button', { name: 'Reconnect', exact: true })).toBeDisabled()
  await expect(first.getByRole('button', { name: 'Disconnect', exact: true })).toBeDisabled()
  await second.getByRole('button', { name: 'Disconnect', exact: true }).click()
  await page.getByRole('dialog', { name: 'Disconnect Second authorization?' }).getByRole('button', { name: 'Disconnect', exact: true }).click()
  await expect(second.getByRole('checkbox', { name: 'messages:read', exact: true })).toBeEnabled()
  await expect(second.getByRole('button', { name: 'Connect', exact: true })).toBeVisible()
  await expect(first.getByText('Connected', { exact: true })).toBeVisible()
  expect(fixture.starts).toEqual(['owner', 'other'])
})

test('a late resource page cannot duplicate a newer authorization refresh', async ({ page, workspace }) => {
  const id = 'owner-page-generation'
  const fixture = await resourceAuthorizationFixture(page, workspace, id, true)
  let removed = false
  let entered!: () => void
  let release!: () => void
  const started = new Promise<void>((resolve) => {
    entered = resolve
  })
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(`**/plugins/${id}/connections/*/resources/workspace*`, async (route) => {
    const url = new URL(route.request().url())
    if (route.request().method() !== 'GET' || !url.pathname.endsWith('/resources/workspace'))
      return route.continue()
    if (url.searchParams.has('cursor')) {
      const oldSales = await workspace.api(`/api${fixture.root}/${fixture.sales.id}`)
      entered()
      await held
      return route.fulfill({ json: { items: [oldSales] } })
    }

    const engineering = await workspace.api(`/api${fixture.root}/${fixture.engineering.id}`)
    const sibling = await workspace.api(`/api${fixture.root}/${removed ? fixture.sales.id : fixture.intro.id}`)
    return route.fulfill({ json: { items: [engineering, sibling], nextCursor: removed ? null : 'fixture-cursor' } })
  })
  try {
    await page.goto(workspacePath(`/plugins/${id}`))
    await signIn(page)
    const first = page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('region', { name: 'First authorization', exact: true })
    await first.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
    await first.getByRole('button', { name: 'Connect', exact: true }).click()
    await page.getByRole('button', { name: 'More workspaces', exact: true }).click()
    await started
    await workspace.api(`/api${fixture.root}/${fixture.intro.id}`, 'DELETE', { revision: fixture.intro.revision })
    removed = true
    await first.getByRole('button', { name: 'Check authorization', exact: true }).click()
    await expect(page.getByRole('article', { name: 'Sales', exact: true })).toHaveCount(1)
    release()
    await expect(page.getByRole('button', { name: 'Refresh resources', exact: true })).toBeEnabled()
    await expect(page.getByRole('article', { name: 'Sales', exact: true })).toHaveCount(1)
    await expect(page.getByRole('article', { name: 'Intro', exact: true })).toHaveCount(0)
  }
  finally { release() }
})

async function resourceAccessFixture(workspace: import('./fixtures').Workspace, id: string, scope = 'channel', readOperation = 'read-messages') {
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Resource helper' })
  const manifest = {
    id,
    name: 'Resource access',
    description: 'Explicit resource permissions.',
    version: '1',
    apiVersion: 2,
    capabilities: ['resources'],
    agentAccess: {},
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
    operations: [
      {
        name: readOperation,
        collection: 'workspace',
        profile: 'read',
        scopes: [scope],
      },
      {
        name: 'send-message',
        collection: 'workspace',
        profile: 'contribute',
        scopes: [scope],
      },
      {
        name: 'delete-message',
        collection: 'workspace',
        profile: 'manage',
        scopes: [scope],
      },
      { name: 'list-people', collection: 'workspace', profile: 'read' },
    ],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
  const instance = await workspace.api(`/api/plugins/${id}/connections`, 'POST', { name: 'Application', enabled: true })
  const root = `/plugins/${id}/connections/${instance.id}/resources/workspace`
  const resource = await workspace.api(`/api${root}`, 'POST', { name: 'Engineering' })
  const path = `/api${root}/${resource.id}/grants`
  return { id, agent, path }
}

for (const scope of ['constructor', 'toString']) {
  test(`resource access accepts the declared ${scope} scope without inherited values`, async ({ page, workspace }) => {
    test.setTimeout(90000)
    const operation = scope === 'constructor' ? 'constructor' : 'read-messages'
    const { id, agent, path } = await resourceAccessFixture(workspace, `scope-${scope.toLowerCase()}`, scope, operation)
    await page.goto(workspacePath(`/plugins/${id}`))
    await signIn(page)
    await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Agent access', exact: true }).click()
    const access = page.getByRole('dialog', { name: 'Agent access · Engineering', exact: true })
    await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
    await expect(access.getByLabel(`Limit ${scope}`, { exact: true })).toHaveValue('')
    await expect(access.getByRole('combobox', { name: operation, exact: true })).toHaveJSProperty('selectedIndex', 0)
    await access.getByRole('combobox', { name: 'Agent', exact: true }).selectOption(agent.id)
    await access.getByRole('combobox', { name: 'Profile', exact: true }).selectOption('read')
    await access.getByLabel(`Limit ${scope}`, { exact: true }).fill('C111')
    await access.getByRole('button', { name: 'Grant access', exact: true }).click()
    await expect(access.getByRole('button', { name: 'Add access rule', exact: true })).toBeVisible()
    const rules = (await workspace.api(path)).items
    expect(rules).toHaveLength(1)
    expect(rules[0]).toMatchObject({
      agentId: agent.id,
      profile: 'read',
      operations: [],
      deniedOperations: [],
      scopes: { [scope]: ['C111'] },
    })
    await expect(access.getByText(`${scope}: C111`, { exact: true })).toBeVisible()
    await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
    await expect(access.getByLabel(`Limit ${scope}`, { exact: true })).toHaveValue('')
    await expect(access.getByRole('combobox', { name: operation, exact: true })).toHaveJSProperty('selectedIndex', 0)
  })
}

test('resource access combines explicit profiles, operation exceptions and scope limits', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await resourceAccessFixture(workspace, 'access-ui')
  const draft = await workspace.api(`/api/plugins/${id}/setup-drafts`, 'POST', { name: 'Inactive configuration' })
  await workspace.api(`/api/plugins/${id}/setup-drafts/${draft.id}/resources/workspace`, 'POST', { name: 'Prepared workspace', setupRevision: draft.revision })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'Continue Inactive configuration', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Resource access', exact: true })
  await expect(setup.getByRole('article', { name: 'Prepared workspace', exact: true })).toBeVisible()
  await expect(setup.getByRole('button', { name: 'Agent access', exact: true })).toHaveCount(0)
  await setup.getByRole('button', { name: 'Close dialog', exact: true }).click()
  const card = page.getByRole('article', { name: 'Engineering', exact: true })
  await card.getByRole('button', { name: 'Agent access', exact: true }).click()
  const access = page.getByRole('dialog', { name: 'Agent access · Engineering', exact: true })
  await expect(access.getByText('No access rules yet.', { exact: true })).toBeVisible()
  expect((await workspace.api(path)).items).toEqual([])
  await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
  await expect(access.getByRole('button', { name: 'Grant access', exact: true })).toBeDisabled()
  await expect(access.getByRole('combobox', { name: 'Agent', exact: true })).toHaveValue('')
  await access.getByRole('combobox', { name: 'Agent', exact: true }).selectOption(agent.id)
  await access.getByRole('combobox', { name: 'Profile', exact: true }).selectOption('contribute')
  await access.getByRole('combobox', { name: 'read-messages', exact: true }).selectOption('deny')
  await access.getByRole('combobox', { name: 'delete-message', exact: true }).selectOption('allow')
  await access.getByLabel('Limit channel', { exact: true }).fill('C111\nC222')
  const preview = access.getByRole('list', { name: 'Operations allowed by this rule' })
  await expect(preview).toContainText('send-message')
  await expect(preview).toContainText('delete-message')
  await expect(preview).not.toContainText('read-messages')
  await expect(preview).not.toContainText('list-people')
  await access.getByRole('button', { name: 'Grant access', exact: true }).click()
  await expect(access.getByRole('button', { name: 'Add access rule', exact: true })).toBeVisible()
  let rules = (await workspace.api(path)).items
  expect(rules).toHaveLength(1)
  expect(rules[0]).toMatchObject({
    agentId: agent.id,
    profile: 'contribute',
    operations: ['delete-message'],
    deniedOperations: ['read-messages'],
    scopes: { channel: ['C111', 'C222'] },
  })
  await expect(access.getByText('channel: C111, C222', { exact: true })).toBeVisible()
  await access.getByRole('button', { name: 'Close dialog', exact: true }).click()
  const application = page.getByRole('article', { name: 'Application', exact: true })
  await expect(application.getByText('No agent can use it yet.', { exact: true })).toHaveCount(0)
  await expect(application.getByText('Instance-wide access: no agents.', { exact: true })).toBeVisible()
  await card.getByRole('button', { name: 'Agent access', exact: true }).click()
  await expect(access.getByText('channel: C111, C222', { exact: true })).toBeVisible()

  await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
  await access.getByRole('combobox', { name: 'Agent', exact: true }).selectOption(agent.id)
  await access.getByRole('combobox', { name: 'read-messages', exact: true }).selectOption('allow')
  await access.getByRole('button', { name: 'Grant access', exact: true }).click()
  await expect(access.getByRole('button', { name: 'Revoke rule' })).toHaveCount(2)
  rules = (await workspace.api(path)).items
  expect(rules).toHaveLength(2)
  expect(rules.some((rule: any) => rule.profile === null && rule.operations.join() === 'read-messages' && Object.keys(rule.scopes).length === 0)).toBe(true)
  await page.screenshot({ path: test.info().outputPath('resource-access-rules.png'), fullPage: true })
  for (let left = 1; left >= 0; left--) {
    await access.getByRole('button', { name: 'Revoke rule' }).first().click()
    await access.getByRole('button', { name: 'Confirm revoke', exact: true }).click()
    await expect(access.getByRole('button', { name: 'Revoke rule' })).toHaveCount(left)
    expect((await workspace.api(path)).items).toHaveLength(left)
  }

  await expect(access.getByText('No access rules yet.', { exact: true })).toBeVisible()
})

test('resource access cancels offline reads but keeps an outstanding write locked', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await resourceAccessFixture(workspace, 'access-offline')
  await useRelayForHttpMocks(page)
  let offline = false
  let reads = 0
  let writes = 0
  let releaseWrite!: () => void
  let writeStarted!: () => void
  const writing = new Promise<void>((resolve) => {
    writeStarted = resolve
  })
  const writeGate = new Promise<void>((resolve) => {
    releaseWrite = resolve
  })
  await page.route('**/overview', async (route) => {
    if (offline)
      return route.fulfill({ status: 503, json: { error: 'Installation unavailable' } })
    return route.continue()
  })
  await page.route(`**${path.replace('/api', '')}**`, async (route) => {
    if (route.request().method() === 'GET') {
      reads++
      if (offline)
        return route.fulfill({ status: 503, json: { error: 'Installation unavailable' } })
    }

    if (route.request().method() === 'POST') {
      writes++
      writeStarted()
      await writeGate
      const response = await route.fetch()
      expect(response.ok()).toBe(true)
      offline = true
      return route.fulfill({ response })
    }

    return route.continue()
  })
  try {
    await page.goto(workspacePath(`/plugins/${id}`))
    await signIn(page)
    const card = page.getByRole('article', { name: 'Engineering', exact: true })
    const access = page.getByRole('dialog', { name: 'Agent access · Engineering', exact: true })
    await expect(card.getByRole('button', { name: 'Agent access', exact: true })).toBeEnabled()
    offline = true
    await card.getByRole('button', { name: 'Agent access', exact: true }).click()
    await expect.poll(() => reads).toBe(1)
    await expect(access.getByRole('button', { name: 'Loading rules…', exact: true })).toBeDisabled()
    await access.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await expect(access).toHaveCount(0)
    let recovered = page.waitForResponse(response => response.url().endsWith('/overview') && response.ok())
    offline = false
    await recovered
    await card.getByRole('button', { name: 'Agent access', exact: true }).click()
    await expect(access.getByText('No access rules yet.', { exact: true })).toBeVisible()
    expect(reads).toBe(2)

    await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
    await access.getByRole('combobox', { name: 'Agent', exact: true }).selectOption(agent.id)
    await access.getByRole('combobox', { name: 'Profile', exact: true }).selectOption('read')
    await access.getByRole('button', { name: 'Grant access', exact: true }).click()
    await writing
    await access.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await expect(access).toBeVisible()
    expect(writes).toBe(1)
    releaseWrite()
    await expect.poll(() => reads).toBe(3)
    await access.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await expect(access).toHaveCount(0)
    expect((await workspace.api(path)).items).toHaveLength(1)
    recovered = page.waitForResponse(response => response.url().endsWith('/overview') && response.ok())
    offline = false
    await recovered
    await card.getByRole('button', { name: 'Agent access', exact: true }).click()
    await expect(access.getByRole('button', { name: 'Revoke rule', exact: true })).toHaveCount(1)
    expect(reads).toBe(4)
    expect(writes).toBe(1)
  }
  finally {
    releaseWrite()
    offline = false
  }
})

test('resource access refreshes uncertain writes without automatically repeating them', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await resourceAccessFixture(workspace, 'access-write')
  await useRelayForHttpMocks(page)
  let writes = 0
  await page.route(`**${path.replace('/api', '')}**`, async (route) => {
    if (!['POST', 'DELETE'].includes(route.request().method()))
      return route.continue()
    writes++
    const response = await route.fetch()
    expect(response.ok()).toBe(true)
    await route.abort('failed')
  })
  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Agent access', exact: true }).click()
  const access = page.getByRole('dialog', { name: 'Agent access · Engineering', exact: true })
  await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
  await access.getByRole('combobox', { name: 'Agent', exact: true }).selectOption(agent.id)
  await access.getByRole('combobox', { name: 'Profile', exact: true }).selectOption('read')
  await access.getByRole('button', { name: 'Grant access', exact: true }).click()
  await expect(access.getByRole('alert')).toContainText('Refresh the rules before making another change.')
  expect((await workspace.api(path)).items).toHaveLength(1)
  expect(writes).toBe(1)
  await expect(access.getByRole('button', { name: 'Add access rule', exact: true })).toHaveCount(0)
  await access.getByRole('button', { name: 'Refresh rules', exact: true }).click()
  await expect(access.getByRole('button', { name: 'Revoke rule', exact: true })).toHaveCount(1)
  expect(writes).toBe(1)
  await access.getByRole('button', { name: 'Revoke rule', exact: true }).click()
  await access.getByRole('button', { name: 'Confirm revoke', exact: true }).click()
  await expect(access.getByRole('alert')).toContainText('Refresh the rules before making another change.')
  expect((await workspace.api(path)).items).toHaveLength(0)
  expect(writes).toBe(2)
  await access.getByRole('button', { name: 'Refresh rules', exact: true }).click()
  await expect(access.getByText('No access rules yet.', { exact: true })).toBeVisible()
  expect(writes).toBe(2)
})
