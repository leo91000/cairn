import type { ViteDevServer } from 'vite'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import vue from '@vitejs/plugin-vue'
import { createServer } from 'vite'

// Keep the actual picker and Vue catalog update, with no installation fixture.
let server: ViteDevServer
let origin: string

test.beforeAll(async () => {
  server = await createServer({
    configFile: false,
    root: process.cwd(),
    plugins: [vue(), {
      name: 'assistant-picker-test-page',
      configureServer(server) {
        server.middlewares.use(async (request, response, next) => {
          if (!request.url?.startsWith('/assistant-picker-fixture'))
            return next()
          const html = await server.transformIndexHtml('/assistant-picker-fixture', `
            <div id="app"></div>
            <script type="module">
              import { createApp, h, nextTick } from 'vue'
              import AssistantPicker from '/apps/web/src/components/AssistantPicker.vue'
              import { modelCatalogs } from '/apps/web/src/models.ts'
              // Control catalog completion independently of the pointer sequence.
              modelCatalogs.opencode.load = async () => {}
              window.populateCatalog = async () => {
                Object.assign(modelCatalogs.opencode.catalog, {
                  loaded: true,
                  models: [{
                    model: 'auto/ling-3-1-flash-free',
                    displayName: 'Ling 3.1 Flash Free',
                    routes: [
                      { model: 'opencode-go/ling-3.1-flash-free', providerName: 'OpenCode Go' },
                      { model: 'opencode/ling-3.1-flash-free', providerName: 'OpenCode Zen' }
                    ]
                  }]
                })
                await nextTick()
              }
              createApp({ render: () => h(AssistantPicker, {
                provider: 'opencode',
                model: 'auto/ling-3-1-flash-free',
                variant: new URLSearchParams(location.search).get('variant')
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

for (const variant of ['pill', 'field']) {
  test(`the ${variant} model picker opens when its catalog labels arrive during a click`, async ({ page }) => {
    await page.goto(`${origin}assistant-picker-fixture?variant=${variant}`)
    const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
    const label = picker.getByText('auto/ling-3-1-flash-free', { exact: true })
    await expect(label).toBeVisible()
    const bounds = (await label.boundingBox())!
    // Stay inside both the raw ID and display name as the catalog replaces the text.
    await page.mouse.move(bounds.x + 20, bounds.y + bounds.height / 2)
    await page.mouse.down()
    await page.evaluate('populateCatalog()')
    await expect(picker).toHaveAccessibleName(/Ling 3\.1 Flash Free/)
    await page.mouse.up()

    const dialog = page.getByRole('dialog', { name: 'Agent & model' })
    await expect(dialog).toBeVisible()
    const connection = dialog.getByRole('radiogroup', { name: 'Connection' })
    await expect(connection.getByRole('radio', { name: 'Auto', exact: true })).toHaveAttribute('aria-checked', 'true')
    await dialog.getByRole('button', { name: 'Close', exact: true }).click()
    await expect(picker).toBeFocused()
    await page.keyboard.press('Enter')
    await expect(dialog).toBeVisible()
  })
}
