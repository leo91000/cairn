import { randomUUID } from 'node:crypto'
import { expect, test } from './fixtures'

test('registers, configures and revokes a node through the owner interface', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await page.goto('/')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.shell')).toBeVisible({ timeout: 30000 })
  await page.goto('/nodes')
  await page.getByRole('button', { name: 'Add a machine', exact: true }).click()
  await page.getByLabel('Machine name').fill('Browser Linux')
  await page.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await page.getByLabel('Single-use enrollment code').textContent()
  expect(code).toHaveLength(43)
  const response = await page.request.post(`${workspace.url}/internal/nodes/enroll`, {
    data: {
      code,
      name: 'Browser Linux',
      protocol: 1,
      runtimeId: 'fixture',
      capabilities: {
        os: 'linux',
        arch: 'x86_64',
        kvm: true,
        cpu: 8,
        memoryMiB: 16384,
        diskMiB: 65536,
      },
    },
  })
  expect(response.ok()).toBe(true)
  await page.reload()
  const node = page.getByRole('article').filter({ has: page.getByRole('heading', { name: 'Browser Linux' }) })
  await expect(node).toContainText('online')
  await expect(node).toContainText('No agent can use this machine yet.')
  await node.getByRole('button', { name: 'Choose agents' }).click()
  await page.getByRole('dialog').getByRole('checkbox').first().check()
  await page.getByRole('dialog').getByRole('button', { name: 'Save access' }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(node).toContainText('Used by')
  await expect(node).not.toContainText('No agent can use this machine yet.')
  await page.screenshot({ path: 'test-results/nodes-desktop.png' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.screenshot({ path: 'test-results/nodes-phone.png', fullPage: true })
  await node.getByRole('button', { name: 'Storage', exact: true }).click()
  const storage = page.getByRole('dialog', { name: 'Storage · Browser Linux' })
  await expect(storage.getByLabel('Clean cache budget (MiB)')).toBeVisible()
  await page.screenshot({ path: 'test-results/storage-phone.png' })
  await page.keyboard.press('Escape')
  await node.getByRole('button', { name: 'Configure' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('CPU ceiling').fill('4')
  await dialog.getByLabel('Tags, separated by commas').fill('fast, linux')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(node).toContainText('of 4 allowed')
  await expect(node).toContainText('fast · linux')
  await page.getByText('Advanced: S3 synchronization and timeouts').click()
  await page.getByRole('button', { name: 'Configure synchronization' }).click()
  await dialog.getByLabel('Synchronization target (seconds)').fill('45')
  await dialog.getByLabel('Pause after disconnection (seconds)').fill('30')
  await expect(dialog.getByLabel('Recovery points to retain')).toHaveCount(0)
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(await workspace.api('/api/nodes/settings')).toMatchObject({ intervalSeconds: 45, disconnectTimeoutSeconds: 30 })
  await node.getByRole('button', { name: 'Revoke', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Revoke node' }).click()
  await expect(node).toHaveCount(0)
  await page.getByRole('button', { name: 'Show revoked machines (1)' }).click()
  await expect(node).toContainText('revoked')
  await expect(node.getByRole('button', { name: 'Configure' })).toHaveCount(0)
})

test('conversation placement distinguishes a preference from a strict pin and shows synchronization age', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Inspect the fixture' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const invitation = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Recovery server' })
  const response = await page.request.post(`${workspace.url}/internal/nodes/enroll`, {
    data: {
      code: invitation.code,
      name: 'Recovery server',
      protocol: 1,
      runtimeId: 'fixture',
      capabilities: {
        os: 'linux',
        arch: 'x86_64',
        kvm: true,
        cpu: 8,
        memoryMiB: 16384,
        diskMiB: 65536,
      },
    },
  })
  expect(response.ok()).toBe(true)
  const node = await response.json()
  const agent = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === detail.run.snapshot.agent.id)
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { ...agent, access: { ...agent.access, nodes: [node.nodeId] } })
  workspace.service.store.updateRun(detail.run.id, { nodeId: node.nodeId, backup: { capturedAt: Date.now() - 120000, status: 'ready' } } as any)
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  const header = page.getByRole('button', { name: 'Execution node', exact: true }).filter({ visible: true })
  await expect(header).toContainText('Recovery server')
  await expect(header).toContainText('Synced 2 min ago', { timeout: 30000 })
  await header.click()
  const panel = page.getByRole('dialog', { name: 'Execution node', exact: true })
  await expect(panel).toContainText('Disk synchronized 2 min ago')
  const save = panel.getByRole('button', { name: 'Save preference' })
  await expect(save).toBeDisabled()
  await panel.getByRole('button', { name: 'Prefer a node' }).click()
  await panel.getByLabel('Node', { exact: true }).selectOption(node.nodeId)
  await save.click()
  await expect.poll(async () => (await workspace.api(`/api/nodes/placement/${detail.run.id}`)).preferredNodeId).toBe(node.nodeId)
  await expect(save).toBeDisabled()
  await panel.getByRole('button', { name: 'Fix to a node' }).click()
  await save.click()
  await expect.poll(async () => (await workspace.api(`/api/nodes/placement/${detail.run.id}`)).pinnedNodeId).toBe(node.nodeId)
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
  await expect(header).toBeFocused()
  workspace.service.store.updateRun(detail.run.id, {
    storage: {
      mode: 'on-demand',
      localBytes: 191392768,
      dirtyBytes: 183890000,
      dirtySince: Date.now() - 60000,
    },
    backup: { capturedAt: Date.now() - 120000, status: 'saving' },
  } as any)
  await expect(header).toContainText('Saving…', { timeout: 15000 })
  await header.click()
  await expect(panel).toContainText('182.5 MiB on this node')
  await expect(panel).toContainText('175.4 MiB not yet saved')
  await page.screenshot({ path: 'test-results/execution-desktop.png' })
  await page.keyboard.press('Escape')
  await page.setViewportSize({ width: 390, height: 844 })
  await header.click()
  await expect(panel).toBeVisible()
  const box = await panel.boundingBox()
  expect(box!.x).toBeGreaterThanOrEqual(0)
  expect(box!.x + box!.width).toBeLessThanOrEqual(390)
  await panel.getByRole('button', { name: 'Move…' }).click()
  const move = page.getByRole('dialog', { name: 'Move to another node', exact: true })
  await expect(move.getByLabel('Destination')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(move).toHaveCount(0)
  await page.screenshot({ path: 'test-results/execution-phone.png' })
  await page.mouse.click(5, 5)
  await expect(panel).toHaveCount(0)
  await expect(header).toBeFocused()
  workspace.service.store.updateRun(detail.run.id, { backup: { capturedAt: Date.now() - 120000, status: 'ready', error: 'Upload timed out' } } as any)
  await expect(header).toContainText('Sync failed', { timeout: 15000 })
})
