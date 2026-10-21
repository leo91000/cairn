import type { Locator, Page } from '@playwright/test'
import type { Service } from '../fixtures/legacy/server/service'
import { execFileSync, spawn } from 'node:child_process'
import { once } from 'node:events'
import {
  copyFile,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import { createServer } from 'node:http'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { test as base, expect } from '@playwright/test'
import { config as loadConfig } from '../fixtures/legacy/server/config'
import { Service as SeedService } from '../fixtures/legacy/server/service'
import { Store } from '../fixtures/legacy/server/store'
import { beaconRelayFixture, executeBeaconSql } from './beacon-relay-fixture'

export interface Workspace {
  service: Service
  restart: () => Promise<void>
  url: string
  installationUrl: string
  projectPath: string
  api: (route: string, method?: string, body?: unknown) => Promise<any>
  signIn: (page: Page) => Promise<void>
  installationId: string
  installationSlug: string
  setAccountUsage: (id: string, value: unknown) => Promise<void>
  setOpenCodeGoUsage: (id: string, apiKey: string, value: unknown) => void
}

// One application per worker; navigation and sign-in share its real beacon context.
let currentWorkspace: Pick<Workspace, 'url' | 'installationId' | 'installationSlug' | 'signIn'> | undefined

export function workspacePath(route: string) {
  if (route.startsWith(currentWorkspace!.url))
    route = route.slice(currentWorkspace!.url.length)
  if (!route.startsWith('/') || route.startsWith('/installations/') || route.startsWith('/oauth') || route.startsWith('/authorize'))
    return route
  return `/installations/${currentWorkspace!.installationSlug}${route}`
}

export async function signIn(page: Page) {
  await currentWorkspace!.signIn(page)
}

// HTTP response fixtures must remain observable after background negotiation.
// Other journeys and the network bench still exercise the real direct transport.
export async function useRelayForHttpMocks(page: Page) {
  await page.route('**/direct/authorize', route => route.fulfill({ json: { available: false } }))
}

// Each Playwright project owns its application, database, worker, and limiter.
// Journeys retain their deliberately ordered persistence checks within one project.
export const test = base.extend<object, { workspace: Workspace }>({
  // Playwright requires a destructuring pattern even with no fixture dependencies.
  // eslint-disable-next-line no-empty-pattern
  workspace: [async ({}, use, workerInfo) => {
    const portBase = Number(process.env.CAIRN_BROWSER_PORT_BASE ?? 4322)
    if (!Number.isInteger(portBase) || portBase < 1024 || portBase + 100 + workerInfo.parallelIndex > 65535)
      throw new Error('CAIRN_BROWSER_PORT_BASE must select valid unprivileged installation and beacon ports.')
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-browser-'))
    const home = path.join(directory, 'home')
    const projectPath = path.join(directory, 'project')
    const port = portBase + workerInfo.parallelIndex
    const managerUrl = `http://127.0.0.1:${port}`
    const beacon = await beaconRelayFixture(port + 100)
    const url = beacon.url
    const database = new URL(process.env.CAIRN_BEACON_TEST_DATABASE_URL!)
    const schema = `browser_${crypto.randomUUID().replaceAll('-', '')}`
    executeBeaconSql(database, `CREATE SCHEMA ${schema}`)
    const isolatedDatabase = new URL(database)
    isolatedDatabase.searchParams.set('options', `-c search_path=${schema}`)
    const accountEmail = `${schema}@example.test`
    await Promise.all([mkdir(home), mkdir(projectPath)])
    const config = loadConfig({
      dataDir: path.join(directory, 'data'),
      home,
      workspaceRoots: [projectPath],
      codexBin: path.resolve('tests/fixtures/codex.mjs'),
      ghBin: '/nonexistent/fixture-gh',
      publicUrl: managerUrl,
      logger: false,
      host: '127.0.0.1',
      port,
      workerEnabled: true,
    })
    // The old service only seeds/inspects the shared database. Every HTTP request,
    // scheduled run, account refresh and chat is handled by the native process.
    const service = new SeedService(new Store(config.dataDir), config)
    const configuration = path.join(directory, 'config.json')
    const usageFile = path.join(directory, 'usage.json')
    const usage: Record<string, unknown> = {}
    const goUsage = new Map<string, unknown>()
    const go = createServer((request, response) => {
      const key = request.headers.authorization?.replace(/^Bearer /, '') ?? ''
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(goUsage.get(key) ?? {
        usage: {
          rolling: { status: 'ok', percent: 25, resetsAt: new Date(Date.now() + 7200_000).toISOString() },
          weekly: { status: 'ok', percent: 60, resetsAt: new Date(Date.now() + 86400_000).toISOString() },
          monthly: { status: 'ok', percent: 85, resetsAt: new Date(Date.now() + 604800_000).toISOString() },
        },
      }))
    })
    go.listen(0, '127.0.0.1')
    await once(go, 'listening')
    const goPort = (go.address() as { port: number }).port
    await writeFile(configuration, JSON.stringify({
      ...config,
      claudeBin: path.resolve('tests/fixtures/claude.mjs'),
      opencodeBin: path.resolve('tests/fixtures/opencode.mjs'),
      opencodeGoUsageUrl: `http://127.0.0.1:${goPort}/usage`,
    }))
    await writeFile(usageFile, '{}')
    // A developer may rebuild Cargo while this fixture is active. Keep the
    // supervisor's current executable stable for the entire browser journey.
    const binary = path.join(directory, 'cairn')
    await copyFile(process.env.CAIRN_TEST_BINARY || path.resolve('target/debug/cairn'), binary)
    let log = ''
    let claimCode = ''
    const start = () => {
      const child = spawn(binary, [], {
        env: {
          ...process.env,
          CAIRN_CONFIG: configuration,
          CAIRN_FIXTURE_USAGE: usageFile,
          CAIRN_BEACON_ORIGIN: url,
          CAIRN_INSTALLATION_CLAIM_CODE: claimCode,
          CAIRN_INSTALLATION_NAME: 'Browser workspace',
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      child.stdout.on('data', chunk => log += chunk)
      child.stderr.on('data', chunk => log += chunk)
      return child
    }

    let child: ReturnType<typeof start>
    let closed: ReturnType<typeof once>
    const stop = async () => {
      if (!child || child.exitCode !== null || child.signalCode !== null)
        return
      child.kill('SIGTERM')
      const force = setTimeout(() => child.kill('SIGKILL'), 10000)
      await closed
      clearTimeout(force)
    }

    const ready = async () => {
      await expect.poll(async () => {
        if (child.exitCode !== null)
          throw new Error(`Native backend exited: ${log}`)
        return fetch(`${managerUrl}/health`).then(response => response.ok).catch(() => false)
      }, { timeout: 15000 }).toBe(true)
    }

    let headers: Record<string, string> = { 'content-type': 'application/json', 'origin': url }
    let installationId = ''
    let installationSlug = ''
    const login = async () => {
      // Request a real email proof; never weaken the service's delivery limits.
      let challenge: { challenge: string } | undefined
      await expect.poll(async () => {
        const response = await fetch(`${url}/api/account/email-code`, {
          method: 'POST',
          headers,
          body: JSON.stringify({ email: accountEmail }),
        })
        if (response.ok)
          challenge = await response.json()
        else if (response.status !== 429)
          throw new Error(`Email sign-in failed: ${response.status}`)
        return response.ok
      }, { timeout: 70000 }).toBe(true)
      const code = beacon.messages.at(-1)!.match(/\b\d{8}\b/)![0]
      const response = await fetch(`${url}/api/account/verify`, {
        method: 'POST',
        headers,
        body: JSON.stringify({ challenge: challenge!.challenge, code }),
      })
      expect(response.ok).toBe(true)
      const session = await response.json()
      headers = { ...headers, 'cookie': response.headers.get('set-cookie')!.split(';')[0]!, 'x-csrf-token': session.csrf }
    }

    const ensureSession = async () => {
      const response = await fetch(`${url}/api/account/session`, { headers })
      if (!(await response.json()).authenticated) {
        // Logout revokes the shared proof. Re-sign-in must wait for the real
        // per-address delivery cooldown; assertions keep their own deadlines.
        const info = test.info()
        info.setTimeout(info.timeout + 70000)
        await login()
      }
    }

    const apiPath = (route: string) => route.startsWith('/api/')
      ? `/api/installations/${installationId}${route.startsWith('/api/tokens') ? route.slice(4) : route}`
      : route
    const api = async (route: string, method = 'GET', body?: unknown) => {
      await ensureSession()
      const response = await fetch(`${url}${apiPath(route)}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) })
      const result = await response.json()
      expect(response.ok, JSON.stringify(result)).toBe(true)
      return result
    }

    const signIn = async (page: Page) => {
      // Navigation's load event can precede the SPA's first session request.
      // Settle that view before adding cookies, so its installation redirect
      // cannot race the reload below.
      const initialAccountView = page.getByLabel('Email address').or(
        page.getByRole('button', { name: 'Sign out', exact: true, includeHidden: true }),
      )
      await expect(initialAccountView.first()).toBeAttached()

      // An authenticated root briefly renders Sign out before choosing its
      // installation. Settle that redirect before cookies or reload, while
      // keeping accounts without installations on their stable account page.
      if (new URL(page.url()).pathname === '/' && await page.getByRole('button', { name: 'Sign out', exact: true, includeHidden: true }).count()) {
        const existing = await (await page.request.get(`${url}/api/account/session`)).json()
        if (existing.authenticated && existing.installations.length) {
          await page.waitForURL(url => url.pathname.startsWith('/installations/'))
          await expect(initialAccountView.first()).toBeAttached()
        }
      }

      await ensureSession()
      const [name, value] = headers.cookie!.split('=')
      await page.context().addCookies([{
        name: name!,
        value: value!,
        url,
        httpOnly: true,
        sameSite: 'Lax',
      }])
      await page.reload()
      await expect(page.getByLabel('Email address')).toHaveCount(0)
    }

    try {
      const beaconChild = beacon.beacon(isolatedDatabase.toString())
      await expect.poll(async () => {
        expect(beaconChild.exitCode).toBeNull()
        return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
      }, { timeout: 60000 }).toBe(true)
      await login()
      const claim = await fetch(`${url}/api/installations/claim-code`, { method: 'POST', headers, body: '{}' })
      expect(claim.ok).toBe(true)
      claimCode = (await claim.json()).code
      child = start()
      closed = once(child, 'exit')
      await ready()
      await expect.poll(async () => {
        const response = await fetch(`${url}/api/installations`, { headers })
        const installations = await response.json()
        installationId = installations[0]?.id || ''
        installationSlug = installations[0]?.slug || installationId
        return installations[0]?.online === true
      }, { timeout: 15000 }).toBe(true)
      currentWorkspace = {
        url,
        installationId,
        installationSlug,
        signIn,
      }

      if (workerInfo.project.name !== 'journeys') {
        const agent = service.agent({ name: 'Release engineer' })
        const project = await service.project({ name: 'Design system', path: projectPath })
        await service.skills.save('review', '---\nname: review\ndescription: Review the project carefully\n---\nInspect the project and report checks.')
        const task = service.task({
          name: 'Weekly dependency review',
          prompt: 'Review dependencies and report the checks you ran. fixture:activity',
          agentId: agent.id,
          projectId: project.id,
          skills: ['global/review'],
          worktree: false,
          enabled: false,
          cron: '0 9 * * 1',
        })
        const run = await service.enqueue(task.id)
        await expect.poll(() => service.store.run(run.id)?.status, { timeout: 10000 }).toBe('succeeded')
        service.task({ ...task, prompt: 'fixture:hang' }, task.id)
        const cancelled = await service.enqueue(task.id)
        await api(`/api/runs/${cancelled.id}/cancel`, 'POST')
      }

      await use({
        url,
        installationUrl: managerUrl,
        installationId,
        installationSlug,
        signIn,
        projectPath,
        service,
        api,
        restart: async () => {
          await stop()
          claimCode = ''
          child = start()
          closed = once(child, 'exit')
          await ready()
          // Local readiness precedes reconnection to the beacon relay.
          await expect.poll(async () => {
            const response = await fetch(`${url}${apiPath('/api/agents')}`, { headers })
            return response.ok
          }, { timeout: 15000 }).toBe(true)
        },
        setAccountUsage: async (id, value) => {
          usage[id] = value
          await writeFile(usageFile, JSON.stringify(usage))
        },
        setOpenCodeGoUsage: (id, apiKey, value) => {
          goUsage.set(apiKey, value)
          // Let the next explicit refresh poll the fixture without waiting a minute.
          const row = service.store.db.prepare('SELECT data FROM records WHERE id=? AND kind=?').get(id, 'agentAccounts') as { data: string }
          const account = JSON.parse(row.data)
          if (account.usage)
            account.usage.attemptedAt = Date.now() - 60001
          service.store.db.prepare('UPDATE records SET data=? WHERE id=? AND kind=?').run(JSON.stringify(account), id, 'agentAccounts')
        },
      })
    }
    finally {
      await stop()
      await new Promise<void>(resolve => go.close(() => resolve()))
      await beacon.close()
      executeBeaconSql(database, `DROP SCHEMA ${schema} CASCADE`)
      currentWorkspace = undefined
      await service.accounts.close()
      service.store.close()
      await rm(directory, { recursive: true, force: true })
    }
  }, { scope: 'worker', timeout: 120000 }],
  baseURL: async ({ workspace }, use) => use(workspace.url),
})
export { expect } from '@playwright/test'

// Visible in the viewport is not enough inside dialogs: sticky headers and footers can cover feedback.
export async function expectUncovered(locator: Locator) {
  await expect.poll(() => locator.evaluate((element) => {
    const bounds = element.getBoundingClientRect()
    return element.contains(document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2))
  })).toBe(true)
}

export async function expectSingleScroll(page: Page) {
  const report = await page.evaluate(() => {
    const scrolls = (element: Element) => element.clientHeight > 0 && element.scrollHeight > element.clientHeight + 1 && /auto|scroll/.test(getComputedStyle(element).overflowY)
    const nested: string[] = []
    for (const element of document.querySelectorAll('*')) {
      if (element.matches('textarea') || element.closest('.vs-popup, .theme-popover') || !scrolls(element))
        continue
      for (let parent = element.parentElement; parent; parent = parent.parentElement) {
        if (scrolls(parent))
          nested.push(`${element.className} inside ${parent.className}`)
      }
    }

    return { root: document.documentElement.scrollHeight - innerHeight, horizontal: document.documentElement.scrollWidth - innerWidth, nested }
  })
  expect(report).toEqual({ root: 0, horizontal: 0, nested: [] })
}

export function initializeRepository(projectPath: string) {
  execFileSync('git', ['init', '-b', 'main', projectPath])
  execFileSync('git', ['-C', projectPath, '-c', 'user.name=Test', '-c', 'user.email=test@example.test', 'commit', '--allow-empty', '-m', 'Initial'])
}

// A followed reader stays at the end when its viewport shrinks; a paused one keeps its offset.
export async function expectFollowing(page: Page) {
  const scroller = page.getByRole('region', { name: 'Activity output' })
  const viewport = page.viewportSize()!
  await page.setViewportSize({ ...viewport, height: viewport.height - 120 })
  await expect.poll(() => scroller.evaluate(el => el.scrollHeight - el.clientHeight - el.scrollTop)).toBeLessThan(2)
  await page.setViewportSize(viewport)
}

// Mobile keeps status in Chat details instead of a permanent metadata row.
export async function expectChatReady(page: Page) {
  const details = page.getByRole('button', { name: 'Chat details', exact: true })
  const mobile = (page.viewportSize()?.width ?? 1280) <= 640
  if (mobile)
    await details.click()
  const context = mobile ? page.getByRole('dialog', { name: 'Chat details', exact: true }) : page
  await expect(context.getByText('Ready', { exact: true })).toBeVisible()
  if (mobile)
    await page.getByRole('button', { name: 'Close dialog', exact: true }).click()
}

// The agent page: sections open from its navigation, on phones through the section list.
export async function openAgentSection(page: Page, name: string) {
  const heading = page.getByRole('heading', { name, exact: true, level: 2 })
  // Phones hide the section list while a section is open.
  await expect(page.locator('nav[aria-label="Agent sections"]')).toBeAttached()
  if (await heading.isVisible())
    return
  const list = page.getByRole('link', { name: 'All settings', exact: true })
  if (await list.isVisible())
    await list.click()
  const pattern = new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`)
  await page.getByRole('navigation', { name: 'Agent sections' }).getByRole('link', { name: pattern }).click()
  await expect(heading).toBeFocused()
}

// New agents start restricted, and restricted agents need an isolated runner. Journeys that
// run a drafted agent on the shared runner grant it everything, as the owner would.
export async function grantFullAccess(page: Page) {
  for (const [section, group] of [['Projects', 'Projects access'], ['Skills', 'Skills access'], ['MCP & tools', 'MCP connections access']]) {
    await openAgentSection(page, section!)
    await page.getByRole('group', { name: group }).getByRole('button', { name: 'All, including future', exact: true }).click()
  }
}

// Creates the agent drafted on /agents/new and waits for its own page.
export async function createAgent(page: Page) {
  await page.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/)
  return page.url().split('/').at(-1)!
}

// Saves the agent page; `review` confirms the review dialog that powers and secrets require.
export async function saveAgent(page: Page, review = false) {
  const bar = page.getByRole('region', { name: 'Unsaved changes' })
  await bar.getByRole('button', { name: 'Save', exact: true }).click()
  if (review) {
    const dialog = page.getByRole('dialog', { name: 'Review before saving' })
    await dialog.getByRole('button', { name: 'Save', exact: true }).click()
    await expect(dialog).toHaveCount(0)
  }

  await expect(bar).toHaveCount(0)
}
