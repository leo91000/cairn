import { randomUUID } from 'node:crypto'
import { MAIN_AGENT_ID } from '../../packages/contracts/constants'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('registers, configures and revokes a node through the owner interface', async ({ page, workspace }) => {
  test.setTimeout(60000)
  // Requests are observed below, so they must go through HTTP.
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible({ timeout: 30000 })
  await page.goto(workspacePath('/nodes'))
  await page.getByRole('button', { name: 'Add a machine', exact: true }).click()
  await page.getByLabel('Machine name').fill('Browser Linux')
  await page.getByLabel('Direct manager address').fill(workspace.url)
  await page.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await page.getByLabel('Single-use enrollment code').textContent()
  expect(code).toHaveLength(43)
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
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
        diskTotalMiB: 131072,
      },
    },
  })
  expect(response.ok()).toBe(true)
  // Once the machine connects, the last step asks who may use it; nothing is preselected.
  const grant = page.getByRole('dialog', { name: 'Who can use “Browser Linux”?' })
  await expect(grant).toBeVisible({ timeout: 20000 })
  await expect(grant).toContainText('You can change this later on each agent’s page.')
  await expect(grant.getByRole('checkbox', { name: 'Main agent', exact: true })).not.toBeChecked()
  await page.screenshot({ path: 'test-results/nodes-enrollment-grant.png' })
  await grant.getByRole('checkbox', { name: 'Main agent', exact: true }).check()
  await grant.getByRole('button', { name: 'Finish', exact: true }).click()
  await expect(grant).toHaveCount(0)
  const node = page.getByRole('article').filter({ has: page.getByRole('heading', { name: 'Browser Linux' }) })
  await expect(node).toContainText('online')
  await expect(node).toContainText('1 agent can run here: Main agent')
  await expect(node).not.toContainText('No agent can use this machine yet.')
  await expect(node.getByRole('button', { name: /Choose agents|^Agents$/ })).toHaveCount(0)
  await expect(node.getByRole('link', { name: 'Manage access from the agents ›' })).toBeVisible()
  // It asks only once: the next refresh leaves the machine alone.
  await page.waitForResponse(reply => new URL(reply.url()).pathname.endsWith('/nodes') && reply.request().method() === 'GET', { timeout: 15000 })
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.screenshot({ path: 'test-results/nodes-desktop.png' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: 'test-results/nodes-desktop-dark.png' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.screenshot({ path: 'test-results/nodes-phone.png', fullPage: true })
  await node.getByRole('button', { name: 'Storage', exact: true }).click()
  const storage = page.getByRole('dialog', { name: 'Storage · Browser Linux' })
  await expect(storage.getByLabel('Clean disk cache budget (MiB)')).toBeVisible()
  await expect(storage.getByLabel('Shared memory cache (MiB)')).toHaveValue('256')
  await page.screenshot({ path: 'test-results/storage-phone.png' })
  await page.keyboard.press('Escape')
  await node.getByRole('button', { name: 'Configure' }).click()
  const dialog = page.getByRole('dialog')
  // Agent access is shown read-only; it is granted on each agent's page.
  await expect(dialog.getByRole('group', { name: 'Exclusive reservations' })).toHaveCount(0)
  await expect(dialog.getByRole('region', { name: 'Agents' })).toContainText('1 agent can run here: Main agent')
  const accept = await dialog.getByRole('checkbox', { name: 'Accept new work' }).boundingBox()
  const acceptLabel = await dialog.locator('label').filter({ hasText: 'Accept new work' }).boundingBox()
  expect(Math.abs(accept!.y + accept!.height / 2 - (acceptLabel!.y + acceptLabel!.height / 2))).toBeLessThan(2)
  expect(accept!.width).toBeLessThan(24)
  await dialog.getByLabel('Shared CPU budget').fill('4')
  await expect(dialog.getByLabel('Shared disk budget (GiB)')).toHaveAttribute('max', '128')
  await dialog.getByLabel('Shared disk budget (GiB)').fill('96')
  await dialog.getByLabel('Execution slots').fill('12')
  await dialog.getByLabel('Tags, separated by commas').fill('fast, linux')
  const configuring = page.waitForRequest(request => request.method() === 'PUT' && /\/nodes\/[^/]+$/.test(new URL(request.url()).pathname))
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect((await configuring).postDataJSON()).not.toHaveProperty('exclusiveAgents')
  await expect(node).toContainText('4 cores')
  await expect(node).toContainText('fast · linux')
  await expect(node).toContainText('12 of 12')
  const configured = (await workspace.api('/api/nodes')).find((value: { name: string }) => value.name === 'Browser Linux')
  expect(configured.limits.diskMiB).toBe(98304)
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
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
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
  const heartbeat = async () => {
    // Legacy seeds bypass the manager's writer. A node heartbeat supplies a
    // real commit notification instead of relying on an empty scheduler pass.
    const reply = await page.request.post(`${workspace.installationUrl}/internal/nodes/heartbeat`, {
      headers: { authorization: `Bearer ${node.token}` },
      data: {},
    })
    expect(reply.ok()).toBe(true)
  }

  const destinationInvitation = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Destination server' })
  const destinationResponse = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code: destinationInvitation.code,
      name: 'Destination server',
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
  expect(destinationResponse.ok()).toBe(true)
  const destinationNode = await destinationResponse.json()
  const agent = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === detail.run.snapshot.agent.id)
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { ...agent, access: { ...agent.access, nodes: [node.nodeId, destinationNode.nodeId] } })
  workspace.service.store.updateRun(detail.run.id, { nodeId: node.nodeId, resources: { cpu: 2, memoryMiB: 4096, diskMiB: 32768 }, backup: { capturedAt: Date.now() - 120000, status: 'ready' } } as any)
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const header = page.getByRole('button', { name: 'Execution node', exact: true }).filter({ visible: true })
  await expect(header).toContainText('Recovery server')
  await expect(header).toContainText('Synced 2 min ago', { timeout: 30000 })
  await header.click()
  const panel = page.getByRole('dialog', { name: 'Execution node', exact: true })
  await expect(panel).toContainText('Disk synchronized 2 min ago')
  const save = panel.getByRole('button', { name: 'Save preference' })
  await expect(save).toBeDisabled()
  await panel.getByRole('button', { name: 'Prefer a node' }).click()
  await panel.getByRole('combobox', { name: 'Node', exact: true }).click()
  await panel.getByRole('option', { name: 'Recovery server', exact: true }).click()
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
  await heartbeat()
  await expect(header).toContainText('Saving…', { timeout: 15000 })
  await header.click()
  await expect(panel).toContainText('182.5 MiB on this node')
  await expect(panel).toContainText('175.4 MiB not yet saved')
  await page.screenshot({ path: 'test-results/execution-desktop.png' })
  await page.keyboard.press('Escape')
  await page.setViewportSize({ width: 390, height: 844 })
  // Phones keep the runner in the Connection sheet, not above the conversation.
  await expect(header).toHaveCount(0)
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('dialog', { name: 'Chat details', exact: true }).getByRole('button', { name: 'Connection', exact: true }).click()
  const connection = page.getByRole('dialog', { name: 'Connection', exact: true })
  await expect(connection.getByRole('button', { name: 'Execution node', exact: true })).toContainText('Recovery server')
  await header.click()
  await expect(panel).toBeVisible()
  // From the low Connection sheet the details open as a full-width bottom sheet, not a squeezed popover.
  const box = await panel.boundingBox()
  expect(box!.x).toBe(0)
  expect(box!.width).toBe(390)
  expect(Math.round(box!.y + box!.height)).toBe(844)
  expect(box!.height).toBeGreaterThan(360)
  expect(await panel.evaluate(element => element.scrollHeight - element.clientHeight)).toBeLessThanOrEqual(1)
  await expect(panel.getByRole('button', { name: 'Save preference' })).toBeInViewport()
  await panel.getByRole('button', { name: 'Move…' }).click()
  const move = page.getByRole('dialog', { name: 'Move to another node', exact: true })
  await move.getByRole('combobox', { name: 'Destination', exact: true }).click()
  await move.getByRole('option', { name: 'Destination server', exact: true }).click()
  await expect(move.getByRole('combobox', { name: 'Destination', exact: true })).toHaveValue('Destination server')
  await expect(move.getByRole('button', { name: 'Move now' })).toBeEnabled()
  await expect(move.getByRole('spinbutton')).toHaveCount(0)
  await page.keyboard.press('Escape')
  await expect(move).toHaveCount(0)
  await page.screenshot({ path: 'test-results/execution-phone.png' })
  await page.mouse.click(5, 5)
  await expect(panel).toHaveCount(0)
  await expect(header).toBeFocused()
  workspace.service.store.updateRun(detail.run.id, { backup: { capturedAt: Date.now() - 120000, status: 'ready', error: 'Upload timed out' } } as any)
  await heartbeat()
  await expect(header).toContainText('Sync failed', { timeout: 15000 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await header.click()
  await expect(panel).toContainText('Upload timed out')
  await page.screenshot({ path: 'test-results/execution-phone-error-dark.png' })
  await page.keyboard.press('Escape')
  await connection.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await expect(connection).toHaveCount(0)

  // Members see their role but neither the runner nor the installation settings.
  await page.route('**/api/account/session', async (route) => {
    const session = await (await route.fetch()).json()
    session.installations = session.installations.map((installation: object) => ({ ...installation, role: 'member' }))
    await route.fulfill({ json: session })
  })
  await page.reload()
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('dialog', { name: 'Chat details', exact: true }).getByRole('button', { name: 'Connection', exact: true }).click()
  await expect(connection.getByText('Role', { exact: true }).locator('..')).toContainText('Member')
  await expect(connection.getByText('Runner', { exact: true })).toHaveCount(0)
  await expect(connection.getByRole('button', { name: 'Execution node', exact: true })).toHaveCount(0)
  await expect(connection.getByRole('button', { name: 'Installation settings', exact: true })).toHaveCount(0)
  await connection.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await page.unroute('**/api/account/session')
  await page.reload()
  const composer = page.getByRole('textbox', { name: 'Message', exact: true })
  await composer.focus()
  await page.setViewportSize({ width: 390, height: 700 })
  await expect(composer).toBeFocused()
})

test('finishing the first step keeps an agent allowed while it was open', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const nightShift = await workspace.api('/api/agents', 'POST', { name: 'Night shift', access: { nodes: [] } })
  // The server only grants agents of the node's backend: a native agent is shown, but cannot be chosen.
  // Native agents need a native node, so this one is only listed by the page.
  await page.route('**/api/agents', async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue()
      return
    }

    const response = await route.fetch()
    const agents = await response.json()
    const main = agents.find((item: { id: string }) => item.id === MAIN_AGENT_ID)
    await route.fulfill({
      response,
      json: [...agents, {
        ...main,
        id: randomUUID(),
        name: 'Native helper',
        access: { ...main.access, backend: 'native', nodes: [randomUUID()] },
      }],
    })
  })
  // The finishing request is observed below, so it must go through HTTP.
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible({ timeout: 30000 })
  await page.goto(workspacePath('/nodes'))
  await page.getByRole('button', { name: 'Add a machine', exact: true }).click()
  await page.getByLabel('Machine name').fill('Shared Linux')
  await page.getByLabel('Direct manager address').fill(workspace.url)
  await page.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await page.getByLabel('Single-use enrollment code').textContent()
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code,
      name: 'Shared Linux',
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
  const { nodeId } = await response.json()
  const grant = page.getByRole('dialog', { name: 'Who can use “Shared Linux”?' })
  await expect(grant).toBeVisible({ timeout: 20000 })
  const night = grant.getByRole('checkbox', { name: /^Night shift/ })
  await expect(night).not.toBeChecked()
  await expect(night).toBeEnabled()
  const native = grant.getByRole('checkbox', { name: /^Native helper/ })
  await expect(native).toBeDisabled()
  await expect(native).not.toBeChecked()
  await expect(grant.locator('label').filter({ hasText: 'Native helper' })).toContainText('Uses native execution: only native nodes can run it')

  // Meanwhile, the owner allows Night shift on this machine from its agent page in another window.
  const agent = (await workspace.api('/api/agents')).find((item: { id: string }) => item.id === nightShift.id)
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { ...agent, access: { ...agent.access, nodes: [nodeId] } })
  await expect(night).toBeChecked({ timeout: 15000 })
  await expect(night).toBeDisabled()
  await expect(grant.locator('label').filter({ hasText: 'Night shift' })).toContainText('Already allowed on this machine')

  const finishing = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/nodes/${nodeId}/agents`))
  await grant.getByRole('checkbox', { name: 'Main agent', exact: true }).check()
  await grant.getByRole('button', { name: 'Finish', exact: true }).click()
  await expect(grant).toHaveCount(0)
  // Finishing only adds the chosen agents: it never replaces the machine's list.
  expect((await finishing).postDataJSON()).toEqual({ agentIds: [MAIN_AGENT_ID], add: true })
  const saved = (await workspace.api('/api/nodes')).find((node: { id: string }) => node.id === nodeId)
  expect(saved.agents.map((item: { id: string }) => item.id).sort()).toEqual([MAIN_AGENT_ID, nightShift.id].sort())
})

test('native enrollment requires explicit people and native agent grants', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const nativeCapabilities = {
    backend: 'native',
    os: 'linux',
    arch: 'x86_64',
    cpu: 8,
    memoryMiB: 16384,
    diskMiB: 65536,
    kvm: false,
    fuse: false,
  }
  // A native agent already runs on another native machine.
  const laptopInvitation = await workspace.api('/api/nodes/enrollments', 'POST', { name: 'Native laptop', backend: 'native' })
  const laptop = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code: laptopInvitation.code,
      name: 'Native laptop',
      protocol: 1,
      runtimeId: 'fixture',
      capabilities: nativeCapabilities,
    },
  })
  expect(laptop.ok()).toBe(true)
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Native worker', access: { backend: 'native', nodes: [(await laptop.json()).nodeId] } })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/nodes'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Add a machine', exact: true }).click()
  const enrollment = page.getByRole('dialog', { name: 'Add a machine', exact: true })
  await enrollment.getByRole('combobox', { name: 'Execution backend' }).click()
  await page.getByRole('option', { name: 'Native process · trusted people only', exact: true }).click()
  await expect(enrollment).toContainText('weaker isolation')
  await enrollment.getByLabel('Machine name').fill('Native desktop')
  await enrollment.getByLabel('Direct manager address').fill(workspace.url)
  await enrollment.getByRole('button', { name: 'Create enrollment code' }).click()
  const code = await enrollment.getByLabel('Single-use enrollment code').textContent()
  const response = await page.request.post(`${workspace.installationUrl}/internal/nodes/enroll`, {
    data: {
      code,
      name: 'Native desktop',
      protocol: 1,
      runtimeId: 'fixture',
      capabilities: nativeCapabilities,
    },
  })
  expect(response.ok()).toBe(true)
  const desktop = (await response.json()).nodeId as string

  // Once connected, the machine is configured first: trusted people, then who may use it.
  const dialog = page.getByRole('dialog', { name: 'Configure node' })
  await expect(dialog).toBeVisible({ timeout: 20000 })
  await expect(dialog.getByLabel('Execution slots')).toBeDisabled()
  await expect(dialog.getByRole('group', { name: 'Exclusive reservations' })).toHaveCount(0)
  await expect(dialog.getByLabel('Enable native execution')).not.toBeChecked()
  const callers = await workspace.api('/api/nodes/native-callers')
  expect(callers.length).toBeGreaterThan(0)
  await dialog.getByLabel(callers[0].email, { exact: true }).check()
  await dialog.getByLabel('Enable native execution').check()
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  const configured = (await workspace.api('/api/nodes')).find((value: { id: string }) => value.id === desktop)
  expect(configured).toMatchObject({ nativeEnabled: true, nativeAccounts: [callers[0].id], slots: 1 })

  const grant = page.getByRole('dialog', { name: 'Who can use “Native desktop”?' })
  await expect(grant).toBeVisible()
  const main = grant.getByRole('checkbox', { name: /^Main agent/ })
  await expect(main).toBeDisabled()
  await expect(grant.locator('label').filter({ hasText: 'Main agent' })).toContainText('Runs in a Firecracker VM: set it to native execution on its page first')
  const finishing = page.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname.endsWith(`/nodes/${desktop}/agents`))
  await grant.getByRole('checkbox', { name: /^Native worker/ }).check()
  await grant.getByRole('button', { name: 'Finish', exact: true }).click()
  await expect(grant).toHaveCount(0)
  expect((await finishing).postDataJSON()).toEqual({ agentIds: [agent.id], add: true })
  const node = page.getByRole('article').filter({ has: page.getByRole('heading', { name: 'Native desktop' }) })
  await expect(node).toContainText('Native process')
  await expect(node.getByRole('button', { name: 'Storage', exact: true })).toHaveCount(0)
  // Agents are granted on their own page; the card lists them read-only.
  await expect(node).toContainText(`1 agent can run here: ${agent.name}`)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).access.nodes).toContain(desktop)

  await node.getByRole('button', { name: 'Configure', exact: true }).click()
  await expect(dialog.getByLabel(callers[0].email, { exact: true })).toBeChecked()
  await dialog.getByLabel('Enable native execution').uncheck()
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  // Configuring it again does not ask who may use it.
  await expect(grant).toHaveCount(0)
  expect((await workspace.api('/api/nodes')).find((value: { id: string }) => value.id === desktop).nativeEnabled).toBe(false)
})
