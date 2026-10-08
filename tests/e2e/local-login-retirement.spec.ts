import { expect, test } from './fixtures'

test('uses the Leo account and refuses the retired local login endpoints', async ({ page, request }) => {
  for (const route of ['/api/setup', '/api/login', '/api/session', '/api/logout']) {
    const response = await request.post(route, {
      data: { setupToken: 'browser-test-setup', password: 'browser-password-long-enough' },
    })
    expect(response.status(), route).toBe(404)
    expect(response.headers()['set-cookie'], route).toBeUndefined()
  }

  await page.goto('/')
  await expect(page.getByLabel('Email address')).toBeVisible()
  await expect(page.getByLabel('Password', { exact: true })).toHaveCount(0)
  await expect(page.getByLabel('Setup token', { exact: true })).toHaveCount(0)
})

test('the default built web entry uses Leo account sign-in', async ({ page, workspace }) => {
  const { preview } = await import('vite')
  const server = await preview({
    configFile: false,
    preview: { host: '127.0.0.1', port: 0, proxy: { '/api': workspace.url } },
  })
  const address = server.httpServer.address() as { port: number }
  try {
    await page.goto(`http://127.0.0.1:${address.port}/`)
    await expect(page.getByLabel('Email address')).toBeVisible()
    await expect(page.getByLabel('Password', { exact: true })).toHaveCount(0)
    await expect(page.getByLabel('Setup token', { exact: true })).toHaveCount(0)
  }
  finally {
    await new Promise<void>((resolve, reject) => server.httpServer.close(error => error ? reject(error) : resolve()))
  }
})
