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
    plugins: [vue(), {
      name: 'plugin-panel-test-page',
      configureServer(server) {
        server.middlewares.use(async (request, response, next) => {
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

test('panel action whitespace matches Kotlin for opening, edits and raw secrets', async ({ page }) => {
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

  for (const code of ['0085', '001C', '001D', '001E', '001F', 'FEFF', '00A0', '2007', '202F', '200B', '2060']) {
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
  }

  expect(errors).toEqual([])
})
