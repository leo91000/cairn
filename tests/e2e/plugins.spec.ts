import type { Locator, Route } from '@playwright/test'
import type { PluginConnection, PluginRoute } from '../../apps/web/src/plugins'
import type { Agent } from '../../packages/contracts/contracts'
import { Buffer } from 'node:buffer'
import { createHmac } from 'node:crypto'
import {
  expect,
  openAgentSection,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

const token = `synthetic_browser_discord_token_${'x'.repeat(48)}`

test.beforeEach(async ({ workspace }) => {
  // Retain the worker's data and sign-in, but do not inherit another scenario's
  // process-local HTTP request budget. Production limits stay unchanged.
  await workspace.restart()
})

async function selectField(scope: Locator, label: string, option: string) {
  await scope.getByRole('combobox', { name: label, exact: true }).fill(option)
  await scope.getByRole('option', { name: option, exact: true }).click()
}

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

test('older hosts keep existing plugin pages without a setup drafts endpoint', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const account = await workspace.api('/api/plugins/onepassword/connections', 'POST', {
    name: 'Earlier host account',
    secrets: { token: 'ops_earlier_host_fixture' },
    enabled: false,
  })
  try {
    let draftReads = 0
    await page.route('**/api/plugins/packages/onepassword', route => route.fulfill({ status: 404, json: { error: 'Not found' } }))
    await page.route('**/api/plugins/packages', route => route.fulfill({ status: 404, json: { error: 'Not found' } }))
    await page.route('**/api/plugins', async (route) => {
      const response = await route.fetch()
      const catalog = await response.json()
      delete catalog.setupDrafts
      delete catalog.resourceGrantCounts
      await route.fulfill({ response, json: catalog })
    })
    await page.route('**/plugins/onepassword/setup-drafts*', async (route) => {
      draftReads++
      await route.fulfill({ status: 404, json: { error: 'Not found' } })
    })
    await page.goto(workspacePath('/plugins/onepassword'))
    await signIn(page)
    const instance = page.getByRole('article', { name: 'Earlier host account', exact: true })
    await expect(instance).toBeVisible()
    await instance.getByRole('button', { name: 'Edit Earlier host account', exact: true }).click()
    const editor = page.getByRole('dialog', { name: 'Edit Earlier host account', exact: true })
    await editor.getByLabel('Name', { exact: true }).fill('Updated on earlier host')
    await editor.getByRole('button', { name: 'Save service account', exact: true }).click()
    await expect(editor).not.toBeVisible()
    await expect(page.getByRole('article', { name: 'Updated on earlier host', exact: true })).toBeVisible()
    expect(draftReads).toBe(0)
    expect((await workspace.api('/api/plugins')).connections.find((item: { id: string }) => item.id === account.id).name).toBe('Updated on earlier host')
  }
  finally {
    await workspace.api(`/api/plugins/onepassword/connections/${account.id}`, 'DELETE')
  }
})

test('closing an editor during its save keeps the original update target and typed secret', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'held',
    name: 'Held package',
    version: '1',
    apiVersion: 2,
    capabilities: [],
    instances: { singular: 'Account', plural: 'Accounts' },
    agentAccess: { description: 'Off by default.' },
    settings: [{
      key: 'token',
      title: 'Credential',
      type: 'secret',
      required: true,
    }],
  }
  const instance = {
    id: 'saved',
    pluginId: 'held',
    name: 'Original account',
    settings: {},
    enabled: false,
    agentIds: ['before'],
    secretKeys: ['token'],
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
  }
  let globalReads = 0
  let writeStarted = false
  let writeFinished = false
  let release!: () => void
  const held = new Promise<void>(resolve => release = resolve)
  const writes: { method: string, path: string, body: any }[] = []
  await page.route('**/api/plugins', (route) => {
    globalReads++
    return route.fulfill({ status: 503, json: { error: 'No global catalogue' } })
  })
  await page.route('**/api/plugins/packages/held', route => route.fulfill({
    json: {
      id: 'held',
      builtin: false,
      status: 'active',
      active: '1',
      manifest,
      setup: {
        steps: [
          {
            id: 'settings',
            kind: 'settings',
            title: 'Settings',
            settings: ['token'],
          },
          { id: 'agents', kind: 'agents', title: 'Agent access' },
        ],
      },
      instanceReads: true,
      accessReads: true,
      setupDrafts: false,
      capabilities: [],
    },
  }))
  await page.route('**/api/plugins/packages/held/connections', route => route.fulfill({ json: { items: [instance] } }))
  await page.route('**/api/plugins/instance-access', route => route.fulfill({ json: { items: [] } }))
  await page.route('**/api/plugins/packages/held/connections/saved', route => route.fulfill({ json: { ...instance, agentIds: ['concurrent'] } }))
  await page.route(/\/api\/plugins\/held\/connections(?:\/saved)?$/, async (route) => {
    writes.push({ method: route.request().method(), path: new URL(route.request().url()).pathname, body: route.request().postDataJSON() })
    writeStarted = true
    await held
    await route.fulfill({ json: {} })
    writeFinished = true
  })
  try {
    await page.goto(workspacePath('/plugins/held'))
    await signIn(page)
    await page.getByRole('button', { name: 'Edit Original account', exact: true }).click()
    const editor = page.getByRole('dialog', { name: 'Edit Original account', exact: true })
    await editor.getByLabel('Name', { exact: true }).fill('Typed name')
    await editor.getByLabel('Credential', { exact: true }).fill('synthetic_changed_credential')
    await editor.getByRole('button', { name: 'Save account', exact: true }).click()
    await expect.poll(() => writeStarted).toBe(true)
    instance.agentIds = ['concurrent']
    await editor.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await expect(editor).not.toBeVisible()
    release()
    await expect.poll(() => writeFinished).toBe(true)
    await expect.poll(() => writes.length).toBe(1)
    expect(writes[0].method).toBe('PUT')
    expect(writes[0].path).toMatch(/\/plugins\/held\/connections\/saved$/)
    expect(writes[0].body).toMatchObject({ name: 'Typed name', secrets: { token: 'synthetic_changed_credential' } })
    expect(writes[0].body).not.toHaveProperty('agentIds')
    expect(globalReads).toBe(0)
  }
  finally { release() }
})

for (const grant of ['root', 'resource'] as const) {
  test(`selected package pages warn about cross-package ${grant} access without full catalogue reads`, async ({ page, workspace }) => {
    const agent = await workspace.api('/api/agents', 'POST', {
      name: `Public helper ${grant}`,
      access: {
        projects: [],
        skills: [],
        mcps: [],
        installation: false,
        sandbox: 'read-only',
      },
    })
    await useRelayForHttpMocks(page)
    await page.route('**/agents', async (route) => {
      if (route.request().method() !== 'GET')
        return route.continue()
      const response = await route.fetch()
      const listing = await response.json()
      // Preview the selected native agent; host admission has its own backend cases.
      return route.fulfill({
        response,
        json: {
          ...listing,
          agents: listing.agents.map((item: Agent) => item.id === agent.id
            ? { ...item, access: { ...item.access, backend: 'native' } }
            : item),
        },
      })
    })
    let globalReads = 0
    await page.route('**/api/plugins', (route) => {
      globalReads++
      return route.fulfill({ status: 503, json: { error: 'Detail must not load the full catalogue' } })
    })
    const manifest = {
      id: 'selected',
      name: 'Selected package',
      version: '1',
      apiVersion: 2,
      capabilities: ['conversations.submit'],
      settings: [],
    }
    await page.route('**/api/plugins/packages/selected', route => route.fulfill({
      json: {
        id: 'selected',
        builtin: false,
        status: 'active',
        active: '1',
        manifest,
        setup: { steps: [{ id: 'people', kind: 'people', title: 'Who can talk to it' }] },
        instanceReads: true,
        accessReads: true,
        setupDrafts: false,
        capabilities: [],
      },
    }))
    const instance = (id: string, name: string) => ({
      id,
      name,
      pluginId: 'selected',
      settings: {},
      enabled: false,
      agentIds: [],
      userIds: [],
      people: [],
      accessMode: 'everyone',
      agentId: agent.id,
      projectId: null,
      maxMessagesPerHour: 20,
      status: { state: 'disconnected' },
      usage: { hour: 0 },
      requests: [],
      activity: [],
    })
    await page.route('**/api/plugins/packages/selected/connections*', route => route.fulfill({
      json: route.request().url().includes('?cursor=')
        ? { items: [instance('two', 'Second instance')], nextCursor: null }
        : { items: [instance('one', 'First instance')], nextCursor: 'one' },
    }))
    await page.route('**/api/plugins/instance-access*', route => route.fulfill({
      json: route.request().url().includes('?cursor=')
        ? {
            items: [{
              id: 'other',
              pluginId: 'elsewhere',
              pluginName: 'Other package',
              name: 'Other workspace',
              enabled: false,
              agentIds: grant === 'root' ? [agent.id] : [],
              resourceAgentIds: grant === 'resource' ? [agent.id] : [],
            }],
            nextCursor: null,
          }
        : { items: [], nextCursor: 'boundary' },
    }))
    try {
      await page.goto(workspacePath('/plugins/selected'))
      await signIn(page)
      const second = page.getByRole('article', { name: 'Second instance', exact: true })
      await expect(second).toBeVisible()
      await expect(page.getByRole('article', { name: 'First instance', exact: true })).toBeVisible()
      await second.getByRole('button', { name: 'Edit Second instance', exact: true }).click()
      const editor = page.getByRole('dialog', { name: 'Edit Second instance', exact: true })
      await expect(editor.getByRole('radio', { name: new RegExp(agent.name) })).toBeEnabled()
      await expect(editor.getByRole('radio', { name: new RegExp(agent.name) })).toBeChecked()
      await expect(editor.getByRole('status').filter({ hasText: 'can use everything' })).toContainText('use its plugins')
      await expect(editor.getByRole('status').filter({ hasText: 'requires the installation owner\'s current native delegation' })).toBeVisible()
      await expect(editor.getByRole('button', { name: 'Save instance', exact: true })).toBeEnabled()
      expect(globalReads).toBe(0)
    }
    finally {
      await workspace.api(`/api/agents/${agent.id}`, 'DELETE')
    }
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
  await expect(page.getByRole('link', { name: 'Discord · Set up' }).getByText('Paste a token')).toBeVisible()
  await page.getByRole('link', { name: 'Discord · Set up' }).click()
  // Before the first bot, the page says what it takes and lists the steps.
  const first = page.getByRole('region', { name: 'Set up your first bot' })
  await expect(first).toContainText('Discord Developer Portal')
  await expect(first.getByRole('list', { name: 'Setup steps' }).getByRole('listitem')).toHaveText([/Bot token/, /Where it answers/, /Who can talk to it/])
  await first.getByRole('button', { name: 'Start setup' }).click()
  const dialog = page.getByRole('dialog', { name: 'New bot' })
  await expect(dialog.getByText('Step 1 of 3')).toBeVisible()
  await expect(dialog.getByText('Create the bot')).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Continue' })).toBeDisabled()
  await expect(dialog.getByRole('button', { name: 'Where it answers' })).toBeDisabled()

  await expect(dialog.getByLabel('Bot token')).toHaveAttribute('type', 'password')
  await dialog.getByLabel('Bot token').fill(token)
  await expect(dialog.getByRole('status').filter({ hasText: 'Signed in as Cairn Bot' })).toBeVisible()
  await expect(dialog.getByRole('link', { name: 'Invite the bot to a server' })).toHaveAttribute('href', /permissions=309237746752/)
  await dialog.getByRole('button', { name: 'Continue' }).click()
  await expect(dialog.getByRole('heading', { name: 'Where it answers' })).toBeVisible()
  // Server and channel are required before the next step.
  await expect(dialog.getByRole('button', { name: 'Continue' })).toBeDisabled()
  await dialog.getByRole('combobox', { name: 'Server' }).selectOption('100')
  await expect(dialog.getByRole('radio', { name: /#leadership/ })).toBeDisabled()
  await expect(dialog.getByText('The bot can’t read this channel')).toBeVisible()
  await dialog.getByRole('radio', { name: /#support/ }).check()
  await expect(dialog.getByRole('checkbox', { name: /Read replies without a mention/ })).not.toBeChecked()
  await dialog.getByRole('button', { name: 'Continue' }).click()

  await expect(dialog.getByRole('heading', { name: 'Who can talk to it' })).toBeVisible()
  await expect(dialog.getByRole('radio', { name: /Approve people as they ask/ })).toBeChecked()
  // Any agent can answer anyone; the owner is warned of what a writing agent allows.
  await dialog.getByRole('radio', { name: /^Anyone/ }).check()
  await dialog.getByRole('radio', { name: /Discord helper/ }).check()
  await expect(dialog.getByText('Anyone in the channel can use everything Discord helper can do')).toBeVisible()
  await expect(dialog.getByText(/they can change files and run commands/)).toBeVisible()
  await dialog.getByRole('radio', { name: /Approve people as they ask/ }).check()
  await expect(dialog.getByText(/can use everything Discord helper can do/)).toBeHidden()
  await dialog.getByLabel('Person’s ID').fill('300')
  await dialog.getByRole('button', { name: 'Add', exact: true }).click()
  await expect(dialog.getByText('300', { exact: true })).toBeVisible()
  await dialog.getByRole('radio', { name: /Release helper/ }).check()
  await expect(dialog.getByText('The people you allow can use everything Release helper can do')).toBeVisible()
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
  // An edit opens every step.
  await editor.getByRole('button', { name: 'Next' }).click()
  await expect(editor.getByRole('radio', { name: /#support/ })).toBeChecked()
  await editor.getByRole('button', { name: 'Who can talk to it' }).click()
  await editor.getByRole('radio', { name: /Only people I add/ }).check()
  await editor.getByLabel('Person’s ID').fill('300')
  await editor.getByRole('button', { name: 'Add', exact: true }).click()
  await editor.getByRole('button', { name: 'Save bot' }).click()
  await expect(bot.getByText('1 person · only listed people')).toBeVisible()
  expect((await workspace.api('/api/plugins')).connections[0].settings.channelName).toBe('support')

  // The agent's page lists the delegation read-only; changing its access offers to reapprove it.
  const botId = (await workspace.api('/api/plugins')).connections[0].id
  await page.goto(workspacePath(`/agents/${helper.id}?section=plugins`))
  const delegation = page.getByRole('region', { name: 'Answers as this agent' })
  await expect(delegation).toContainText('Answers as this agent on #support on Acme')
  await expect(delegation.getByText('Set on the instance', { exact: true })).toBeVisible()
  await openAgentSection(page, 'Skills')
  await page.getByRole('group', { name: 'Skills access' }).getByRole('button', { name: 'All, including future', exact: true }).click()
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review.getByRole('checkbox', { name: /Reapprove the new access for #support on Acme/ })).toBeChecked()
  const saving = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${helper.id}`))
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  expect((await saving).postDataJSON().review.reapproveDelegations).toEqual([botId])
  await expect(review).toHaveCount(0)

  // An earlier change elsewhere already suspended the bot: a later edit never reapproves it by default.
  const reapproved = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === helper.id)
  await workspace.api(`/api/agents/${helper.id}`, 'PUT', { ...reapproved, access: { ...reapproved.access, skills: [] } })
  await page.goto(workspacePath(`/agents/${helper.id}?section=identity`))
  const describe = async (text: string) => {
    await page.getByLabel('Description', { exact: true }).fill(text)
    await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  }

  // Alone, a delegation already suspended does not open the review, and is not reapproved.
  const direct = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${helper.id}`))
  await describe('Answers support questions')
  expect((await direct).postDataJSON().review.reapproveDelegations).toEqual([])
  await expect(page.getByRole('region', { name: 'Unsaved changes' })).toHaveCount(0)
  await expect(review).toHaveCount(0)

  // When the review opens for another reason, here queued work, it is listed unchecked.
  await page.route(`**/api/agents/${helper.id}/preview`, async (route) => {
    const response = await route.fetch()
    const preview = await response.json()
    await route.fulfill({ response, json: { ...preview, runs: { queued: 1, running: 0 } } })
  })
  await describe('Answers support questions on Acme')
  const earlier = review.getByRole('checkbox', { name: /Reapprove the new access for #support on Acme/ })
  await expect(earlier).not.toBeChecked()
  await expect(review).toContainText('Already suspended by an earlier change. Reapprove only after checking its current access.')
  const unchecked = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${helper.id}`))
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  expect((await unchecked).postDataJSON().review.reapproveDelegations).toEqual([])
  await expect(review).toHaveCount(0)
  // Checked deliberately, it is reapproved.
  await describe('Answers support questions in Acme')
  await earlier.check()
  const checked = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${helper.id}`))
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  expect((await checked).postDataJSON().review.reapproveDelegations).toEqual([botId])
  await expect(review).toHaveCount(0)
  await page.goto(workspacePath('/plugins/discord'))

  await bot.getByRole('button', { name: 'Delete #support on Acme' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete bot' }).click()
  await expect(page.getByRole('region', { name: 'Set up your first bot' })).toBeVisible()
})

test('TOML packages wait for review and connect through a generated form', async ({ page, workspace }) => {
  test.setTimeout(90000)
  // Split into a module, loaded with require.
  const source = `local notes = require("./notes")
return {
  start = function(event) cairn.storage.set("started", notes.started) end,
  form = function(event)
    local values = event.values or {}
    local errors = {}
    if not values.team or values.team == "" then errors.team = "Enter a team" end
    local choices = {}
    if values.more and values.extra then choices = {{ value = "room", label = "Room" }} end
    return { errors = errors, options = { destination = cairn.array(choices) } }
  end,
}`
  const module = 'return { started = true }'
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/plugins'))
  await signIn(page)
  await page.getByRole('button', { name: 'Install package' }).click()
  const install = page.getByRole('dialog', { name: 'Install a plugin package' })
  const files = (maxLength: number) => [
    {
      name: 'manifest.toml',
      mimeType: 'text/plain',
      buffer: Buffer.from(`
id = "notes"
name = "Team notes"
version = "1.0.0"
description = "Keeps a note for each team."
apiVersion = 2
capabilities = ["storage", "timers"]
instanceForm = true
[[settings]]
key = "team"
title = "Team"
type = "string"
required = true
maxLength = ${maxLength}
[[settings]]
key = "more"
title = "More destinations"
type = "boolean"
refresh = true
[[settings]]
key = "extra"
title = "Extra destinations"
type = "boolean"
refresh = true
[[settings]]
key = "destination"
title = "Destination"
type = "select"
dynamic = true
[[setup.steps]]
id = "settings"
title = "Settings"
settings = ["team", "more", "extra", "destination"]
[[setup.steps.presets]]
id = "basic"
title = "Basic"
[[setup.steps.presets]]
id = "more"
title = "More places"
values = { more = true, extra = true }
`),
    },
    { name: 'main.luau', mimeType: 'text/plain', buffer: Buffer.from(source) },
    { name: 'notes.luau', mimeType: 'text/plain', buffer: Buffer.from(module) },
  ]
  // Admission names the limit rather than accepting a declaration it would cap on save.
  await install.getByLabel('Package files').setInputFiles(files(10001))
  await install.getByRole('button', { name: 'Install', exact: true }).click()
  await expect(install.getByRole('alert')).toHaveText('Plugin setting maxLength must be an integer from 1 to 10000 (text limit).')
  await install.getByLabel('Package files').setInputFiles(files(10000))
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
  await expect(form.getByText('Enter a team', { exact: true })).toBeVisible()
  await form.getByLabel('Team').fill('core')
  // Non-refresh fields must not remain blocked by errors for their previous values.
  await expect(form.getByRole('button', { name: 'Save instance' })).toBeEnabled()
  await expect(form.getByText('Enter a team', { exact: true })).toHaveCount(0)
  const refreshed: string[] = []
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/plugins/notes/form')) {
      const changed = request.postDataJSON().changed
      if (changed)
        refreshed.push(changed)
    }
  })
  await form.getByRole('radio', { name: 'More places' }).check()
  await expect.poll(() => refreshed).toEqual(['more', 'extra'])
  await expect(form.getByRole('option', { name: 'Room' })).toHaveCount(1)
  await form.getByRole('combobox', { name: /^Destination/ }).selectOption('room')
  await form.getByRole('button', { name: 'Save instance' }).click()
  const connection = page.getByRole('article', { name: 'Team notes' })
  await expect(connection.getByText('core', { exact: true })).toBeVisible()
  await expect(connection.getByRole('status')).toHaveText('Online', { timeout: 20000 })

  const catalog = await workspace.api('/api/plugins')
  expect(catalog.packages.find((item: { id: string }) => item.id === 'notes')).toMatchObject({ status: 'active', active: '1.0.0' })
  expect(catalog.connections[0]).toMatchObject({
    pluginId: 'notes',
    settings: {
      team: 'core',
      more: true,
      extra: true,
      destination: 'room',
    },
    agentId: '',
  })

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
  await expect(editor).not.toBeVisible()
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

test('setup forms and activation follow manager preparation operations', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const id = 'cold-setup-ui'
  const manifest = {
    id,
    name: 'Prepared service',
    version: '1',
    apiVersion: 2,
    capabilities: ['resources'],
    instanceForm: true,
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {form = function() return {} end}' })
  await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
  const draft = await workspace.api(`/api/plugins/${id}/setup-drafts`, 'POST', { name: 'Prepared app' })
  const root = `/plugins/${id}/setup-drafts/${draft.id}`
  const results = new Map<string, unknown>()
  let formPolls = 0
  let formRequests = 0
  let activation: unknown
  let release!: () => void
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  let started!: () => void
  const entered = new Promise<void>((resolve) => {
    started = resolve
  })
  await page.route(`**${root}/form`, async (route) => {
    const response = await route.fetch()
    expect(response.ok()).toBe(true)
    const ticket = `setup-form-${++formRequests}`
    results.set(ticket, await response.json())
    await route.fulfill({ json: { operation: ticket } })
  })
  await page.route(`**${root}/activate`, async (route) => {
    activation = route.request().postDataJSON()
    await route.fulfill({ json: { operation: 'setup-activate' } })
  })
  await page.route('**/plugins/packages/operations/setup-*', async (route) => {
    const ticket = new URL(route.request().url()).pathname.split('/').at(-1)!
    if (ticket === 'setup-activate') {
      started()
      await gate
      const result = await workspace.api(`/api${root}/activate`, 'POST', activation)
      await route.fulfill({ json: { state: 'completed', result } })
    }
    else {
      formPolls++
      expect(results.has(ticket)).toBe(true)
      await route.fulfill({ json: { state: 'completed', result: results.get(ticket) } })
    }
  })
  try {
    await page.goto(workspacePath(`/plugins/${id}`))
    await signIn(page)
    await page.getByRole('button', { name: 'Continue Prepared app', exact: true }).click()
    const setup = page.getByRole('dialog', { name: 'Set up Prepared service', exact: true })
    await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeEnabled()
    expect(formPolls).toBeGreaterThan(0)
    await setup.getByRole('button', { name: 'Activate instance', exact: true }).click()
    await entered
    await expect(setup).toBeVisible()
    await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
    expect((await workspace.api('/api/plugins')).connections.filter((item: any) => item.pluginId === id)).toEqual([])
    release()
    await expect(setup).not.toBeVisible()
    expect((await workspace.api('/api/plugins')).connections.find((item: any) => item.pluginId === id).id).toBe(draft.id)
  }
  finally {
    release()
  }
})

test('setup agent and resource parent selection preserve explicit authority', async ({ page, workspace }) => {
  const agent = await workspace.api('/api/agents', 'POST', {
    name: 'Dedicated selector helper',
    access: {
      projects: [],
      skills: [],
      mcps: [],
      installation: false,
    },
  })
  const installed = await workspace.api('/api/plugins/packages', 'POST', {
    manifest: {
      id: 'selector-ui',
      name: 'Selector service',
      version: '1',
      apiVersion: 2,
      description: 'Explicit owner selections.',
      capabilities: ['resources', 'conversations.submit'],
      agentAccess: {},
      resources: [
        { name: 'workspace', title: 'Workspaces', singular: 'Workspace' },
        {
          name: 'channel',
          title: 'Channels',
          singular: 'Channel',
          parent: 'workspace',
        },
      ],
    },
    source: 'return {}',
  })
  await workspace.api('/api/plugins/packages/selector-ui/approve', 'POST', { version: '1', digest: installed.digest })
  const draft = await workspace.api('/api/plugins/selector-ui/setup-drafts', 'POST', { name: 'Prepared configuration' })
  const root = `/api/plugins/selector-ui/setup-drafts/${draft.id}/resources`
  const engineering = await workspace.api(`${root}/workspace`, 'POST', { name: 'Engineering', setupRevision: draft.revision })
  await workspace.api(`${root}/workspace`, 'POST', { name: 'Sales', setupRevision: draft.revision })
  await page.goto(workspacePath('/plugins/selector-ui'))
  await signIn(page)
  await page.getByRole('button', { name: 'Continue Prepared configuration', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Selector service', exact: true })
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  await expect(setup.getByRole('combobox', { name: 'Agent for incoming conversations', exact: true })).toHaveValue('')
  await setup.getByRole('button', { name: 'Add channel', exact: true }).click()
  const channel = setup.getByRole('form', { name: 'New channel', exact: true })
  await channel.getByLabel('Name', { exact: true }).fill('Support')
  await channel.getByRole('button', { name: 'Save channel', exact: true }).click()
  await expect(channel.getByText('Choose parent to continue.', { exact: true })).toBeVisible()
  expect((await workspace.api(`${root}/channel`)).items).toEqual([])
  await selectField(channel, 'Parent', 'Engineering')
  await channel.getByRole('button', { name: 'Save channel', exact: true }).click()
  await expect(setup.getByRole('article', { name: 'Support', exact: true })).toBeVisible()
  expect((await workspace.api(`${root}/channel`)).items[0].parentId).toBe(engineering.id)
  await selectField(setup, 'Agent for incoming conversations', agent.name)
  await setup.getByRole('checkbox', { name: 'Start immediately', exact: true }).uncheck()
  await setup.getByRole('button', { name: 'Activate instance', exact: true }).click()
  await expect(setup).not.toBeVisible()
  const active = (await workspace.api('/api/plugins')).connections.find((item: any) => item.id === draft.id)
  expect(active.agentId).toBe(agent.id)
  expect(active.agentIds).toEqual([])
  expect(active.enabled).toBe(false)
})

test('root setup signs in with its saved revision and can cancel before activation', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'root-consent-ui',
    name: 'Root service',
    version: '1',
    apiVersion: 2,
    capabilities: ['http', 'resources'],
    hosts: ['provider.example.test'],
    credential: {
      label: 'Service account',
      header: 'authorization',
      prefix: 'Bearer ',
      oauth: {
        authorizationUrl: 'https://provider.example.test/authorize',
        tokenUrl: 'https://provider.example.test/token',
        clientIdSetting: 'clientId',
        scopes: ['app:read'],
      },
    },
    settings: [{
      key: 'clientId',
      title: 'Client ID',
      type: 'string',
      required: true,
    }],
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api(`/api/plugins/packages/${manifest.id}/approve`, 'POST', { version: '1', digest: installed.digest })
  await page.goto(workspacePath(`/plugins/${manifest.id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'Add instance', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Root service' })
  const connect = setup.getByRole('button', { name: 'Sign in with Root service', exact: true })
  await expect(connect).toBeVisible()
  await expect(setup.getByLabel('Service account', { exact: true })).toHaveCount(0)
  await setup.getByLabel('Client ID', { exact: true }).fill('draft-client-ui')
  await expect(connect).toBeDisabled()
  await setup.getByRole('button', { name: 'Save setup', exact: true }).click()
  await expect(setup.getByText('All changes saved', { exact: true })).toBeVisible()
  await expect(connect).toBeEnabled()
  const drafts = await workspace.api(`/api/plugins/${manifest.id}/setup-drafts`)
  const draft = drafts.items[0]
  const start = page.waitForRequest(request => request.method() === 'POST' && request.url().includes(`/setup-drafts/${draft.id}/oauth`))
  await connect.click()
  expect((await start).postDataJSON()).toEqual({ revision: draft.revision })
  const link = setup.getByRole('link', { name: 'Open sign-in page', exact: true })
  await expect(link).toBeVisible()
  const url = new URL((await link.getAttribute('href'))!)
  expect(url.origin).toBe('https://provider.example.test')
  expect(url.searchParams.get('client_id')).toBe('draft-client-ui')
  expect(url.searchParams.get('state')).toBeTruthy()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  await setup.getByRole('button', { name: 'Cancel sign-in', exact: true }).click()
  await expect(link).toHaveCount(0)
  await expect(connect).toBeEnabled()
  const saved = await workspace.api(`/api/plugins/${manifest.id}/setup-drafts/${draft.id}`)
  expect(saved.credential).toBe(false)
  const catalog = await workspace.api('/api/plugins')
  expect(catalog.connections.filter((item: any) => item.pluginId === manifest.id)).toEqual([])
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

test('personal account refresh preserves later-page sign-ins and their original consent', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const root = '/plugins/personal-pages/connections/app/resources/workspace'
  const connected = new Set<string>()
  const starts: string[] = []
  const finishes: string[] = []
  let shifted = false

  function item(id: string) {
    return {
      id,
      pluginId: 'personal-pages',
      pluginName: 'Personal service',
      instanceId: 'app',
      instanceName: 'Company service',
      collection: 'workspace',
      resourceId: id,
      resourceName: id,
      providerName: 'personal',
      title: 'Personal access',
      scopes: ['messages:read'],
      enabled: true,
      authorization: connected.has(id)
        ? { status: 'authorized', scopes: ['messages:read'], subject: `U-${id}` }
        : { status: 'disconnected' },
    }
  }

  await page.route('**/plugins/personal-authorizations*', async (route) => {
    const cursor = new URL(route.request().url()).searchParams.get('cursor')
    const ids = shifted ? ['Intro', 'Engineering', 'Sales'] : ['Engineering', 'Sales']
    const index = cursor ? ids.indexOf(cursor) + 1 : 0
    const id = ids[index]
    await route.fulfill({
      json: {
        items: id ? [item(id)] : [],
        nextCursor: index + 1 < ids.length ? id : null,
      },
    })
  })
  await page.route(`**${root}/*/oauth/personal{,/finish}`, async (route) => {
    const path = new URL(route.request().url()).pathname
    const id = path.split('/workspace/')[1].split('/')[0]
    if (path.endsWith('/finish')) {
      expect(route.request().postDataJSON()).toEqual({ state: `consent-${id}` })
      finishes.push(id)
      connected.add(id)
      await route.fulfill({ json: item(id).authorization })
    }
    else if (route.request().method() === 'POST') {
      expect(route.request().postDataJSON()).toEqual({ scopes: ['messages:read'] })
      starts.push(id)
      await route.fulfill({
        json: {
          state: `consent-${id}`,
          url: `https://provider.example.test/authorize?resource=${id}`,
          expiresAt: Date.now() + 600000,
        },
      })
    }
    else {
      connected.delete(id)
      await route.fulfill({ json: item(id).authorization })
    }
  })
  await page.goto(workspacePath('/personal-connections'))
  await signIn(page)
  const first = page.getByRole('article', { name: 'Engineering · Personal access', exact: true })
  const second = page.getByRole('article', { name: 'Sales · Personal access', exact: true })
  await first.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await first.getByRole('button', { name: 'Connect', exact: true }).click()
  await page.getByRole('button', { name: 'More personal connections', exact: true }).click()
  await second.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await second.getByRole('button', { name: 'Connect', exact: true }).click()
  const link = second.getByRole('link', { name: 'Open authorization page', exact: true })
  await expect(link).toHaveAttribute('href', 'https://provider.example.test/authorize?resource=Sales')
  await first.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled()
  await expect(first.getByText('Connected as U-Engineering', { exact: true })).toBeVisible()
  await expect(link).toHaveAttribute('href', 'https://provider.example.test/authorize?resource=Sales')
  // Inserting a first-page resource must also retain the already-open consent.
  shifted = true
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await expect(page.getByRole('article', { name: 'Intro · Personal access', exact: true })).toBeVisible()
  await expect(link).toHaveAttribute('href', 'https://provider.example.test/authorize?resource=Sales')
  await second.getByRole('button', { name: 'Check authorization', exact: true }).click()
  await expect(second.getByText('Connected as U-Sales', { exact: true })).toBeVisible()
  expect(starts).toEqual(['Engineering', 'Sales'])
  expect(finishes).toEqual(['Engineering', 'Sales'])
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
  await first.getByRole('button', { name: 'Manage authorization', exact: true }).click()
  await expect(first.getByRole('button', { name: 'Connect', exact: true })).toBeDisabled()
  await expect(setup.getByText('Personal access', { exact: true })).not.toBeVisible()
  await first.getByRole('checkbox', { name: 'messages:read', exact: true }).check()
  await first.getByLabel('Direct HTTPS origin (optional)').fill('https://cairn.example.test')
  await first.getByRole('button', { name: 'Connect', exact: true }).click()
  await expect(first.getByRole('link', { name: 'Open authorization page' })).toBeVisible()
  await expect(first.getByText('Callback URL: https://cairn.example.test/callback', { exact: true })).toBeVisible()
  await setup.getByRole('button', { name: 'More workspaces', exact: true }).click()
  await second.getByRole('button', { name: 'Manage authorization', exact: true }).click()
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
  await live.getByRole('button', { name: 'Manage authorization', exact: true }).click()
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
  await first.getByRole('button', { name: 'Manage authorization', exact: true }).click()
  await second.getByRole('button', { name: 'Manage authorization', exact: true }).click()
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
    await first.getByRole('button', { name: 'Manage authorization', exact: true }).click()
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
  const agent = await workspace.api('/api/agents', 'POST', { name: `Resource helper ${id}` })
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
  return {
    id,
    agent,
    path,
    instance,
  }
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
    await expect(access.getByRole('combobox', { name: operation, exact: true })).toHaveValue('Use profile')
    await selectField(access, 'Agent', agent.name)
    await selectField(access, 'Profile', 'Read')
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
    await expect(access.getByRole('combobox', { name: operation, exact: true })).toHaveValue('Use profile')
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
  await expect(access.getByRole('combobox', { name: 'Profile', exact: true })).toHaveValue('Only selected operations')
  await selectField(access, 'Agent', agent.name)
  await selectField(access, 'Profile', 'Only selected operations')
  await expect(access.getByRole('button', { name: 'Grant access', exact: true })).toBeDisabled()
  await selectField(access, 'Profile', 'Contribution')
  await selectField(access, 'read-messages', 'Deny in this rule')
  await selectField(access, 'delete-message', 'Allow explicitly')
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
  await expect(application).toContainText('Instance-wide access: no agents.')
  await card.getByRole('button', { name: 'Agent access', exact: true }).click()
  await expect(access.getByText('channel: C111, C222', { exact: true })).toBeVisible()

  await access.getByRole('button', { name: 'Add access rule', exact: true }).click()
  await selectField(access, 'Agent', agent.name)
  await selectField(access, 'read-messages', 'Allow explicitly')
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
    await selectField(access, 'Agent', agent.name)
    await selectField(access, 'Profile', 'Read')
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
  await selectField(access, 'Agent', agent.name)
  await selectField(access, 'Profile', 'Read')
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

test('stored versions that fail admission explain why and remain removable', async ({ page }) => {
  await useRelayForHttpMocks(page)
  // This case exercises an earlier installation's complete catalogue contract.
  await page.route('**/api/plugins/packages', route => route.fulfill({ status: 404, json: { error: 'Not found.' } }))
  const error = 'Plugin setting maxLength must be an integer from 1 to 10000 (text limit).'
  await page.route('**/api/plugins', route => route.fulfill({
    json: {
      packages: [{
        id: 'legacy',
        builtin: false,
        status: 'invalid',
        active: null,
        manifest: null,
        versions: [{ version: '1.0.0', error }],
      }],
      capabilities: [],
      connections: [],
    },
  }))
  await page.goto(workspacePath('/plugins'))
  await signIn(page)
  const card = page.getByRole('button', { name: 'legacy · Review' })
  await expect(card).toContainText('Invalid')
  await expect(card).toContainText('Check the package files')
  await card.click()
  const review = page.getByRole('dialog', { name: 'Review legacy' })
  await expect(review.getByText(`Version 1.0.0 can’t be used: ${error}`)).toBeVisible()
  await expect(review.getByRole('button', { name: 'Uninstall' })).toBeVisible()
  await expect(review.getByRole('button', { name: 'Approve and activate' })).toHaveCount(0)
  await review.getByRole('button', { name: 'Close', exact: true }).click()

  await page.goto(workspacePath('/plugins/legacy'))
  const notice = page.getByRole('status').filter({ hasText: 'No version of this plugin can be used.' })
  await expect(notice).toBeVisible()
  await notice.getByRole('button', { name: 'Review' }).click()
  await expect(page.getByRole('dialog', { name: 'Review legacy' }).getByText(`Version 1.0.0 can’t be used: ${error}`)).toBeVisible()
})

test('compact catalogue pages and exact version review ignore late selection reads', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const manifest = (version: string, name: string, capabilities: string[] = []) => ({
    id: 'review',
    name,
    description: 'Exact review fixture',
    version,
    apiVersion: 2,
    capabilities,
  })
  const presentation = {
    ...manifest('3', 'Review package'),
    needsWorker: false,
    delegates: false,
    agentAccess: false,
    contributions: {
      commands: 0,
      tools: 0,
      instructions: false,
      view: false,
      test: false,
      form: false,
      collections: 0,
    },
  }
  const card = {
    id: 'review',
    builtin: false,
    active: null,
    status: 'needs_approval',
    presentation,
    versionCount: 3,
    summary: {
      instances: 0,
      states: {},
      pendingRequests: 0,
      authorizedAgents: 0,
    },
  }
  let globalReads = 0
  await page.route('**/api/plugins', async (route) => {
    globalReads++
    await route.fulfill({ status: 500, json: { error: 'Complete catalogue must not be used.' } })
  })
  const root = '**/api/plugins/packages'
  await page.route(root, route => route.fulfill({
    json: {
      packageReads: true,
      instanceReads: true,
      capabilities: [],
      items: [],
      nextCursor: 'next',
    },
  }))
  await page.route(`${root}?cursor=next`, route => route.fulfill({ json: { items: [card], nextCursor: null } }))
  await page.route(`${root}/review`, route => route.fulfill({
    json: {
      id: 'review',
      builtin: false,
      active: null,
      status: 'needs_approval',
      manifest: manifest('3', 'Review package'),
      versionCount: 3,
    },
  }))
  await page.route(`${root}/review/versions`, route => route.fulfill({
    json: {
      items: [{ version: '1', digest: 'old', approved: true }],
      nextCursor: '1',
    },
  }))
  await page.route(`${root}/review/versions?cursor=1`, route => route.fulfill({
    json: {
      items: [{ version: '2', digest: 'metadata-two', approved: false }, { version: '3', digest: 'metadata-three', approved: false }],
    },
  }))
  let releaseLate!: () => void
  const late = new Promise<void>(resolve => releaseLate = resolve)
  let startedLate!: () => void
  const started = new Promise<void>(resolve => startedLate = resolve)
  await page.route(`${root}/review/versions/3`, async (route) => {
    startedLate()
    await late
    await route.fulfill({
      json: {
        version: '3',
        digest: 'late-three',
        approved: false,
        manifest: manifest('3', 'Late version three'),
      },
    }).catch(() => {})
  })
  await page.route(`${root}/review/versions/2`, route => route.fulfill({
    json: {
      version: '2',
      digest: 'reviewed-two',
      approved: false,
      manifest: manifest('2', 'Inspected version two', ['http']),
      files: [{ path: 'lib/review.luau', bytes: 100 }],
    },
  }))
  await page.route(`${root}/review/versions/1`, route => route.fulfill({
    json: {
      version: '1',
      digest: 'old',
      approved: true,
      manifest: manifest('1', 'Review package'),
    },
  }))
  const writes: unknown[] = []
  await page.route(`${root}/review/approve`, async (route) => {
    writes.push(route.request().postDataJSON())
    await route.fulfill({ json: {} })
  })
  try {
    await page.goto(workspacePath('/plugins'))
    await signIn(page)
    await page.getByRole('button', { name: 'Review package · Review' }).click()
    const review = page.getByRole('dialog', { name: 'Review Review package' })
    await started
    await expect(review.getByRole('button', { name: 'Approve and activate' })).toHaveCount(0)
    await review.getByRole('radio', { name: '2 Needs approval' }).check()
    await expect(review.getByText('Inspected version two', { exact: false })).toBeVisible()
    releaseLate()
    await expect(review.getByText('Late version three', { exact: false })).toHaveCount(0)
    await expect(review.getByText('Web requests', { exact: true })).toBeVisible()
    await expect(review.getByLabel('Package files')).toContainText('lib/review.luau')
    await review.getByRole('button', { name: 'Approve and activate' }).click()
    await expect(review).toHaveCount(0)
    expect(writes).toEqual([{ version: '2', digest: 'reviewed-two' }])
    expect(globalReads).toBe(0)
  }
  finally {
    releaseLate()
  }
})

test('program releases can be checked, installed and restored independently of plugin accounts', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const release = (version: string) => ({
    version,
    url: `https://cache.agilebits.com/${version}.zip`,
    format: 'zip',
    executable: 'op',
  })
  const installed = (version: string) => ({ release: release(version), archiveSha256: 'a'.repeat(64), binarySha256: 'b'.repeat(64) })
  const program = {
    name: 'op',
    installer: 'programs/op.luau',
    installed: installed('2.39.0'),
    previous: null as ReturnType<typeof installed> | null,
    candidate: null as ReturnType<typeof release> | null,
    digest: null as string | null,
  }
  const actions: string[] = []
  let polls = 0
  await page.route('**/api/plugins/packages/operations/program-job', route => route.fulfill({
    json: ++polls % 2 ? { state: 'running' } : { state: 'completed', result: program },
  }))
  await page.route('**/api/plugins/onepassword/programs**', async (route) => {
    const action = new URL(route.request().url()).pathname.split('/').at(-1)
    if (route.request().method() === 'POST') {
      actions.push(action!)
      if (action === 'check') {
        program.candidate = release('2.41.0')
        program.digest = 'reviewed-release'
      }
      else if (action === 'install') {
        expect(route.request().postDataJSON()).toEqual({ digest: 'reviewed-release' })
        program.previous = program.installed
        program.installed = installed('2.41.0')
        program.candidate = null
        program.digest = null
      }
      else if (action === 'rollback') {
        program.installed = program.previous!
        program.previous = null
      }

      await route.fulfill({ json: { operation: 'program-job' } })
    }
    else {
      await route.fulfill({ json: { programs: [program] } })
    }
  })
  await page.goto(workspacePath('/plugins/onepassword'))
  await signIn(page)
  const programs = page.getByRole('region', { name: 'Installed programs' })
  await expect(programs.getByText('op · Version 2.39.0')).toBeVisible()
  await programs.getByRole('button', { name: 'Check op for updates' }).click()
  await expect(programs.getByText(/HTTPS provenance/)).toBeVisible()
  await programs.getByRole('button', { name: 'Install op 2.41.0' }).click()
  await expect(programs.getByText('op · Version 2.41.0')).toBeVisible()
  await programs.getByRole('button', { name: 'Restore op 2.39.0' }).click()
  await expect(programs.getByText('op · Version 2.39.0')).toBeVisible()
  expect(actions).toEqual(['check', 'install', 'rollback'])
  expect(polls).toBe(6)

  // First use and updates from another client also change the authoritative lock.
  program.installed = installed('2.42.0')
  await expect(programs.getByText('op · Version 2.42.0')).toBeVisible({ timeout: 12000 })
  expect(actions).toEqual(['check', 'install', 'rollback'])
})

async function conversationRoutesFixture(workspace: import('./fixtures').Workspace, id: string) {
  const agent = await workspace.api('/api/agents', 'POST', {
    name: `Route helper ${id}`,
    access: {
      projects: [],
      installation: false,
      mcps: [],
      sandbox: 'read-only',
    },
  })
  const installed = await workspace.api('/api/plugins/packages', 'POST', {
    manifest: {
      id,
      name: 'Conversation routes',
      description: 'Native resource delegation.',
      version: '1',
      apiVersion: 2,
      capabilities: ['resources', 'conversations.submit', 'webhooks', 'inbound'],
      settings: [{
        key: 'signature',
        title: 'Signature',
        type: 'secret',
        required: true,
      }],
      resources: [{
        name: 'workspace',
        title: 'Workspaces',
        singular: 'Workspace',
        delegation: true,
      }],
      webhooks: [{
        name: 'events',
        signature: {
          secret: 'signature',
          header: 'x-signature',
          prefix: 'v0=',
          template: 'v0:{{timestamp}}:{{body}}',
          timestamp: { header: 'x-timestamp', maxAgeSeconds: 300 },
        },
      }],
    },
    source: `return {webhook = function(event)
      local result = cairn.inbound.deliver(cairn.json.decode(event.body))
      return {status = 200, body = cairn.json.encode(result)}
    end}`,
  })
  await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
  const instance = await workspace.api(`/api/plugins/${id}/connections`, 'POST', {
    name: 'Application',
    enabled: true,
    secrets: { signature: 'synthetic-browser-route-signature' },
  })
  const root = `/api/plugins/${id}/connections/${instance.id}/resources/workspace`
  const resource = await workspace.api(root, 'POST', { name: 'Engineering' })
  return {
    id,
    agent,
    instance,
    resource,
    path: `${root}/${resource.id}/routes`,
  }
}

test('a resource-routed app can save its root details without a root answering agent', async ({ page, workspace }) => {
  const fixture = await conversationRoutesFixture(workspace, 'routed-root-edit')
  await page.goto(workspacePath(`/plugins/${fixture.id}`))
  await signIn(page)
  const app = page.getByRole('article', { name: 'Application', exact: true })
  await app.getByRole('button', { name: 'Edit Application', exact: true }).click()
  const editor = page.getByRole('dialog', { name: 'Edit Application', exact: true })
  await editor.getByLabel('Name', { exact: true }).fill('Renamed application')
  await expect(editor.getByRole('button', { name: 'Save instance', exact: true })).toBeEnabled()
  await editor.getByRole('button', { name: 'Save instance', exact: true }).click()
  await expect(editor).not.toBeVisible()
  const saved = await workspace.api(`/api/plugins/packages/${fixture.id}/connections/${fixture.instance.id}`)
  expect(saved.name).toBe('Renamed application')
  expect(saved.agentId).toBe('')
  expect((await workspace.api(fixture.path)).items).toEqual([])
})

for (const kind of ['routes', 'grants'] as const) {
  test(`resource ${kind} load every page before editing and discard a closed read`, async ({ page, workspace }) => {
    const fixture = kind === 'routes'
      ? await conversationRoutesFixture(workspace, 'routes-pages')
      : await resourceAccessFixture(workspace, 'grants-pages')
    const records = []
    for (const name of ['First', 'Last']) {
      records.push(await workspace.api(fixture.path, 'POST', kind === 'routes'
        ? {
            name,
            enabled: true,
            match: { channel: name },
            agentId: fixture.agent.id,
            userIds: ['alice'],
          }
        : { agentId: fixture.agent.id, profile: 'read', scopes: { channel: [name] } }))
    }

    records.sort((a, b) => a.id.localeCompare(b.id))
    const last = records[1]
    await useRelayForHttpMocks(page)
    let release!: () => void
    let started!: () => void
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    const secondPage = new Promise<void>((resolve) => {
      started = resolve
    })
    let hold = true
    await page.route(`**${fixture.path.replace('/api', '')}**`, async (route) => {
      const request = route.request()
      if (request.method() !== 'GET' || !new URL(request.url()).pathname.endsWith(`/${kind}`))
        return route.continue()
      const cursor = new URL(request.url()).searchParams.get('cursor')
      const response = await route.fetch()
      const saved = (await response.json()).items.sort((a: { id: string }, b: { id: string }) => a.id.localeCompare(b.id)).filter((item: { id: string }) => !cursor || item.id > cursor)
      if (cursor && hold) {
        started()
        await gate
      }

      if (request.failure())
        return
      await route.fulfill({ response, json: { items: saved.slice(0, 1), nextCursor: saved.length > 1 ? saved[0].id : null } })
    })
    try {
      await page.goto(workspacePath(`/plugins/${fixture.id}`))
      await signIn(page)
      const resource = page.getByRole('article', { name: 'Engineering', exact: true })
      const action = kind === 'routes' ? 'Conversation routes' : 'Agent access'
      const dialog = page.getByRole('dialog', { name: `${action} · Engineering`, exact: true })
      await resource.getByRole('button', { name: action, exact: true }).click()
      await secondPage
      await expect(dialog.getByRole('button', { name: /Loading (routes|rules)…/ })).toBeDisabled()
      await dialog.getByRole('button', { name: 'Close dialog', exact: true }).click()
      hold = false
      release()
      await expect(dialog).toHaveCount(0)
      await resource.getByRole('button', { name: action, exact: true }).click()
      const article = kind === 'routes'
        ? dialog.getByRole('article', { name: last.name, exact: true })
        : dialog.getByRole('article', { name: 'Access rule', exact: true }).filter({ hasText: `channel: ${last.scopes.channel[0]}` })
      await expect(article).toBeVisible()
      await article.getByRole('button', { name: kind === 'routes' ? 'Delete route' : 'Revoke rule', exact: true }).click()
      await article.getByRole('button', { name: kind === 'routes' ? 'Confirm delete' : 'Confirm revoke', exact: true }).click()
      await expect(article).toHaveCount(0)
      await expect(dialog.getByRole('button', { name: kind === 'routes' ? 'Add route' : 'Add access rule', exact: true })).toBeEnabled()
      expect((await workspace.api(fixture.path)).items.map((item: { id: string }) => item.id)).not.toContain(last.id)
    }
    finally {
      hold = false
      release()
    }
  })
}

test('resource routes preserve exact conditions, require explicit grants and edit paused overrides', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await conversationRoutesFixture(workspace, 'routes-ui')
  const broad = await workspace.api('/api/agents', 'POST', { name: 'Broad route helper' })
  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  const resource = page.getByRole('article', { name: 'Engineering', exact: true })
  await resource.getByRole('button', { name: 'Conversation routes', exact: true }).click()
  const routes = page.getByRole('dialog', { name: 'Conversation routes · Engineering', exact: true })
  await expect(routes.getByText('No routes yet. No one can start a conversation through this resource.', { exact: true })).toBeVisible()
  await routes.getByRole('button', { name: 'Add route', exact: true }).click()
  await routes.getByLabel('Route name', { exact: true }).fill('Support')
  await expect(routes.getByRole('combobox', { name: 'Agent', exact: true })).toHaveValue('')
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeDisabled()
  await expect(routes.getByRole('combobox', { name: 'Agent', exact: true })).toBeEnabled()
  await expect(routes.getByRole('combobox', { name: 'Project', exact: true })).toHaveValue('No project')
  await selectField(routes, 'Project', 'No project')
  const agentSelect = routes.getByRole('combobox', { name: 'Agent', exact: true })
  await agentSelect.fill(broad.name)
  await expect(routes.getByRole('option', { name: broad.name, exact: true })).toBeEnabled()
  await routes.getByRole('option', { name: broad.name, exact: true }).click()
  await expect(routes.getByRole('status').filter({ hasText: 'can use everything' })).toBeVisible()
  await selectField(routes, 'Agent', agent.name)
  await expect(routes.getByRole('status').filter({ hasText: 'can use everything' })).toHaveCount(0)
  await selectField(routes, 'Who may participate', 'Listed people only')
  await routes.getByLabel('Allowed person IDs', { exact: true }).fill('U1\nU2')
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeDisabled()
  await routes.getByRole('button', { name: 'Add condition', exact: true }).click()
  await routes.getByLabel('Condition 1 key', { exact: true }).fill('constructor')
  await routes.getByLabel('Condition 1 value', { exact: true }).fill('C1')
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeEnabled()
  await routes.getByRole('button', { name: 'Save route', exact: true }).click()
  const route = routes.getByRole('article', { name: 'Support', exact: true })
  await expect(route).toBeVisible()
  const saved = (await workspace.api(path)).items
  expect(saved).toHaveLength(1)
  expect(saved[0]).toMatchObject({
    name: 'Support',
    enabled: true,
    agentId: agent.id,
    userIds: ['U1', 'U2'],
    accessMode: 'allowlist',
    match: { constructor: 'C1' },
    maxMessagesPerHour: 20,
    maxPerPersonPerHour: 5,
  })
  await route.getByRole('button', { name: 'Edit route', exact: true }).click()
  await expect(routes.getByLabel('Condition 1 key', { exact: true })).toHaveValue('constructor')
  await expect(routes.getByLabel('Condition 1 value', { exact: true })).toHaveValue('C1')
  await routes.getByRole('checkbox', { name: 'Enabled', exact: true }).uncheck()
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeEnabled()
  await routes.getByRole('button', { name: 'Save route', exact: true }).click()
  await expect(route.getByText('Support · Paused', { exact: true })).toBeVisible()
  const changed = (await workspace.api(path)).items[0]
  expect(changed.match).toEqual({ constructor: 'C1' })
  expect(changed.enabled).toBe(false)
  const viewport = page.viewportSize()
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(route.getByText('Support · Paused', { exact: true })).toBeVisible()
  await page.screenshot({ path: test.info().outputPath('resource-routes-phone.png'), animations: 'disabled' })
  if (viewport)
    await page.setViewportSize(viewport)
  await route.getByRole('button', { name: 'Delete route', exact: true }).click()
  await expect(route.getByRole('alert')).toContainText('A broader route may then match its sources.')
  await route.getByRole('button', { name: 'Confirm delete', exact: true }).click()
  await expect(routes.getByText('No routes yet. No one can start a conversation through this resource.', { exact: true })).toBeVisible()
  expect((await workspace.api(path)).items).toHaveLength(0)
})

test('resource route warnings include scoped grants without refusing the selected agent', async ({ page, workspace }) => {
  const fixture = await conversationRoutesFixture(workspace, 'route-warning')
  await useRelayForHttpMocks(page)
  let nativePreview = false
  await page.route('**/agents', async (route) => {
    if (route.request().method() !== 'GET')
      return route.continue()
    const response = await route.fetch()
    const listing = await response.json()
    // Client metadata fixture; native admission remains tested by the backend.
    return route.fulfill({
      response,
      json: {
        ...listing,
        agents: listing.agents.map((agent: Agent) => nativePreview && agent.id === fixture.agent.id
          ? { ...agent, access: { ...agent.access, backend: 'native' } }
          : agent),
      },
    })
  })
  await workspace.api(`/api/agents/${fixture.agent.id}`, 'PUT', {
    name: fixture.agent.name,
    revision: fixture.agent.revision,
    access: { ...fixture.agent.access, delegation: true },
  })
  const permissions = await resourceAccessFixture(workspace, 'route-warning-permissions')
  await workspace.api(permissions.path, 'POST', { agentId: fixture.agent.id, profile: 'read', scopes: { channel: ['C1'] } })
  await page.goto(workspacePath(`/plugins/${fixture.id}`))
  await signIn(page)
  await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Conversation routes', exact: true }).click()
  const routes = page.getByRole('dialog', { name: 'Conversation routes · Engineering', exact: true })
  await routes.getByRole('button', { name: 'Add route', exact: true }).click()
  await routes.getByLabel('Route name', { exact: true }).fill('Public support')
  await selectField(routes, 'Who may participate', 'Anyone')
  await selectField(routes, 'Agent', fixture.agent.name)
  await expect(routes.getByRole('status').filter({ hasText: 'use its plugins' })).toBeVisible()
  await expect(routes.getByRole('status').filter({ hasText: 'delegate work to other coding agents and models' })).toBeVisible()
  await selectField(routes, 'Who may participate', 'Approve people as they ask')
  await expect(routes.getByRole('status').filter({ hasText: 'use its plugins' })).toHaveCount(0)
  await expect(routes.getByRole('status').filter({ hasText: 'delegate work to other coding agents and models' })).toBeVisible()
  await selectField(routes, 'Who may participate', 'Anyone')
  await expect(routes.getByRole('status').filter({ hasText: 'use its plugins' })).toBeVisible()
  await routes.getByRole('button', { name: 'Add condition', exact: true }).click()
  await routes.getByLabel('Condition 1 key', { exact: true }).fill('channel')
  await routes.getByLabel('Condition 1 value', { exact: true }).fill('C1')
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeEnabled()
  await routes.getByRole('button', { name: 'Save route', exact: true }).click()
  await expect(routes.getByRole('article', { name: 'Public support', exact: true })).toBeVisible()
  expect((await workspace.api(fixture.path)).items[0]).toMatchObject({ agentId: fixture.agent.id, accessMode: 'everyone' })
  const agent = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === fixture.agent.id)
  expect(agent.plugins).toEqual([])
  nativePreview = true
  await page.reload()
  await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Conversation routes', exact: true }).click()
  await routes.getByRole('article', { name: 'Public support', exact: true }).getByRole('button', { name: 'Edit route', exact: true }).click()
  const nativeWarning = routes.getByRole('status').filter({ hasText: 'External bot messages cannot start work with a native agent' })
  await expect(nativeWarning).toBeVisible()
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeEnabled()
  await selectField(routes, 'Who may participate', 'Approve people as they ask')
  await expect(nativeWarning).toBeVisible()
})

test('resource routes refresh an uncertain creation without repeating the write', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await conversationRoutesFixture(workspace, 'routes-uncertain')
  await useRelayForHttpMocks(page)
  let writes = 0
  await page.route(`**${path.replace('/api', '')}`, async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    writes++
    const response = await route.fetch()
    expect(response.ok()).toBe(true)
    return route.abort('failed')
  })
  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Conversation routes', exact: true }).click()
  const routes = page.getByRole('dialog', { name: 'Conversation routes · Engineering', exact: true })
  await routes.getByRole('button', { name: 'Add route', exact: true }).click()
  await routes.getByLabel('Route name', { exact: true }).fill('Default')
  await expect(routes.getByRole('combobox', { name: 'Agent', exact: true })).toBeEnabled()
  await selectField(routes, 'Agent', agent.name)
  await routes.getByRole('checkbox', { name: 'Match all sources in this resource', exact: true }).check()
  await routes.getByRole('button', { name: 'Save route', exact: true }).click()
  await expect(routes.getByRole('alert')).toContainText('Refresh routes before making another change.')
  expect(writes).toBe(1)
  expect((await workspace.api(path)).items).toHaveLength(1)
  await expect(routes.getByRole('button', { name: 'Add route', exact: true })).toHaveCount(0)
  await routes.getByRole('button', { name: 'Refresh routes', exact: true }).click()
  await expect(routes.getByRole('article', { name: 'Default', exact: true })).toBeVisible()
  expect(writes).toBe(1)
})

test('route summaries distinguish loading missing data and failed refreshes', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await conversationRoutesFixture(workspace, 'routes-summary')
  const saved: PluginRoute[] = []
  for (const name of ['Support', 'Paused route', 'Earlier host']) {
    saved.push(await workspace.api(path, 'POST', {
      name,
      enabled: name !== 'Paused route',
      match: { channel: name },
      agentId: agent.id,
      accessMode: 'requests',
    }))
  }

  await useRelayForHttpMocks(page)
  let release!: () => void
  let requested = false
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  let reads = 0
  let mode = 'initial'
  const period = 1700002800000
  await page.route(`**${path.replace('/api', '')}`, async (route) => {
    if (route.request().method() !== 'GET')
      return route.continue()
    if (reads++ === 0) {
      requested = true
      await held
    }

    if (mode === 'failed')
      return route.fulfill({ status: 503, json: { error: 'Summary fixture unavailable.' } })
    const items = saved.map((item, index) => ({
      ...item,
      summary: index === 2
        ? undefined
        : {
            hourStartedAt: period,
            messagesThisHour: index === 1 ? 3 : mode === 'changed' ? 8 : 7,
            pendingRequests: index === 1 ? 0 : mode === 'changed' ? 1 : 2,
          },
    }))
    return route.fulfill({ json: { items, nextCursor: null } })
  })
  try {
    await page.goto(workspacePath(`/plugins/${id}`))
    await signIn(page)
    await page.getByRole('article', { name: 'Engineering', exact: true })
      .getByRole('button', { name: 'Conversation routes', exact: true })
      .click()
    const routes = page.getByRole('dialog', { name: 'Conversation routes · Engineering', exact: true })
    await expect.poll(() => requested).toBe(true)
    await expect(routes.getByText('0 pending access requests', { exact: true })).toHaveCount(0)
    await expect(routes.getByText('7 / 20 messages', { exact: true })).toHaveCount(0)
    release()
    const support = routes.getByRole('article', { name: 'Support', exact: true })
    await expect(support.getByText('7 / 20 messages', { exact: true })).toBeVisible()
    await expect(support.getByText('2 pending access requests', { exact: true })).toBeVisible()
    await expect(routes.getByText('Hour starting', { exact: false })).toHaveCount(2)
    await expect(routes.getByRole('article', { name: 'Paused route', exact: true })
      .getByText('3 / 20 messages', { exact: true })).toBeVisible()
    const earlier = routes.getByRole('article', { name: 'Earlier host', exact: true })
    await expect(earlier.getByText('Usage and pending request counts unavailable.', { exact: true })).toBeVisible()
    await expect(earlier.getByText('0 pending access requests', { exact: true })).toHaveCount(0)
    mode = 'failed'
    await routes.getByRole('button', { name: 'Refresh routes', exact: true }).click()
    await expect(routes.getByRole('alert')).toContainText('Summary fixture unavailable.')
    await expect(routes.getByText('7 / 20 messages', { exact: true })).toHaveCount(0)
    await expect(routes.getByText('0 pending access requests', { exact: true })).toHaveCount(0)
    mode = 'changed'
    await routes.getByRole('button', { name: 'Refresh routes', exact: true }).click()
    await expect(support.getByText('8 / 20 messages', { exact: true })).toBeVisible()
    await expect(support.getByText('1 pending access request', { exact: true })).toBeVisible()
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'wait' })
  }
})

test('resource routes manage people and requests only on their selected route', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const {
    id,
    instance,
    resource,
    agent,
    path,
  } = await conversationRoutesFixture(workspace, 'routes-people')
  const support = await workspace.api(path, 'POST', {
    name: 'Support',
    enabled: true,
    match: { channel: 'C1' },
    agentId: agent.id,
    accessMode: 'requests',
  })
  const operations = await workspace.api(path, 'POST', {
    name: 'Operations',
    enabled: true,
    match: { channel: 'C2' },
    agentId: agent.id,
    accessMode: 'requests',
  })
  for (const [channel, person] of [['C1', 'U1'], ['C2', 'U2']]) {
    const body = JSON.stringify({
      id: `request-${person}`,
      resource: resource.id,
      author: { id: person, name: `Person ${person}` },
      context: { channel, thread: person },
      body: { text: 'Unapproved content must stay outside the request list' },
    })
    const timestamp = Math.floor(Date.now() / 1000).toString()
    const signature = createHmac('sha256', 'synthetic-browser-route-signature').update(`v0:${timestamp}:${body}`).digest('hex')
    const response = await fetch(`${workspace.installationUrl}/api/plugin-webhooks/${instance.id}/events`, {
      method: 'POST',
      body,
      headers: { 'content-type': 'application/json', 'x-signature': `v0=${signature}`, 'x-timestamp': timestamp },
    })
    expect(response.ok).toBe(true)
    expect((await response.json()).status).toBe('requested')
  }

  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Conversation routes', exact: true }).click()
  const routes = page.getByRole('dialog', { name: 'Conversation routes · Engineering', exact: true })
  const supportRow = routes.getByRole('article', { name: 'Support', exact: true })
  const operationsRow = routes.getByRole('article', { name: 'Operations', exact: true })
  await expect(supportRow.getByText('0 / 20 messages', { exact: true })).toBeVisible()
  await expect(supportRow.getByText('1 pending access request', { exact: true })).toBeVisible()
  await expect(operationsRow.getByText('1 pending access request', { exact: true })).toBeVisible()
  await supportRow.getByRole('button', { name: 'People and requests', exact: true }).click()
  await expect(routes.getByText('Person U1', { exact: true })).toBeVisible()
  await expect(routes.getByText('Person U2', { exact: true })).toHaveCount(0)
  await expect(routes.getByText('Unapproved content must stay outside the request list', { exact: true })).toHaveCount(0)
  await routes.getByRole('button', { name: 'Approve U1', exact: true }).click()
  await expect(supportRow).toBeVisible()
  await expect(supportRow.getByText('0 pending access requests', { exact: true })).toBeVisible()
  await expect(operationsRow.getByText('1 pending access request', { exact: true })).toBeVisible()
  expect((await workspace.api(path)).items.find((item: any) => item.id === support.id).userIds).toEqual(['U1'])
  expect((await workspace.api(`${path}/${support.id}/requests`)).items).toHaveLength(0)
  expect((await workspace.api(`${path}/${operations.id}/requests`)).items).toHaveLength(1)
  await supportRow.getByRole('button', { name: 'People and requests', exact: true }).click()
  await routes.getByRole('button', { name: 'Remove U1', exact: true }).click()
  await expect(supportRow).toBeVisible()
  expect((await workspace.api(path)).items.find((item: any) => item.id === support.id).userIds).toEqual([])
  await routes.getByRole('article', { name: 'Operations', exact: true }).getByRole('button', { name: 'People and requests', exact: true }).click()
  await expect(routes.getByText('Person U2', { exact: true })).toBeVisible()
  await routes.getByRole('button', { name: 'Deny U2', exact: true }).click()
  await expect(routes.getByRole('article', { name: 'Operations', exact: true })).toBeVisible()
  expect((await workspace.api(`${path}/${operations.id}/requests`)).items[0].status).toBe('denied')
  await expect(operationsRow.getByText('0 pending access requests', { exact: true })).toBeVisible()
})

test('a stale route editor cannot restore a participant removed by another owner action', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { id, agent, path } = await conversationRoutesFixture(workspace, 'routes-stale')
  const saved = await workspace.api(path, 'POST', {
    name: 'Support',
    enabled: true,
    match: { channel: 'C1' },
    agentId: agent.id,
    accessMode: 'allowlist',
    userIds: ['U1'],
  })
  await useRelayForHttpMocks(page)
  let writes = 0
  await page.route(`**${path.replace('/api', '')}/${saved.id}`, (route) => {
    if (route.request().method() === 'PUT')
      writes++
    return route.continue()
  })
  await page.goto(workspacePath(`/plugins/${id}`))
  await signIn(page)
  await page.getByRole('article', { name: 'Engineering', exact: true }).getByRole('button', { name: 'Conversation routes', exact: true }).click()
  const routes = page.getByRole('dialog', { name: 'Conversation routes · Engineering', exact: true })
  const row = routes.getByRole('article', { name: 'Support', exact: true })
  await row.getByRole('button', { name: 'Edit route', exact: true }).click()
  await expect(routes.getByLabel('Allowed person IDs', { exact: true })).toHaveValue('U1')
  await workspace.api(`${path}/${saved.id}/people/U1`, 'POST', { action: 'remove', revision: saved.revision })
  await routes.getByLabel('Route name', { exact: true }).fill('Stale change')
  await expect(routes.getByRole('button', { name: 'Save route', exact: true })).toBeEnabled()
  await routes.getByRole('button', { name: 'Save route', exact: true }).click()
  await expect(routes.getByRole('alert')).toContainText('Refresh routes before making another change.')
  expect(writes).toBe(1)
  let current = (await workspace.api(path)).items[0]
  expect(current.name).toBe('Support')
  expect(current.userIds).toEqual([])
  await routes.getByRole('button', { name: 'Refresh routes', exact: true }).click()
  await row.getByRole('button', { name: 'Edit route', exact: true }).click()
  await expect(routes.getByLabel('Allowed person IDs', { exact: true })).toHaveValue('')
  expect(writes).toBe(1)
  current = (await workspace.api(path)).items[0]
  expect(current.userIds).toEqual([])
})

test('required choices and Unicode text keep the shared form whitespace contract', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.route('**/api/plugins/packages/forms', route => route.fulfill({
    json: {
      id: 'forms',
      builtin: false,
      status: 'active',
      active: '1.0.0',
      setup: {
        effort: 'token',
        steps: [{
          id: 'settings',
          kind: 'settings',
          title: 'Settings',
          settings: ['region', 'team', 'password'],
        }],
      },
      instanceReads: true,
      accessReads: true,
      setupDrafts: false,
      capabilities: [],
      manifest: {
        id: 'forms',
        name: 'Form examples',
        version: '1.0.0',
        apiVersion: 2,
        capabilities: [],
        settings: [
          {
            key: 'region',
            title: 'Region',
            type: 'select',
            required: true,
            default: ' ',
            options: [{ value: ' ', label: 'Space option' }],
          },
          {
            key: 'team',
            title: 'Team',
            type: 'string',
            required: true,
          },
          {
            key: 'password',
            title: 'Password',
            type: 'secret',
            required: true,
          },
        ],
      },
    },
  }))
  await page.route('**/api/plugins/packages/forms/connections', route => route.fulfill({ json: { items: [] } }))
  await page.route('**/api/plugins/instance-access', route => route.fulfill({ json: { items: [] } }))
  const saves: Record<string, any>[] = []
  await page.route('**/api/plugins/forms/connections', async (route) => {
    saves.push(route.request().postDataJSON())
    await route.fulfill({ json: {} })
  })
  await page.goto(workspacePath('/plugins/forms'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add instance', exact: true }).click()
  const form = page.getByRole('dialog', { name: 'New Form examples instance' })
  await expect(form.getByRole('combobox', { name: 'Region' })).toHaveValue(' ')
  await form.getByLabel('Team', { exact: true }).fill('   ')
  await form.getByLabel('Password', { exact: true }).fill('\uFEFF')
  await expect(form.getByRole('button', { name: 'Save instance' })).toBeDisabled()
  await form.getByLabel('Team', { exact: true }).fill('\uFEFF')
  await expect(form.getByRole('button', { name: 'Save instance' })).toBeEnabled()
  await form.getByRole('button', { name: 'Save instance' }).click()
  await expect.poll(() => saves.length).toBe(1)
  expect(saves[0]).toMatchObject({ settings: { region: ' ', team: '\uFEFF' }, secrets: { password: '\uFEFF' } })
})

test('instance and panel forms share defaults, clearing, hidden values and secret rules', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const fields = [
    {
      key: 'title',
      title: 'Title',
      type: 'string',
      default: 'Default title',
    },
    {
      key: 'count',
      title: 'Count',
      type: 'number',
      default: 0,
    },
    {
      key: 'enabled',
      title: 'Enabled',
      type: 'boolean',
      default: true,
    },
    {
      key: 'scopes',
      title: 'Scopes',
      type: 'multiselect',
      options: [{ value: 'read', label: 'Read' }],
      default: ['read'],
    },
    {
      key: 'derived',
      title: 'Derived',
      type: 'string',
      hidden: true,
      default: ' hidden ',
    },
    { key: 'password', title: 'Password', type: 'secret' },
    { key: 'constructor', title: 'Constructor secret', type: 'secret' },
    {
      key: 'region',
      title: 'Region',
      type: 'select',
      default: ' eu ',
      options: [{ value: ' eu ', label: 'Europe' }],
    },
  ]
  const manifest = {
    id: 'forms',
    name: 'Form examples',
    version: '1.0.0',
    apiVersion: 2,
    description: 'Synthetic form fixture.',
    capabilities: [],
    settings: fields,
    instanceView: true,
    instanceForm: true,
  }
  const connection = {
    id: 'example',
    pluginId: 'forms',
    name: 'Example',
    settings: {},
    secretKeys: ['password'],
    agentIds: [],
    agentId: '',
    accessMode: 'requests',
    userIds: [],
    people: [],
    requests: [],
    enabled: true,
    status: { state: 'connected' },
  }
  const panelNode = (values = {}) => ({
    type: 'form',
    fields,
    values,
    submit: { label: 'Apply values', action: 'apply' },
  })
  await page.route('**/api/plugins/packages/forms', route => route.fulfill({
    json: {
      id: 'forms',
      builtin: false,
      status: 'active',
      active: '1.0.0',
      setup: {
        steps: [{
          id: 'settings',
          kind: 'settings',
          title: 'Settings',
          settings: fields.filter(field => !field.hidden).map(field => field.key),
        }],
      },
      manifest,
      instanceReads: true,
      accessReads: true,
      setupDrafts: false,
      capabilities: [],
    },
  }))
  await page.route('**/api/plugins/packages/forms/connections', route => route.fulfill({ json: { items: [connection] } }))
  await page.route('**/api/plugins/packages/forms/connections/example', route => route.fulfill({ json: connection }))
  await page.route('**/api/plugins/instance-access', route => route.fulfill({ json: { items: [] } }))
  const initialPanelValues = { password: 'synthetic_never_prefill', unknown: 'drop' }
  await page.route('**/api/plugins/forms/connections/example/view', route => route.fulfill({
    json: { view: [panelNode(initialPanelValues)] },
  }))
  const actions: Record<string, any>[] = []
  await page.route('**/api/plugins/forms/connections/example/actions', async (route) => {
    actions.push(route.request().postDataJSON().values)
    const result = actions.length === 2
      ? { view: [panelNode(initialPanelValues)] }
      : actions.length === 3
        ? { view: [panelNode({ title: 'Replacement title', count: 7 })] }
        : {}
    await route.fulfill({ json: result })
  })
  const completions: Record<string, any>[] = []
  await page.route('**/api/plugins/forms/form', async (route) => {
    completions.push(route.request().postDataJSON())
    await route.fulfill({
      json: {
        options: {},
        values: { derived: ' completed ', unknown: 'drop' },
        errors: {},
        view: [{ type: 'text', text: 'Synthetic completion applied' }],
      },
    })
  })
  const saves: Record<string, any>[] = []
  await page.route('**/api/plugins/forms/connections/example', async (route) => {
    saves.push(route.request().postDataJSON())
    await route.fulfill({ json: {} })
  })
  await page.goto(workspacePath('/plugins/forms'))
  await signIn(page)
  const panel = page.getByRole('region', { name: 'Example panel' })
  await expect(panel.getByLabel('Title', { exact: true })).toHaveValue('Default title')
  await expect(panel.getByLabel('Count', { exact: true })).toHaveValue('0')
  await expect(panel.getByLabel('Password', { exact: true })).toHaveValue('')
  await expect(panel.getByLabel('Constructor secret', { exact: true })).toHaveValue('')
  await expect(panel.getByRole('combobox', { name: 'Region', exact: true })).toHaveValue(' eu ')
  await expect(panel.getByLabel('Derived', { exact: true })).toHaveCount(0)
  await panel.getByRole('button', { name: 'Apply values' }).click()
  await expect.poll(() => actions.length).toBe(1)
  expect(actions[0]).toEqual({
    title: 'Default title',
    count: 0,
    enabled: true,
    scopes: ['read'],
    derived: 'hidden',
    region: ' eu ',
  })
  await panel.getByLabel('Title', { exact: true }).fill('   ')
  await panel.getByLabel('Count', { exact: true }).fill('3.5')
  await panel.getByLabel('Enabled', { exact: true }).uncheck()
  await panel.getByLabel('Read', { exact: true }).uncheck()
  await panel.getByLabel('Password', { exact: true }).fill(' synthetic_panel_secret ')
  await panel.getByLabel('Constructor secret', { exact: true }).fill(' synthetic_constructor_secret ')
  await panel.getByRole('button', { name: 'Apply values' }).click()
  await expect.poll(() => actions.length).toBe(2)
  expect(actions[1]).toEqual({
    count: 3.5,
    enabled: false,
    scopes: [],
    derived: 'hidden',
    region: ' eu ',
    password: ' synthetic_panel_secret ',
    constructor: ' synthetic_constructor_secret ',
  })
  await expect(panel.getByLabel('Password', { exact: true })).toHaveValue('')
  await expect(panel.getByLabel('Constructor secret', { exact: true })).toHaveValue('')
  await expect(panel.getByLabel('Title', { exact: true })).toHaveValue('   ')
  await expect(panel.getByLabel('Count', { exact: true })).toHaveValue('3.5')
  await panel.getByRole('button', { name: 'Apply values' }).click()
  await expect.poll(() => actions.length).toBe(3)
  expect(actions[2]).toEqual({
    count: 3.5,
    enabled: false,
    scopes: [],
    derived: 'hidden',
    region: ' eu ',
  })
  await expect(panel.getByLabel('Title', { exact: true })).toHaveValue('Replacement title')
  await expect(panel.getByLabel('Count', { exact: true })).toHaveValue('7')

  await page.getByRole('button', { name: 'Edit Example', exact: true }).click()
  const editor = page.getByRole('dialog', { name: 'Edit Example' })
  await expect.poll(() => completions.length).toBe(1)
  expect(completions[0].settings).toEqual(actions[0])
  expect(completions[0].secrets).toEqual({})
  await expect(editor.getByLabel('Title', { exact: true })).toHaveValue('Default title')
  await expect(editor.getByLabel('Password', { exact: true })).toHaveValue('')
  await expect(editor.getByLabel('Constructor secret', { exact: true })).toHaveValue('')
  // The completion's `errors: {}` has no entry for a field named constructor.
  await expect(editor.getByText('Synthetic completion applied')).toBeVisible()
  await expect(editor.getByLabel('Constructor secret', { exact: true })).not.toHaveAttribute('aria-describedby')
  await editor.getByLabel('Title', { exact: true }).fill('')
  await editor.getByLabel('Count', { exact: true }).fill('')
  await editor.getByLabel('Password', { exact: true }).fill('   ')
  await editor.getByRole('button', { name: 'Save instance', exact: true }).click()
  await expect.poll(() => saves.length).toBe(1)
  expect(saves[0].settings).toEqual({
    enabled: true,
    scopes: ['read'],
    derived: 'completed',
    region: ' eu ',
  })
  expect(saves[0].secrets).toEqual({})
})

for (const prepared of [false, true]) {
  for (const resource of [false, true]) {
    test(`remote choices preserve selections and discard old queries in ${prepared ? 'prepared' : 'active'} ${resource ? 'resource' : 'instance'} forms`, async ({ page, workspace }) => {
      test.setTimeout(90000)
      await useRelayForHttpMocks(page)
      const id = `remote-${prepared ? 'prepared' : 'active'}-${resource ? 'resource' : 'root'}`
      const fields = [
        {
          key: 'channel',
          title: 'Channel',
          type: 'select',
          dynamic: true,
          remote: true,
        },
        {
          key: 'members',
          title: 'Members',
          type: 'multiselect',
          dynamic: true,
          remote: true,
        },
        { key: 'token', title: 'Access secret', type: 'secret' },
      ]
      const manifest = {
        id,
        name: 'Remote service',
        version: '1',
        apiVersion: 2,
        capabilities: ['resources'],
        instanceForm: true,
        settings: fields,
        resources: [
          { name: 'workspace', title: 'Workspaces', singular: 'Workspace' },
          {
            name: 'channel',
            title: 'Channels',
            singular: 'Channel',
            parent: 'workspace',
            form: true,
            settings: fields,
          },
        ],
      }
      const source = `return {form = function(event)
        if event.resource and event.resource.parent == nil then return {errors = {parentId = 'Choose a workspace first.'}} end
        if not event.lookup then return nil end
        local lookup = event.lookup
        local function page(items, next, known)
          local options = {}; options[lookup.field] = items
          if not known then return {options = options} end
          local pages = {}; pages[lookup.field] = {nextCursor = next}
          return {options = options, pages = pages}
        end
        if lookup.query == 'unknown' then return page(cairn.array(), nil, false) end
        local label = 'General'
        local value = 'C1'
        local next = cairn.json.null
        if lookup.query ~= '' then label = lookup.query .. ' choice'; value = lookup.query
        elseif lookup.cursor == nil then next = 'empty'
        elseif lookup.cursor == 'empty' then return page(cairn.array(), 'last', true)
        else label = 'Engineering'; value = 'C2' end
        if lookup.field == 'members' then label ..= ' member'; value = 'M' .. value end
        return page({{value = value, label = label}}, next, true)
      end}`
      const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source })
      await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
      const base = `/api/plugins/${id}/${prepared ? 'setup-drafts' : 'connections'}`
      const instance = await workspace.api(base, 'POST', {
        name: 'Remote app',
        settings: { channel: 'saved-off-page', members: [] },
        secrets: { token: 'synthetic-saved-secret' },
        ...(!prepared && { enabled: false }),
      })
      const root = `${base}/${instance.id}`
      if (resource)
        await workspace.api(`${root}/resources/workspace`, 'POST', { name: 'Workspace', ...(prepared && { setupRevision: instance.revision }) })
      let release!: () => void
      const gate = new Promise<void>(resolve => release = resolve)
      let started!: () => void
      const entered = new Promise<void>(resolve => started = resolve)
      let releaseCurrent!: () => void
      const currentGate = new Promise<void>(resolve => releaseCurrent = resolve)
      let currentStarted!: () => void
      const currentEntered = new Promise<void>(resolve => currentStarted = resolve)
      const endpoint = resource ? `${root}/resources/channel/form` : prepared ? `${root}/form` : `/api/plugins/${id}/form`
      await page.route(`**${endpoint.replace('/api', '')}`, async (route) => {
        const response = await route.fetch()
        const body = route.request().postDataJSON()
        if (body.lookup?.query === 'old') {
          started()
          await gate
        }

        if (prepared && !resource && body.lookup?.query === 'new') {
          currentStarted()
          await currentGate
        }

        await route.fulfill({ response })
      })
      try {
        await page.goto(workspacePath(`/plugins/${id}`))
        await signIn(page)
        if (prepared)
          await page.getByRole('button', { name: 'Continue Remote app', exact: true }).click()
        const owner = prepared ? page.getByRole('dialog', { name: 'Set up Remote service', exact: true }) : page.getByRole('article', { name: 'Remote app', exact: true })
        if (resource)
          await owner.getByRole('button', { name: 'Add channel', exact: true }).click()
        else if (!prepared)
          await owner.getByRole('button', { name: 'Edit Remote app', exact: true }).click()
        const form = resource ? owner.getByRole('form', { name: 'New channel', exact: true }) : prepared ? owner : page.getByRole('dialog', { name: 'Edit Remote app', exact: true })
        if (resource) {
          await form.getByLabel('Name', { exact: true }).fill('Support')
          await selectField(form, 'Parent', 'Workspace')
        }

        const choice = form.getByRole('combobox', { name: 'Channel', exact: true })
        const channelPicker = form.locator('.virtual-select').filter({ has: page.getByRole('combobox', { name: 'Channel', exact: true }) })
        await choice.click()
        const channelChecked = page.waitForResponse(response => response.url().endsWith('/form') && response.request().postDataJSON().changed === 'channel')
        await page.getByRole('option', { name: 'General', exact: true }).click()
        await channelChecked
        await expect(choice).toHaveValue('General')
        if (prepared && !resource)
          await expect(form.getByRole('button', { name: 'Save setup', exact: true })).toBeEnabled()
        await choice.click()
        const nextPage = channelPicker.getByRole('button', { name: 'Next page', exact: true })
        await expect(nextPage).toBeEnabled()
        await choice.press('Tab')
        await expect(nextPage).toBeFocused()
        await nextPage.press('Shift+Tab')
        await expect(choice).toBeFocused()
        await expect(choice).toHaveAttribute('aria-expanded', 'true')
        await choice.press('Tab')
        await nextPage.press('Enter')
        await expect(channelPicker.getByText('0 choices on this page · More available', { exact: true }).first()).toBeVisible()
        await choice.press('Tab')
        await expect(nextPage).toBeFocused()
        await nextPage.press('Enter')
        await expect(page.getByRole('option', { name: 'Engineering', exact: true })).toBeVisible()
        await choice.press('Escape')
        await expect(choice).toHaveValue('General')
        const unknown = page.waitForResponse(response => response.url().endsWith('/form') && response.request().postDataJSON().lookup?.field === 'channel' && response.request().postDataJSON().lookup?.query === 'unknown')
        await choice.fill('unknown')
        await unknown
        await expect(page.getByText('Loading options…', { exact: true })).toHaveCount(0)
        await expect(channelPicker.getByText('0 choices on this page · Completeness unknown', { exact: true }).first()).toBeVisible()
        await choice.fill('old')
        await entered
        await choice.fill('new')
        if (prepared && !resource) {
          await currentEntered
          // Name changes rerender the schema without changing the lookup scope.
          await form.getByLabel('Name', { exact: true }).fill('Renamed remote app')
          releaseCurrent()
        }

        const currentChoice = page.getByRole('option', { name: 'new choice', exact: true, includeHidden: true })
        await expect(currentChoice).toBeAttached()
        if (!prepared || resource)
          await expect(currentChoice).toBeVisible()
        const oldAnswer = page.waitForResponse(response => response.url().endsWith('/form') && response.request().postDataJSON().lookup?.query === 'old')
        release()
        await oldAnswer
        await expect(page.getByRole('option', { name: 'old choice', exact: true, includeHidden: true })).toHaveCount(0)
        if (prepared && !resource)
          await expect(choice).toHaveAttribute('aria-expanded', 'false')
        else
          await choice.press('Escape')
        await expect(choice).toHaveValue('General')
        const members = form.getByRole('group', { name: 'Members', exact: true })
        const membersChecked = page.waitForResponse(response => response.url().endsWith('/form') && response.request().postDataJSON().changed === 'members')
        const channelRefreshed = page.waitForResponse((response) => {
          if (!response.url().endsWith('/form'))
            return false
          const body = response.request().postDataJSON()
          return body.lookup?.field === 'channel' && body.settings.members?.includes('MC1')
        })
        await members.getByRole('checkbox', { name: 'General member', exact: true }).check()
        // The dependent channel lookup can change the layout above this button.
        // Finish that update before exercising pointer pagination.
        await Promise.all([membersChecked, channelRefreshed])
        await expect(page.getByText('Loading options…', { exact: true })).toHaveCount(0)
        await expect(members.getByRole('button', { name: 'Next page', exact: true })).toBeEnabled()
        await members.getByRole('button', { name: 'Next page', exact: true }).click()
        await expect(members.getByText('0 choices on this page · More available', { exact: true })).toBeVisible()
        await expect(members.getByRole('button', { name: 'Remove General member', exact: true })).toBeVisible()
        await members.getByRole('button', { name: 'Next page', exact: true }).click()
        const membersUnknown = page.waitForResponse(response => response.url().endsWith('/form') && response.request().postDataJSON().lookup?.field === 'members' && response.request().postDataJSON().lookup?.query === 'unknown')
        await members.getByRole('searchbox', { name: 'Search members', exact: true }).fill('unknown')
        await membersUnknown
        await expect(members.getByText('Loading choices…', { exact: true })).toHaveCount(0)
        await expect(members.getByText('0 choices on this page · Completeness unknown', { exact: true })).toBeVisible()
        const save = form.getByRole('button', { name: resource ? 'Save channel' : prepared ? 'Save setup' : 'Save instance', exact: true })
        await expect(save).toBeEnabled()
        const persisted = page.waitForResponse(response => response.url().endsWith(resource ? `${root}/resources/channel` : root) && response.request().method() === (resource ? 'POST' : prepared ? 'PATCH' : 'PUT'))
        await save.click()
        const response = await persisted
        expect(response.ok()).toBe(true)
        const saved = await response.json()
        expect(saved.name).toBe(resource ? 'Support' : prepared ? 'Renamed remote app' : 'Remote app')
        expect(saved.settings.channel).toBe('C1')
        expect(saved.settings.members).toEqual(['MC1'])
        expect(saved.secretKeys).toEqual(resource ? [] : ['token'])
      }
      finally {
        release()
        releaseCurrent()
        // Keep the page alive until intercepted requests from this scenario finish.
        await page.unrouteAll({ behavior: 'wait' })
      }
    })
  }
}

for (const prepared of [false, true]) {
  test(`scoped ${prepared ? 'prepared' : 'active'} resource forms discard stale parents and reconcile an uncertain save`, async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    const id = `scoped-form-${prepared ? 'prepared' : 'active'}`
    const manifest = {
      id,
      name: 'Scoped service',
      version: '1',
      apiVersion: 2,
      capabilities: ['resources'],
      resources: [
        { name: 'workspace', title: 'Workspaces', singular: 'Workspace' },
        {
          name: 'channel',
          title: 'Channels',
          singular: 'Channel',
          parent: 'workspace',
          form: true,
          settings: [
            {
              key: 'channel',
              title: 'Channel',
              type: 'select',
              dynamic: true,
              refresh: true,
              required: true,
            },
            {
              key: 'verifiedName',
              title: 'Verified name',
              type: 'string',
              hidden: true,
              required: true,
            },
            { key: 'token', title: 'Channel token', type: 'secret' },
          ],
        },
      ],
    }
    const source = `return {form = function(event)
      local parent = event.resource.parent
      if parent == nil then return {errors = {parentId = 'Choose a workspace first.'}} end
      local channel = parent.name .. '-general'
      if event.submit and event.values.channel ~= channel then return {errors = {channel = 'Choose a channel in this workspace.'}} end
      return {options = {channel = {{value = channel, label = parent.name .. ' channel'}}},
        values = {verifiedName = parent.name .. (event.submit and ' verified' or ' preview')},
        view = {{type = 'alert', text = 'Workspace: ' .. parent.name, tone = 'info'}}}
    end}`
    const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source })
    await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
    const root = `/api/plugins/${id}/${prepared ? 'setup-drafts' : 'connections'}`
    const instance = await workspace.api(root, 'POST', { name: 'Scoped app', ...(!prepared && { enabled: false }) })
    const path = `${root}/${instance.id}/resources`
    const revision = prepared ? { setupRevision: instance.revision } : {}
    const engineering = await workspace.api(`${path}/workspace`, 'POST', { name: 'Engineering', ...revision })
    const sales = await workspace.api(`${path}/workspace`, 'POST', { name: 'Sales', ...revision })
    let releaseOld!: () => void
    const oldGate = new Promise<void>((resolve) => {
      releaseOld = resolve
    })
    let oldStarted!: () => void
    const oldRequest = new Promise<void>((resolve) => {
      oldStarted = resolve
    })
    let held = false
    await page.route(`**${path.replace('/api', '')}/channel/form`, async (route) => {
      const response = await route.fetch()
      expect(response.ok()).toBe(true)
      if (route.request().postDataJSON().parentId === engineering.id && !held) {
        held = true
        oldStarted()
        await oldGate
      }

      await route.fulfill({ response })
    })
    let writes = 0
    await page.route(`**${path.replace('/api', '')}/channel`, async (route) => {
      if (route.request().method() !== 'POST')
        return route.continue()
      writes++
      const response = await route.fetch()
      expect(response.ok()).toBe(true)
      await route.abort('failed')
    })
    try {
      await page.goto(workspacePath(`/plugins/${id}`))
      await signIn(page)
      if (prepared)
        await page.getByRole('button', { name: 'Continue Scoped app', exact: true }).click()
      const owner = prepared ? page.getByRole('dialog', { name: 'Set up Scoped service', exact: true }) : page.getByRole('article', { name: 'Scoped app', exact: true })
      await owner.getByRole('button', { name: 'Add channel', exact: true }).click()
      const form = owner.getByRole('form', { name: 'New channel', exact: true })
      await form.getByLabel('Name', { exact: true }).fill('Support')
      await form.getByLabel('Channel token', { exact: true }).fill('synthetic-scoped-secret')
      await selectField(form, 'Parent', 'Engineering')
      await oldRequest
      if (!prepared) {
        // Keep the real five-second selected-package refresh inside this regression.
        await page.waitForResponse(response => response.url().endsWith(`/plugins/packages/${id}`) && response.request().method() === 'GET' && response.ok())
      }

      await expect(form.getByRole('button', { name: 'Save channel', exact: true })).toBeDisabled()
      await selectField(form, 'Parent', 'Sales')
      await expect(form.getByRole('combobox', { name: 'Channel', exact: true }).getByRole('option', { name: 'Sales channel', exact: true })).toHaveCount(1)
      await form.getByRole('combobox', { name: 'Channel', exact: true }).selectOption('Sales-general')
      await expect(form.getByRole('button', { name: 'Save channel', exact: true })).toBeEnabled()
      const oldResponse = page.waitForResponse(response => response.url().endsWith('/channel/form') && response.request().postDataJSON().parentId === engineering.id)
      releaseOld()
      await oldResponse
      await expect(form.getByRole('combobox', { name: 'Channel', exact: true })).toHaveValue('Sales-general')
      await expect(form.getByText('Workspace: Sales', { exact: true })).toBeVisible()
      await form.getByRole('button', { name: 'Save channel', exact: true }).click()
      await expect(form.getByText('Check whether this resource was saved before making another change.', { exact: false })).toBeVisible()
      expect(writes).toBe(1)
      await expect(form.getByRole('button', { name: 'Save channel', exact: true })).toBeDisabled()
      await form.getByRole('button', { name: 'Check save result', exact: true }).click()
      const saved = (await workspace.api(`${path}/channel`)).items[0]
      expect(saved.parentId).toBe(sales.id)
      expect(saved.settings).toEqual({ channel: 'Sales-general', verifiedName: 'Sales verified' })
      expect(saved.secretKeys).toEqual(['token'])
      expect(JSON.stringify(saved)).not.toContain('synthetic-scoped-secret')
      const card = owner.getByRole('article', { name: 'Support', exact: true })
      await expect(card).toBeVisible()
      await card.getByRole('button', { name: 'Edit Support', exact: true }).click()
      const edit = owner.getByRole('form', { name: 'Edit Support', exact: true })
      await expect(edit.getByLabel('Channel token', { exact: true })).toHaveValue('')
      await expect(edit.getByRole('button', { name: 'Save channel', exact: true })).toBeEnabled()
      await edit.getByRole('button', { name: 'Cancel', exact: true }).click()
      expect(writes).toBe(1)
    }
    finally {
      releaseOld()
    }
  })
}

test('resource forms share native defaults, exact choices and secret normalization', async ({ page, workspace }) => {
  test.setTimeout(90000)
  // The payload assertion observes HTTP; direct calls use data-channel frames.
  await useRelayForHttpMocks(page)

  const manifest = {
    id: 'resource-form-values',
    name: 'Resource form values',
    description: 'Synthetic resource form boundary fixture.',
    version: '1',
    apiVersion: 2,
    capabilities: ['resources'],
    resources: [{
      name: 'workspace',
      title: 'Workspaces',
      singular: 'Workspace',
      settings: [
        {
          key: 'constructor',
          title: 'Constructor value',
          type: 'string',
          default: 'Declared default',
        },
        { key: 'title', title: 'Optional title', type: 'string' },
        {
          key: 'count',
          title: 'Count',
          type: 'number',
          default: 0,
        },
        {
          key: 'enabled',
          title: 'Feature enabled',
          type: 'boolean',
          default: true,
        },
        {
          key: 'scopes',
          title: 'Scopes',
          type: 'multiselect',
          default: ['read'],
          options: [{ value: 'read', label: 'Read' }],
        },
        {
          key: 'region',
          title: 'Region',
          type: 'select',
          default: ' eu ',
          options: [{ value: ' eu ', label: 'Europe' }],
        },
        { key: 'password', title: 'Workspace secret', type: 'secret' },
      ],
    }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api('/api/plugins/packages/resource-form-values/approve', 'POST', { version: '1', digest: installed.digest })
  const connection = await workspace.api('/api/plugins/resource-form-values/connections', 'POST', {
    name: 'Form service',
    enabled: true,
    settings: {},
  })
  await page.goto(workspacePath('/plugins/resource-form-values'))
  await signIn(page)
  const instance = page.getByRole('article', { name: 'Form service', exact: true })
  await instance.getByRole('button', { name: 'Add workspace', exact: true }).click()
  const form = instance.getByRole('form', { name: 'New workspace', exact: true })
  await expect(form.getByLabel('Constructor value', { exact: true })).toHaveValue('Declared default')
  await expect(form.getByLabel('Count', { exact: true })).toHaveValue('0')
  await expect(form.getByRole('combobox', { name: 'Region', exact: true })).toHaveValue(' eu ')
  await expect(form.getByLabel('Workspace secret', { exact: true })).toHaveValue('')
  await form.getByLabel('Name', { exact: true }).fill('Engineering')
  await form.getByLabel('Optional title', { exact: true }).fill('   ')
  await form.getByLabel('Count', { exact: true }).fill('3.5')
  await form.getByLabel('Feature enabled', { exact: true }).uncheck()
  await form.getByLabel('Read', { exact: true }).uncheck()
  await form.getByLabel('Workspace secret', { exact: true }).fill(' \uFEFFsynthetic-resource-secret\uFEFF ')
  const requested = page.waitForRequest(request => request.method() === 'POST' && request.url().endsWith('/resources/workspace'))
  await form.getByRole('button', { name: 'Save workspace', exact: true }).click()
  const request = await requested
  expect(request.postDataJSON().settings).toEqual({
    constructor: 'Declared default',
    count: 3.5,
    enabled: false,
    scopes: [],
    region: ' eu ',
  })
  expect(request.postDataJSON().secrets).toEqual({ password: '\uFEFFsynthetic-resource-secret\uFEFF' })
  await expect(instance.getByRole('article', { name: 'Engineering', exact: true })).toBeVisible()
  const saved = await workspace.api(`/api/plugins/resource-form-values/connections/${connection.id}/resources/workspace`)
  expect(saved.items[0].settings).toEqual({
    constructor: 'Declared default',
    count: 3.5,
    enabled: false,
    scopes: [],
    region: ' eu ',
  })
  expect(saved.items[0].secretKeys).toEqual(['password'])
})

test('setup drafts load every page before resuming and discarding the last draft', async ({ page, workspace }) => {
  const manifest = {
    id: 'draft-pages',
    name: 'Draft pages',
    version: '1',
    apiVersion: 2,
    capabilities: [],
    instanceForm: true,
    settings: [{ key: 'label', title: 'Label', type: 'string' }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', {
    manifest,
    source: 'return {form = function() return {} end}',
  })
  await workspace.api('/api/plugins/packages/draft-pages/approve', 'POST', { version: '1', digest: installed.digest })
  for (const label of ['First', 'Second'])
    await workspace.api('/api/plugins/draft-pages/setup-drafts', 'POST', { name: `${label} setup`, settings: { label } })
  const saved = (await workspace.api('/api/plugins/draft-pages/setup-drafts')).items
  const last = saved.at(-1)
  let entered!: () => void
  let release!: () => void
  const secondPage = new Promise<void>((resolve) => {
    entered = resolve
  })
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  await useRelayForHttpMocks(page)
  await page.route('**/plugins/draft-pages/setup-drafts*', async (route) => {
    const url = new URL(route.request().url())
    if (route.request().method() !== 'GET' || !url.pathname.endsWith('/setup-drafts')) {
      await route.continue()
      return
    }

    const response = await route.fetch()
    const value = await response.json()
    if (url.searchParams.has('cursor')) {
      entered()
      await held
    }

    await route.fulfill({
      response,
      json: {
        ...value,
        items: value.items.slice(0, 1),
        nextCursor: value.items.length > 1 ? value.items[0].id : null,
      },
    })
  })
  await page.goto(workspacePath('/plugins/draft-pages'))
  await signIn(page)
  await secondPage
  await expect(page.getByRole('button', { name: `Continue ${last.name}`, exact: true })).not.toBeVisible()
  release()
  await page.getByRole('button', { name: `Continue ${last.name}`, exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Draft pages' })
  await expect(setup.getByLabel('Label', { exact: true })).toHaveValue(last.settings.label)
  await setup.getByRole('button', { name: 'Close dialog' }).click()
  await page.getByRole('button', { name: `Discard ${last.name}`, exact: true }).click()
  await page.getByRole('dialog', { name: `Discard ${last.name}?` }).getByRole('button', { name: 'Discard setup', exact: true }).click()
  await expect(page.getByRole('button', { name: `Continue ${last.name}`, exact: true })).not.toBeVisible()
  expect((await workspace.api('/api/plugins/draft-pages/setup-drafts')).items.map((item: any) => item.id)).not.toContain(last.id)
})

for (const prepared of [false, true]) {
  test(`uncertain ${prepared ? 'prepared' : 'active'} resource creation requires reconciliation without a second POST`, async ({ page, workspace }) => {
    test.setTimeout(90000)
    await useRelayForHttpMocks(page)
    const id = `reconcile-${prepared ? 'prepared' : 'active'}`
    const manifest = {
      id,
      name: 'Resource reconcile',
      description: 'Save result verification.',
      version: '1',
      apiVersion: 2,
      capabilities: ['resources'],
      resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
    }
    const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
    await workspace.api(`/api/plugins/packages/${id}/approve`, 'POST', { version: '1', digest: installed.digest })
    const instance = await workspace.api(`/api/plugins/${id}/${prepared ? 'setup-drafts' : 'connections'}`, 'POST', { name: 'Application', ...(prepared ? {} : { enabled: true }) })
    const root = `/plugins/${id}/${prepared ? 'setup-drafts' : 'connections'}/${instance.id}/resources/workspace`
    let writes = 0
    let reads = 0
    let failRead = false
    let committed = false
    await page.route(`**${root}`, async (route) => {
      if (route.request().method() === 'GET') {
        reads++
        if (failRead) {
          failRead = false
          return route.fulfill({ status: 503, json: { error: 'Synthetic read outage.' } })
        }

        return route.fallback()
      }

      if (route.request().method() !== 'POST')
        return route.fallback()
      writes++
      const response = await route.fetch()
      expect(response.ok()).toBe(true)
      committed = true
      await route.abort('failed')
    })
    await page.goto(workspacePath(`/plugins/${id}`))
    await signIn(page)
    if (prepared)
      await page.getByRole('button', { name: 'Continue Application', exact: true }).click()
    await page.getByRole('button', { name: 'Add workspace', exact: true }).click()
    const form = page.getByRole('form', { name: 'New workspace', exact: true })
    await form.getByLabel('Name', { exact: true }).fill('Accepted but unacknowledged')
    const save = form.getByRole('button', { name: 'Save workspace', exact: true })
    await save.click()
    await expect.poll(() => committed).toBe(true)
    await expect(form.getByRole('button', { name: 'Cancel', exact: true })).toBeEnabled()
    expect((await workspace.api(`/api${root}`)).items).toHaveLength(1)
    await expect(save).toBeDisabled()
    expect(writes).toBe(1)
    const before = reads
    failRead = true
    const reconcile = form.getByRole('button', { name: prepared ? 'Cancel' : 'Check save result', exact: true })
    await reconcile.click()
    await expect.poll(() => reads).toBeGreaterThan(before)
    await expect(reconcile).toBeEnabled()
    await expect(save).toBeDisabled()
    await expect(page.getByRole('button', { name: 'Add workspace', exact: true })).toBeDisabled()
    expect(writes).toBe(1)
    await reconcile.click()
    await expect(form).toHaveCount(0)
    await expect(page.getByRole('article', { name: 'Accepted but unacknowledged', exact: true })).toHaveCount(1)
    expect((await workspace.api(`/api${root}`)).items).toHaveLength(1)
    expect(writes).toBe(1)
  })
}

test('agent pages show scoped resource rights independently from instance grants', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await useRelayForHttpMocks(page)
  const reads: string[] = []
  let schemaReads = 0
  await page.route('**/api/plugins/packages/agent-resource-access', async (route) => {
    if (schemaReads++ === 0)
      await route.fulfill({ status: 503, json: { error: 'Schema temporarily unavailable' } })
    else
      await route.continue()
  })
  page.on('request', (request) => {
    if (request.method() === 'GET')
      reads.push(new URL(request.url()).pathname)
  })
  const { agent, path, instance } = await resourceAccessFixture(workspace, 'agent-resource-access')
  await resourceAccessFixture(workspace, 'agent-resource-access-other')
  await workspace.api('/api/plugins/agent-resource-access/connections', 'POST', { name: 'Application', enabled: true })
  await workspace.api(path, 'POST', { agentId: agent.id, profile: 'read', scopes: { channel: ['C111'] } })
  await page.goto(workspacePath(`/agents/${agent.id}`))
  await signIn(page)
  await expect(page.getByRole('region', { name: 'What this agent can access', exact: true }).getByRole('button', { name: /Secrets & plugins/ })).toContainText('1 saved resource rule')
  await openAgentSection(page, 'Secrets & plugins')
  const access = page.getByRole('region', { name: 'Resource operation access', exact: true })
  await expect(access.getByRole('article', { name: 'Saved resource access rule' })).toHaveCount(1)
  await expect(access.getByText('channel: C111', { exact: true })).toBeVisible()
  await access.getByRole('button', { name: 'Revoke resource rule', exact: true }).click()
  await access.getByRole('button', { name: 'Confirm revoke', exact: true }).click()
  await expect(access.getByText('No saved resource access rules.', { exact: true })).toBeVisible()
  expect((await workspace.api(path)).items).toEqual([])
  expect(reads.some(path => path.endsWith('/api/plugins'))).toBe(false)
  expect(reads.some(path => path.endsWith('/api/plugins/packages/agent-resource-access'))).toBe(false)
  await selectField(access, 'Resource app instance', `Application · ${instance.id} · agent-resource-access (Resource access)`)
  await expect(access.getByRole('button', { name: 'Retry permission schema', exact: true })).toBeVisible()
  await expect(access.getByRole('combobox', { name: 'Resource collection', exact: true })).toHaveCount(0)
  await access.getByRole('button', { name: 'Retry permission schema', exact: true }).click()
  await selectField(access, 'Resource collection', 'Workspaces')
  await selectField(access, 'Resource for agent access', 'Engineering')
  expect(reads.some(path => path.endsWith('/api/plugins/packages/agent-resource-access'))).toBe(true)
  expect(reads.some(path => path.endsWith('/api/plugins/packages/agent-resource-access-other'))).toBe(false)
  expect(reads.some(path => path.includes('/resource-choices/workspace'))).toBe(true)
  await access.getByRole('button', { name: 'Agent access', exact: true }).click()
  const editor = page.getByRole('dialog', { name: 'Agent access · Engineering', exact: true })
  await editor.getByRole('button', { name: 'Add access rule', exact: true }).click()
  await expect(editor.getByRole('combobox', { name: 'Agent', exact: true })).toHaveCount(0)
  await selectField(editor, 'Profile', 'Contribution')
  await editor.getByLabel('Limit channel', { exact: true }).fill('C222')
  await editor.getByRole('button', { name: 'Grant access', exact: true }).click()
  await expect(editor.getByRole('button', { name: 'Add access rule', exact: true })).toBeVisible()
  const rules = (await workspace.api(path)).items
  expect(rules).toHaveLength(1)
  expect(rules[0]).toMatchObject({ agentId: agent.id, profile: 'contribute', scopes: { channel: ['C222'] } })
  const saved = (await workspace.api('/api/agents')).find((item: any) => item.id === agent.id)
  expect(saved.plugins).toEqual([])
})

test('bot warnings include the current instance and new draft plugin grants', async ({ page, workspace }) => {
  const helper = await workspace.api('/api/agents', 'POST', {
    name: 'Plugin helper',
    access: {
      projects: [],
      mcps: [],
      installation: false,
      sandbox: 'read-only',
    },
  })
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'hybrid',
    name: 'Hybrid',
    version: '1.0.0',
    apiVersion: 2,
    description: 'A plugin that both answers people and offers tools to agents.',
    capabilities: ['conversations.submit'],
    instances: { singular: 'Bot', plural: 'Bots' },
    agentAccess: { description: 'Agents can use this bot’s tools.' },
    settings: [],
  }
  const connection: PluginConnection = {
    id: 'hybrid-bot',
    pluginId: 'hybrid',
    name: 'Hybrid bot',
    settings: {},
    agentIds: [helper.id],
    agentId: helper.id,
    projectId: null,
    accessMode: 'everyone',
    userIds: [],
    people: [],
    requests: [],
    enabled: false,
    status: { state: 'paused' },
    usage: { hour: 0 },
    activity: [],
    maxMessagesPerHour: 20,
    maxPerPersonPerHour: 5,
  }
  await page.route('**/plugins/packages/hybrid', route => route.fulfill({
    json: {
      id: 'hybrid',
      builtin: false,
      status: 'active',
      active: '1.0.0',
      setup: { steps: [{ id: 'people', kind: 'people', title: 'Who can talk to it' }, { id: 'agents', kind: 'agents', title: 'Agents' }] },
      manifest,
      capabilities: [],
      instanceReads: true,
      accessReads: true,
    },
  }))
  await page.route('**/plugins/packages/hybrid/connections', route => route.fulfill({
    json: { items: [connection], nextCursor: null },
  }))
  await page.route('**/plugins/instance-access', route => route.fulfill({
    json: {
      items: [{
        id: connection.id,
        pluginId: 'hybrid',
        pluginName: 'Hybrid',
        name: connection.name,
        enabled: connection.enabled,
        agentIds: connection.agentIds,
        resourceAgentIds: [],
      }],
      nextCursor: null,
    },
  }))
  await page.goto(workspacePath('/plugins/hybrid'))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit Hybrid bot', exact: true }).click()
  const edited = page.getByRole('dialog', { name: 'Edit Hybrid bot' })
  await expect(edited.getByRole('status').filter({ hasText: 'they can use its plugins' })).toBeVisible()
  await edited.getByRole('button', { name: 'Cancel', exact: true }).click()

  // Another instance has no grant: only the new draft can provide plugin access.
  connection.agentIds = []
  await page.reload()
  await page.getByRole('button', { name: 'Add bot', exact: true }).click()
  const draft = page.getByRole('dialog', { name: 'New bot' })
  await draft.getByRole('radio', { name: /^Anyone/ }).check()
  await draft.getByRole('radio', { name: /Plugin helper/ }).check()
  await expect(draft.getByText(/can use everything Plugin helper can do/)).toBeHidden()
  await draft.getByRole('button', { name: 'Continue', exact: true }).click()
  await draft.getByRole('checkbox', { name: /Plugin helper/ }).check()
  await expect(draft.getByRole('status').filter({ hasText: 'they can use its plugins' })).toBeVisible()
  await draft.getByRole('checkbox', { name: /Plugin helper/ }).uncheck()
  await expect(draft.getByText(/can use everything Plugin helper can do/)).toBeHidden()
})

test('a package with device sign-in saves an account without the token reaching the browser', async ({ page }) => {
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'codehost',
    name: 'Code host',
    version: '1.0.0',
    apiVersion: 2,
    description: 'Synthetic device sign-in fixture.',
    capabilities: ['http'],
    instances: { singular: 'Account', plural: 'Accounts' },
    credential: {
      label: 'Code host token',
      header: 'authorization',
      prefix: 'Bearer ',
      deviceFlow: { authorizationUrl: 'https://code.example/device/code', tokenUrl: 'https://code.example/token', clientId: 'fixture' },
    },
    settings: [{
      key: 'login',
      title: 'Account',
      type: 'string',
      hidden: true,
    }],
    instanceForm: true,
  }
  await page.route('**/api/plugins/packages/codehost', route => route.fulfill({
    json: {
      id: 'codehost',
      builtin: true,
      status: 'active',
      active: '1.0.0',
      setup: { effort: 'signin', steps: [{ id: 'credential', kind: 'credential', title: 'Sign in with Code host' }] },
      manifest,
      capabilities: [],
      instanceReads: true,
      accessReads: true,
    },
  }))
  await page.route('**/api/plugins/packages/codehost/connections', route => route.fulfill({ json: { items: [] } }))
  await page.route('**/api/plugins/instance-access', route => route.fulfill({ json: { items: [] } }))
  const flow = (id: string, state: string) => ({
    id,
    pluginId: 'codehost',
    state,
    userCode: id === 'first' ? 'FIRS-T000' : 'ABCD-1234',
    verificationUri: 'https://code.example/device',
    verificationUriComplete: `https://code.example/device?code=${id}`,
    expiresAt: Date.now() + 900000,
  })
  const started: string[] = []
  const cancelled: string[] = []
  let polls = 0
  await page.route('**/api/plugins/codehost/device-flow', async (route) => {
    const id = started.length ? 'second' : 'first'
    started.push(id)
    await route.fulfill({ json: flow(id, 'pending') })
  })
  await page.route('**/api/plugins/codehost/device-flow/*', async (route) => {
    const id = new URL(route.request().url()).pathname.split('/').at(-1)!
    if (route.request().method() === 'DELETE') {
      cancelled.push(id)
      return route.fulfill({ json: { cancelled: true } })
    }

    polls++
    await route.fulfill({ json: flow(id, id === 'second' && polls > 1 ? 'completed' : 'pending') })
  })
  const completions: Record<string, any>[] = []
  await page.route('**/api/plugins/codehost/form', async (route) => {
    const body = route.request().postDataJSON()
    completions.push(body)
    const signedIn = body.credentialFlow === 'second'
    await route.fulfill({
      json: {
        options: {},
        values: signedIn ? { login: 'octocat' } : {},
        errors: {},
        view: signedIn ? [{ type: 'alert', tone: 'success', title: 'Signed in as octocat' }] : [],
      },
    })
  })
  const saves: Record<string, any>[] = []
  await page.route('**/api/plugins/codehost/connections', async (route) => {
    saves.push(route.request().postDataJSON())
    await route.fulfill({ json: {} })
  })
  await page.goto(workspacePath('/plugins/codehost'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  const editor = page.getByRole('dialog', { name: 'New account' })
  await expect(editor.getByLabel('Code host token')).toHaveAttribute('required', '')

  // A sign-in shows its code and the service's page, and can be cancelled.
  await editor.getByRole('button', { name: 'Sign in with Code host', exact: true }).click()
  await expect(editor.getByLabel('Verification code', { exact: true })).toHaveText('FIRS-T000')
  await expect(editor.getByRole('link', { name: 'Open Code host' })).toHaveAttribute('href', 'https://code.example/device?code=first')
  await editor.getByRole('button', { name: 'Cancel sign-in', exact: true }).click()
  await expect.poll(() => cancelled).toEqual(['first'])
  await expect(editor.getByLabel('Verification code', { exact: true })).toHaveCount(0)

  // Polled every 2 s until it completes; the form handler then says who signed in.
  await editor.getByRole('button', { name: 'Sign in with Code host', exact: true }).click()
  await expect(editor.getByLabel('Verification code', { exact: true })).toHaveText('ABCD-1234')
  await expect(editor.getByText('Signed in. Save to keep this account.')).toBeVisible({ timeout: 10000 })
  await expect(editor.getByText('Signed in as octocat')).toBeVisible()
  expect(completions.at(-1)).toMatchObject({ credentialFlow: 'second', changed: 'credential' })
  expect(completions.at(-1)).not.toHaveProperty('token')
  await expect(editor.getByLabel('Code host token')).not.toHaveAttribute('required', '')
  await editor.getByLabel('Name', { exact: true }).fill('Work')
  await editor.getByRole('button', { name: 'Save account', exact: true }).click()
  await expect(editor).toHaveCount(0)
  expect(saves).toHaveLength(1)
  expect(saves[0]).toMatchObject({ name: 'Work', credentialFlow: 'second', settings: { login: 'octocat' } })
  expect(saves[0]).not.toHaveProperty('token')
  // The save consumed the sign-in: nothing is left to cancel.
  expect(cancelled).toEqual(['first'])
})
