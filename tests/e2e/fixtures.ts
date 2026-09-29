import type { Page } from '@playwright/test'
import type { Service } from '../legacy/server/service'
import { execFileSync, spawn } from 'node:child_process'
import { once } from 'node:events'
import {
  copyFile,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { test as base, expect } from '@playwright/test'
import { config as loadConfig } from '../legacy/server/config'
import { Service as SeedService } from '../legacy/server/service'
import { Store } from '../legacy/server/store'

export interface Workspace {
  service: Service
  restart: () => Promise<void>
  url: string
  projectPath: string
  api: (route: string, method?: string, body?: unknown) => Promise<any>
  setAccountUsage: (id: string, value: unknown) => Promise<void>
}

// Each Playwright project owns its application, database, worker, and limiter.
// Journeys retain their deliberately ordered persistence checks within one project.
export const test = base.extend<object, { workspace: Workspace }>({
  // Playwright requires a destructuring pattern even with no fixture dependencies.
  // eslint-disable-next-line no-empty-pattern
  workspace: [async ({}, use, workerInfo) => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'leo-browser-'))
    const home = path.join(directory, 'home')
    const projectPath = path.join(directory, 'project')
    const port = 4322 + workerInfo.parallelIndex
    const url = `http://127.0.0.1:${port}`
    await Promise.all([mkdir(home), mkdir(projectPath)])
    const config = loadConfig({
      dataDir: path.join(directory, 'data'),
      home,
      workspaceRoots: [projectPath],
      setupToken: 'browser-test-setup',
      codexBin: path.resolve('tests/fixtures/codex.mjs'),
      ghBin: '/nonexistent/fixture-gh',
      publicUrl: url,
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
    await writeFile(configuration, JSON.stringify({ ...config, claudeBin: path.resolve('tests/fixtures/claude.mjs') }))
    await writeFile(usageFile, '{}')
    // A developer may rebuild Cargo while this fixture is active. Keep the
    // supervisor's current executable stable for the entire browser journey.
    const binary = path.join(directory, 'leo')
    await copyFile(process.env.LEO_TEST_BINARY || path.resolve('target/debug/leo'), binary)
    let log = ''
    const start = () => {
      const child = spawn(binary, [], { env: { ...process.env, LEO_CONFIG: configuration, LEO_FIXTURE_USAGE: usageFile }, stdio: ['ignore', 'pipe', 'pipe'] })
      child.stdout.on('data', chunk => log += chunk)
      child.stderr.on('data', chunk => log += chunk)
      return child
    }

    let child = start()
    let closed = once(child, 'exit')
    const stop = async () => {
      child.kill('SIGTERM')
      const force = setTimeout(() => child.kill('SIGKILL'), 10000)
      await closed
      clearTimeout(force)
    }

    const ready = async () => {
      await expect.poll(async () => {
        if (child.exitCode !== null)
          throw new Error(`Native backend exited: ${log}`)
        return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
      }, { timeout: 15000 }).toBe(true)
    }

    let headers: Record<string, string> = { 'content-type': 'application/json' }
    const api = async (route: string, method = 'GET', body?: unknown) => {
      const response = await fetch(`${url}${route}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) })
      const result = await response.json()
      expect(response.ok, JSON.stringify(result)).toBe(true)
      if (response.headers.has('set-cookie'))
        headers = { ...headers, 'cookie': response.headers.get('set-cookie')!.split(';')[0], 'x-csrf-token': result.csrf }
      return result
    }

    try {
      await ready()
      if (workerInfo.project.name !== 'journeys') {
        await api('/api/setup', 'POST', { setupToken: 'browser-test-setup', password: 'browser-password-long-enough' })
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
        projectPath,
        service,
        api,
        restart: async () => {
          await stop()
          child = start()
          closed = once(child, 'exit')
          await ready()
        },
        setAccountUsage: async (id, value) => {
          usage[id] = value
          await writeFile(usageFile, JSON.stringify(usage))
        },
      })
    }
    finally {
      await stop()
      await service.accounts.close()
      service.store.close()
      await rm(directory, { recursive: true, force: true })
    }
  }, { scope: 'worker' }],
  baseURL: async ({ workspace }, use) => use(workspace.url),
})
export { expect } from '@playwright/test'

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

// Mobile keeps status in Chat details instead of a permanent metadata row.
export async function expectChatReady(page: Page) {
  const details = page.getByRole('button', { name: 'Chat details', exact: true })
  const mobile = await details.isVisible()
  if (mobile)
    await details.click()
  const context = mobile ? page.getByRole('dialog', { name: 'Chat details', exact: true }) : page
  await expect(context.getByText('Ready', { exact: true })).toBeVisible()
  if (mobile)
    await page.getByRole('button', { name: 'Close dialog', exact: true }).click()
}
