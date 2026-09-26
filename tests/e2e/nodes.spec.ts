import { expect, test } from './fixtures'

test('registers, configures and revokes a node through the owner interface', async ({ page, workspace }) => {
  await page.goto('/')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.shell')).toBeVisible()
  await page.goto('/nodes')
  await page.getByLabel('Machine name').fill('Browser Linux')
  await page.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await page.locator('code').textContent()
  expect(code).toHaveLength(43)
  const response = await page.request.post(`${workspace.url}/internal/nodes/enroll`, { data: {
    code,
    name: 'Browser Linux',
    protocol: 1,
    runtimeId: 'fixture',
    capabilities: { os: 'linux', arch: 'x86_64', kvm: true, cpu: 8, memoryMiB: 16384, diskMiB: 65536 },
  } })
  expect(response.ok()).toBe(true)
  await page.reload()
  const node = page.getByRole('article').filter({ has: page.getByRole('heading', { name: 'Browser Linux' }) })
  await expect(node).toContainText('online')
  await node.getByRole('button', { name: 'Configure' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('CPU ceiling').fill('4')
  await dialog.getByLabel('Tags, separated by commas').fill('fast, linux')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(node).toContainText('4 CPU')
  await expect(node).toContainText('fast · linux')
  await node.getByRole('button', { name: 'Revoke', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Revoke node' }).click()
  await expect(node).toContainText('revoked')
  await expect(node.getByRole('button', { name: 'Configure' })).toHaveCount(0)
})
