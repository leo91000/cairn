import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('email sign-in opens the empty installation screen, persists and signs out', async ({ page }) => {
  const messages: Array<{ to: string[], text: string }> = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body))
    response.writeHead(200, { 'content-type': 'application/json' }).end('{"id":"fixture"}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const mailPort = (mail.address() as { port: number }).port
  const url = 'http://127.0.0.1:4398'
  const child = spawn('target/debug/leo-official', [], {
    env: {
      ...process.env,
      LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
      LEO_OFFICIAL_ORIGIN: url,
      LEO_OFFICIAL_LISTEN: '127.0.0.1:4398',
      LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${mailPort}/emails`,
      LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
      LEO_OFFICIAL_EMAIL_FROM: 'Leo <leo@example.test>',
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  const exited = once(child, 'exit')
  let log = ''
  child.stderr.on('data', chunk => log += chunk)
  try {
    await expect.poll(async () => {
      if (child.exitCode !== null)
        throw new Error(`Official service exited: ${log}`)
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }).toBe(true)
    await page.goto(url)
    await page.getByLabel('Email address').fill(`browser-${Date.now()}@example.test`)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect(page.getByLabel('Email code')).toBeVisible()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill('wrong')
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Invalid or expired code')
    await page.getByLabel('Email code').fill(messages[0]!.text.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.reload()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 844 })
      await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible()
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    }

    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page.getByLabel('Email address')).toBeVisible()
    await page.reload()
    await expect(page.getByLabel('Email address')).toBeVisible()
  }
  finally {
    child.kill('SIGTERM')
    await exited
    await new Promise<void>(resolve => mail.close(() => resolve()))
  }
})
