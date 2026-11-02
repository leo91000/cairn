import type { Page } from '@playwright/test'
import type { AddressInfo } from 'node:net'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { test as base, expect } from '@playwright/test'
import { build, preview } from 'vite'

// Exercise the shipped chunks without starting agents or needing account credentials.
const previousBuildId = '0'.repeat(64)
const test = base.extend<object, { webUrl: string, previousBuild: string }>({
  // eslint-disable-next-line no-empty-pattern
  previousBuild: [async ({}, use) => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-previous-web-'))
    try {
      await build({
        configFile: 'apps/web/vite.config.ts',
        logLevel: 'silent',
        define: { __CAIRN_WEB_BUILD_ID__: JSON.stringify(previousBuildId) },
        build: { outDir: directory },
      })
      await use(directory)
    }
    finally {
      await rm(directory, { recursive: true, force: true })
    }
  }, { scope: 'worker' }],
  // Playwright requires destructuring even with no fixture dependencies.
  // eslint-disable-next-line no-empty-pattern
  webUrl: [async ({}, use) => {
    const server = await preview({ configFile: 'apps/web/vite.config.ts', preview: { host: '127.0.0.1', port: 0 } })
    try {
      await use(`http://127.0.0.1:${(server.httpServer.address() as AddressInfo).port}`)
    }
    finally {
      await new Promise<void>((resolve, reject) => server.httpServer.close(error => error ? reject(error) : resolve()))
    }
  }, { scope: 'worker' }],
})

async function account(page: Page, authenticated = true) {
  await page.route('**/api/**', (route) => {
    const pathname = new URL(route.request().url()).pathname
    if (pathname.endsWith('/stream'))
      return route.fulfill({ contentType: 'text/event-stream', body: ': fixture\n\n' })
    const lists: Record<string, unknown> = {
      agents: { agents: [] },
      projects: { projects: [] },
      tasks: { tasks: [] },
      skills: [],
      mcps: { mcps: [] },
      activity: [],
    }
    const output = pathname === '/api/account/session'
      ? {
          authenticated,
          account: { id: 'test-account', email: 'update@example.test' },
          csrf: 'test-csrf',
          installations: [{
            id: 'demo',
            slug: 'demo',
            name: 'Test installation',
            role: 'owner',
            online: false,
            updateRequired: false,
          }],
        }
      : lists[pathname.split('/').at(-1)!] ?? {}
    return route.fulfill({ json: output })
  })
}

test('offers recovery when Missions cannot load after a deployment', async ({ page, webUrl, previousBuild }) => {
  let deployed = false
  // Two actual builds give the old tab stale imports and the reload fresh URLs.
  // Reusing a failed module URL would test a transient outage, not a deployment.
  await page.route(`${webUrl}/**`, async (route) => {
    const pathname = new URL(route.request().url()).pathname
    if (pathname.startsWith('/api/'))
      return route.fallback()
    if (pathname === '/web-build.json' && !deployed)
      return route.fulfill({ json: { buildId: previousBuildId } })
    if (!route.request().isNavigationRequest() && !pathname.endsWith('.js'))
      return route.fallback()
    const directory = deployed ? 'dist' : previousBuild
    let file = path.join(directory, route.request().isNavigationRequest() ? 'index.html' : pathname)
    try {
      await readFile(file)
    }
    catch {
      file = path.join(directory, 'index.html')
    }

    return route.fulfill({ path: file, headers: { 'cache-control': 'no-store' } })
  })
  await account(page)
  await page.goto(`${webUrl}/installations/demo/atelier`)
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Reload Cairn' })).toHaveCount(0)
  deployed = true
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.getByRole('link', { name: 'Missions', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Reload Cairn' })).toContainText('This page couldn’t load.')
  await expect(page).toHaveURL(`${webUrl}/installations/demo/atelier`)
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await page.screenshot({ path: test.info().outputPath('web-update.png') })
  await page.getByRole('button', { name: 'Reload Cairn' }).click()
  await expect(page).toHaveURL(`${webUrl}/installations/demo/tasks`)
  await expect(page.getByRole('heading', { name: 'Missions', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Reload Cairn' })).toHaveCount(0)
  expect(errors).toEqual([])
})

test('keeps a failed initial route recoverable without a reload loop', async ({ page, webUrl }) => {
  await account(page)
  await page.route('**/assets/Atelier-*.js', route => route.abort('failed'))
  const destination = `${webUrl}/installations/demo/atelier?view=resources#projects`
  let documents = 0
  page.on('request', request => documents += Number(request.isNavigationRequest()))
  await page.goto(destination)
  await expect(page.getByRole('button', { name: 'Reload Cairn' })).toBeVisible()
  expect(documents).toBe(1)
  await page.getByRole('button', { name: 'Reload Cairn' }).click()
  await expect(page.getByRole('button', { name: 'Reload Cairn' })).toBeVisible()
  expect(documents).toBe(2)
  await page.unroute('**/assets/Atelier-*.js')
  await page.getByRole('button', { name: 'Reload Cairn' }).click()
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await expect(page).toHaveURL(destination)
  expect(documents).toBe(3)
})

test('announces a deployed build on focus and waits for the user to reload', async ({ page, webUrl }) => {
  await account(page, false)
  const current = JSON.parse(await readFile('dist/web-build.json', 'utf8'))
  let build = current
  await page.route('**/web-build.json', route => route.fulfill({ json: build }))
  await page.goto(`${webUrl}/?return_to=keep#draft`)
  const email = page.getByRole('textbox', { name: 'Email address', exact: true })
  await email.fill('draft@example.test')
  await expect(page.getByRole('button', { name: 'Reload Cairn' })).toHaveCount(0)
  build = { buildId: current.buildId === 'a'.repeat(64) ? 'b'.repeat(64) : 'a'.repeat(64) }
  await page.evaluate(() => window.dispatchEvent(new Event('focus')))
  await expect(page.getByRole('alert')).toContainText('A new version of Cairn is ready.')
  await expect(email).toHaveValue('draft@example.test')
  await expect(page).toHaveURL(`${webUrl}/?return_to=keep#draft`)
  await page.getByRole('button', { name: 'Reload Cairn' }).click()
  await expect(email).toBeEmpty()
  await expect(page).toHaveURL(`${webUrl}/?return_to=keep#draft`)
})
