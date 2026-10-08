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
