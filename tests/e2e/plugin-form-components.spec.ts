import type { ViteDevServer } from 'vite'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import vue from '@vitejs/plugin-vue'
import { createServer } from 'vite'
import whitespace from '../fixtures/plugin-form-whitespace.json' with { type: 'json' }

// Exercise the actual panel -> form -> action HTTP seam with synthetic admitted
// view values. No installation/backend fixture or manifest defaults are needed.
let server: ViteDevServer
let origin: string

test.beforeAll(async () => {
  server = await createServer({
    configFile: false,
    root: process.cwd(),
    optimizeDeps: {
      entries: [
        'apps/web/src/components/PluginInstancePanel.vue',
        'apps/web/src/components/PluginResources.vue',
      ],
    },
    plugins: [vue(), {
      name: 'plugin-panel-test-page',
      configureServer(server) {
        server.middlewares.use(async (request, response, next) => {
          if (request.url === '/plugin-resource-authorization-fixture') {
            const html = await server.transformIndexHtml('/plugin-resource-authorization-fixture', `
              <div id="app"></div>
              <script type="module">
                import { createApp, h } from 'vue'
                import PluginResources from '/apps/web/src/components/PluginResources.vue'
                createApp({ render: () => h(PluginResources, {
                  plugin: 'forms', instance: 'example', manifest: {
                    version: '1',
                    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace', settings: [] }],
                    oauth: Array.from({ length: 8 }, (_, index) => ({
                      name: 'provider' + index, title: 'Provider ' + index, collection: 'workspace', scopes: ['messages:read']
                    }))
                  }
                }) }).mount('#app')
              </script>`)
            response.setHeader('Content-Type', 'text/html')
            response.end(html)
            return
          }

          if (request.url === '/plugin-resource-replacement-fixture') {
            const html = await server.transformIndexHtml('/plugin-resource-replacement-fixture', `
              <div id="app"></div>
              <script type="module">
                import { computed, createApp, h, ref } from 'vue'
                import PluginResources from '/apps/web/src/components/PluginResources.vue'
                const version = ref('old')
                const instance = ref('example')
                const manifest = computed(() => ({ version: version.value, resources: [
                  { name: 'parents', title: 'Parents', singular: 'Parent', settings: [] },
                  { name: 'children', title: 'Children', singular: 'Child', parent: 'parents',
                    settings: version.value === 'old' ? [] : [{ key: 'required', title: 'Required value', type: 'string', required: true }] }
                ] }))
                createApp({ render: () => h('main', [
                  h('button', { onClick: () => version.value = 'new' }, 'Replace schema'),
                  h('button', { onClick: () => instance.value = 'replacement' }, 'Replace instance'),
                  h('p', \`Schema \${version.value} · Instance \${instance.value}\`),
                  h(PluginResources, { plugin: 'forms', instance: instance.value, manifest: manifest.value })
                ]) }).mount('#app')
              </script>
`)
            response.setHeader('Content-Type', 'text/html')
            response.end(html)
            return
          }

          if (request.url === '/plugin-resource-fixture') {
            const html = await server.transformIndexHtml('/plugin-resource-fixture', `
              <div id="app"></div>
              <script type="module">
                import { createApp, h } from 'vue'
                import PluginResources from '/apps/web/src/components/PluginResources.vue'
                document.getElementById('app').dataset.fixtureReady = 'true'
                createApp({ render: () => h(PluginResources, {
                  plugin: 'forms', instance: 'example', manifest: {
                    resources: [{
                      name: 'constructor', title: 'Workspaces', singular: 'Workspace', settings: [
                        { key: 'constructor', title: 'Constructor value', type: 'string', default: 'Declared default' },
                        { key: 'title', title: 'Optional title', type: 'string' },
                        { key: 'count', title: 'Count', type: 'number', default: 0 },
                        { key: 'enabled', title: 'Feature enabled', type: 'boolean', default: true },
                        { key: 'scopes', title: 'Scopes', type: 'multiselect', default: ['read'], options: [{ value: 'read', label: 'Read' }] },
                        { key: 'region', title: 'Region', type: 'select', default: ' eu ', options: [{ value: ' eu ', label: 'Europe' }] },
                        { key: 'password', title: 'Workspace secret', type: 'secret' }
                      ]
                    }]
                  }
                }) }).mount('#app')
              </script>`)
            response.setHeader('Content-Type', 'text/html')
            response.end(html)
            return
          }

          if (request.url !== '/plugin-panel-fixture')
            return next()
          const html = await server.transformIndexHtml('/plugin-panel-fixture', `
            <div id="app"></div>
            <script type="module">
              import { createApp, h } from 'vue'
              import PluginInstancePanel from '/apps/web/src/components/PluginInstancePanel.vue'
              createApp({ render: () => h(PluginInstancePanel, {
                plugin: 'forms', instance: 'example', name: 'Whitespace'
              }) }).mount('#app')
            </script>`)
          response.setHeader('Content-Type', 'text/html')
          response.end(html)
        })
      },
    }],
    server: { host: '127.0.0.1', port: 0 },
  })
  await server.listen()
  origin = server.resolvedUrls!.local[0]
})

test.afterAll(async () => {
  await server?.close()
})

test('resource OAuth reads only requested providers and keeps that choice across resource revisions', async ({ page }) => {
  test.setTimeout(90000)
  let revision = 'resource0'
  const statusReads: string[] = []
  const root = '/api/plugins/forms/connections/example/resources/workspace'
  // A native resource page admits up to 100 small records, and a package up to 8
  // OAuth providers. 41 × 8 eager reads alone exceed the 300 account GET/minute cap.
  await page.route(`**${root}`, route => route.fulfill({
    json: {
      items: Array.from({ length: 41 }, (_, index) => ({
        id: `resource${index}`,
        collection: 'workspace',
        name: `Workspace ${index}`,
        revision,
        enabled: true,
        settings: {},
        secretKeys: [],
      })),
      nextCursor: null,
    },
  }))
  await page.route(`**${root}/*/oauth/*`, (route) => {
    statusReads.push(new URL(route.request().url()).pathname)
    return route.fulfill({ json: { status: 'disconnected' } })
  })
  await page.goto(`${origin}plugin-resource-authorization-fixture`, { waitUntil: 'load' })
  await expect(page.getByRole('heading', { name: 'Provider 0', exact: true })).toHaveCount(41)
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
  expect(statusReads).toEqual([])
  await expect(page.getByRole('button', { name: 'Checking authorization…', exact: true })).toHaveCount(0)
  const first = page.getByRole('article', { name: 'Workspace 0', exact: true }).getByRole('region', { name: 'Provider 0', exact: true })
  await first.getByRole('button', { name: 'Manage authorization', exact: true }).click()
  await expect(first.getByRole('checkbox', { name: 'messages:read', exact: true })).toBeVisible()
  expect(statusReads).toEqual([`${root}/resource0/oauth/provider0`])
  revision = 'resource1'
  await page.getByRole('button', { name: 'Refresh resources', exact: true }).click()
  await expect.poll(() => statusReads.length).toBe(2)
  await expect(first.getByRole('checkbox', { name: 'messages:read', exact: true })).toBeVisible()
  expect(statusReads).toEqual(Array.from({ length: 2 }).fill(`${root}/resource0/oauth/provider0`))
})

for (const code of ['0085', '001C', '001D', '001E', '001F', 'FEFF', '00A0', '2007', '202F', '200B', '2060']) {
  test(`panel action U+${code} whitespace matches Kotlin for opening, edits and raw secrets`, async ({ page }) => {
    test.setTimeout(90000)
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    const fields = [
      { key: 'note', title: 'Note', type: 'text' },
      { key: 'password', title: 'Password', type: 'secret' },
      { key: 'arrayFlag', title: 'Array flag', type: 'boolean' },
      { key: 'stringArrayFlag', title: 'String array flag', type: 'boolean' },
      { key: 'objectFlag', title: 'Object flag', type: 'boolean' },
      { key: 'primitive', title: 'Primitive flag', type: 'boolean' },
      {
        key: 'choice',
        title: 'Choice',
        type: 'select',
        required: true,
        options: [{ value: '\u00A0', label: 'Exact whitespace ID' }],
      },
    ]
    let value = ''
    const actions: Record<string, unknown>[] = []
    await page.route('**/api/plugins/forms/connections/example/view', route => route.fulfill({
      json: {
        view: [{
          type: 'form',
          fields,
          values: {
            note: value,
            choice: '\u00A0',
            arrayFlag: [true],
            stringArrayFlag: ['true'],
            objectFlag: { value: true },
            primitive: true,
          },
          submit: { label: 'Apply', action: 'apply' },
        }],
      },
    }))
    await page.route('**/api/plugins/forms/connections/example/actions', async (route) => {
      const body = route.request().postDataJSON()
      expect(body.action).toBe('apply')
      actions.push(body.values)
      await route.fulfill({ json: {} })
    })

    const character = String.fromCodePoint(Number.parseInt(code, 16))
    const blank = whitespace.blank.includes(code)
    for (const input of [character, `${character}x`, `x${character}`, `${character}x${character}`]) {
      value = input
      await page.goto(`${origin}plugin-panel-fixture`)
      const panel = page.getByRole('region', { name: 'Whitespace panel' })
      await expect(panel.getByLabel('Note', { exact: true })).toHaveValue(input)
      await expect(panel.getByLabel('Password', { exact: true })).toHaveValue('')
      await expect(panel.getByLabel('Array flag', { exact: true })).not.toBeChecked()
      await expect(panel.getByLabel('String array flag', { exact: true })).not.toBeChecked()
      await expect(panel.getByLabel('Object flag', { exact: true })).not.toBeChecked()
      await expect(panel.getByLabel('Primitive flag', { exact: true })).toBeChecked()
      await panel.getByLabel('Password', { exact: true }).fill(input)
      const normalized = blank ? input.replaceAll(character, '') : input
      const flags = {
        arrayFlag: false,
        stringArrayFlag: false,
        objectFlag: false,
        primitive: true,
      }
      const expected = { ...flags, choice: '\u00A0', ...(normalized ? { note: normalized, password: input } : {}) }
      const count = actions.length
      await panel.getByRole('button', { name: 'Apply', exact: true }).click()
      await expect.poll(() => actions.length).toBe(count + 1)
      expect(actions[count], `opening U+${code}`).toEqual(expected)
      await expect(panel.getByLabel('Password', { exact: true })).toHaveValue('')

      // Edit through the native DOM controls, then observe the real action POST.
      const edited = `${character}edited${character}`
      await panel.getByLabel('Note', { exact: true }).fill(edited)
      await panel.getByLabel('Password', { exact: true }).fill(edited)
      await panel.getByRole('button', { name: 'Apply', exact: true }).click()
      await expect.poll(() => actions.length).toBe(count + 2)
      expect(actions[count + 1], `edited U+${code}`).toEqual({
        ...flags,
        choice: '\u00A0',
        note: blank ? 'edited' : edited,
        password: edited,
      })
    }

    expect(errors).toEqual([])
  })
}

test('a declared constructor collection stays visible while its resources load', async ({ page }) => {
  test.setTimeout(90000)
  let releaseResources!: () => void
  let resourcesEntered!: () => void
  const held = new Promise<void>((resolve) => {
    releaseResources = resolve
  })
  const entered = new Promise<void>((resolve) => {
    resourcesEntered = resolve
  })
  let saved: Record<string, unknown> | undefined
  let submitted: Record<string, unknown> | undefined
  await page.route('**/api/plugins/forms/connections/example/resources/constructor', async (route) => {
    if (route.request().method() === 'POST') {
      const input = route.request().postDataJSON()
      submitted = input
      saved = {
        id: 'resource',
        revision: 'saved',
        collection: 'constructor',
        name: input.name,
        enabled: input.enabled,
        settings: input.settings,
        secretKeys: Object.keys(input.secrets ?? {}),
      }
      return route.fulfill({ json: saved })
    }

    resourcesEntered()
    await held
    await route.fulfill({ json: { items: saved ? [saved] : [] } })
  })
  try {
    await page.goto(`${origin}plugin-resource-fixture`, { waitUntil: 'commit' })
    await page.waitForFunction(() => document.getElementById('app')?.dataset.fixtureReady === 'true')
    await expect(page.getByRole('button', { name: 'Add workspace', exact: true })).toBeVisible()
    await entered
    await expect(page.getByRole('button', { name: 'Add workspace', exact: true })).toBeDisabled()
  }
  finally {
    releaseResources()
  }

  await expect(page.getByRole('button', { name: 'Add workspace', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Add workspace', exact: true }).click()
  const form = page.getByRole('form', { name: 'New workspace', exact: true })
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
  await form.getByRole('button', { name: 'Save workspace', exact: true }).click()
  await expect(page.getByRole('article', { name: 'Engineering', exact: true })).toBeVisible()
  expect(saved?.name).toBe('Engineering')
  expect(submitted?.settings).toEqual({
    constructor: 'Declared default',
    count: 3.5,
    enabled: false,
    scopes: [],
    region: ' eu ',
  })
  expect(submitted?.secrets).toEqual({ password: '\uFEFFsynthetic-resource-secret\uFEFF' })
})

for (const replacement of ['schema', 'instance', 'failed schema read']) {
  test(`replacing ${replacement} discards an unfinished resource editor opening`, async ({ page }) => {
    test.setTimeout(90000)
    let release!: () => void
    let entered!: () => void
    const held = new Promise<void>((resolve) => {
      release = resolve
    })
    const waiting = new Promise<void>((resolve) => {
      entered = resolve
    })
    let laterReads = 0
    let firstReads = 0
    let submitted: { instance: string, body: Record<string, unknown> } | undefined
    const resource = (id: string, name: string) => ({
      id,
      name,
      revision: id,
      collection: 'parents',
      enabled: true,
      settings: {},
      secretKeys: [],
    })
    await page.route('**/api/plugins/forms/connections/*/resources/**', async (route) => {
      const url = new URL(route.request().url())
      const instance = url.pathname.split('/connections/')[1].split('/')[0]
      if (url.pathname.endsWith('/children')) {
        if (route.request().method() === 'POST') {
          submitted = { instance, body: route.request().postDataJSON() }
          return route.fulfill({ json: {} })
        }

        return route.fulfill({ json: { items: [] } })
      }

      if (!url.searchParams.has('cursor')) {
        firstReads++
        return route.fulfill({ json: { items: [resource('first', 'First parent')], nextCursor: 'later' } })
      }

      laterReads++
      if (laterReads === 1) {
        entered()
        await held
        return replacement === 'failed schema read'
          ? route.fulfill({ status: 400, json: { error: 'Obsolete directory failure' } })
          : route.fulfill({ json: { items: [resource('stale', 'Obsolete parent')] } })
      }

      return route.fulfill({ json: { items: [resource('fresh', 'Fresh parent')] } })
    })
    try {
      await page.goto(`${origin}plugin-resource-replacement-fixture`)
      const add = page.getByRole('button', { name: 'Add child', exact: true })
      await expect(add).toBeEnabled()
      await add.click()
      await waiting
      await page.getByRole('button', { name: replacement === 'instance' ? 'Replace instance' : 'Replace schema', exact: true }).click()
      await expect(page.getByText(replacement === 'instance' ? 'Schema old · Instance replacement' : 'Schema new · Instance example', { exact: true })).toBeVisible()
      await expect.poll(() => firstReads).toBe(2)
    }
    finally {
      release()
    }

    const add = page.getByRole('button', { name: 'Add child', exact: true })
    await expect(add).toBeEnabled()
    await expect(page.getByRole('form', { name: 'New child', exact: true })).not.toBeVisible()
    await expect(page.getByText('Obsolete parent', { exact: true })).not.toBeVisible()
    await expect(page.getByText('Obsolete directory failure', { exact: true })).not.toBeVisible()
    await add.click()
    const form = page.getByRole('form', { name: 'New child', exact: true })
    await form.getByLabel('Name', { exact: true }).fill('Current child')
    await form.getByRole('combobox', { name: 'Parent', exact: true }).fill('Fresh parent')
    await form.getByRole('option', { name: 'Fresh parent', exact: true }).click()
    if (replacement !== 'instance')
      await form.getByLabel('Required value', { exact: true }).fill('Reviewed new schema')
    await form.getByRole('button', { name: 'Save child', exact: true }).click()
    await expect(form).not.toBeVisible()
    expect(submitted?.instance).toBe(replacement === 'instance' ? 'replacement' : 'example')
    expect(submitted?.body.settings).toEqual(replacement === 'instance' ? {} : { required: 'Reviewed new schema' })
    expect(submitted?.body.parentId).toBe('fresh')
  })
}
