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
