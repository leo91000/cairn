import type { Route } from '@playwright/test'
import { Buffer } from 'node:buffer'
import {
  expect,
  openAgentSection,
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
  expect((await saving).postDataJSON().reapproveDelegations).toEqual([botId])
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
  expect((await direct).postDataJSON()).not.toHaveProperty('reapproveDelegations')
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
  expect((await unchecked).postDataJSON()).not.toHaveProperty('reapproveDelegations')
  await expect(review).toHaveCount(0)
  // Checked deliberately, it is reapproved.
  await describe('Answers support questions in Acme')
  await earlier.check()
  const checked = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/agents/${helper.id}`))
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  expect((await checked).postDataJSON().reapproveDelegations).toEqual([botId])
  await expect(review).toHaveCount(0)
  await page.goto(workspacePath('/plugins/discord'))

  await bot.getByRole('button', { name: 'Delete #support on Acme' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete bot' }).click()
  await expect(page.getByText('No bots yet. Add one to start using this plugin.')).toBeVisible()
})

test('TOML packages wait for review and connect through a generated form', async ({ page, workspace }) => {
  test.setTimeout(90000)
  // Split into a module, loaded with require.
  const source = 'local notes = require("./notes")\nreturn { start = function(event) cairn.storage.set("started", notes.started) end }'
  const module = 'return { started = true }'
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
[[settings]]
key = "team"
title = "Team"
type = "string"
required = true
maxLength = ${maxLength}
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

test('stored versions that fail admission explain why and remain removable', async ({ page }) => {
  await useRelayForHttpMocks(page)
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
  await page.route('**/api/plugins', route => route.fulfill({
    json: {
      packages: [{
        id: 'forms',
        builtin: false,
        status: 'active',
        active: '1.0.0',
        manifest,
        versions: [],
      }],
      capabilities: [],
      connections: [connection],
    },
  }))
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
